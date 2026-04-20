//! P12.4 — Reflexion-style learn-from-failure.
//!
//! Implements the scaffolding from Shinn et al. 2023, "Reflexion:
//! Language Agents with Verbal Reinforcement Learning"
//! (arXiv:2303.11366). After a failed attempt, the agent generates
//! a natural-language self-critique ("reflection") and stores it in
//! a long-lived buffer that is prepended to the next attempt's
//! prompt. No weight updates — the "RL signal" is verbal.
//!
//! This module provides:
//!
//! 1. [`AttemptOutcome`] — tagged result of one attempt (success or
//!    failure with diagnostic text).
//! 2. [`Reflection`] — one stored lesson with timestamp + tags.
//! 3. [`ReflexionMemory`] — bounded ring buffer of reflections, with
//!    selection by recency and tag filter.
//! 4. [`Reflector`] trait — pluggable critic; default
//!    [`HeuristicReflector`] turns a failure message into a
//!    "next time, avoid X" lesson without an LLM call (useful for
//!    tests and offline runs).
//!
//! The orchestrator wires this in by calling `record_outcome` after
//! each task completion and `prelude_for_prompt` when starting a new
//! attempt on the same logical task.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
pub enum AttemptOutcome {
    Success {
        summary: String,
    },
    Failure {
        /// Whatever diagnostic the verifier or test harness produced.
        /// Stack traces, assertion messages, evaluator feedback.
        error: String,
        /// Optional snippet of what the agent actually tried.
        attempted_action: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Reflection {
    pub task_tag: String,
    pub lesson: String,
    pub created_at: SystemTime,
    /// P13.15 — Ebbinghaus decay: reinforcement count. Each access via
    /// [`ReflexionMemory::recent_for`] / [`ReflexionMemory::reinforce`]
    /// increments this. Higher strength → slower decay.
    pub strength: u32,
    /// P13.15 — last-access timestamp; refreshes on reinforcement.
    pub last_accessed: SystemTime,
}

impl Reflection {
    pub fn new(task_tag: impl Into<String>, lesson: impl Into<String>) -> Self {
        let now = SystemTime::now();
        Self {
            task_tag: task_tag.into(),
            lesson: lesson.into(),
            created_at: now,
            strength: 1,
            last_accessed: now,
        }
    }

    /// P13.15 — Ebbinghaus retention: `R = exp(-t / (S · half_life_secs))`.
    /// Returns a value in `[0, 1]`. `1.0` immediately after access; decays
    /// exponentially with elapsed time, slower for higher‑strength entries.
    /// Cite: Ebbinghaus (1885) *Über das Gedächtnis*; Murre & Dros (2015)
    /// *Replication and Analysis of Ebbinghaus' Forgetting Curve*.
    pub fn recall_strength(&self, now: SystemTime, half_life: Duration) -> f32 {
        let age = now
            .duration_since(self.last_accessed)
            .unwrap_or(Duration::ZERO)
            .as_secs_f32();
        let s = (self.strength as f32) * half_life.as_secs_f32().max(1e-3);
        (-age / s).exp()
    }
}

/// Convert one attempt outcome (or a sequence of them) into a
/// reflection. Returning `None` means "nothing useful to record"
/// (e.g. the attempt succeeded on the first try).
pub trait Reflector {
    fn reflect(
        &self,
        task_tag: &str,
        outcome: &AttemptOutcome,
        prior: &[Reflection],
    ) -> Option<Reflection>;
}

/// Reflector that does not require an LLM. Turns a failure error
/// message into a "next time, avoid X" lesson by extracting the
/// first non-empty line and prefixing it. Successes produce no
/// reflection.
#[derive(Debug, Default, Clone)]
pub struct HeuristicReflector;

impl Reflector for HeuristicReflector {
    fn reflect(
        &self,
        task_tag: &str,
        outcome: &AttemptOutcome,
        _prior: &[Reflection],
    ) -> Option<Reflection> {
        match outcome {
            AttemptOutcome::Success { .. } => None,
            AttemptOutcome::Failure {
                error,
                attempted_action,
            } => {
                let first_line = error
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("(no detail)");
                let lesson = match attempted_action {
                    Some(act) if !act.trim().is_empty() => format!(
                        "Last attempt tried `{}` and failed with: {}. Avoid that path.",
                        act.trim(),
                        first_line
                    ),
                    _ => format!("Previous attempt failed: {}. Avoid the same path.", first_line),
                };
                Some(Reflection::new(task_tag, lesson))
            }
        }
    }
}

/// Bounded ring buffer of reflections. Old entries drop FIFO once
/// `capacity` is exceeded. Optional `ttl` evicts stale lessons even
/// if the buffer isn't full — important so a year-old failure
/// doesn't keep nagging the prompt.
#[derive(Debug)]
pub struct ReflexionMemory {
    pub capacity: usize,
    pub ttl: Option<Duration>,
    /// P13.15 — when set, [`ReflexionMemory::recent_for_with_decay`] returns
    /// only entries whose Ebbinghaus recall strength ≥ `decay_threshold`,
    /// using `decay_half_life` as the time constant. `None` keeps the legacy
    /// recency‑only behaviour.
    pub decay_half_life: Option<Duration>,
    pub decay_threshold: f32,
    buf: VecDeque<Reflection>,
}

impl ReflexionMemory {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            ttl: None,
            decay_half_life: None,
            decay_threshold: 0.5,
            buf: VecDeque::new(),
        }
    }

