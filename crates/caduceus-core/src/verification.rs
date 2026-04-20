//! Verification strategies for agent turns (gap G3).
//!
//! Single-trajectory agents commit to the first plausible answer the model
//! emits. Verification re-samples the answer N times and picks the
//! consensus, which Anthropic's SWE-Bench-Verified report (2024) showed
//! to be worth +8–15 percentage points on hard coding tasks.
//!
//! This module contains:
//! - [`VerificationStrategy`] — selector enum (Off / RolloutVote / TestGated)
//! - [`majority_vote`]        — pure tally function (no I/O), unit-testable
//! - [`VoteOutcome`]          — what the harness should do with the result
//!
//! The strategy is plumbed into `caduceus-orchestrator::AgentHarness`
//! which decides when to actually invoke a verifier; this crate only owns
//! the data contract + tally so consumers (eval harness, future learners)
//! can reason about strategies without depending on the orchestrator.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Verification strategy applied AFTER the main agent loop finishes.
///
/// The loop itself is unchanged — verification wraps the loop's final
/// textual answer. Side-effecting tool calls are NOT replayed N times.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VerificationStrategy {
    /// Default. Fall through with whatever the loop produced.
    #[default]
    Off,
    /// Self-consistency vote: re-sample the final answer `samples` times
    /// from the same prompt + transcript, return the plurality answer.
    /// Anthropic SWE-Bench-Verified used N=3; we default to that.
    RolloutVote { samples: usize },
    /// Re-run the project's test command against each of N candidate
    /// final answers; return the first that passes. Wired in P2.2.
    TestGated { samples: usize },
    /// Self-consistency vote weighted by per-sample PRM (process-reward)
    /// scores from a [`crate::StepVerifier`]. Wired in P8.3 (gap G29).
    /// Falls back to plain plurality when every sample is rejected.
    PrmWeightedVote { samples: usize },
    /// Confidence-Informed Self-Consistency (CISC, Aggarwal et al. 2023
    /// / "Internal Consistency Improves Self-Consistency"). Each rollout
    /// is weighted by the model's own internal confidence — the average
    /// `exp(token_logprob)` over the answer tokens. Strong answers
    /// dominate the vote without an external verifier. Implemented via
    /// the `logprobs` field on `ChatRequest`; falls back to plain
    /// plurality if logprobs aren't returned. Wired in P10.1 (gap G30).
    CiscWeightedVote { samples: usize },
}

impl VerificationStrategy {
    /// Convenience: standard 3-sample self-consistency vote.
    pub fn rollout_vote_default() -> Self {
        VerificationStrategy::RolloutVote { samples: 3 }
    }

    /// How many extra answer rollouts this strategy will request, on top
    /// of the original loop's answer. `Off` returns 0.
    pub fn extra_samples(&self) -> usize {
        match self {
            VerificationStrategy::Off => 0,
            VerificationStrategy::RolloutVote { samples }
            | VerificationStrategy::TestGated { samples }
            | VerificationStrategy::PrmWeightedVote { samples }
            | VerificationStrategy::CiscWeightedVote { samples } => {
                // Sanity-clamp: 0 or 1 sample is meaningless for a vote
                // (no second opinion). Normalise to the minimum that
                // changes behaviour, so a misconfigured `samples=0`
                // doesn't silently disable verification.
                (*samples).max(2)
            }
        }
    }
}

/// Result of voting across N candidate answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteOutcome {
    /// The winning answer (string equality, post-trim).
    pub winner: String,
    /// Vote count for the winner.
    pub winner_votes: usize,
    /// Total ballots cast (including the original answer).
    pub total_votes: usize,
    /// True iff one answer received a strict majority (> total/2).
    /// When false, the winner won a plurality or by tie-break.
    pub had_majority: bool,
}

