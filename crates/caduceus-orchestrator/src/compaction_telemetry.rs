//! Compaction telemetry collector (gap G5 step 1 / P5.1).
//!
//! Phase 1 of the "learned-compaction" roadmap: instrument every
//! compaction event with structured telemetry so we can later train a
//! Bradley–Terry scorer (P5.2) and finally replace the heuristic
//! strategy selector with a learned policy (P5.3).
//!
//! Each compaction emits a `CompactionEvent` with:
//!   * `strategy` — which built-in strategy fired
//!     (ToolCollapse / Summarize / SlidingWindow / FactDistill),
//!   * `tokens_before` / `tokens_after` so we can compute the savings,
//!   * `messages_before` / `messages_after` for sanity-checking,
//!   * `turn_index` so we can join with downstream telemetry,
//!   * `downstream_re_ask` — true if the NEXT user turn re-asked for
//!     information that was just compacted away (a strong negative
//!     signal). Filled in by a follow-up `mark_re_ask` call once
//!     observed; `None` until then.
//!
//! This module is intentionally storage-agnostic: it owns an in-RAM
//! ring buffer plus a `drain_jsonl` helper for batch export to disk.
//! Wiring into the orchestrator's compaction sites and into the next
//! turn's "did the user re-ask?" detector is left to the caller.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Which built-in compaction strategy fired. String-typed (snake_case)
/// so the trainer can ingest events from future strategies without
/// schema migration.
pub type StrategyName = String;

/// Single compaction telemetry record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionEvent {
    pub strategy: StrategyName,
    /// Approximate input-token count before compaction ran.
    pub tokens_before: u32,
    /// Approximate input-token count after compaction. Always
    /// `<= tokens_before`; saturating subtraction yields `tokens_dropped`.
    pub tokens_after: u32,
    pub messages_before: u32,
    pub messages_after: u32,
    /// 1-indexed turn the compaction ran ON (i.e. before this turn
    /// went out to the model). Lets us correlate with the NEXT
    /// downstream user turn for `downstream_re_ask`.
    pub turn_index: u32,
    /// Wall-clock seconds since epoch.
    pub at_secs: u64,
    /// Filled in later: did the next user turn re-ask for information
    /// that was just compacted away? `None` = not yet observed.
    /// `Some(true)` = bad outcome (lost relevant context),
    /// `Some(false)` = compaction was safe.
    pub downstream_re_ask: Option<bool>,
}

impl CompactionEvent {
    pub fn tokens_dropped(&self) -> u32 {
        self.tokens_before.saturating_sub(self.tokens_after)
    }

    pub fn messages_dropped(&self) -> u32 {
        self.messages_before.saturating_sub(self.messages_after)
    }
}

/// Bounded ring of recent compaction events. Default cap 1024 ⇒ ~weeks
/// of typical session activity at a few compactions per session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionTelemetry {
    events: VecDeque<CompactionEvent>,
    cap: usize,
}

impl Default for CompactionTelemetry {
    fn default() -> Self {
        Self::with_capacity(1024)
    }
}