    pub fn with_ttl(capacity: usize, ttl: Duration) -> Self {
        Self {
            capacity: capacity.max(1),
            ttl: Some(ttl),
            decay_half_life: None,
            decay_threshold: 0.5,
            buf: VecDeque::new(),
        }
    }

    /// P13.15 — Build a memory with Ebbinghaus decay enabled. `half_life` is
    /// the time after which an unreinforced lesson decays to ~37 % strength;
    /// `threshold` is the cut‑off in `[0, 1]` for `recent_for_with_decay`.
    pub fn with_decay(capacity: usize, half_life: Duration, threshold: f32) -> Self {
        Self {
            capacity: capacity.max(1),
            ttl: None,
            decay_half_life: Some(half_life),
            decay_threshold: threshold.clamp(0.0, 1.0),
            buf: VecDeque::new(),
        }
    }

    /// Returns the count after the operation.
    pub fn record(&mut self, r: Reflection) -> usize {
        self.evict_stale();
        if self.buf.len() >= self.capacity {
            self.buf.pop_front();
        }
        self.buf.push_back(r);
        self.buf.len()
    }

    /// Convenience: combine [`Reflector`] + [`AttemptOutcome`] in one
    /// call. Returns the recorded reflection (if any).
    pub fn record_outcome<R: Reflector>(
        &mut self,
        reflector: &R,
        task_tag: &str,
        outcome: &AttemptOutcome,
    ) -> Option<Reflection> {
        let prior: Vec<Reflection> = self.buf.iter().cloned().collect();
        if let Some(r) = reflector.reflect(task_tag, outcome, &prior) {
            self.record(r.clone());
            Some(r)
        } else {
            None
        }
    }