/// Tally a slice of candidate answers and pick the most common.
///
/// Rules:
/// - Trims each ballot before comparison so trailing whitespace doesn't
///   split votes. Returned `winner` preserves the original (trimmed) form.
/// - Tie-breaks by the FIRST occurrence in the slice — important because
///   the original loop's answer is conventionally placed at index 0,
///   so a tie defaults to "trust the original" rather than to a random
///   re-roll.
/// - Returns `None` for an empty slice; callers should fall back to the
///   original answer.
pub fn majority_vote(ballots: &[String]) -> Option<VoteOutcome> {
    if ballots.is_empty() {
        return None;
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut first_seen: HashMap<&str, usize> = HashMap::new();
    for (idx, b) in ballots.iter().enumerate() {
        let key = b.trim();
        *counts.entry(key).or_insert(0) += 1;
        first_seen.entry(key).or_insert(idx);
    }
    // Pick max by (count desc, first_seen asc).
    let (winner_key, winner_votes) = counts
        .iter()
        .max_by(|a, b| {
            a.1.cmp(b.1).then_with(|| {
                // Lower first_seen wins on ties.
                first_seen[b.0].cmp(&first_seen[a.0])
            })
        })
        .map(|(k, v)| (*k, *v))?;
    let total = ballots.len();
    Some(VoteOutcome {
        winner: winner_key.to_string(),
        winner_votes,
        total_votes: total,
        had_majority: winner_votes * 2 > total,
    })
}

/// PRM-weighted vote (gap G29 / P8.3).
///
/// Same shape as [`majority_vote`] but each ballot carries a per-step
/// weight from a [`crate::StepVerifier`]. Used by self-consistency
/// ensembles where some rollouts are known a-priori to be better than
/// others.
///
/// Weighting rule (Wang et al. 2024, "Math-Shepherd"):
/// - Each ballot's weight is `(reward + 1.0) / 2.0` clamped to `[0.0, 1.0]`,
///   so a strongly-rejected step (`reward = -1.0`) contributes 0 votes,
///   neutral steps contribute 0.5, perfect steps contribute 1.0.
/// - When the total weight collapses to 0 (every ballot was rejected),
///   we fall back to plain [`majority_vote`] over the same ballots so the
///   harness still produces *some* answer instead of returning `None`.
/// - `winner_votes` and `total_votes` in the returned [`VoteOutcome`] are
///   `weighted_count.round()` for compatibility with the existing UI,
///   while `had_majority` is computed against the (continuous) weighted
///   total. This means a barely-positive weighted plurality of 0.51 still
///   reports `had_majority = false` — we report only strict majorities.
pub fn weighted_majority_vote(ballots: &[(String, f32)]) -> Option<VoteOutcome> {
    if ballots.is_empty() {
        return None;
    }
    let mut weights: HashMap<&str, f64> = HashMap::new();
    let mut first_seen: HashMap<&str, usize> = HashMap::new();
    let mut total_weight: f64 = 0.0;
    for (idx, (b, reward)) in ballots.iter().enumerate() {
        let key = b.trim();
        let w = if reward.is_finite() {
            ((*reward as f64 + 1.0) / 2.0).clamp(0.0, 1.0)
        } else {
            0.5
        };
        *weights.entry(key).or_insert(0.0) += w;
        first_seen.entry(key).or_insert(idx);
        total_weight += w;
    }
    if total_weight <= 0.0 {
        // Every ballot was max-rejected. Fall back to unweighted vote so
        // we still surface *some* answer for the UI.
        let plain: Vec<String> = ballots.iter().map(|(b, _)| b.clone()).collect();
        return majority_vote(&plain);
    }
    let (winner_key, winner_weight) = weights
        .iter()
        .max_by(|a, b| {
            a.1.partial_cmp(b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| first_seen[b.0].cmp(&first_seen[a.0]))
        })
        .map(|(k, v)| (*k, *v))?;
    let had_majority = winner_weight * 2.0 > total_weight;
    Some(VoteOutcome {
        winner: winner_key.to_string(),
        // Round so the UI stays integer-shaped; preserves the contract
        // that `winner_votes <= total_votes`.
        winner_votes: winner_weight.round().max(0.0) as usize,
        total_votes: total_weight.round().max(0.0) as usize,
        had_majority,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_samples_clamps_low_values() {
        assert_eq!(VerificationStrategy::Off.extra_samples(), 0);
        assert_eq!(
            VerificationStrategy::RolloutVote { samples: 0 }.extra_samples(),
            2,
            "samples=0 must clamp to 2 so a misconfig still verifies"
        );
        assert_eq!(
            VerificationStrategy::RolloutVote { samples: 1 }.extra_samples(),
            2
        );
        assert_eq!(
            VerificationStrategy::RolloutVote { samples: 5 }.extra_samples(),
            5
        );
    }

    #[test]
    fn rollout_vote_default_is_three() {
        assert_eq!(
            VerificationStrategy::rollout_vote_default(),
            VerificationStrategy::RolloutVote { samples: 3 }
        );
    }

    #[test]
    fn vote_unanimous_returns_majority() {
        let ballots = vec!["yes".into(), "yes".into(), "yes".into()];
        let r = majority_vote(&ballots).unwrap();
        assert_eq!(r.winner, "yes");
        assert_eq!(r.winner_votes, 3);
        assert!(r.had_majority);
    }

    #[test]
    fn vote_plurality_wins_without_majority() {
        // 2 vs 1 vs 1 — winner has plurality but not strict majority.
        let ballots = vec!["a".into(), "a".into(), "b".into(), "c".into()];
        let r = majority_vote(&ballots).unwrap();
        assert_eq!(r.winner, "a");
        assert_eq!(r.winner_votes, 2);
        assert!(!r.had_majority, "2/4 is not strict majority");
    }

    #[test]
    fn vote_tie_breaks_to_first_seen() {
        // a@0, b@1, b@2, a@3 → tie 2–2, "a" wins because seen first.
        let ballots = vec!["a".into(), "b".into(), "b".into(), "a".into()];
        let r = majority_vote(&ballots).unwrap();
        assert_eq!(r.winner, "a");
    }

    #[test]
    fn vote_trims_whitespace_before_compare() {
        let ballots = vec!["yes".into(), "yes\n".into(), "  yes ".into()];
        let r = majority_vote(&ballots).unwrap();
        assert_eq!(r.winner, "yes");
        assert_eq!(r.winner_votes, 3);
    }

    #[test]
    fn vote_empty_returns_none() {
        let ballots: Vec<String> = vec![];
        assert!(majority_vote(&ballots).is_none());
    }

    #[test]
    fn vote_single_ballot_is_majority() {
        let r = majority_vote(&["solo".into()]).unwrap();
        assert!(r.had_majority);
        assert_eq!(r.winner_votes, 1);
    }

    #[test]
    fn strategy_serde_roundtrip() {
        let s = VerificationStrategy::RolloutVote { samples: 5 };
        let json = serde_json::to_string(&s).unwrap();
        let back: VerificationStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    // ── G29 / P8.3 — weighted_majority_vote ───────────────────────────────────

    #[test]
    fn weighted_vote_unanimous_high_reward_returns_majority() {
        let ballots = vec![
            ("yes".to_string(), 1.0),
            ("yes".to_string(), 1.0),
            ("yes".to_string(), 1.0),
        ];
        let r = weighted_majority_vote(&ballots).unwrap();
        assert_eq!(r.winner, "yes");
        assert!(r.had_majority);
    }

    #[test]
    fn weighted_vote_high_weighted_minority_overrides_low_weighted_majority() {
        // 3 weak rollouts say "wrong" (reward 0.1 each → 0.55 each, total 1.65)
        // 2 strong rollouts say "right" (reward 1.0 each → 1.0 each, total 2.0)
        // Weighted winner must be "right" even though it has fewer raw ballots.
        let ballots = vec![
            ("wrong".to_string(), 0.1),
            ("wrong".to_string(), 0.1),
            ("wrong".to_string(), 0.1),
            ("right".to_string(), 1.0),
            ("right".to_string(), 1.0),
        ];
        let r = weighted_majority_vote(&ballots).unwrap();
        assert_eq!(
            r.winner, "right",
            "PRM weighting must override raw-count plurality"
        );
    }

    #[test]
    fn weighted_vote_falls_back_to_plain_when_all_rejected() {
        // Every ballot has reward = -1.0 → weight 0 → total weight 0.
        // Fallback: plain plurality.
        let ballots = vec![
            ("a".to_string(), -1.0),
            ("a".to_string(), -1.0),
            ("b".to_string(), -1.0),
        ];
        let r = weighted_majority_vote(&ballots).unwrap();
        assert_eq!(
            r.winner, "a",
            "must fall back to plain majority on zero weight"
        );
    }

    #[test]
    fn weighted_vote_handles_nan_reward_as_neutral() {
        // NaN weight collapses to 0.5 (neutral), so a NaN ballot still
        // contributes equally to both branches. Two NaNs for "a" beat one
        // strong "b".
        let ballots = vec![
            ("a".to_string(), f32::NAN),
            ("a".to_string(), f32::NAN),
            ("b".to_string(), 1.0),
        ];
        let r = weighted_majority_vote(&ballots).unwrap();
        assert_eq!(r.winner, "a");
    }

    #[test]
    fn weighted_vote_empty_returns_none() {
        let ballots: Vec<(String, f32)> = vec![];
        assert!(weighted_majority_vote(&ballots).is_none());
    }

    #[test]
    fn weighted_vote_trims_whitespace_before_compare() {
        let ballots = vec![
            ("yes".to_string(), 1.0),
            ("yes\n".to_string(), 1.0),
            ("  yes ".to_string(), 1.0),
        ];
        let r = weighted_majority_vote(&ballots).unwrap();
        assert_eq!(r.winner, "yes");
        assert!(r.had_majority);
    }

    #[test]
    fn weighted_vote_tie_breaks_to_first_seen() {
        // Two answers tied at weight 1.0; first-seen wins.
        let ballots = vec![("a".to_string(), 1.0), ("b".to_string(), 1.0)];
        let r = weighted_majority_vote(&ballots).unwrap();
        assert_eq!(r.winner, "a");
    }

    #[test]
    fn weighted_vote_neutral_reward_yields_no_strict_majority_two_way_tie() {
        // Both sides at neutral (weight 0.5 each) — neither strictly > total/2.
        let ballots = vec![("a".to_string(), 0.0), ("b".to_string(), 0.0)];
        let r = weighted_majority_vote(&ballots).unwrap();
        assert!(!r.had_majority, "tied weights cannot be strict majority");
    }
}
