//! P13.8 — Self‑consistency voting for high‑risk (Destructive) tool plans.
//!
//! Implements Wang et al. "Self‑Consistency Improves Chain‑of‑Thought Reasoning"
//! (ICLR 2023, arXiv:2203.11171). Sample N candidate tool argument payloads and
//! majority‑vote: only execute if ≥ ⌈N/2⌉+1 candidates agree on a canonical
//! representation of the arguments. Otherwise escalate to the approval gate.
//!
//! This module is intentionally pure data‑in / data‑out so it can be unit‑tested
//! without an LLM. Sampling is the caller's responsibility.

use serde_json::Value;
use std::collections::HashMap;

/// Verdict produced by [`vote`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfConsistencyVerdict {
    /// Quorum reached; `winner` is the canonical agreed‑upon argument payload
    /// (one of the inputs, byte‑for‑byte). `votes` is the count.
    Quorum { winner: Value, votes: usize },
    /// Quorum not reached. `top_candidate` is the plurality choice (or `None`
    /// if tied / empty); `top_votes` is its count. The caller should escalate
    /// to approval / human review.
    NoQuorum {
        top_candidate: Option<Value>,
        top_votes: usize,
    },
}

/// Quorum threshold: strict majority — ⌈N/2⌉ + 1.
///
/// - N=1 → 2 (impossible — we never run self‑consistency with N=1).
/// - N=2 → 2 (both must agree).
/// - N=3 → 2 (≥ 2 of 3).
/// - N=4 → 3.
/// - N=5 → 3 (≥ 3 of 5).
pub fn quorum_threshold(n: usize) -> usize {
    n / 2 + 1
}

/// Canonicalise a JSON value so semantically equivalent payloads vote together.
///
/// Object keys are emitted in sorted order; whitespace is removed; arrays keep
/// their order (order is meaningful for tool args). Numbers and strings are
/// passed through serde_json's normal representation.
pub fn canonicalise(v: &Value) -> String {
    let mut out = String::new();
    canon_into(v, &mut out);
    out
}

fn canon_into(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canon_into(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                out.push_str(k);
                out.push('"');
                out.push(':');
                canon_into(&map[*k], out);
            }
            out.push('}');
        }
    }
}

/// Run the self‑consistency vote.
///
/// Returns [`SelfConsistencyVerdict::Quorum`] iff the plurality candidate has
/// ≥ ⌈N/2⌉+1 votes after canonicalisation.
pub fn vote(samples: &[Value]) -> SelfConsistencyVerdict {
    if samples.is_empty() {
        return SelfConsistencyVerdict::NoQuorum {
            top_candidate: None,
            top_votes: 0,
        };
    }
    // Bucket by canonical form, remember first‑seen original payload.
    let mut buckets: HashMap<String, (Value, usize)> = HashMap::new();
    for s in samples {
        let key = canonicalise(s);
        buckets
            .entry(key)
            .and_modify(|e| e.1 += 1)
            .or_insert_with(|| (s.clone(), 1));
    }
    // Find plurality — break ties deterministically by canonical key.
    let mut entries: Vec<(&String, &(Value, usize))> = buckets.iter().collect();
    entries.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then_with(|| a.0.cmp(b.0)));
    let (top_key, (top_val, top_votes)) = entries[0];
    let _ = top_key;
    let n = samples.len();
    let threshold = quorum_threshold(n);
    if *top_votes >= threshold {
        SelfConsistencyVerdict::Quorum {
            winner: top_val.clone(),
            votes: *top_votes,
        }
    } else {
        SelfConsistencyVerdict::NoQuorum {
            top_candidate: Some(top_val.clone()),
            top_votes: *top_votes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn p13_8_quorum_threshold_majority() {
        assert_eq!(quorum_threshold(2), 2);
        assert_eq!(quorum_threshold(3), 2);
        assert_eq!(quorum_threshold(4), 3);
        assert_eq!(quorum_threshold(5), 3);
        assert_eq!(quorum_threshold(7), 4);
    }

    #[test]
    fn p13_8_canonicalises_object_key_order() {
        let a = json!({"path": "/x", "force": true});
        let b = json!({"force": true, "path": "/x"});
        assert_eq!(canonicalise(&a), canonicalise(&b));
    }

    #[test]
    fn p13_8_canonical_arrays_keep_order() {
        let a = json!([1, 2, 3]);
        let b = json!([3, 2, 1]);
        assert_ne!(canonicalise(&a), canonicalise(&b));
    }

    #[test]
    fn p13_8_vote_empty_is_no_quorum() {
        match vote(&[]) {
            SelfConsistencyVerdict::NoQuorum {
                top_candidate,
                top_votes,
            } => {
                assert!(top_candidate.is_none());
                assert_eq!(top_votes, 0);
            }
            _ => panic!("empty must be NoQuorum"),
        }
    }

    #[test]
    fn p13_8_vote_unanimous_three_quorum() {
        let s = vec![
            json!({"path": "/tmp/a"}),
            json!({"path": "/tmp/a"}),
            json!({"path": "/tmp/a"}),
        ];
        match vote(&s) {
            SelfConsistencyVerdict::Quorum { votes, winner } => {
                assert_eq!(votes, 3);
                assert_eq!(winner, json!({"path": "/tmp/a"}));
            }
            _ => panic!("must reach quorum"),
        }
    }

    #[test]
    fn p13_8_vote_two_of_three_reaches_quorum() {
        let s = vec![
            json!({"path": "/tmp/a"}),
            json!({"path": "/tmp/a"}),
            json!({"path": "/tmp/b"}),
        ];
        match vote(&s) {
            SelfConsistencyVerdict::Quorum { votes, .. } => assert_eq!(votes, 2),
            _ => panic!("2/3 must reach quorum"),
        }
    }

    #[test]
    fn p13_8_vote_three_way_split_no_quorum() {
        let s = vec![
            json!({"path": "/a"}),
            json!({"path": "/b"}),
            json!({"path": "/c"}),
        ];
        match vote(&s) {
            SelfConsistencyVerdict::NoQuorum { top_votes, .. } => assert_eq!(top_votes, 1),
            _ => panic!("split must NOT reach quorum"),
        }
    }

    #[test]
    fn p13_8_vote_treats_key_order_as_same_vote() {
        let s = vec![
            json!({"path": "/x", "force": true}),
            json!({"force": true, "path": "/x"}),
            json!({"path": "/y", "force": true}),
        ];
        match vote(&s) {
            SelfConsistencyVerdict::Quorum { votes, .. } => assert_eq!(votes, 2),
            _ => panic!("canonical equality should pool votes"),
        }
    }

    #[test]
    fn p13_8_vote_two_of_four_no_quorum_threshold_is_three() {
        let s = vec![
            json!({"path": "/a"}),
            json!({"path": "/a"}),
            json!({"path": "/b"}),
            json!({"path": "/b"}),
        ];
        // 2/4 < ⌈4/2⌉+1 = 3 → no quorum.
        match vote(&s) {
            SelfConsistencyVerdict::NoQuorum { top_votes, .. } => assert_eq!(top_votes, 2),
            _ => panic!("2/4 must NOT reach quorum"),
        }
    }
}