    pub fn evict_stale(&mut self) {
        let Some(ttl) = self.ttl else { return };
        let now = SystemTime::now();
        while let Some(front) = self.buf.front() {
            let age = now
                .duration_since(front.created_at)
                .unwrap_or(Duration::ZERO);
            if age > ttl {
                self.buf.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Most-recent-first list of reflections matching `task_tag`,
    /// limited to `max_n`.
    pub fn recent_for(&self, task_tag: &str, max_n: usize) -> Vec<Reflection> {
        self.buf
            .iter()
            .rev()
            .filter(|r| r.task_tag == task_tag)
            .take(max_n)
            .cloned()
            .collect()
    }

    /// P13.15 — Decay‑aware selection. Returns up to `max_n` reflections
    /// matching `task_tag` whose Ebbinghaus recall strength ≥ the configured
    /// `decay_threshold`. If `decay_half_life` is `None`, falls back to
    /// [`Self::recent_for`]. **This method REINFORCES the returned entries**
    /// (bumps strength + last_accessed) so frequently‑recalled lessons
    /// decay slower — the spaced‑repetition effect.
    pub fn recent_for_with_decay(
        &mut self,
        task_tag: &str,
        max_n: usize,
    ) -> Vec<Reflection> {
        let Some(hl) = self.decay_half_life else {
            return self.recent_for(task_tag, max_n);
        };
        let now = SystemTime::now();
        let threshold = self.decay_threshold;
        // Find indices of survivors (most‑recent first) within tag.
        let mut hits: Vec<usize> = self
            .buf
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, r)| {
                r.task_tag == task_tag && r.recall_strength(now, hl) >= threshold
            })
            .take(max_n)
            .map(|(i, _)| i)
            .collect();
        hits.sort_unstable();
        // Reinforce + clone in one pass.
        let mut out = Vec::with_capacity(hits.len());
        for i in hits {
            if let Some(r) = self.buf.get_mut(i) {
                r.strength = r.strength.saturating_add(1);
                r.last_accessed = now;
                out.push(r.clone());
            }
        }
        // Restore most‑recent‑first to match `recent_for` contract.
        out.reverse();
        out
    }

    /// P13.15 — Manually reinforce the most‑recent reflection matching
    /// `task_tag` (e.g. when the orchestrator confirms a lesson was useful
    /// without explicitly retrieving it). Returns `true` if one was found.
    pub fn reinforce_latest(&mut self, task_tag: &str) -> bool {
        let now = SystemTime::now();
        for r in self.buf.iter_mut().rev() {
            if r.task_tag == task_tag {
                r.strength = r.strength.saturating_add(1);
                r.last_accessed = now;
                return true;
            }
        }
        false
    }

    /// Render reflections into a prompt prelude. Empty string when
    /// no matching reflections exist — caller can unconditionally
    /// concatenate.
    pub fn prelude_for_prompt(&self, task_tag: &str, max_n: usize) -> String {
        let lessons = self.recent_for(task_tag, max_n);
        if lessons.is_empty() {
            return String::new();
        }
        let mut out = String::from("Lessons from previous attempts:\n");
        for (i, l) in lessons.iter().enumerate() {
            out.push_str(&format!("  {}. {}\n", i + 1, l.lesson));
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn p12_4_heuristic_reflector_emits_lesson_on_failure() {
        let r = HeuristicReflector;
        let out = AttemptOutcome::Failure {
            error: "AssertionError: expected 5, got 4\n  at line 12".into(),
            attempted_action: Some("solve_arithmetic(x+1)".into()),
        };
        let lesson = r.reflect("math-task", &out, &[]).expect("must reflect");
        assert_eq!(lesson.task_tag, "math-task");
        assert!(lesson.lesson.contains("solve_arithmetic"));
        assert!(lesson.lesson.contains("AssertionError"));
    }

    #[test]
    fn p12_4_heuristic_reflector_skips_success() {
        let r = HeuristicReflector;
        let out = AttemptOutcome::Success {
            summary: "ok".into(),
        };
        assert!(r.reflect("t", &out, &[]).is_none());
    }

    #[test]
    fn p12_4_memory_capacity_evicts_fifo() {
        let mut mem = ReflexionMemory::new(2);
        mem.record(Reflection::new("t", "lesson 1"));
        mem.record(Reflection::new("t", "lesson 2"));
        mem.record(Reflection::new("t", "lesson 3"));
        assert_eq!(mem.len(), 2);
        let recent = mem.recent_for("t", 10);
        // Most-recent-first; oldest ("lesson 1") was evicted.
        assert_eq!(recent[0].lesson, "lesson 3");
        assert_eq!(recent[1].lesson, "lesson 2");
    }

    #[test]
    fn p12_4_memory_filters_by_task_tag() {
        let mut mem = ReflexionMemory::new(10);
        mem.record(Reflection::new("a", "A1"));
        mem.record(Reflection::new("b", "B1"));
        mem.record(Reflection::new("a", "A2"));
        let a = mem.recent_for("a", 10);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].lesson, "A2");
        let b = mem.recent_for("b", 10);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn p12_4_prelude_renders_when_lessons_exist_else_empty() {
        let mut mem = ReflexionMemory::new(5);
        assert_eq!(mem.prelude_for_prompt("t", 3), "");
        mem.record(Reflection::new("t", "do not call X"));
        let s = mem.prelude_for_prompt("t", 3);
        assert!(s.starts_with("Lessons from previous attempts:"));
        assert!(s.contains("do not call X"));
    }

    #[test]
    fn p12_4_ttl_evicts_stale_entries() {
        let mut mem = ReflexionMemory::with_ttl(10, Duration::from_millis(40));
        mem.record(Reflection::new("t", "old"));
        sleep(Duration::from_millis(80));
        mem.record(Reflection::new("t", "fresh"));
        // record() runs evict_stale before insert, so "old" is gone.
        assert_eq!(mem.len(), 1);
        assert_eq!(mem.recent_for("t", 10)[0].lesson, "fresh");
    }

    #[test]
    fn p12_4_record_outcome_round_trip_with_heuristic() {
        let mut mem = ReflexionMemory::new(5);
        let r = HeuristicReflector;
        let out = AttemptOutcome::Failure {
            error: "timeout".into(),
            attempted_action: None,
        };
        let lesson = mem.record_outcome(&r, "task1", &out).expect("recorded");
        assert!(lesson.lesson.contains("timeout"));
        assert_eq!(mem.len(), 1);
        // Success -> no record.
        let ok = AttemptOutcome::Success {
            summary: "done".into(),
        };
        assert!(mem.record_outcome(&r, "task1", &ok).is_none());
        assert_eq!(mem.len(), 1);
    }

    // ── P13.15 — Ebbinghaus decay ────────────────────────────────────────

    #[test]
    fn p13_15_recall_strength_starts_at_one_and_decays() {
        let r = Reflection::new("t", "lesson");
        let now = SystemTime::now();
        let s0 = r.recall_strength(now, Duration::from_secs(60));
        assert!((s0 - 1.0).abs() < 0.01);
        let later = now + Duration::from_secs(60);
        let s1 = r.recall_strength(later, Duration::from_secs(60));
        // After 1 half-life with strength=1 → exp(-1) ≈ 0.368.
        assert!(s1 < 0.4 && s1 > 0.3, "got {s1}");
    }

    #[test]
    fn p13_15_higher_strength_decays_slower() {
        let mut r = Reflection::new("t", "lesson");
        r.strength = 4;
        let later = r.last_accessed + Duration::from_secs(60);
        let s = r.recall_strength(later, Duration::from_secs(60));
        // strength=4 → effective half-life is 4× → s = exp(-1/4) ≈ 0.78.
        assert!(s > 0.7 && s < 0.85, "got {s}");
    }

    #[test]
    fn p13_15_decay_filter_drops_weak_entries() {
        let mut mem = ReflexionMemory::with_decay(10, Duration::from_millis(20), 0.5);
        mem.record(Reflection::new("t", "old"));
        sleep(Duration::from_millis(80));
        mem.record(Reflection::new("t", "new"));
        let kept = mem.recent_for_with_decay("t", 5);
        // "old" should be below threshold (decayed); "new" survives.
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].lesson, "new");
    }

