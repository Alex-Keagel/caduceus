//! Per-step (process-reward) verification scaffolding (gap G29 / P8.1).
//!
//! `caduceus-core::verification` (G3) scores **outcomes** — a single
//! VoteOutcome over the final answer. PRM (Process-Reward-Model) techniques
//! score **each intermediate reasoning step** and use the per-step signal to
//! prune low-value branches early, weight rollouts in self-consistency
//! voting (Lightman et al. 2023, "Let's Verify Step-by-Step"; Wang et al.
//! 2024, "Math-Shepherd"), or feed a chain-of-verifiers (Dhuliawala et al.
//! 2023). This module owns the *seam*: a small, dependency-free trait plus
//! the value types the rest of the engine can build on.
//!
//! P8.1 ships:
//! - [`StepView`]      — read-only snapshot of a single agent step
//! - [`StepScore`]     — verifier output (reward in `[-1.0, 1.0]` + rationale)
//! - [`StepVerifier`]  — async trait with a single `score` method
//! - [`OffStepVerifier`] — no-op default that always returns `0.0` with a
//!   "verifier disabled" rationale; lets call sites unconditionally invoke
//!   the verifier without scattering `Option<...>` checks.
//!
//! Subsequent P8 todos plug concrete verifiers into this seam:
//! - P8.2 — `RolloutPRM` (LLM-as-judge over the rendered step)
//! - P8.3 — PRM-weighted vote in `verification::majority_vote`
//! - P8.4 — chain-of-verifiers worker that runs N verifiers in parallel
//!
//! ## Design notes
//! - Reward is a single scalar in `[-1.0, 1.0]` (clamped on construction).
//!   Lightman/Math-Shepherd both showed that a tri-state {good, neutral, bad}
//!   collapsed to a scalar carries most of the information; richer schemas
//!   (per-criterion vectors) can be added later without breaking callers.
//! - The trait is async because real verifiers are LLM calls. A sync
//!   wrapper can always be built on top.
//! - `StepView` deliberately **does not** include the full message history —
//!   verifiers should look only at the local step. Cross-step coupling
//!   belongs in P8.4 (chain-of-verifiers), not in the trait shape.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A single tool invocation observed inside a step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservedToolCall {
    pub name: String,
    pub args_summary: String,
    /// Raw textual result the tool returned. Truncate before constructing
    /// the view if the tool returns megabytes — verifiers don't need the
    /// full payload to decide if a step looks reasonable.
    pub result_summary: String,
    /// `true` when the tool reported an error (HTTP 4xx/5xx, exception,
    /// failed assertion, etc.). Matches `ToolResultEnd::is_error`.
    pub is_error: bool,
}

/// Read-only snapshot of one step the verifier scores.
///
/// Holds *only* the local step. Anything broader (prior step rationale,
/// dependency on a sibling branch) is the chain-of-verifiers worker's job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepView {
    /// Monotonic step id allocated by [`crate::SessionState::next_step`].
    pub step_id: u64,
    /// The user's request / system instruction *as the model saw it on this
    /// step*. May be a sliding-window slice when compaction has run.
    pub prompt: String,
    /// The model's text reply for this step. Empty string when the step was
    /// pure tool-call (no `text_delta`).
    pub assistant_text: String,
    /// Tool calls dispatched during this step (oldest-first), with
    /// truncated args + results.
    pub tool_calls: Vec<ObservedToolCall>,
}

impl StepView {
    pub fn new(step_id: u64, prompt: impl Into<String>, assistant_text: impl Into<String>) -> Self {
        Self {
            step_id,
            prompt: prompt.into(),
            assistant_text: assistant_text.into(),
            tool_calls: Vec::new(),
        }
    }

    pub fn with_tool_calls(mut self, calls: Vec<ObservedToolCall>) -> Self {
        self.tool_calls = calls;
        self
    }
}

/// Verifier output for a single step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepScore {
    /// Scalar reward in `[-1.0, 1.0]`. Higher is better. `0.0` is "neutral
    /// / no opinion". Always clamped on construction so downstream
    /// PRM-weighted voting can multiply ballots without out-of-range
    /// weights blowing past `f32::MAX`.
    pub reward: f32,
    /// Free-form natural-language rationale. Surfaced to the UI for HITL
    /// debugging and persisted in trajectories so replays are auditable.
    pub rationale: String,
    /// Identifier of the verifier that produced this score (e.g.
    /// `"rollout-prm:claude-sonnet-4.6"`, `"chain-of-verifiers:3-of-3"`).
    /// Used for telemetry and to gate ensemble combiners.
    pub source: String,
}

