//! P13.9 — Tool argument linting / schema repair (G‑R6.4).
//!
//! Pre‑call hook: validates a JSON arg payload against a tool's
//! `input_schema` (a JSON‑Schema‑like object). Performs *gentle* repair
//! in the SWE‑agent spirit:
//! - Inject `default` values for missing optional fields.
//! - Coerce numeric ↔ string for required scalar fields.
//! - Coerce a single JSON value to a one‑element array when the schema
//!   demands `array` (forgiving the LLM that returned the bare item).
//!
//! Returns the (possibly‑repaired) arg payload on success, or
//! [`LintError`] describing the first un‑repairable problem.
//!
//! Cite: Yang et al., *SWE‑agent*, NeurIPS 2024 (arXiv:2405.15793) —
//! "interface engineering" reduces tool‑call failure rate ~30 % via
//! schema‑aware argument repair.

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LintError {
    /// `input` was not an object but the schema declared `type: object`.
    NotAnObject,
    /// A `required` field was missing and could not be repaired (no `default`).
    MissingRequired { field: String },
    /// A field's value did not match its declared type and could not be coerced.
    TypeMismatch {
        field: String,
        expected: String,
        actual: String,
    },
    /// `enum` validation failed and no fuzzy match was possible.
    EnumViolation {
        field: String,
        allowed: Vec<String>,
        actual: String,
    },
}

impl std::fmt::Display for LintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnObject => write!(f, "tool input is not a JSON object"),
            Self::MissingRequired { field } => {
                write!(f, "missing required field '{field}' (no default)")
            }
            Self::TypeMismatch {
                field,
                expected,
                actual,
            } => write!(
                f,
                "field '{field}' expected type '{expected}', got '{actual}'"
            ),
            Self::EnumViolation {
                field,
                allowed,
                actual,
            } => write!(
                f,
                "field '{field}' has value '{actual}' but must be one of [{}]",
                allowed.join(", ")
            ),
        }
    }
}

impl std::error::Error for LintError {}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(v: &Value, target: &str) -> bool {
    match (target, v) {
        ("integer", Value::Number(n)) => n.is_i64() || n.is_u64() || n.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false),
        ("number", Value::Number(_)) => true,
        _ => json_type_name(v) == target,
    }
}

/// Try to coerce `v` to the schema-declared type. Returns `Some(coerced)` on
/// success, `None` if no safe coercion exists.
fn try_coerce(v: &Value, target: &str) -> Option<Value> {
    if type_matches(v, target) {
        return Some(v.clone());
    }
    match (v, target) {
        // number → string: lossless.
        (Value::Number(n), "string") => Some(Value::String(n.to_string())),
        // bool → string.
        (Value::Bool(b), "string") => Some(Value::String(b.to_string())),
        // string → number: only if it parses cleanly.
        (Value::String(s), "number") | (Value::String(s), "integer") => {
            if let Ok(i) = s.parse::<i64>() {
                Some(Value::Number(i.into()))
            } else if let Ok(f) = s.parse::<f64>() {
                serde_json::Number::from_f64(f).map(Value::Number)
            } else {
                None
            }
        }
        // string "true"/"false" → bool.
        (Value::String(s), "boolean") => match s.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(Value::Bool(true)),
            "false" | "0" | "no" => Some(Value::Bool(false)),
            _ => None,
        },
        // bare scalar → 1‑element array.
        (
            Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Object(_),
            "array",
        ) => Some(Value::Array(vec![v.clone()])),
        _ => None,
    }
}

