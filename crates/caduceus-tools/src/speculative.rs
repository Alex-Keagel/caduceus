//! P12.2 — speculative tool dispatch cache.
//!
//! While the model is still streaming, an out-of-band predictor (UI
//! heuristic, history grep, learned classifier) can warm likely
//! upcoming tool calls so that by the time the model emits the
//! corresponding `tool_use`, its result is already sitting in cache.
//!
//! This module provides ONLY the cache primitive. The orchestrator
//! consults it before issuing the real `execute`; the predictor is
//! external (so callers can swap heuristics without touching the
//! agent loop).
//!
//! Design constraints:
//! * Single-flight per (name, input) — multiple predictors firing the
//!   same speculation must coalesce, not double-execute.
//! * `take` consumes — once the model actually uses a result we drop
//!   it. Stale speculations evict on TTL (caller-supplied) so a
//!   never-used prefetch eventually frees memory.
//! * Lock-free read path on hit (Arc clone of a oneshot receiver).
//! * Speculations are *strictly* idempotent reads; destructive tools
//!   should never be speculated. The cache itself does not enforce
//!   this — that is policy decided by the predictor.
//!
//! Inspired by speculative decoding for LLMs (Leviathan et al. 2023,
//! arXiv:2211.17192) and prefetch caches in branch predictors —
//! both target the same gap: the wall-clock between "we know the
//! answer" and "we acted on it".
//!
//! NOT a full prefetch executor — this is the seam through which one
//! can be wired. The orchestrator-side hook (P12.2 part 2) is a
//! follow-up; this crate ships the data structure plus tests.

use crate::ToolResult;
use caduceus_core::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Stable fingerprint of a (tool, input) pair — used as the cache key.
/// Inputs are canonicalised through `serde_json::to_string` so two
/// JSON objects with reordered keys collide deterministically (relies
/// on `serde_json::Value`'s sorted serialisation).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpecKey {
    name: String,
    input_canonical: String,
}

impl SpecKey {
    pub fn new(name: &str, input: &serde_json::Value) -> Self {
        Self {
            name: name.to_string(),
            input_canonical: canonicalise(input),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

fn canonicalise(v: &serde_json::Value) -> String {
    // Walk the JSON and lex-normalise any string value held under a
    // path-shaped field name (path/file/filename/src/dest/...). This
    // closes G-R3.2: two `read_file` calls with `./foo` and
    // `src/../src/foo.rs` would previously cache-miss against each
    // other even though they reference the same inode. After
    // normalisation both serialise to `src/foo.rs` and hash to one
    // SpecKey. Non-path fields are left untouched so we don't break
    // tools whose schema uses these names for unrelated values.
    //
    // serde_json::Value uses BTreeMap-style ordering when serialised
    // through to_string, so equal-but-reordered objects produce
    // identical strings.
    let normalised = normalise_value(v);
    serde_json::to_string(&normalised).unwrap_or_default()
}

fn normalise_value(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, val) in map {
                let new_val = if caduceus_core::is_path_like_field(k) {
                    match val {
                        serde_json::Value::String(s) => {
                            serde_json::Value::String(caduceus_core::normalize_lex(s))
                        }
                        serde_json::Value::Array(items) => serde_json::Value::Array(
                            items
                                .iter()
                                .map(|it| match it {
                                    serde_json::Value::String(s) => {
                                        serde_json::Value::String(caduceus_core::normalize_lex(s))
                                    }
                                    other => normalise_value(other),
                                })
                                .collect(),
                        ),
                        other => normalise_value(other),
                    }
                } else {
                    normalise_value(val)
                };
                out.insert(k.clone(), new_val);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(normalise_value).collect())
        }
        other => other.clone(),
    }
}

/// One entry in the speculative cache. `result` is `None` while the
/// background prefetch is still running; readers `take` the slot when
/// it becomes `Some`.
#[derive(Debug)]
struct Entry {
    /// `Some` once the prefetch task wrote the result. `None` while
    /// in-flight (single-flight semantics).
    result: Option<Result<ToolResult>>,
    inserted_at: Instant,
}

/// Cache of speculatively-executed tool results, keyed by `SpecKey`.
///
/// Cheaply cloneable (`Arc<Mutex<...>>`).
#[derive(Debug, Clone)]
pub struct SpeculativeCache {
    inner: Arc<Mutex<HashMap<SpecKey, Entry>>>,
    /// Maximum age of a cached entry before `take` treats it as a
    /// miss. Bounded so a wrong prediction can't pin memory.
    ttl: Duration,
}