impl StepScore {
    /// Construct a score, clamping `reward` into `[-1.0, 1.0]` and
    /// snapping `NaN` / `±inf` to `0.0` so a buggy verifier can never
    /// poison downstream weighted aggregations.
    pub fn new(reward: f32, rationale: impl Into<String>, source: impl Into<String>) -> Self {
        let reward = if reward.is_finite() {
            reward.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        Self {
            reward,
            rationale: rationale.into(),
            source: source.into(),
        }
    }

    /// Construct a neutral (0.0) score from the named verifier with the
    /// supplied rationale. Useful for "verifier abstained" code paths.
    pub fn neutral(source: impl Into<String>, rationale: impl Into<String>) -> Self {
        Self::new(0.0, rationale, source)
    }

    /// `true` iff `reward > 0.0` — semantic shorthand for "PRM thinks this
    /// step was at least slightly correct".
    pub fn is_positive(&self) -> bool {
        self.reward > 0.0
    }
}

/// Pluggable per-step verifier.
///
/// Implementations may be expensive (LLM call, code execution); callers
/// should wrap in a timeout or run inside a bounded pool.
#[async_trait]
pub trait StepVerifier: Send + Sync {
    /// Identifier for telemetry (`StepScore::source`).
    fn name(&self) -> &str;

    /// Score a single step. Implementations MUST NOT panic on adversarial
    /// inputs; on internal failure return [`StepScore::neutral`] with a
    /// rationale that explains the abstention.
    async fn score(&self, step: &StepView) -> StepScore;
}

/// No-op verifier. Always returns a neutral score. Lets the orchestrator
/// instantiate a non-`Option` verifier seam by default.
#[derive(Debug, Default, Clone, Copy)]
pub struct OffStepVerifier;

#[async_trait]
impl StepVerifier for OffStepVerifier {
    fn name(&self) -> &str {
        "off"
    }

    async fn score(&self, _step: &StepView) -> StepScore {
        StepScore::neutral("off", "verifier disabled")
    }
}

// ── G29 / P8.4 — Chain-of-verifiers ensemble ─────────────────────────────────

/// How an [`EnsembleStepVerifier`] combines its members' scores.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EnsembleCombiner {
    /// Arithmetic mean of all member rewards. Robust default.
    Mean,
    /// Median of member rewards. Robust to outlier verifiers but
    /// requires odd-N for a unique value (even-N takes the lower of the
    /// two middle values to stay conservative).
    Median,
    /// Threshold-quorum: returns `+1.0` if at least `frac * N` members
    /// score strictly positive, else `-1.0`. `frac` is clamped to
    /// `[0.0, 1.0]`. A neutral (zero) member counts as NOT positive,
    /// matching `StepScore::is_positive`.
    Threshold(f32),
}

/// Chain-of-verifiers (gap G29 / P8.4).
///
/// Runs N [`StepVerifier`]s in parallel via [`futures::future::join_all`]
/// (all members are independent) and combines their [`StepScore`]s into
/// a single score. The ensemble's `source` tag includes its name and a
/// `K-of-N` summary of how many members were positive — useful for
/// downstream telemetry and debugging false-negatives.
///
/// Failure modes:
/// - **Empty member list**: `score()` returns [`StepScore::neutral`]
///   with rationale `"empty ensemble"`. We deliberately do NOT panic;
///   callers may construct empty ensembles dynamically.
/// - **Member returns NaN/inf**: [`StepScore::new`] already snaps these
///   to zero; the combiner sees only finite rewards.
///
/// Threading: members are polled via `join_all`. Members that themselves
/// spawn tasks must do so independently — this struct has no internal
/// runtime control beyond what `tokio` provides. Callers that need a
/// per-member timeout should wrap each member in a [`TimeoutVerifier`]
/// (planned, not in this PR) or `tokio::time::timeout` adapter.
pub struct EnsembleStepVerifier {
    members: Vec<Arc<dyn StepVerifier>>,
    combiner: EnsembleCombiner,
    // Cached human-readable name for telemetry (`StepScore::source`).
    name_str: String,
}

