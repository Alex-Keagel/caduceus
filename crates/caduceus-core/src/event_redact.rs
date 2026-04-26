//! Secret redaction for `AgentEvent` payloads.
//!
//! `AgentEvent`s are cloned into the broadcast channel and the retention
//! ring in `caduceus-orchestrator::agent_event_emitter`, which means any
//! field on an event fans out to every subscriber and is held in memory
//! for the retention window. Raw tool-call input (`ChatRequest` →
//! `tool_use.input`) can legitimately contain secrets: environment
//! variables, HTTP headers with bearer tokens, API keys passed as
//! arguments, credentials embedded in URLs.
//!
//! [`redact_secrets_for_event`] produces a cloned `serde_json::Value`
//! safe to attach to an `AgentEvent` for fan-out and retention. It
//! preserves the top-level matching fields that approval rules care
//! about (`command`, `path`, `url`, ...) but replaces values of
//! well-known secret-shaped keys with the sentinel string
//! `"<redacted>"`.
//!
//! ## Threat model
//!
//! This is a **defense-in-depth** filter, not a guarantee. Users who
//! type plaintext secrets into positional arguments (e.g.
//! `bash.command = "curl -H 'Authorization: Bearer sk-...'"`) are not
//! protected — the LLM already produced that text, the tool already has
//! it, and pattern-matching inside the middle of a shell command is a
//! separate problem (terminal redaction). What we prevent here is the
//! more common accident: tools whose input schema *structurally*
//! carries a secret field (e.g. a `headers` map, a dedicated
//! `api_key` argument, or an `env` override) leaking that field into
//! every subscribed consumer and the 5-minute retention ring.
//!
//! ## Redaction rules
//!
//! Applied to **object values only** (arrays and scalars pass through
//! unchanged). Keys are compared case-insensitively by substring
//! containment, so `Api-Key`, `api_key`, and `X-API-KEY` all match.
//!
//! - `api_key`, `apikey`
//! - `auth`, `authorization`
//! - `bearer`
//! - `credential`, `credentials`
//! - `env`  (wholesale — env maps commonly carry secrets)
//! - `headers`, `header`  (wholesale — HTTP headers carry auth tokens)
//! - `password`, `pwd`
//! - `secret`
//! - `token`
//!
//! For nested objects the function recurses so e.g.
//! `{"request": {"headers": {...}}}` also has its `headers` redacted.
//!
//! The redactor is a pure function over owned [`Value`]s and allocates
//! proportional to the input size.

use serde_json::Value;

/// Keys whose values get replaced with `"<redacted>"` when encountered
/// as object keys. Comparison is case-insensitive substring, so
/// `"X-Api-Key"` matches `"api_key"` via the `api` key-stem rule below.
const SECRET_KEY_STEMS: &[&str] = &[
    "api_key",
    "apikey",
    "authorization",
    "bearer",
    "credential", // covers credential/credentials
    "env",
    "header", // covers header/headers
    "password",
    "pwd",
    "secret",
    "token",
    // `auth` last so it can't shadow more-specific matches in future
    // extensions. Substring match still fires for auth-only keys.
    "auth",
];

/// Sentinel used to replace redacted values.
pub const REDACTED_SENTINEL: &str = "<redacted>";

/// Walk `v` and return a cloned, redacted `Value` safe for event fan-out.
///
/// See module docs for the threat model and the key list.
pub fn redact_secrets_for_event(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, child) in map {
                if is_secret_key(&k) {
                    out.insert(k, Value::String(REDACTED_SENTINEL.to_string()));
                } else {
                    out.insert(k, redact_secrets_for_event(child));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            Value::Array(items.into_iter().map(redact_secrets_for_event).collect())
        }
        other => other,
    }
}

