//! Learned compaction-strategy selector (gap G5 step 3 / P5.3).
//!
//! Replaces the hard-coded "run all strategies in fixed order"
//! pipeline with a Bradley–Terry-driven priority order. The
//! `LearnedSelector` takes a trained `BradleyTerryModel` (from
//! `compaction_scorer::fit`) plus a slice of candidate strategies
//! and returns them re-ordered from "most likely to succeed" to
//! "least". The pipeline can then try them in that order and stop
//! when context drops below budget.
//!
//! Important design choices:
//!
//!  * **Heuristic fallback.** When the model has fewer than
//!    `min_pairs_observed` pairs supporting it, we fall back to the
//:    caller-supplied heuristic order. This prevents a freshly-
//!    initialised model with one or two noisy pairs from making
//!    catastrophic choices.
//!
//!  * **Tie-breaking.** Strategies with identical model scores
//!    (e.g. both unknown → both fall back to mean) preserve their
//!    original relative order from `candidates`. This makes the
//!    selector deterministic and easy to test.
//!
//!  * **Confidence reporting.** `select_with_confidence` returns
//!    the score margin between top-1 and top-2. Callers (UI) can
//!    show "uncertain" badges when the margin is small.
//!
//! Note: this module is intentionally orthogonal to `compaction.rs`
//! — it only knows strategy names. Wiring into the actual pipeline
//! is deferred to whichever caller wants the learned policy
//! (matches the P3-P5 pattern of data-modules + caller wiring).

use crate::compaction_scorer::BradleyTerryModel;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectionMode {
    /// Use the model unconditionally.
    Learned,
    /// Use the heuristic order as-is (model ignored).
    Heuristic,
    /// Auto: use learned if model has >= `min_pairs_observed`
    /// distinct strategies trained, else heuristic.
    Auto,
}

#[derive(Debug, Clone)]
pub struct LearnedSelector {
    model: BradleyTerryModel,
    pub mode: SelectionMode,
    /// Below this many trained strategies, Auto mode falls back to
    /// heuristic. Default: 2 (need at least one pairwise comparison).
    pub min_strategies_observed: usize,
}

impl LearnedSelector {
    pub fn new(model: BradleyTerryModel) -> Self {
        Self {
            model,
            mode: SelectionMode::Auto,
            min_strategies_observed: 2,
        }
    }

