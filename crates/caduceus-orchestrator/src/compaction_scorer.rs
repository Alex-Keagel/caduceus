//! Bradley–Terry scorer for compaction strategies (gap G5 step 2 / P5.2).
//!
//! Given a stream of `CompactionEvent`s with `downstream_re_ask`
//! labels (collected by `compaction_telemetry`), this module fits a
//! per-strategy "skill" score using the classical Bradley–Terry
//! pairwise-comparison model:
//!
//!     P(A beats B) = exp(s_A) / (exp(s_A) + exp(s_B))
//!
//! "A beats B" means: in two events with comparable context size,
//! strategy A had `downstream_re_ask = false` and B had `=
//! true`. We bucket events by a coarse context-size key
//! (`tokens_before / 4096`) to ensure we're comparing strategies
//! under similar pressure rather than mixing trivial and emergency
//! compactions.
//!
//! Parameter estimation is done via gradient ascent on the
//! log-likelihood. Convergence is fast (typically <100 iterations)
//! because the BT log-likelihood is concave. We anchor the first
//! strategy at score 0 so the absolute scale is fixed (BT is
//! invariant to additive shifts).
//!
//! P5.3 will use the resulting `BradleyTerryModel` to override the
//! heuristic strategy selector at compaction time.

use crate::compaction_telemetry::CompactionEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A pairwise comparison: `winner` had a better outcome than `loser`
/// under comparable context pressure. Weight allows soft labels (e.g.
/// when both events were `re_ask=false`, use weight 0).
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub winner: String,
    pub loser: String,
    pub weight: f64,
}

/// Trained Bradley–Terry skill scores per strategy. Higher = better
/// (lower downstream re-ask rate under comparable pressure).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BradleyTerryModel {
    pub scores: HashMap<String, f64>,
    /// Final log-likelihood reached at convergence — useful for
    /// telemetry / regression tests.
    pub log_likelihood: f64,
    pub iterations: u32,
}

impl BradleyTerryModel {
    /// Pick the highest-scoring strategy among `candidates`. Returns
    /// `None` if `candidates` is empty. Unknown strategies (not in
    /// the trained set) get the global mean score so they aren't
    /// silently buried.
    pub fn pick<'a>(&self, candidates: &'a [&str]) -> Option<&'a str> {
        if candidates.is_empty() {
            return None;
        }
        let fallback = if self.scores.is_empty() {
            0.0
        } else {
            self.scores.values().sum::<f64>() / self.scores.len() as f64
        };
        let mut best = candidates[0];
        let mut best_score = self.scores.get(best).copied().unwrap_or(fallback);
        for &c in &candidates[1..] {
            let s = self.scores.get(c).copied().unwrap_or(fallback);
            if s > best_score {
                best_score = s;
                best = c;
            }
        }
        Some(best)
    }
}

/// Bucket events by coarse context size and emit one Pair per
/// (winner, loser) within each bucket where outcomes differ. Buckets
/// of width 4096 tokens balance "comparable pressure" vs sample size.
pub fn pairs_from_events(events: &[CompactionEvent]) -> Vec<Pair> {
    const BUCKET_TOKENS: u32 = 4096;
    let mut buckets: HashMap<u32, Vec<&CompactionEvent>> = HashMap::new();
    for ev in events {
        if ev.downstream_re_ask.is_none() {
            continue; // unlabelled events are useless for training
        }
        let key = ev.tokens_before / BUCKET_TOKENS;
        buckets.entry(key).or_default().push(ev);
    }

    let mut pairs = Vec::new();
    for (_bucket, evs) in buckets {
        for i in 0..evs.len() {
            for j in (i + 1)..evs.len() {
                let a = evs[i];
                let b = evs[j];
                if a.strategy == b.strategy {
                    continue; // same strategy → no preference signal
                }
                let a_good = a.downstream_re_ask == Some(false);
                let b_good = b.downstream_re_ask == Some(false);
                if a_good && !b_good {
                    pairs.push(Pair {
                        winner: a.strategy.clone(),
                        loser: b.strategy.clone(),
                        weight: 1.0,
                    });
                } else if b_good && !a_good {
                    pairs.push(Pair {
                        winner: b.strategy.clone(),
                        loser: a.strategy.clone(),
                        weight: 1.0,
                    });
                }
                // Both good or both bad: no signal — drop.
            }
        }
    }
    pairs
}