impl EnsembleStepVerifier {
    pub fn new(members: Vec<Arc<dyn StepVerifier>>, combiner: EnsembleCombiner) -> Self {
        let name_str = format!(
            "chain-of-verifiers:{}",
            match combiner {
                EnsembleCombiner::Mean => "mean".to_string(),
                EnsembleCombiner::Median => "median".to_string(),
                EnsembleCombiner::Threshold(f) => format!("threshold({:.2})", f.clamp(0.0, 1.0)),
            }
        );
        Self {
            members,
            combiner,
            name_str,
        }
    }
}

#[async_trait]
impl StepVerifier for EnsembleStepVerifier {
    fn name(&self) -> &str {
        &self.name_str
    }

    async fn score(&self, step: &StepView) -> StepScore {
        if self.members.is_empty() {
            return StepScore::neutral(self.name(), "empty ensemble");
        }
        // Borrow `self` so closures don't move it; `Arc` clones are cheap.
        let futs: Vec<_> = self
            .members
            .iter()
            .map(|m| {
                let m = Arc::clone(m);
                let s = step.clone();
                async move { m.score(&s).await }
            })
            .collect();
        let scores = futures::future::join_all(futs).await;

        let positive: usize = scores.iter().filter(|s| s.is_positive()).count();
        let total = scores.len();

        let combined = match self.combiner {
            EnsembleCombiner::Mean => {
                let sum: f32 = scores.iter().map(|s| s.reward).sum();
                sum / (total as f32)
            }
            EnsembleCombiner::Median => {
                let mut rs: Vec<f32> = scores.iter().map(|s| s.reward).collect();
                rs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                // Even-N: take the lower of the two middles to stay
                // conservative (don't manufacture an unobserved value).
                rs[(total - 1) / 2]
            }
            EnsembleCombiner::Threshold(frac) => {
                let f = frac.clamp(0.0, 1.0);
                let needed = (f * total as f32).ceil() as usize;
                if positive >= needed.max(1) {
                    1.0
                } else {
                    -1.0
                }
            }
        };

        let rationale = format!("{} of {} members scored positive", positive, total);
        StepScore::new(combined, rationale, self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_score_clamps_above_one() {
        let s = StepScore::new(2.5, "way too high", "test");
        assert_eq!(s.reward, 1.0);
        assert!(s.is_positive());
    }

    #[test]
    fn step_score_clamps_below_neg_one() {
        let s = StepScore::new(-99.0, "way too low", "test");
        assert_eq!(s.reward, -1.0);
        assert!(!s.is_positive());
    }

    #[test]
    fn step_score_snaps_nan_to_zero() {
        let s = StepScore::new(f32::NAN, "buggy verifier", "test");
        assert_eq!(s.reward, 0.0, "NaN must snap to 0.0 to avoid poisoning");
    }

    #[test]
    fn step_score_snaps_inf_to_zero() {
        for v in [f32::INFINITY, f32::NEG_INFINITY] {
            let s = StepScore::new(v, "buggy verifier", "test");
            assert_eq!(s.reward, 0.0, "±inf must snap to 0.0, got {v}");
        }
    }

    #[test]
    fn step_score_neutral_is_zero_and_not_positive() {
        let s = StepScore::neutral("test", "abstain");
        assert_eq!(s.reward, 0.0);
        assert!(!s.is_positive());
    }

    #[test]
    fn step_view_builder_attaches_tool_calls() {
        let calls = vec![ObservedToolCall {
            name: "read_file".into(),
            args_summary: "path=hello.txt".into(),
            result_summary: "world".into(),
            is_error: false,
        }];
        let v = StepView::new(7, "hi", "ok").with_tool_calls(calls.clone());
        assert_eq!(v.step_id, 7);
        assert_eq!(v.tool_calls, calls);
    }

    #[tokio::test]
    async fn off_step_verifier_returns_neutral_score() {
        let v = OffStepVerifier;
        assert_eq!(v.name(), "off");
        let s = v.score(&StepView::new(1, "p", "a")).await;
        assert_eq!(s.reward, 0.0);
        assert_eq!(s.source, "off");
        assert!(s.rationale.contains("disabled"));
    }

    #[test]
    fn step_score_serde_roundtrip() {
        let s = StepScore::new(0.42, "looks reasonable", "rollout-prm");
        let json = serde_json::to_string(&s).expect("serialise");
        let back: StepScore = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, s);
    }

    #[test]
    fn step_view_serde_roundtrip() {
        let v = StepView::new(3, "prompt", "answer").with_tool_calls(vec![ObservedToolCall {
            name: "shell".into(),
            args_summary: "ls".into(),
            result_summary: "a\nb".into(),
            is_error: false,
        }]);
        let json = serde_json::to_string(&v).expect("serialise");
        let back: StepView = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, v);
    }

