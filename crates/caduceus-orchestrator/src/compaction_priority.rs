//! P13.5 (G‑R8.1, G‑R8.2) — priority‑scoring compaction.
//!
//! Existing compaction strategies (sliding window, summarisation,
//! tool collapse) operate on positional or syntactic heuristics.
//! `PriorityScoreStrategy` instead scores each non‑system message
//! group on four signals — **pinned**, **recency**, **relevance**,
//! **importance** — and greedily packs the highest‑scoring groups
//! (by score‑per‑token) into a token budget. Anything that doesn't
//! fit is evicted.
//!
//! The model is inspired by attention‑sink / pinned‑prefix work
//! (Xiao et al., "StreamingLLM" 2024) and the priority‑aware
//! retrieval evaluations in Liu et al., "Lost in the Middle" (TACL
//! 2024) — both argue that strict recency wastes context on
//! repetitive or redundant turns when older‑but‑relevant material
//! would survive a downstream needle‑in‑haystack probe better.
//!
//! ## Scoring
//!
//! For each non‑system group `g` at distance `d` from the end of
//! the conversation:
//!
//! * `pinned(g)` ∈ {0, 1} — explicit pin via [`UnitScorer::pinned`].
//! * `recency(g) = 1 / (1 + d)` — exponential‑ish decay so the last
//!   turn dominates.
//! * `relevance(g)` — fraction of `query_terms` that appear (case‑
//!   insensitive substring) in any message in `g`. Normalised to
//!   [0.0, 1.0].
//! * `importance(g)` ∈ {0, 1} — atomic groups (paired tool calls +
//!   their results) and the very first user turn count as important.
//!
//! `score(g) = w_p · pinned + w_r · recency + w_q · relevance + w_i · importance`
//!
//! Pinned groups receive `f32::INFINITY` so they always survive,
//! independent of the weighted score.
//!
//! ## Packing
//!
//! 1. Score every non‑system group.
//! 2. Sort descending by `score / token_count` (efficiency).
//! 3. Greedy‑pack into `budget`; ties broken by recency (newer
//!    wins) so a deterministic ordering is produced.
//! 4. Re‑emit groups in their ORIGINAL conversation order — the
//!    compactor never re‑orders messages, only filters.
//!
//! System groups are always kept and do NOT count against `budget`.

use crate::compaction::{CompactionResult, CompactionStrategy, MessageGroup, MessageGroupKind};

/// Tunable weights and metadata for [`PriorityScoreStrategy`].
#[derive(Debug, Clone)]
pub struct UnitScorer {
    pub w_pinned: f32,
    pub w_recency: f32,
    pub w_relevance: f32,
    pub w_importance: f32,
    /// Indices (in non‑system order, 0‑based, end‑relative) that
    /// MUST survive regardless of score. Use this to pin a recent
    /// goal restatement or a costly tool result.
    pub pinned: Vec<usize>,
    /// Lower‑case query terms used for relevance scoring. Empty
    /// disables the relevance signal.
    pub query_terms: Vec<String>,
}

impl Default for UnitScorer {
    fn default() -> Self {
        Self {
            w_pinned: 1.0,
            w_recency: 1.0,
            w_relevance: 2.0,
            w_importance: 0.5,
            pinned: Vec::new(),
            query_terms: Vec::new(),
        }
    }
}

impl UnitScorer {
    pub fn with_query_terms<I, S>(mut self, terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.query_terms = terms
            .into_iter()
            .map(|s| s.into().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        self
    }

    pub fn with_pins(mut self, pins: impl IntoIterator<Item = usize>) -> Self {
        self.pinned = pins.into_iter().collect();
        self
    }

    /// Compute raw score for a group. `idx_from_end` is 0 for the
    /// most recent non‑system group. `is_first_user` flags the very
    /// first user turn in the conversation.
    pub fn score_group(&self, g: &MessageGroup, idx_from_end: usize, is_first_user: bool) -> f32 {
        let recency = 1.0 / (1.0 + idx_from_end as f32);
        let relevance = if self.query_terms.is_empty() {
            0.0
        } else {
            let body: String = g
                .messages
                .iter()
                .map(|m| m.content.to_lowercase())
                .collect::<Vec<_>>()
                .join(" ");
            let hits = self
                .query_terms
                .iter()
                .filter(|t| body.contains(t.as_str()))
                .count();
            hits as f32 / self.query_terms.len() as f32
        };
        let importance = if g.is_atomic() || is_first_user {
            1.0
        } else {
            0.0
        };
        self.w_recency * recency + self.w_relevance * relevance + self.w_importance * importance
    }
}

/// Compaction strategy that greedily packs the highest‑scoring
/// non‑system groups into a token `budget`.
#[derive(Debug, Clone)]
pub struct PriorityScoreStrategy {
    pub budget: usize,
    pub scorer: UnitScorer,
}

impl PriorityScoreStrategy {
    pub fn new(budget: usize, scorer: UnitScorer) -> Self {
        Self { budget, scorer }
    }
}

impl CompactionStrategy for PriorityScoreStrategy {
    fn name(&self) -> &str {
        "priority-score"
    }