    pub fn with_mode(mut self, mode: SelectionMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn model(&self) -> &BradleyTerryModel {
        &self.model
    }

    /// Returns true iff the model has enough data to be trusted in
    /// Auto mode.
    pub fn has_enough_data(&self) -> bool {
        self.model.scores.len() >= self.min_strategies_observed
    }

    /// Re-order `candidates` from best to worst per the model.
    /// Stable: equal scores preserve input order.
    pub fn rank<'a>(&self, candidates: &[&'a str]) -> Vec<&'a str> {
        if candidates.is_empty() {
            return Vec::new();
        }
        let use_model = match self.mode {
            SelectionMode::Heuristic => false,
            SelectionMode::Learned => true,
            SelectionMode::Auto => self.has_enough_data(),
        };
        if !use_model {
            return candidates.to_vec();
        }
        let fallback = if self.model.scores.is_empty() {
            0.0
        } else {
            self.model.scores.values().sum::<f64>() / self.model.scores.len() as f64
        };
        let mut indexed: Vec<(usize, &str, f64)> = candidates
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                let s = self.model.scores.get(c).copied().unwrap_or(fallback);
                (i, c, s)
            })
            .collect();
        // Sort by score descending, then by original index ascending.
        indexed.sort_by(|a, b| {
            b.2.partial_cmp(&a.2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        indexed.into_iter().map(|(_, c, _)| c).collect()
    }

    /// Pick the top strategy, or `None` if no candidates.
    pub fn select<'a>(&self, candidates: &[&'a str]) -> Option<&'a str> {
        self.rank(candidates).into_iter().next()
    }

    /// Pick the top strategy and report the score margin over the
    /// runner-up. `margin` is 0 when only one candidate is given.
    /// Useful for UI confidence badges.
    pub fn select_with_confidence<'a>(&self, candidates: &[&'a str]) -> Option<(&'a str, f64)> {
        let ranked = self.rank(candidates);
        match ranked.as_slice() {
            [] => None,
            [only] => Some((*only, 0.0)),
            [first, second, ..] => {
                let fallback = if self.model.scores.is_empty() {
                    0.0
                } else {
                    self.model.scores.values().sum::<f64>() / self.model.scores.len() as f64
                };
                let s1 = self.model.scores.get(*first).copied().unwrap_or(fallback);
                let s2 = self.model.scores.get(*second).copied().unwrap_or(fallback);
                Some((*first, s1 - s2))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction_scorer::{fit, Pair};

    fn model_with(prefs: &[(&str, &str)]) -> BradleyTerryModel {
        let pairs: Vec<Pair> = prefs
            .iter()
            .flat_map(|(w, l)| {
                std::iter::repeat_with(move || Pair {
                    winner: (*w).into(),
                    loser: (*l).into(),
                    weight: 1.0,
                })
                .take(20)
            })
            .collect();
        fit(&pairs)
    }

    #[test]
    fn empty_candidates_returns_empty() {
        let s = LearnedSelector::new(BradleyTerryModel::default());
        assert!(s.rank(&[]).is_empty());
        assert!(s.select(&[]).is_none());
        assert!(s.select_with_confidence(&[]).is_none());
    }

    #[test]
    fn auto_mode_falls_back_to_heuristic_with_empty_model() {
        let s = LearnedSelector::new(BradleyTerryModel::default());
        assert_eq!(s.rank(&["A", "B", "C"]), vec!["A", "B", "C"]);
    }

    #[test]
    fn auto_mode_uses_model_when_enough_data() {
        let m = model_with(&[("B", "A"), ("B", "C"), ("A", "C")]);
        let s = LearnedSelector::new(m);
        assert!(s.has_enough_data());
        let ranked = s.rank(&["A", "B", "C"]);
        assert_eq!(ranked[0], "B", "B should rank first, got {:?}", ranked);
    }

    #[test]
    fn heuristic_mode_ignores_model() {
        let m = model_with(&[("B", "A")]);
        let s = LearnedSelector::new(m).with_mode(SelectionMode::Heuristic);
        assert_eq!(s.rank(&["A", "B"]), vec!["A", "B"]);
    }

    #[test]
    fn learned_mode_uses_model_even_with_thin_data() {
        let m = model_with(&[("B", "A")]);
        let s = LearnedSelector::new(m).with_mode(SelectionMode::Learned);
        let ranked = s.rank(&["A", "B"]);
        assert_eq!(ranked[0], "B");
    }

    #[test]
    fn select_with_confidence_reports_margin() {
        let m = model_with(&[("B", "A"), ("B", "A"), ("B", "A")]);
        let s = LearnedSelector::new(m);
        let (winner, margin) = s.select_with_confidence(&["A", "B"]).unwrap();
        assert_eq!(winner, "B");
        assert!(margin > 0.0, "margin should be positive: {margin}");
    }

    #[test]
    fn select_with_confidence_zero_margin_for_single_candidate() {
        let m = model_with(&[("B", "A")]);
        let s = LearnedSelector::new(m);
        let (winner, margin) = s.select_with_confidence(&["A"]).unwrap();
        assert_eq!(winner, "A");
        assert_eq!(margin, 0.0);
    }

    #[test]
    fn unknown_strategies_get_mean_score() {
        let m = model_with(&[("A", "B")]); // A=0 (anchor), B=negative
        let s = LearnedSelector::new(m).with_mode(SelectionMode::Learned);
        // C is unknown → gets mean. mean = (0 + B_score)/2, which
        // is negative. So C ranks between A and B.
        let ranked = s.rank(&["B", "C", "A"]);
        assert_eq!(ranked[0], "A");
        assert_eq!(ranked[1], "C");
        assert_eq!(ranked[2], "B");
    }

    #[test]
    fn equal_scores_preserve_input_order() {
        let s =
            LearnedSelector::new(BradleyTerryModel::default()).with_mode(SelectionMode::Learned);
        // All unknown → all get fallback 0 → input order preserved.
        assert_eq!(s.rank(&["X", "Y", "Z"]), vec!["X", "Y", "Z"]);
        assert_eq!(s.rank(&["Z", "Y", "X"]), vec!["Z", "Y", "X"]);
    }

    #[test]
    fn min_strategies_observed_threshold_respected() {
        let mut m = BradleyTerryModel::default();
        m.scores.insert("A".into(), 1.0);
        let mut s = LearnedSelector::new(m);
        s.min_strategies_observed = 5;
        assert!(!s.has_enough_data());
        // Auto falls back to heuristic.
        assert_eq!(s.rank(&["A", "B"]), vec!["A", "B"]);
    }
}