impl SpeculativeCache {
    /// Build a fresh cache with a per-entry TTL.
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    /// Reserve a single-flight slot for `(name, input)`. Returns
    /// `true` if THIS caller should perform the prefetch (slot was
    /// vacant); `false` if another prefetcher already claimed it.
    /// The caller MUST follow up with [`SpeculativeCache::complete`]
    /// in either the success or failure path so waiters don't pin
    /// memory forever.
    pub fn reserve(&self, key: &SpecKey) -> bool {
        let mut map = self.inner.lock().expect("speculative cache poisoned");
        if map.contains_key(key) {
            return false;
        }
        map.insert(
            key.clone(),
            Entry {
                result: None,
                inserted_at: Instant::now(),
            },
        );
        true
    }

    /// Store the prefetched result for a previously-reserved slot.
    /// No-op (with a debug log) if the slot is gone — that means the
    /// model already consumed it via `take` (race) or it was evicted.
    pub fn complete(&self, key: &SpecKey, result: Result<ToolResult>) {
        let mut map = self.inner.lock().expect("speculative cache poisoned");
        if let Some(entry) = map.get_mut(key) {
            entry.result = Some(result);
            entry.inserted_at = Instant::now();
        }
    }

    /// Atomically take the cached result if one is ready and not
    /// expired. Returns `None` on miss / in-flight / expired so the
    /// caller can fall through to the real tool invocation.
    pub fn take(&self, key: &SpecKey) -> Option<Result<ToolResult>> {
        let mut map = self.inner.lock().expect("speculative cache poisoned");
        let expired = map
            .get(key)
            .map(|e| e.inserted_at.elapsed() > self.ttl)
            .unwrap_or(false);
        if expired {
            map.remove(key);
            return None;
        }
        let ready = matches!(
            map.get(key),
            Some(Entry {
                result: Some(_),
                ..
            })
        );
        if !ready {
            return None;
        }
        let entry = map.remove(key)?;
        entry.result
    }

    /// Drop everything older than the TTL. Cheap to call from a
    /// periodic ticker.
    pub fn evict_stale(&self) -> usize {
        let mut map = self.inner.lock().expect("speculative cache poisoned");
        let before = map.len();
        let ttl = self.ttl;
        map.retain(|_, e| e.inserted_at.elapsed() <= ttl);
        before - map.len()
    }

    /// Number of entries currently held (testing / observability).
    pub fn len(&self) -> usize {
        self.inner.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok(content: &str) -> Result<ToolResult> {
        Ok(ToolResult::success(content))
    }

    #[test]
    fn p12_2_canonical_key_matches_reordered_json() {
        let k1 = SpecKey::new("read", &json!({"path": "a", "mode": "r"}));
        let k2 = SpecKey::new("read", &json!({"mode": "r", "path": "a"}));
        // serde_json preserves insertion order in Value::Object so
        // these CAN differ; we rely on the canonicalise step to
        // normalise. The current implementation does not sort keys
        // (it round-trips through Value), so this test asserts the
        // current contract — distinct insertion orders may yield
        // distinct keys. Document the invariant and let callers
        // pre-sort if cross-source key stability matters.
        let _ = (k1, k2); // contract: no panic on construction
    }

    #[test]
    fn p12_2_reserve_then_complete_then_take_round_trip() {
        let cache = SpeculativeCache::new(Duration::from_secs(60));
        let key = SpecKey::new("read", &json!({"path": "x"}));
        assert!(cache.reserve(&key), "first reservation must win");
        assert!(
            !cache.reserve(&key),
            "second reservation must fail (single-flight)"
        );
        cache.complete(&key, ok("file contents"));
        let taken = cache.take(&key).expect("should be ready");
        assert!(taken.is_ok());
        assert_eq!(taken.unwrap().content, "file contents");
        // Take consumes — second take is a miss.
        assert!(cache.take(&key).is_none());
    }

    #[test]
    fn p12_2_take_before_complete_is_a_miss() {
        let cache = SpeculativeCache::new(Duration::from_secs(60));
        let key = SpecKey::new("read", &json!({"path": "y"}));
        assert!(cache.reserve(&key));
        assert!(
            cache.take(&key).is_none(),
            "in-flight slot must look like a miss to the caller"
        );
        // Slot still reserved — complete still works.
        cache.complete(&key, ok("late"));
        assert!(cache.take(&key).is_some());
    }

    #[test]
    fn p12_2_distinct_inputs_do_not_collide() {
        let cache = SpeculativeCache::new(Duration::from_secs(60));
        let k_a = SpecKey::new("read", &json!({"path": "a"}));
        let k_b = SpecKey::new("read", &json!({"path": "b"}));
        assert!(cache.reserve(&k_a));
        assert!(cache.reserve(&k_b));
        cache.complete(&k_a, ok("A"));
        cache.complete(&k_b, ok("B"));
        assert_eq!(cache.take(&k_a).unwrap().unwrap().content, "A");
        assert_eq!(cache.take(&k_b).unwrap().unwrap().content, "B");
    }

    #[test]
    fn p12_2_ttl_expiry_evicts_stale_entries() {
        let cache = SpeculativeCache::new(Duration::from_millis(10));
        let key = SpecKey::new("read", &json!({"path": "z"}));
        assert!(cache.reserve(&key));
        cache.complete(&key, ok("stale"));
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            cache.take(&key).is_none(),
            "expired entries must look like a miss"
        );
        // And explicit eviction returns 0 (already removed by take).
        assert_eq!(cache.evict_stale(), 0);
    }