    fn compact(&self, groups: &mut Vec<MessageGroup>) -> CompactionResult {
        // 1. Identify non-system groups; compute end-relative index
        //    and "is_first_user" flags.
        let total = groups.len();
        let mut non_sys_indices: Vec<usize> =
            (0..total).filter(|&i| !groups[i].is_system()).collect();
        if non_sys_indices.is_empty() {
            return CompactionResult {
                removed_tokens: 0,
                groups_affected: 0,
                evicted: Vec::new(),
            };
        }

        // First user turn: earliest non-system group whose kind is User.
        let first_user_idx = non_sys_indices
            .iter()
            .find(|&&i| matches!(groups[i].kind, MessageGroupKind::User))
            .copied();

        let last_non_sys_pos = non_sys_indices.len() - 1;
        let pin_set: std::collections::HashSet<usize> =
            self.scorer.pinned.iter().copied().collect();

        // 2. Score each.
        struct Scored {
            orig_idx: usize,
            tokens: usize,
            score: f32,
            efficiency: f32,
            pinned: bool,
            recency_pos: usize,
        }
        let mut scored: Vec<Scored> = Vec::with_capacity(non_sys_indices.len());
        for (pos, &i) in non_sys_indices.iter().enumerate() {
            let idx_from_end = last_non_sys_pos - pos;
            let is_first_user = first_user_idx == Some(i);
            let raw = self
                .scorer
                .score_group(&groups[i], idx_from_end, is_first_user);
            let pinned = pin_set.contains(&idx_from_end);
            let final_score = if pinned { f32::INFINITY } else { raw };
            let tokens = groups[i].token_count.max(1);
            let efficiency = if pinned {
                f32::INFINITY
            } else {
                final_score / tokens as f32
            };
            scored.push(Scored {
                orig_idx: i,
                tokens,
                score: final_score,
                efficiency,
                pinned,
                recency_pos: pos,
            });
        }

        // 3. Sort by efficiency desc, ties by recency (newer = larger pos).
        scored.sort_by(|a, b| {
            b.efficiency
                .partial_cmp(&a.efficiency)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.recency_pos.cmp(&a.recency_pos))
        });

        // 4. Greedy pack.
        let mut keep = std::collections::HashSet::<usize>::new();
        let mut used_tokens: usize = 0;
        for s in &scored {
            // Always keep pinned even if it busts the budget — the
            // user explicitly asked for it; better to overshoot than
            // silently drop.
            if s.pinned {
                keep.insert(s.orig_idx);
                used_tokens = used_tokens.saturating_add(s.tokens);
                continue;
            }
            if used_tokens.saturating_add(s.tokens) <= self.budget {
                keep.insert(s.orig_idx);
                used_tokens += s.tokens;
            }
        }

        // 5. Evict in original order; preserve survivors' order.
        let mut evicted_refs: Vec<caduceus_core::EvictedGroupRef> = Vec::new();
        let mut removed_tokens = 0usize;
        let mut groups_affected = 0usize;
        let mut new_groups: Vec<MessageGroup> = Vec::with_capacity(total);
        for (i, g) in groups.drain(..).enumerate() {
            if g.is_system() || keep.contains(&i) {
                new_groups.push(g);
            } else {
                evicted_refs.push(caduceus_core::EvictedGroupRef {
                    kind: format!("{:?}", g.kind).to_lowercase(),
                    message_count: g.messages.len() as u32,
                    token_count: g.token_count as u32,
                    reason: "priority-evict".to_string(),
                });
                removed_tokens += g.token_count;
                groups_affected += 1;
                drop(g);
            }
        }
        *groups = new_groups;