/// Fit BT scores to the given pairs. Anchors the first strategy
/// (alphabetically) at score 0 so the absolute scale is fixed.
/// Returns a model with empty scores if `pairs` is empty.
///
/// Hyperparameters tuned for typical sample sizes (10–10K pairs):
///   * learning_rate = 0.1
///   * max_iters = 500
///   * tol = 1e-5 on log-likelihood improvement
pub fn fit(pairs: &[Pair]) -> BradleyTerryModel {
    if pairs.is_empty() {
        return BradleyTerryModel::default();
    }

    // Collect strategy set; sort for determinism.
    let mut strategies: Vec<String> = pairs
        .iter()
        .flat_map(|p| [p.winner.clone(), p.loser.clone()])
        .collect();
    strategies.sort();
    strategies.dedup();
    if strategies.len() < 2 {
        // Only one strategy seen — can't compare; return uniform.
        let mut scores = HashMap::new();
        scores.insert(strategies[0].clone(), 0.0);
        return BradleyTerryModel {
            scores,
            log_likelihood: 0.0,
            iterations: 0,
        };
    }

    let idx: HashMap<String, usize> = strategies
        .iter()
        .enumerate()
        .map(|(i, s)| (s.clone(), i))
        .collect();
    let n = strategies.len();
    let mut s = vec![0.0_f64; n];

    let lr = 0.1_f64;
    let max_iters = 500_u32;
    let tol = 1e-5_f64;

    let log_lik = |s: &[f64]| -> f64 {
        pairs
            .iter()
            .map(|p| {
                let w = idx[&p.winner];
                let l = idx[&p.loser];
                let diff = s[w] - s[l];
                p.weight * (-((-diff).exp()).ln_1p())
            })
            .sum()
    };

    let mut prev_ll = log_lik(&s);
    let mut iters = 0_u32;
    for it in 0..max_iters {
        iters = it + 1;
        let mut grad = vec![0.0_f64; n];
        for p in pairs {
            let w = idx[&p.winner];
            let l = idx[&p.loser];
            // dLL/d s_w =  weight * sigmoid(s_l - s_w)
            // dLL/d s_l = -weight * sigmoid(s_l - s_w)
            let sig = sigmoid(s[l] - s[w]);
            grad[w] += p.weight * sig;
            grad[l] -= p.weight * sig;
        }
        for i in 0..n {
            s[i] += lr * grad[i];
        }
        // Anchor strategy 0 at 0 (BT is shift-invariant).
        let anchor = s[0];
        for v in s.iter_mut() {
            *v -= anchor;
        }
        let ll = log_lik(&s);
        if (ll - prev_ll).abs() < tol {
            prev_ll = ll;
            break;
        }
        prev_ll = ll;
    }

    let scores: HashMap<String, f64> = strategies
        .iter()
        .zip(s.iter())
        .map(|(name, &v)| (name.clone(), v))
        .collect();
    BradleyTerryModel {
        scores,
        log_likelihood: prev_ll,
        iterations: iters,
    }
}

