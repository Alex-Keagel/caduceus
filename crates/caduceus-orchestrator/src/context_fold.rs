//! Context folding for large subagent transcripts (gap G8 / P4.2).
//!
//! When a parent agent invokes a subagent (via `task` / `spawn_agent`
//! / similar), the subagent's full transcript is returned as a tool
//! result. Pasting that verbatim into the parent's context burns
//! tokens proportional to subagent depth and frequently re-injects
//! redundant retrieval results.
//!
//! Context folding (see "Composable In-Context Memory" / OpenReview
//! 2024) replaces the verbatim transcript with a compact structured
//! summary plus a stable `transcript_id`. The parent can later call
//! `expand_subagent { transcript_id }` to retrieve the full text on
//! demand (e.g., when debugging a wrong answer).
//!
//! This module owns:
//!   * the threshold-based fold decision,
//!   * the `FoldedTranscript` payload the parent sees,
//!   * an in-memory `TranscriptStore` keyed by id, with a TTL +
//!     capacity ring so a long session doesn't grow unbounded.
//!
//! Wiring into the orchestrator (replacing tool result text + handling
//! the new `expand_subagent` tool) is left to the caller — this
//! module is data-only and trivially testable.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Default threshold above which a tool result is folded.
///
/// 5_000 tokens ≈ 20_000 chars at the 4-char rule of thumb. Matches
/// the OpenReview 2024 paper's reported sweet spot for
/// recall-vs-cost.
pub const DEFAULT_FOLD_THRESHOLD_CHARS: usize = 20_000;

/// Cap on retained transcripts (FIFO). 256 is enough for ~64
/// concurrent subagent fan-outs at depth 4 without ever evicting a
/// transcript the parent might still want.
pub const DEFAULT_STORE_CAPACITY: usize = 256;

/// TTL for retained transcripts. After this window an `expand`
/// returns `Expired`. 30 minutes covers a typical multi-turn debug
/// session without pinning RAM.
pub const DEFAULT_TTL_SECS: u64 = 1_800;

/// Stable identifier for a folded transcript. Monotonic and never
/// reused so a stale `expand_subagent` call fails closed via
/// `ExpandError::Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TranscriptId(pub u64);

impl TranscriptId {
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Compact replacement that the parent sees in place of the full
/// subagent transcript. Designed to be cheap to embed in a chat
/// message: a few hundred chars at most.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldedTranscript {
    pub id: TranscriptId,
    /// Subagent name / role for the UI ("security_critic", "test_writer").
    pub subagent: String,
    /// Final outcome string the subagent returned (one-liner). This
    /// is the part the parent almost always actually needs.
    pub outcome: String,
    /// Coarse stats so the parent's reasoning model can decide
    /// whether to expand.
    pub original_chars: u32,
    pub original_tokens_estimate: u32,
    /// The first heading and any explicit `KEY:`-prefixed lines from
    /// the original transcript. Kept short (~30 lines) to stay below
    /// 1k chars.
    pub key_points: Vec<String>,
}

#[derive(Debug, thiserror::Error, Clone, Serialize, Deserialize)]
pub enum ExpandError {
    #[error("transcript {0:?} not found (evicted or never existed)")]
    Unknown(TranscriptId),
    #[error("transcript {0:?} expired")]
    Expired(TranscriptId),
}

#[derive(Debug, Clone)]
struct StoredTranscript {
    full_text: String,
    folded: FoldedTranscript,
    stored_at: Instant,
}

/// In-memory store for subagent transcripts. Bounded ring (FIFO
/// eviction) with TTL on lookup.
pub struct TranscriptStore {
    items: HashMap<TranscriptId, StoredTranscript>,
    /// Insertion order tracker for FIFO eviction. We use a Vec rather
    /// than a VecDeque because the cap is tiny and we want
    /// O(capacity) `retain` to drop on TTL eviction without an extra
    /// secondary structure.
    order: Vec<TranscriptId>,
    capacity: usize,
    ttl: Duration,
    next_id: u64,
}