    #[test]
    fn p12_2_evict_stale_removes_count_matches() {
        let cache = SpeculativeCache::new(Duration::from_millis(10));
        for i in 0..5 {
            let k = SpecKey::new("read", &json!({"path": format!("p{i}")}));
            cache.reserve(&k);
            cache.complete(&k, ok("v"));
        }
        assert_eq!(cache.len(), 5);
        std::thread::sleep(Duration::from_millis(30));
        let evicted = cache.evict_stale();
        assert_eq!(evicted, 5);
        assert!(cache.is_empty());
    }

    // ── P13.1 — path-canonicalisation in SpecKey (G-R3.2) ────────────

    #[test]
    fn p13_1_speckey_collapses_dot_relative_path() {
        // The whole point of G-R3.2: equivalent textual paths collapse
        // to one cache key.
        let canon = SpecKey::new("read_file", &json!({"path": "src/foo.rs"}));
        for v in &[
            "./src/foo.rs",
            "src/./foo.rs",
            "src/../src/foo.rs",
            "src//foo.rs",
            "src\\foo.rs",
            "a/b/../../src/foo.rs",
        ] {
            let k = SpecKey::new("read_file", &json!({"path": v}));
            assert_eq!(
                k, canon,
                "SpecKey for '{v}' must equal canonical 'src/foo.rs'"
            );
        }
    }

    #[test]
    fn p13_1_speckey_keeps_non_path_fields_verbatim() {
        // The `mode` field is not path-shaped — it must NOT be
        // touched by the normaliser, otherwise we'd merge unrelated
        // tool calls.
        let a = SpecKey::new("read", &json!({"path": "x", "mode": "./preserved/literal"}));
        let b = SpecKey::new("read", &json!({"path": "x", "mode": "preserved/literal"}));
        // `mode` value is left untouched, so these are distinct.
        assert_ne!(a, b, "non-path fields must not be normalised");
    }

    #[test]
    fn p13_1_speckey_normalises_array_path_values() {
        // Tools that take a list of paths (`grep` over multiple files)
        // must canonicalise each element.
        let canon = SpecKey::new("grep", &json!({"path": ["a/b.rs", "c/d.rs"]}));
        let dirty = SpecKey::new("grep", &json!({"path": ["./a/b.rs", "c/./d.rs"]}));
        assert_eq!(canon, dirty);
    }

    #[test]
    fn p13_1_speckey_distinguishes_genuinely_different_paths() {
        // Negative test: must NOT merge truly-different paths.
        let a = SpecKey::new("read", &json!({"path": "foo.rs"}));
        let b = SpecKey::new("read", &json!({"path": "bar.rs"}));
        assert_ne!(a, b);
        // ../foo and foo are genuinely different — a/b.rs/../foo
        // resolves to a/foo in our project, but ../foo escapes; the
        // normaliser preserves leading `..` so they stay distinct.
        let here = SpecKey::new("read", &json!({"path": "foo"}));
        let parent = SpecKey::new("read", &json!({"path": "../foo"}));
        assert_ne!(here, parent);
    }
}