#[inline]
fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// Convenience: parse a JSONL stream of `CompactionEvent` records,
/// extract pairwise comparisons, and fit the Bradley–Terry model.
/// Lines that fail to parse are skipped silently — callers wanting
/// strict validation should use `pairs_from_events(..)` + `fit(..)`
/// directly. Designed for the session-startup trainer that reads the
/// previous session's drained telemetry tape (P9.2).
pub fn train_from_jsonl(jsonl: &str) -> BradleyTerryModel {
    let events: Vec<CompactionEvent> = jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let pairs = pairs_from_events(&events);
    fit(&pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(strategy: &str, before: u32, re_ask: bool, turn: u32) -> CompactionEvent {
        CompactionEvent {
            strategy: strategy.into(),
            tokens_before: before,
            tokens_after: before / 2,
            messages_before: 10,
            messages_after: 5,
            turn_index: turn,
            at_secs: 0,
            downstream_re_ask: Some(re_ask),
        }
    }

    #[test]
    fn empty_input_returns_empty_model() {
        let m = fit(&[]);
        assert!(m.scores.is_empty());
        assert_eq!(m.iterations, 0);
    }

    #[test]
    fn pairs_from_events_drops_unlabelled() {
        let mut a = ev("A", 1000, false, 1);
        a.downstream_re_ask = None;
        let b = ev("B", 1000, true, 2);
        let pairs = pairs_from_events(&[a, b]);
        assert!(pairs.is_empty());
    }

    #[test]
    fn pairs_from_events_buckets_by_context_size() {
        let a = ev("A", 100, false, 1); // bucket 0
        let b = ev("B", 100, true, 2); // bucket 0 → A beats B
        let c = ev("A", 9000, false, 3); // bucket 2
        let d = ev("C", 9000, true, 4); // bucket 2 → A beats C
        let pairs = pairs_from_events(&[a, b, c, d]);
        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().any(|p| p.winner == "A" && p.loser == "B"));
        assert!(pairs.iter().any(|p| p.winner == "A" && p.loser == "C"));
    }

    #[test]
    fn pairs_skip_same_strategy_pairs() {
        let a = ev("A", 100, false, 1);
        let b = ev("A", 100, true, 2);
        let pairs = pairs_from_events(&[a, b]);
        assert!(pairs.is_empty());
    }

    #[test]
    fn pairs_skip_when_outcomes_match() {
        let a = ev("A", 100, false, 1);
        let b = ev("B", 100, false, 2);
        let pairs = pairs_from_events(&[a, b]);
        assert!(pairs.is_empty());
    }

    #[test]
    fn fit_recovers_clear_preference() {
        // 50 pairs all saying A beats B → s_A should be much higher.
        let pairs: Vec<Pair> = (0..50)
            .map(|_| Pair {
                winner: "A".into(),
                loser: "B".into(),
                weight: 1.0,
            })
            .collect();
        let m = fit(&pairs);
        let sa = m.scores["A"];
        let sb = m.scores["B"];
        assert!(sa > sb, "expected A>B, got A={sa} B={sb}");
        assert!(sa - sb > 1.0, "preference should be strong: {sa}-{sb}");
    }

    #[test]
    fn fit_handles_three_way_ranking() {
        // Synthetic ranking: A > B > C (A beats both, B beats C only).
        let mut pairs = Vec::new();
        for _ in 0..20 {
            pairs.push(Pair {
                winner: "A".into(),
                loser: "B".into(),
                weight: 1.0,
            });
            pairs.push(Pair {
                winner: "A".into(),
                loser: "C".into(),
                weight: 1.0,
            });
            pairs.push(Pair {
                winner: "B".into(),
                loser: "C".into(),
                weight: 1.0,
            });
        }
        let m = fit(&pairs);
        let sa = m.scores["A"];
        let sb = m.scores["B"];
        let sc = m.scores["C"];
        assert!(sa > sb, "A should rank above B");
        assert!(sb > sc, "B should rank above C");
    }

    #[test]
    fn pick_returns_highest_scoring_candidate() {
        let mut m = BradleyTerryModel::default();
        m.scores.insert("A".into(), 1.5);
        m.scores.insert("B".into(), 0.0);
        m.scores.insert("C".into(), 2.7);
        assert_eq!(m.pick(&["A", "B"]), Some("A"));
        assert_eq!(m.pick(&["B", "C"]), Some("C"));
        assert_eq!(m.pick(&[]), None);
    }

    #[test]
    fn pick_falls_back_to_mean_for_unknown_strategy() {
        let mut m = BradleyTerryModel::default();
        m.scores.insert("A".into(), 0.0);
        m.scores.insert("B".into(), 2.0);
        // C unknown → gets mean (1.0) → loses to B (2.0), beats A (0.0).
        assert_eq!(m.pick(&["A", "C"]), Some("C"));
        assert_eq!(m.pick(&["B", "C"]), Some("B"));
    }

    #[test]
    fn fit_anchors_first_strategy_at_zero() {
        let pairs = vec![Pair {
            winner: "A".into(),
            loser: "B".into(),
            weight: 1.0,
        }];
        let m = fit(&pairs);
        // Strategies sorted alphabetically → A is index 0 → anchored at 0.
        assert!((m.scores["A"]).abs() < 1e-6);
    }

    #[test]
    fn fit_single_strategy_returns_uniform() {
        let pairs = vec![Pair {
            winner: "A".into(),
            loser: "A".into(),
            weight: 1.0,
        }];
        let m = fit(&pairs);
        assert_eq!(m.scores.len(), 1);
        assert_eq!(m.scores["A"], 0.0);
    }

    #[test]
    fn end_to_end_from_events_recovers_preference() {
        // Realistic flow: a stream of events where Summarize is good
        // and SlidingWindow is bad under high pressure.
        let mut events = Vec::new();
        for i in 0..30 {
            events.push(ev("Summarize", 12_000, false, i)); // bucket 2
        }
        for i in 0..30 {
            events.push(ev("SlidingWindow", 12_000, true, 30 + i));
        }
        let pairs = pairs_from_events(&events);
        assert!(
            pairs.len() > 100,
            "expected many pairs, got {}",
            pairs.len()
        );
        let m = fit(&pairs);
        let s_sum = m.scores["Summarize"];
        let s_win = m.scores["SlidingWindow"];
        assert!(
            s_sum > s_win,
            "Summarize ({s_sum}) should outrank SlidingWindow ({s_win})"
        );
    }
}