    // ── G29 / P8.4 — EnsembleStepVerifier tests ───────────────────────────

    /// Test verifier that returns a fixed score.
    struct FixedVerifier(f32);

    #[async_trait]
    impl StepVerifier for FixedVerifier {
        fn name(&self) -> &str {
            "fixed"
        }
        async fn score(&self, _step: &StepView) -> StepScore {
            StepScore::new(self.0, "fixed", "test:fixed")
        }
    }

    fn step() -> StepView {
        StepView::new(0, "p", "a")
    }

    #[tokio::test]
    async fn ensemble_empty_returns_neutral() {
        let e = EnsembleStepVerifier::new(vec![], EnsembleCombiner::Mean);
        let s = e.score(&step()).await;
        assert_eq!(s.reward, 0.0);
        assert!(s.rationale.contains("empty"));
    }

    #[tokio::test]
    async fn ensemble_mean_averages_member_rewards() {
        let members: Vec<Arc<dyn StepVerifier>> = vec![
            Arc::new(FixedVerifier(1.0)),
            Arc::new(FixedVerifier(0.0)),
            Arc::new(FixedVerifier(-1.0)),
        ];
        let e = EnsembleStepVerifier::new(members, EnsembleCombiner::Mean);
        let s = e.score(&step()).await;
        assert!((s.reward - 0.0).abs() < 1e-6, "mean of 1,0,-1 = 0");
        assert!(
            s.rationale.contains("1 of 3"),
            "rationale = {}",
            s.rationale
        );
    }

    #[tokio::test]
    async fn ensemble_median_returns_middle_value() {
        let members: Vec<Arc<dyn StepVerifier>> = vec![
            Arc::new(FixedVerifier(-0.9)),
            Arc::new(FixedVerifier(0.5)),
            Arc::new(FixedVerifier(0.9)),
        ];
        let e = EnsembleStepVerifier::new(members, EnsembleCombiner::Median);
        let s = e.score(&step()).await;
        assert!((s.reward - 0.5).abs() < 1e-6);
    }

    #[tokio::test]
    async fn ensemble_threshold_quorum_returns_pos_when_met() {
        // 2 of 3 positive ≥ ceil(0.6 * 3) = 2 → positive.
        let members: Vec<Arc<dyn StepVerifier>> = vec![
            Arc::new(FixedVerifier(1.0)),
            Arc::new(FixedVerifier(1.0)),
            Arc::new(FixedVerifier(-1.0)),
        ];
        let e = EnsembleStepVerifier::new(members, EnsembleCombiner::Threshold(0.6));
        let s = e.score(&step()).await;
        assert_eq!(s.reward, 1.0);
    }

    #[tokio::test]
    async fn ensemble_threshold_quorum_returns_neg_when_unmet() {
        // 1 of 3 positive < ceil(0.6 * 3) = 2 → negative.
        let members: Vec<Arc<dyn StepVerifier>> = vec![
            Arc::new(FixedVerifier(1.0)),
            Arc::new(FixedVerifier(-1.0)),
            Arc::new(FixedVerifier(0.0)),
        ];
        let e = EnsembleStepVerifier::new(members, EnsembleCombiner::Threshold(0.6));
        let s = e.score(&step()).await;
        assert_eq!(s.reward, -1.0);
    }

    #[tokio::test]
    async fn ensemble_source_tag_includes_combiner_name() {
        let e =
            EnsembleStepVerifier::new(vec![Arc::new(FixedVerifier(1.0))], EnsembleCombiner::Mean);
        let s = e.score(&step()).await;
        assert_eq!(s.source, "chain-of-verifiers:mean");
    }

    #[tokio::test]
    async fn ensemble_member_with_nan_does_not_poison() {
        // FixedVerifier(NaN) is snapped to 0.0 by StepScore::new before
        // the combiner sees it; mean of [1.0, 0.0] = 0.5 (not NaN).
        let members: Vec<Arc<dyn StepVerifier>> = vec![
            Arc::new(FixedVerifier(1.0)),
            Arc::new(FixedVerifier(f32::NAN)),
        ];
        let e = EnsembleStepVerifier::new(members, EnsembleCombiner::Mean);
        let s = e.score(&step()).await;
        assert!((s.reward - 0.5).abs() < 1e-6, "got {}", s.reward);
    }
}