    #[test]
    fn p13_15_access_reinforces_strength() {
        let mut mem = ReflexionMemory::with_decay(10, Duration::from_secs(60), 0.5);
        mem.record(Reflection::new("t", "lesson"));
        let _ = mem.recent_for_with_decay("t", 5);
        let _ = mem.recent_for_with_decay("t", 5);
        // After two retrievals strength should be 3 (1 base + 2 reinforcements).
        let snapshot = mem.recent_for("t", 5);
        assert_eq!(snapshot[0].strength, 3);
    }

    #[test]
    fn p13_15_reinforce_latest_finds_match() {
        let mut mem = ReflexionMemory::with_decay(10, Duration::from_secs(60), 0.5);
        mem.record(Reflection::new("t", "a"));
        mem.record(Reflection::new("u", "b"));
        assert!(mem.reinforce_latest("t"));
        let s = mem.recent_for("t", 1);
        assert_eq!(s[0].strength, 2);
        assert!(!mem.reinforce_latest("missing"));
    }

    #[test]
    fn p13_15_no_decay_falls_back_to_recency() {
        // Without decay configured, recent_for_with_decay returns the same
        // thing recent_for would.
        let mut mem = ReflexionMemory::new(5);
        mem.record(Reflection::new("t", "a"));
        mem.record(Reflection::new("t", "b"));
        let with = mem.recent_for_with_decay("t", 5);
        assert_eq!(with.len(), 2);
        assert_eq!(with[0].lesson, "b");
    }
}