impl CompactionTelemetry {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(cap.max(1)),
            cap: cap.max(1),
        }
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Append a compaction event. Oldest event is evicted FIFO when
    /// the ring is full.
    pub fn record(&mut self, ev: CompactionEvent) {
        self.events.push_back(ev);
        while self.events.len() > self.cap {
            self.events.pop_front();
        }
    }

    /// Mark the most-recent compaction at `turn_index` as having
    /// caused (or not) a downstream re-ask. Returns `true` if a
    /// matching event was found and updated. We update the LATEST
    /// match in case a single turn ran multiple compactions.
    pub fn mark_re_ask(&mut self, turn_index: u32, re_asked: bool) -> bool {
        if let Some(ev) = self
            .events
            .iter_mut()
            .rev()
            .find(|e| e.turn_index == turn_index)
        {
            ev.downstream_re_ask = Some(re_asked);
            true
        } else {
            false
        }
    }

    /// Snapshot the ring (newest-last) as a JSONL string for export
    /// to the trainer. Does NOT clear the ring; callers can keep
    /// telemetry around for in-process debugging.
    pub fn to_jsonl(&self) -> String {
        self.events
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Drain all events as a JSONL string AND clear the ring.
    pub fn drain_jsonl(&mut self) -> String {
        let s = self.to_jsonl();
        self.events.clear();
        s
    }

    /// Aggregate per-strategy stats useful for at-a-glance dashboards.
    /// Returns `(strategy, count, mean_tokens_dropped, re_ask_rate)`.
    /// `re_ask_rate` is `None` if no event of that strategy has been
    /// labelled yet.
    pub fn per_strategy_stats(&self) -> Vec<(StrategyName, u32, f64, Option<f64>)> {
        use std::collections::HashMap;
        let mut buckets: HashMap<&str, (u32, u64, u32, u32)> = HashMap::new();
        for ev in &self.events {
            let entry = buckets.entry(ev.strategy.as_str()).or_default();
            entry.0 += 1;
            entry.1 += ev.tokens_dropped() as u64;
            if let Some(b) = ev.downstream_re_ask {
                entry.2 += 1;
                if b {
                    entry.3 += 1;
                }
            }
        }
        let mut out: Vec<_> = buckets
            .into_iter()
            .map(|(s, (count, total_drop, labelled, bad))| {
                let mean = if count > 0 {
                    total_drop as f64 / count as f64
                } else {
                    0.0
                };
                let rate = if labelled > 0 {
                    Some(bad as f64 / labelled as f64)
                } else {
                    None
                };
                (s.to_string(), count, mean, rate)
            })
            .collect();
        out.sort_by_key(|b| std::cmp::Reverse(b.1));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(strategy: &str, before: u32, after: u32, turn: u32) -> CompactionEvent {
        CompactionEvent {
            strategy: strategy.into(),
            tokens_before: before,
            tokens_after: after,
            messages_before: 10,
            messages_after: 5,
            turn_index: turn,
            at_secs: 0,
            downstream_re_ask: None,
        }
    }

    #[test]
    fn record_and_query() {
        let mut tel = CompactionTelemetry::default();
        tel.record(ev("ToolCollapse", 1000, 700, 1));
        tel.record(ev("Summarize", 2000, 1200, 2));
        assert_eq!(tel.len(), 2);
        assert_eq!(tel.events[0].tokens_dropped(), 300);
        assert_eq!(tel.events[1].tokens_dropped(), 800);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let mut tel = CompactionTelemetry::with_capacity(2);
        tel.record(ev("A", 100, 50, 1));
        tel.record(ev("B", 100, 50, 2));
        tel.record(ev("C", 100, 50, 3));
        assert_eq!(tel.len(), 2);
        assert_eq!(tel.events[0].strategy, "B");
    }

    #[test]
    fn mark_re_ask_finds_matching_turn() {
        let mut tel = CompactionTelemetry::default();
        tel.record(ev("Summarize", 100, 50, 7));
        assert!(tel.mark_re_ask(7, true));
        assert_eq!(tel.events[0].downstream_re_ask, Some(true));
    }

    #[test]
    fn mark_re_ask_misses_unknown_turn() {
        let mut tel = CompactionTelemetry::default();
        tel.record(ev("Summarize", 100, 50, 7));
        assert!(!tel.mark_re_ask(99, true));
    }

    #[test]
    fn mark_re_ask_picks_latest_when_turn_repeats() {
        // Two compactions on the same turn — the most-recent one
        // (which is what would have caused a re-ask) gets the label.
        let mut tel = CompactionTelemetry::default();
        let mut a = ev("Summarize", 100, 50, 5);
        a.tokens_after = 60;
        let b = ev("ToolCollapse", 60, 30, 5);
        tel.record(a);
        tel.record(b);
        assert!(tel.mark_re_ask(5, false));
        assert_eq!(tel.events[1].downstream_re_ask, Some(false));
        assert_eq!(tel.events[0].downstream_re_ask, None);
    }

    #[test]
    fn jsonl_roundtrip() {
        let mut tel = CompactionTelemetry::default();
        tel.record(ev("Summarize", 100, 50, 1));
        let jsonl = tel.to_jsonl();
        let parsed: CompactionEvent = serde_json::from_str(&jsonl).unwrap();
        assert_eq!(parsed.strategy, "Summarize");
        // to_jsonl does not drain.
        assert_eq!(tel.len(), 1);
    }

    #[test]
    fn drain_jsonl_clears_ring() {
        let mut tel = CompactionTelemetry::default();
        tel.record(ev("Summarize", 100, 50, 1));
        tel.record(ev("ToolCollapse", 200, 150, 2));
        let jsonl = tel.drain_jsonl();
        assert!(jsonl.contains("Summarize"));
        assert!(jsonl.contains("ToolCollapse"));
        assert!(tel.is_empty());
    }

    #[test]
    fn per_strategy_stats_aggregates_correctly() {
        let mut tel = CompactionTelemetry::default();
        let mut a = ev("Summarize", 100, 60, 1);
        a.downstream_re_ask = Some(true);
        let mut b = ev("Summarize", 200, 100, 2);
        b.downstream_re_ask = Some(false);
        let c = ev("ToolCollapse", 50, 30, 3);
        tel.record(a);
        tel.record(b);
        tel.record(c);
        let stats = tel.per_strategy_stats();
        let summarize = stats.iter().find(|(s, _, _, _)| s == "Summarize").unwrap();
        assert_eq!(summarize.1, 2);
        assert!((summarize.2 - 70.0).abs() < 1e-6);
        assert_eq!(summarize.3, Some(0.5));
        let collapse = stats
            .iter()
            .find(|(s, _, _, _)| s == "ToolCollapse")
            .unwrap();
        assert_eq!(collapse.1, 1);
        assert_eq!(collapse.3, None); // no labels yet
    }

    #[test]
    fn tokens_dropped_saturates_when_after_exceeds_before() {
        // Defensive: if a buggy strategy ever reports growth, we
        // return 0 dropped rather than overflowing.
        let e = ev("Buggy", 100, 200, 1);
        assert_eq!(e.tokens_dropped(), 0);
    }
}