fn is_secret_key(key: &str) -> bool {
    // Normalize common separators so `API-Key`, `api_key`, `APIKey`
    // and `X-API-KEY` all match a single `apikey` stem. Users and
    // tool schemas mix kebab / snake / camel freely.
    let normalized: String = key
        .chars()
        .filter_map(|c| {
            if c == '-' || c == '_' {
                None
            } else {
                Some(c.to_ascii_lowercase())
            }
        })
        .collect();
    SECRET_KEY_STEMS.iter().any(|stem| {
        let stem_norm: String = stem.chars().filter(|c| *c != '-' && *c != '_').collect();
        normalized.contains(&stem_norm)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalars_pass_through() {
        assert_eq!(redact_secrets_for_event(json!(null)), json!(null));
        assert_eq!(redact_secrets_for_event(json!(42)), json!(42));
        assert_eq!(redact_secrets_for_event(json!("hi")), json!("hi"));
        assert_eq!(redact_secrets_for_event(json!(true)), json!(true));
    }

    #[test]
    fn matching_fields_preserved() {
        let input = json!({
            "command": "ls -la",
            "path": "/tmp/x",
            "url": "https://example.com/api",
        });
        assert_eq!(redact_secrets_for_event(input.clone()), input);
    }

    #[test]
    fn top_level_secret_keys_redacted() {
        let out = redact_secrets_for_event(json!({
            "api_key": "sk-abc",
            "apikey": "sk-abc",
            "authorization": "Bearer sk-abc",
            "token": "t",
            "password": "p",
            "secret": "s",
            "env": {"FOO": "bar"},
            "headers": {"X-Auth": "x"},
            "command": "ls",
        }));
        let obj = out.as_object().unwrap();
        for k in [
            "api_key",
            "apikey",
            "authorization",
            "token",
            "password",
            "secret",
            "env",
            "headers",
        ] {
            assert_eq!(
                obj.get(k).and_then(Value::as_str),
                Some(REDACTED_SENTINEL),
                "key {k} should be redacted",
            );
        }
        assert_eq!(obj.get("command").and_then(Value::as_str), Some("ls"));
    }

    #[test]
    fn case_insensitive_and_substring_matches() {
        let out = redact_secrets_for_event(json!({
            "API-Key": "x",
            "X-API-KEY": "x",
            "Authorization": "x",
            "BearerToken": "x",
            "my_password_field": "x",
        }));
        let obj = out.as_object().unwrap();
        for (k, _) in obj {
            assert_eq!(
                obj.get(k).and_then(Value::as_str),
                Some(REDACTED_SENTINEL),
                "case/substring variant {k} should be redacted",
            );
        }
    }

    #[test]
    fn nested_objects_redacted() {
        let out = redact_secrets_for_event(json!({
            "request": {
                "url": "https://x.test",
                "headers": {"Authorization": "Bearer sk"}
            },
            "outer": {
                "inner": {"password": "p"}
            }
        }));
        assert_eq!(
            out["request"]["url"].as_str(),
            Some("https://x.test"),
            "url preserved"
        );
        assert_eq!(
            out["request"]["headers"].as_str(),
            Some(REDACTED_SENTINEL),
            "headers redacted wholesale",
        );
        assert_eq!(
            out["outer"]["inner"]["password"].as_str(),
            Some(REDACTED_SENTINEL),
        );
    }

    #[test]
    fn arrays_recurse() {
        let out = redact_secrets_for_event(json!({
            "items": [
                {"name": "a", "token": "t1"},
                {"name": "b", "token": "t2"}
            ]
        }));
        assert_eq!(out["items"][0]["name"].as_str(), Some("a"));
        assert_eq!(out["items"][0]["token"].as_str(), Some(REDACTED_SENTINEL),);
        assert_eq!(out["items"][1]["token"].as_str(), Some(REDACTED_SENTINEL),);
    }

    #[test]
    fn non_string_secret_values_still_redacted() {
        // Some tools use structured headers (map-of-strings) or numeric
        // tokens. Value shape must not matter — the replacement is
        // unconditional on the key match.
        let out = redact_secrets_for_event(json!({
            "token": 12345,
            "headers": [["X-Auth", "abc"]],
        }));
        assert_eq!(out["token"].as_str(), Some(REDACTED_SENTINEL));
        assert_eq!(out["headers"].as_str(), Some(REDACTED_SENTINEL));
    }
}