/// Run lint + repair against `schema`. Returns the (possibly mutated)
/// payload or the first un‑repairable error.
pub fn lint(input: &Value, schema: &Value) -> Result<Value, LintError> {
    // Top‑level type guard.
    let schema_type = schema.get("type").and_then(|v| v.as_str());
    if schema_type == Some("object") && !input.is_object() {
        return Err(LintError::NotAnObject);
    }
    let Value::Object(input_map) = input else {
        // Non‑object schemas: just validate scalar type.
        if let Some(t) = schema_type {
            if let Some(coerced) = try_coerce(input, t) {
                return Ok(coerced);
            }
            return Err(LintError::TypeMismatch {
                field: "<root>".into(),
                expected: t.into(),
                actual: json_type_name(input).into(),
            });
        }
        return Ok(input.clone());
    };
    let mut out: Map<String, Value> = input_map.clone();

    let props_owner = schema.get("properties").and_then(|v| v.as_object());
    let required: Vec<String> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if let Some(props) = props_owner {
        // 1. Inject defaults for missing optional fields.
        for (name, prop_schema) in props.iter() {
            if !out.contains_key(name) {
                if let Some(def) = prop_schema.get("default") {
                    out.insert(name.clone(), def.clone());
                }
            }
        }
        // 2. Coerce / validate present fields.
        let keys: Vec<String> = out.keys().cloned().collect();
        for k in keys {
            let Some(prop_schema) = props.get(&k) else {
                continue; // unknown field — pass through.
            };
            let cur = out.get(&k).cloned().unwrap_or(Value::Null);
            if let Some(t) = prop_schema.get("type").and_then(|v| v.as_str()) {
                if !type_matches(&cur, t) {
                    match try_coerce(&cur, t) {
                        Some(coerced) => {
                            out.insert(k.clone(), coerced);
                        }
                        None => {
                            return Err(LintError::TypeMismatch {
                                field: k,
                                expected: t.into(),
                                actual: json_type_name(&cur).into(),
                            });
                        }
                    }
                }
            }
            // Enum validation (after coercion).
            if let Some(allowed) = prop_schema.get("enum").and_then(|v| v.as_array()) {
                let cur = out.get(&k).cloned().unwrap_or(Value::Null);
                if !allowed.iter().any(|a| a == &cur) {
                    let allowed_strs: Vec<String> =
                        allowed.iter().map(|v| v.to_string()).collect();
                    return Err(LintError::EnumViolation {
                        field: k,
                        allowed: allowed_strs,
                        actual: cur.to_string(),
                    });
                }
            }
        }
    }
    // 3. Required‑field check.
    for r in &required {
        if !out.contains_key(r) {
            return Err(LintError::MissingRequired { field: r.clone() });
        }
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "lines": {"type": "integer"},
                "force": {"type": "boolean", "default": false},
                "mode": {"type": "string", "enum": ["read", "write"]},
                "tags": {"type": "array"}
            },
            "required": ["path"]
        })
    }

    #[test]
    fn p13_9_passes_clean_input_unchanged() {
        let input = json!({"path": "/x", "lines": 5, "mode": "read"});
        let out = lint(&input, &schema()).unwrap();
        assert_eq!(out.get("path").unwrap(), "/x");
        assert_eq!(out.get("force").unwrap(), false); // default injected
    }

    #[test]
    fn p13_9_rejects_missing_required_with_no_default() {
        let input = json!({"lines": 5});
        match lint(&input, &schema()) {
            Err(LintError::MissingRequired { field }) => assert_eq!(field, "path"),
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn p13_9_coerces_string_to_integer() {
        let input = json!({"path": "/x", "lines": "42"});
        let out = lint(&input, &schema()).unwrap();
        assert_eq!(out.get("lines").unwrap(), 42);
    }

    #[test]
    fn p13_9_coerces_number_to_string() {
        let schema = json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        });
        let input = json!({"name": 7});
        let out = lint(&input, &schema).unwrap();
        assert_eq!(out.get("name").unwrap(), "7");
    }

    #[test]
    fn p13_9_coerces_string_to_bool() {
        let input = json!({"path": "/x", "force": "true"});
        let out = lint(&input, &schema()).unwrap();
        assert_eq!(out.get("force").unwrap(), true);
    }

    #[test]
    fn p13_9_wraps_scalar_in_array() {
        let input = json!({"path": "/x", "tags": "hot"});
        let out = lint(&input, &schema()).unwrap();
        assert_eq!(out.get("tags").unwrap(), &json!(["hot"]));
    }

    #[test]
    fn p13_9_rejects_unrepairable_type() {
        let input = json!({"path": "/x", "lines": "not a number"});
        match lint(&input, &schema()) {
            Err(LintError::TypeMismatch { field, .. }) => assert_eq!(field, "lines"),
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn p13_9_enforces_enum() {
        let input = json!({"path": "/x", "mode": "delete"});
        match lint(&input, &schema()) {
            Err(LintError::EnumViolation { field, .. }) => assert_eq!(field, "mode"),
            other => panic!("expected EnumViolation, got {other:?}"),
        }
    }

    #[test]
    fn p13_9_passes_unknown_fields_through() {
        let input = json!({"path": "/x", "extra_field": 99});
        let out = lint(&input, &schema()).unwrap();
        assert_eq!(out.get("extra_field").unwrap(), 99);
    }

    #[test]
    fn p13_9_rejects_non_object_against_object_schema() {
        let input = json!("not an object");
        assert_eq!(lint(&input, &schema()), Err(LintError::NotAnObject));
    }

    #[test]
    fn p13_9_default_injection_for_missing_optional() {
        let input = json!({"path": "/x"});
        let out = lint(&input, &schema()).unwrap();
        assert_eq!(out.get("force").unwrap(), false);
    }
}
