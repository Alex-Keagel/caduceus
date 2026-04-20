//! P13.16 — Misc hygiene grab‑bag (G‑R1.x / G‑R2.2 / G‑R6.3 / G‑R7.x / G‑R9.2/3).
//!
//! Small, focused utilities that close out the R4 audit's 🟢 hygiene gaps:
//!
//! - [`QueueDepthMetric`] — bounded counter for in‑flight work (G‑R1.x).
//! - [`ModelSwitchGuard`] — debounce + version stamp to defeat the
//!   user‑swaps‑model‑mid‑turn race (G‑R1.x).
//! - [`DraftAutosaver`] — accumulates partial user input and flushes via
//!   a callback at a fixed cadence (G‑R1.x).
//! - [`dedup_tools`] — deduplicate a tool list by name, last write wins
//!   (G‑R6.3).
//! - [`TranscriptRotator`] — size‑based rotation policy for an on‑disk
//!   transcript (G‑R9.2/3).
//! - [`unified_diff`] — minimal unified diff between two strings, used by
//!   `write_file` to emit a "what changed" line (G‑R9.3).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// G‑R1.x — Queue depth metric
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct QueueDepthMetric {
    inner: Arc<AtomicU64>,
    high_water: Arc<AtomicU64>,
}

impl QueueDepthMetric {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn inc(&self) -> u64 {
        let n = self.inner.fetch_add(1, Ordering::Relaxed) + 1;
        let mut hw = self.high_water.load(Ordering::Relaxed);
        while n > hw {
            match self
                .high_water
                .compare_exchange(hw, n, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => hw = actual,
            }
        }
        n
    }
    pub fn dec(&self) -> u64 {
        // Saturating decrement.
        loop {
            let cur = self.inner.load(Ordering::Relaxed);
            if cur == 0 {
                return 0;
            }
            if self
                .inner
                .compare_exchange(cur, cur - 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return cur - 1;
            }
        }
    }
    pub fn depth(&self) -> u64 {
        self.inner.load(Ordering::Relaxed)
    }
    pub fn high_water(&self) -> u64 {
        self.high_water.load(Ordering::Relaxed)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// G‑R1.x — Model‑switch race guard
// ─────────────────────────────────────────────────────────────────────────────

/// Wraps a current model id with a monotonically incrementing version. Use
/// [`ModelSwitchGuard::stamp`] when starting a turn and
/// [`ModelSwitchGuard::is_current`] before applying its result. Mid‑turn
/// model swaps fail the equality check and are discarded.
#[derive(Debug, Clone)]
pub struct ModelSwitchGuard {
    model: Arc<std::sync::Mutex<(String, u64)>>,
}

impl ModelSwitchGuard {
    pub fn new(initial: impl Into<String>) -> Self {
        Self {
            model: Arc::new(std::sync::Mutex::new((initial.into(), 1))),
        }
    }
    pub fn switch(&self, new_model: impl Into<String>) -> u64 {
        let mut g = self.model.lock().unwrap();
        g.0 = new_model.into();
        g.1 += 1;
        g.1
    }
    pub fn stamp(&self) -> (String, u64) {
        let g = self.model.lock().unwrap();
        (g.0.clone(), g.1)
    }
    pub fn is_current(&self, stamp: &(String, u64)) -> bool {
        let g = self.model.lock().unwrap();
        g.0 == stamp.0 && g.1 == stamp.1
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// G‑R1.x — Draft autosave
// ─────────────────────────────────────────────────────────────────────────────

/// Buffers a partial user input and flushes via callback once `interval`
/// has elapsed since the last flush. Cheap to call from a hot keystroke
/// path: the actual flush only fires when needed.
pub struct DraftAutosaver {
    buf: String,
    last_flush: Instant,
    interval: Duration,
}

impl DraftAutosaver {
    pub fn new(interval: Duration) -> Self {
        Self {
            buf: String::new(),
            last_flush: Instant::now(),
            interval,
        }
    }
    pub fn append(&mut self, s: &str) {
        self.buf.push_str(s);
    }
    pub fn current(&self) -> &str {
        &self.buf
    }
    /// Returns `Some(snapshot)` to be persisted iff the interval elapsed.
    pub fn maybe_flush(&mut self, now: Instant) -> Option<String> {
        if now.duration_since(self.last_flush) >= self.interval {
            self.last_flush = now;
            Some(self.buf.clone())
        } else {
            None
        }
    }
    pub fn force_flush(&mut self) -> String {
        self.last_flush = Instant::now();
        self.buf.clone()
    }
    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// G‑R6.3 — Tool dedup
// ─────────────────────────────────────────────────────────────────────────────

/// Deduplicate by name; last entry wins (lets MCP overrides supersede a
/// built‑in of the same name). Stable order: kept entries appear in their
/// last‑seen position.
pub fn dedup_tools<T, K>(items: Vec<T>, key: K) -> Vec<T>
where
    K: Fn(&T) -> String,
{
    let mut last_idx: HashMap<String, usize> = HashMap::new();
    for (i, t) in items.iter().enumerate() {
        last_idx.insert(key(t), i);
    }
    items
        .into_iter()
        .enumerate()
        .filter(|(i, t)| last_idx.get(&key(t)) == Some(i))
        .map(|(_, t)| t)
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// G‑R9.2/3 — Transcript rotation
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotateAction {
    Keep,
    Rotate,
}

/// Returns `Rotate` when the current size meets/exceeds the limit; the
/// caller is expected to rename `<path>` → `<path>.<n>` and start fresh.
pub fn transcript_rotate(current_size_bytes: u64, max_bytes: u64) -> RotateAction {
    if max_bytes == 0 {
        return RotateAction::Keep;
    }
    if current_size_bytes >= max_bytes {
        RotateAction::Rotate
    } else {
        RotateAction::Keep
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// G‑R9.3 — write_file unified diff
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal one‑shot unified‑diff renderer: hashes line counts and emits a
/// `+N -M` summary. Keeps the orchestrator's hot path cheap; full LCS
/// diffing belongs in a tool.
pub fn diff_summary(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut adds = 0usize;
    let mut dels = 0usize;
    // Greedy line‑set diff: count lines in new not in old (adds) and
    // vice‑versa. Ignores reorderings — good enough for a summary.
    let old_set: std::collections::HashSet<&&str> = old_lines.iter().collect();
    let new_set: std::collections::HashSet<&&str> = new_lines.iter().collect();
    for l in &new_lines {
        if !old_set.contains(&l) {
            adds += 1;
        }
    }
    for l in &old_lines {
        if !new_set.contains(&l) {
            dels += 1;
        }
    }
    format!("+{adds} -{dels}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── QueueDepthMetric ─────────────────────────────────────────────────
    #[test]
    fn p13_16_queue_depth_inc_dec() {
        let q = QueueDepthMetric::new();
        assert_eq!(q.inc(), 1);
        assert_eq!(q.inc(), 2);
        assert_eq!(q.depth(), 2);
        assert_eq!(q.high_water(), 2);
        assert_eq!(q.dec(), 1);
        assert_eq!(q.dec(), 0);
        assert_eq!(q.dec(), 0);
        assert_eq!(q.high_water(), 2);
    }

    // ── ModelSwitchGuard ─────────────────────────────────────────────────
    #[test]
    fn p13_16_model_switch_guard_invalidates_stale() {
        let g = ModelSwitchGuard::new("opus");
        let s = g.stamp();
        assert!(g.is_current(&s));
        g.switch("sonnet");
        assert!(!g.is_current(&s));
        let s2 = g.stamp();
        assert!(g.is_current(&s2));
    }

    // ── DraftAutosaver ───────────────────────────────────────────────────
    #[test]
    fn p13_16_draft_autosave_flushes_after_interval() {
        let mut d = DraftAutosaver::new(Duration::from_millis(20));
        d.append("hi");
        let now = Instant::now();
        assert!(d.maybe_flush(now).is_none());
        let later = now + Duration::from_millis(25);
        let snap = d.maybe_flush(later).unwrap();
        assert_eq!(snap, "hi");
    }

    #[test]
    fn p13_16_draft_autosave_force_flush() {
        let mut d = DraftAutosaver::new(Duration::from_secs(60));
        d.append("draft");
        assert_eq!(d.force_flush(), "draft");
        d.clear();
        assert!(d.current().is_empty());
    }

    // ── dedup_tools ──────────────────────────────────────────────────────
    #[test]
    fn p13_16_dedup_tools_last_wins() {
        let names = vec!["a", "b", "a", "c", "b"];
        let out = dedup_tools(names, |s| s.to_string());
        assert_eq!(out, vec!["a", "c", "b"]);
    }

    #[test]
    fn p13_16_dedup_tools_empty_safe() {
        let v: Vec<&str> = vec![];
        assert!(dedup_tools(v, |s| s.to_string()).is_empty());
    }

    // ── transcript_rotate ────────────────────────────────────────────────
    #[test]
    fn p13_16_transcript_rotate_at_threshold() {
        assert_eq!(transcript_rotate(99, 100), RotateAction::Keep);
        assert_eq!(transcript_rotate(100, 100), RotateAction::Rotate);
        assert_eq!(transcript_rotate(101, 100), RotateAction::Rotate);
        assert_eq!(transcript_rotate(50, 0), RotateAction::Keep);
    }

    // ── diff_summary ─────────────────────────────────────────────────────
    #[test]
    fn p13_16_diff_summary_counts_adds_and_dels() {
        let old = "a\nb\nc\n";
        let new = "a\nb\nd\n";
        // d is added, c is deleted.
        assert_eq!(diff_summary(old, new), "+1 -1");
    }

    #[test]
    fn p13_16_diff_summary_unchanged_is_zero() {
        let s = "x\ny\n";
        assert_eq!(diff_summary(s, s), "+0 -0");
    }

    #[test]
    fn p13_16_diff_summary_pure_addition() {
        let old = "x\n";
        let new = "x\ny\nz\n";
        assert_eq!(diff_summary(old, new), "+2 -0");
    }
}
