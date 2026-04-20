//! P11.4 — Resource-key extraction for MCP tool inputs.
//!
//! The orchestrator's resource-aware parallel dispatcher
//! ([`caduceus_tools::ToolRegistry::execute_parallel_locked`])
//! serialises tasks whose declared `resource_keys` overlap and runs
//! disjoint tasks concurrently. Native first-party tools override
//! [`caduceus_tools::Tool::resource_keys`] with input-specific keys
//! (e.g. the file path for `read_file`).
//!
//! MCP-bridged tools cannot do that on their own — their input shape
//! is server-defined. Without a sensible default they all fall through
//! to the dispatcher's `__global__` sentinel and serialise against
//! every other tool in the batch, even when the calls touch entirely
//! different files / URLs.
//!
//! This module provides a small heuristic that extracts the most
//! common resource-identifying fields (`path`, `file`, `filename`,
//! `uri`, `url`) from the input object. The MCP→Tool bridge (added
//! later) will plug these directly into its `Tool::resource_keys`
//! impl, so MCP calls participate in the same fine-grained per-key
//! locking as native tools.
//!
//! Stateless and pure — safe to call from any thread, no I/O.

use serde_json::Value;

/// Field names — in priority order — that are treated as identifying
/// the resource a tool will touch. Order matters only for documentation
/// purposes; all matching keys are emitted.
pub const RESOURCE_FIELDS: &[&str] = &[
    "path", "file", "filename", "filepath", "uri", "url", "resource",
];

/// Extract a deduplicated, deterministically-ordered list of resource
/// keys from the JSON input the model passed to an MCP tool.
///
/// Rules:
/// 1. If `input` is not a JSON object, return an empty `Vec` —
///    callers MUST treat that as "global" and fall back to the
///    dispatcher's sentinel. This is the conservative behaviour.
/// 2. Top-level fields named in [`RESOURCE_FIELDS`] are inspected.
/// 3. String values become keys verbatim, prefixed with the field
///    name (`path:src/main.rs`) so a `path` and a `url` that happen
///    to share the same string don't collide.
/// 4. Array-of-string values contribute one key per element, same
///    prefix, deduplicated.
/// 5. Empty / whitespace-only strings are skipped.
/// 6. Output is sorted ascending — identical to the dispatcher's
///    deterministic-order requirement (prevents ABBA deadlock).
///
/// Anything else is ignored. Nested objects are deliberately NOT
/// walked: that would require schema knowledge and risk false-positive
/// serialisation across unrelated inputs.
pub fn extract(input: &Value) -> Vec<String> {
    let Some(obj) = input.as_object() else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for &field in RESOURCE_FIELDS {
        let Some(v) = obj.get(field) else { continue };
        // P13.1 (G-R6.1): if the field is a known path-shaped one,
        // lex-normalise the string before keying. Otherwise (`uri`,
        // `url`, opaque `resource`) keep the value verbatim — URLs
        // have their own canonicalisation rules and we don't want to
        // collapse, e.g., `https://a.com/x` and
        // `https://a.com/y/../x` (which the server may treat as
        // distinct endpoints).
        let is_path = caduceus_core::is_path_like_field(field);
        match v {
            Value::String(s) => {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    let canonical: String = if is_path {
                        caduceus_core::normalize_lex(trimmed)
                    } else {
                        trimmed.to_string()
                    };
                    out.push(format!("{field}:{canonical}"));
                }
            }
            Value::Array(items) => {
                for item in items {
                    if let Some(s) = item.as_str() {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            let canonical: String = if is_path {
                                caduceus_core::normalize_lex(trimmed)
                            } else {
                                trimmed.to_string()
                            };
                            out.push(format!("{field}:{canonical}"));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // Deduplicate while preserving stable order.
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn p11_4_extracts_path_field() {
        let keys = extract(&json!({"path": "src/main.rs"}));
        assert_eq!(keys, vec!["path:src/main.rs"]);
    }

    #[test]
    fn p11_4_extracts_url_and_uri_with_distinct_prefixes() {
        let keys = extract(&json!({
            "url": "https://example.com/a",
            "uri": "https://example.com/a"
        }));
        // Same string under different fields → distinct keys (no collision).
        assert_eq!(
            keys,
            vec![
                "uri:https://example.com/a".to_string(),
                "url:https://example.com/a".to_string()
            ]
        );
    }

    #[test]
    fn p11_4_returns_empty_for_non_object_input() {
        assert!(extract(&json!("naked string")).is_empty());
        assert!(extract(&json!(42)).is_empty());
        assert!(extract(&json!(null)).is_empty());
        assert!(extract(&json!(["array", "of", "strings"])).is_empty());
    }

    #[test]
    fn p11_4_handles_array_paths_and_dedupes_and_skips_blanks() {
        let keys = extract(&json!({
            "path": ["a.rs", "b.rs", "a.rs", "  ", ""]
        }));
        // Dedup keeps a.rs once; whitespace-only / empty are skipped.
        assert_eq!(keys, vec!["path:a.rs", "path:b.rs"]);
    }

    #[test]
    fn p11_4_keys_are_sorted_for_deterministic_lock_order() {
        // Two batches differing only in field-write order must produce
        // identical key vectors so the dispatcher's sorted-acquire
        // invariant holds (no ABBA deadlock between callers).
        let k1 = extract(&json!({"url": "z", "path": "a"}));
        let k2 = extract(&json!({"path": "a", "url": "z"}));
        assert_eq!(k1, k2);
        // And they must be ascending.
        let mut sorted = k1.clone();
        sorted.sort();
        assert_eq!(k1, sorted);
    }

    // ── P13.1 — path canonicalisation in resource_keys (G-R6.1) ──────

    #[test]
    fn p13_1_path_field_is_lex_normalised() {
        // ./foo and src/../src/foo collapse onto the same lock key,
        // so two parallel tool calls touching the same logical inode
        // serialise correctly instead of racing.
        let canon = extract(&json!({"path": "src/foo.rs"}));
        for v in &[
            "./src/foo.rs",
            "src/./foo.rs",
            "src/../src/foo.rs",
            "src//foo.rs",
            "src\\foo.rs",
        ] {
            assert_eq!(
                extract(&json!({"path": v})),
                canon,
                "input '{v}' must canon"
            );
        }
    }

    #[test]
    fn p13_1_uri_and_url_are_left_verbatim() {
        // URIs/URLs have their own canonicalisation rules and the
        // server may treat path-like rewrites as semantically distinct.
        // Verify we DO NOT touch them.
        let dirty = extract(&json!({"uri": "https://x/./y/../z"}));
        assert_eq!(dirty, vec!["uri:https://x/./y/../z".to_string()]);
        let dirty_url = extract(&json!({"url": "https://x/./y/../z"}));
        assert_eq!(dirty_url, vec!["url:https://x/./y/../z".to_string()]);
    }

    #[test]
    fn p13_1_array_of_paths_each_canonicalised() {
        let canon = extract(&json!({"path": ["a/b.rs", "c/d.rs"]}));
        let dirty = extract(&json!({"path": ["./a/b.rs", "c/./d.rs"]}));
        assert_eq!(canon, dirty);
    }
}