        CompactionResult {
            removed_tokens,
            groups_affected,
            evicted: evicted_refs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::CompactMessage;

    fn user_group(content: &str) -> MessageGroup {
        let mut g = MessageGroup::new(MessageGroupKind::User);
        g.add_message(CompactMessage::new("user", content));
        g
    }

    fn assistant_group(content: &str) -> MessageGroup {
        let mut g = MessageGroup::new(MessageGroupKind::AssistantText);
        g.add_message(CompactMessage::new("assistant", content));
        g
    }

    fn system_group(content: &str) -> MessageGroup {
        let mut g = MessageGroup::new(MessageGroupKind::System);
        g.add_message(CompactMessage::new("system", content));
        g
    }

    #[test]
    fn p13_5_keeps_recent_when_under_budget() {
        let mut groups = vec![
            system_group("sys"),
            user_group("hello"),
            assistant_group("hi"),
        ];
        let strat = PriorityScoreStrategy::new(10_000, UnitScorer::default());
        let r = strat.compact(&mut groups);
        assert_eq!(r.groups_affected, 0);
        assert_eq!(groups.len(), 3);
    }

    #[test]
    fn p13_5_evicts_lowest_score_when_over_budget() {
        // Three non-system groups; budget admits two of them.
        let mut groups = vec![
            system_group("sys"),
            user_group("first turn very ancient nothing relevant"),
            assistant_group("middle reply also unrelated"),
            user_group("latest turn please answer"),
        ];
        // Set tokens manually so packing math is predictable.
        for g in &mut groups {
            g.token_count = 100;
        }
        let strat = PriorityScoreStrategy::new(200, UnitScorer::default());
        let r = strat.compact(&mut groups);
        assert_eq!(r.groups_affected, 1, "exactly one non-sys group must drop");
        // System group must remain.
        assert!(groups.iter().any(|g| g.is_system()));
        // The most-recent group must survive (recency dominates without
        // relevance terms).
        assert!(groups
            .iter()
            .any(|g| g.messages.iter().any(|m| m.content.contains("latest turn"))));
    }

    #[test]
    fn p13_5_pinned_group_survives_even_when_busting_budget() {
        let mut groups = vec![
            system_group("sys"),
            user_group("ancient pinned content"),
            assistant_group("trash trash trash"),
            user_group("recent"),
        ];
        for g in &mut groups {
            g.token_count = 100;
        }
        // Pin the OLDEST non-system group (idx_from_end=2).
        let scorer = UnitScorer::default().with_pins([2usize]);
        let strat = PriorityScoreStrategy::new(50, scorer); // budget too small
        let _ = strat.compact(&mut groups);
        assert!(
            groups.iter().any(|g| g
                .messages
                .iter()
                .any(|m| m.content.contains("ancient pinned"))),
            "pinned group must survive even when over budget"
        );
    }

    #[test]
    fn p13_5_relevance_boosts_query_matching_groups() {
        let mut groups = vec![
            system_group("sys"),
            user_group("authentication module needs review"),
            assistant_group("sky is blue"),
            user_group("today's weather is sunny"),
        ];
        for g in &mut groups {
            g.token_count = 100;
        }
        // Budget allows 2; query terms favour the auth message even
        // though it's the OLDEST.
        let scorer = UnitScorer::default().with_query_terms(["authentication"]);
        let strat = PriorityScoreStrategy::new(200, scorer);
        let _ = strat.compact(&mut groups);
        assert!(
            groups.iter().any(|g| g
                .messages
                .iter()
                .any(|m| m.content.contains("authentication"))),
            "relevance must promote query-matching group above pure-recency"
        );
    }

    #[test]
    fn p13_5_preserves_original_order_of_survivors() {
        let mut groups = vec![
            system_group("sys"),
            user_group("A"),
            assistant_group("B"),
            user_group("C"),
            assistant_group("D"),
        ];
        for g in &mut groups {
            g.token_count = 100;
        }
        let strat = PriorityScoreStrategy::new(300, UnitScorer::default());
        let _ = strat.compact(&mut groups);
        let survivors_text: Vec<String> = groups
            .iter()
            .filter(|g| !g.is_system())
            .map(|g| g.messages[0].content.clone())
            .collect();
        // Whatever survived must be in original alphabetical order;
        // strict-recency would keep B,C,D (in that order).
        let mut sorted = survivors_text.clone();
        sorted.sort();
        assert_eq!(survivors_text, sorted, "survivors must keep original order");
    }
}