impl Default for TranscriptStore {
    fn default() -> Self {
        Self::new(DEFAULT_STORE_CAPACITY, Duration::from_secs(DEFAULT_TTL_SECS))
    }
}

impl TranscriptStore {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            items: HashMap::new(),
            order: Vec::new(),
            capacity: capacity.max(1),
            ttl,
            next_id: 1,
        }
    }

    /// Should this tool result be folded? Caller can override the
    /// default threshold via `threshold_chars`.
    pub fn should_fold(text: &str, threshold_chars: usize) -> bool {
        text.len() >= threshold_chars
    }

    /// Fold a subagent transcript and store the original. Returns the
    /// `FoldedTranscript` the parent should see in place of the
    /// original tool-result text.
    pub fn fold(
        &mut self,
        subagent: impl Into<String>,
        outcome: impl Into<String>,
        full_text: String,
    ) -> FoldedTranscript {
        let id = TranscriptId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);

        let folded = FoldedTranscript {
            id,
            subagent: subagent.into(),
            outcome: outcome.into(),
            original_chars: full_text.len() as u32,
            // Match BlockLimits' 4-chars-per-token estimate.
            original_tokens_estimate: (full_text.len() / 4) as u32,
            key_points: extract_key_points(&full_text),
        };

        self.items.insert(
            id,
            StoredTranscript {
                full_text,
                folded: folded.clone(),
                stored_at: Instant::now(),
            },
        );
        self.order.push(id);

        // Enforce capacity. Eviction is FIFO — oldest insertion goes
        // first, even if a newer transcript is unused.
        while self.order.len() > self.capacity {
            let evict = self.order.remove(0);
            self.items.remove(&evict);
        }

        folded
    }

    /// Retrieve the full transcript by id. Returns `Unknown` if
    /// evicted/never-existed and `Expired` if past TTL.
    pub fn expand(&self, id: TranscriptId) -> Result<&str, ExpandError> {
        let entry = self.items.get(&id).ok_or(ExpandError::Unknown(id))?;
        if entry.stored_at.elapsed() > self.ttl {
            return Err(ExpandError::Expired(id));
        }
        Ok(&entry.full_text)
    }

    /// Same as `expand` but returns the folded payload (so the UI can
    /// render the summary panel without doing a fold round-trip).
    pub fn metadata(&self, id: TranscriptId) -> Result<&FoldedTranscript, ExpandError> {
        let entry = self.items.get(&id).ok_or(ExpandError::Unknown(id))?;
        if entry.stored_at.elapsed() > self.ttl {
            return Err(ExpandError::Expired(id));
        }
        Ok(&entry.folded)
    }

    /// Drop expired entries proactively. Caller can run this on a
    /// timer; otherwise expiry is enforced lazily on lookup.
    pub fn purge_expired(&mut self) -> usize {
        let ttl = self.ttl;
        let before = self.items.len();
        let alive: Vec<TranscriptId> = self
            .order
            .iter()
            .copied()
            .filter(|id| {
                self.items
                    .get(id)
                    .map(|e| e.stored_at.elapsed() <= ttl)
                    .unwrap_or(false)
            })
            .collect();
        self.order = alive;
        self.items.retain(|_, e| e.stored_at.elapsed() <= ttl);
        before - self.items.len()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Extract a short list of "key points" from a transcript:
///   * the first markdown heading we find,
///   * any line starting with `KEY:` (case-insensitive).
/// Caps at 30 lines / 1000 chars total to keep the folded payload
/// small.
fn extract_key_points(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut total_chars = 0usize;
    let max_chars = 1_000;
    let max_lines = 30;

    if let Some(first_heading) = text.lines().find(|l| l.trim_start().starts_with('#')) {
        let s = first_heading.trim().to_string();
        total_chars += s.len();
        out.push(s);
    }

    for line in text.lines() {
        if out.len() >= max_lines || total_chars >= max_chars {
            break;
        }
        let trimmed = line.trim_start();
        if trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("KEY:") {
            let s = trimmed.to_string();
            total_chars += s.len();
            out.push(s);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_fold_threshold() {
        assert!(!TranscriptStore::should_fold("short", 100));
        assert!(TranscriptStore::should_fold(&"x".repeat(100), 100));
        assert!(TranscriptStore::should_fold(&"x".repeat(101), 100));
    }

    #[test]
    fn fold_then_expand_roundtrip() {
        let mut store = TranscriptStore::default();
        let original = "## Subagent Report\nbody body body".to_string();
        let folded = store.fold("test_writer", "all green", original.clone());
        assert_eq!(folded.subagent, "test_writer");
        assert_eq!(folded.outcome, "all green");
        assert_eq!(folded.original_chars, original.len() as u32);
        assert_eq!(store.expand(folded.id).unwrap(), original);
    }

    #[test]
    fn key_points_extracts_heading_and_key_lines() {
        let text = "## Big report\nblah\nKEY: critical finding 1\n  KEY: case-insensitive\nrandom\nkey: lowercase ok";
        let pts = extract_key_points(text);
        assert_eq!(pts[0], "## Big report");
        assert!(pts.iter().any(|p| p.contains("critical finding 1")));
        assert!(pts.iter().any(|p| p.contains("case-insensitive")));
        assert!(pts.iter().any(|p| p.contains("lowercase ok")));
    }

    #[test]
    fn capacity_evicts_oldest_fifo() {
        let mut store = TranscriptStore::new(2, Duration::from_secs(60));
        let a = store.fold("s", "o", "A".into());
        let b = store.fold("s", "o", "B".into());
        let c = store.fold("s", "o", "C".into());
        assert!(matches!(store.expand(a.id), Err(ExpandError::Unknown(_))));
        assert_eq!(store.expand(b.id).unwrap(), "B");
        assert_eq!(store.expand(c.id).unwrap(), "C");
    }

    #[test]
    fn expand_unknown_fails_closed() {
        let store = TranscriptStore::default();
        let bogus = TranscriptId(9999);
        assert!(matches!(store.expand(bogus), Err(ExpandError::Unknown(_))));
    }

    #[test]
    fn expand_expired_returns_expired() {
        let mut store = TranscriptStore::new(8, Duration::from_millis(1));
        let f = store.fold("s", "o", "X".into());
        std::thread::sleep(Duration::from_millis(5));
        assert!(matches!(store.expand(f.id), Err(ExpandError::Expired(_))));
        assert!(matches!(store.metadata(f.id), Err(ExpandError::Expired(_))));
    }

    #[test]
    fn purge_expired_drops_old_entries() {
        let mut store = TranscriptStore::new(8, Duration::from_millis(1));
        let _ = store.fold("s", "o", "X".into());
        let _ = store.fold("s", "o", "Y".into());
        std::thread::sleep(Duration::from_millis(5));
        let dropped = store.purge_expired();
        assert_eq!(dropped, 2);
        assert!(store.is_empty());
    }

    #[test]
    fn ids_never_reused() {
        let mut store = TranscriptStore::new(1, Duration::from_secs(60));
        let a = store.fold("s", "o", "A".into());
        let _b = store.fold("s", "o", "B".into()); // evicts a
        let c = store.fold("s", "o", "C".into()); // evicts b
        assert_ne!(c.id, a.id);
        assert!(c.id.raw() > a.id.raw());
    }

    #[test]
    fn folded_payload_is_serde_safe() {
        let mut store = TranscriptStore::default();
        let f = store.fold("s", "outcome", "x".repeat(50_000));
        let json = serde_json::to_string(&f).unwrap();
        let back: FoldedTranscript = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, f.id);
        assert_eq!(back.original_chars, 50_000);
        assert_eq!(back.original_tokens_estimate, 12_500);
    }
}
