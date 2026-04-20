//! P13.3 (G‑R3.3) — LLM‑backed expander / scorer for the branching
//! planner, plus an async beam‑search driver.
//!
//! The base [`crate::branching_planner`] uses a SYNC trait pair
//! (`BranchExpander` / `BranchScorer`) so the search loop is
//! deterministic and embeddable in non‑async contexts. LLM calls are
//! inherently async, so rather than fighting `block_in_place` from
//! inside the sync loop we provide:
//!
//! * [`AsyncBranchExpander`] / [`AsyncBranchScorer`] — async twins.
//! * [`LlmExpander`] / [`LlmScorer`] — concrete `String` impls that
//!   wrap an [`caduceus_providers::LlmAdapter`].
//! * [`search_async`] — a beam‑search driver that mirrors the sync
//!   search algorithm but awaits the LLM calls.
//!
//! The high‑level entry point is [`crate::AgentHarness::plan_with_llm_tot`]
//! which wires the harness's adapter into the LLM expander/scorer with
//! sensible defaults so Plan‑mode callers can opt in by simply
//! constructing the harness with [`crate::AgentHarness::with_tot_config`].
//!
//! Citation: Yao et al., "Tree of Thoughts" (NeurIPS 2023).

use crate::branching_planner::{PlanResult, PlannerConfig, ThoughtNode};
use anyhow::Result;
use async_trait::async_trait;
use caduceus_providers::{ChatRequest, LlmAdapter, Message};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

#[async_trait]
pub trait AsyncBranchExpander<T>: Send + Sync {
    async fn expand(
        &self,
        node: &ThoughtNode<T>,
        branching_factor: usize,
    ) -> Result<Vec<(T, bool)>>;
}

#[async_trait]
pub trait AsyncBranchScorer<T>: Send + Sync {
    async fn score(&self, node: &ThoughtNode<T>) -> Result<f32>;
}

/// LLM‑backed expander producing `String` thoughts. Each call asks
/// the model for K candidate continuations of the current planning
/// state, one per line. A line ending in `DONE.` marks the candidate
/// as a terminal (complete‑plan) node.
///
/// The expander is intentionally text‑shaped so a Plan‑mode model
/// can produce free‑form steps. Callers wanting structured outputs
/// can wire their own `AsyncBranchExpander` impl over a JSON schema.
pub struct LlmExpander {
    pub adapter: Arc<dyn LlmAdapter>,
    pub model: String,
    /// Optional task header prepended to the expansion prompt — gives
    /// the model the original user goal so each expansion step stays
    /// on‑topic. Empty string disables.
    pub task_context: String,
}

impl LlmExpander {
    pub fn new(adapter: Arc<dyn LlmAdapter>, model: impl Into<String>) -> Self {
        Self {
            adapter,
            model: model.into(),
            task_context: String::new(),
        }
    }

    pub fn with_task_context(mut self, ctx: impl Into<String>) -> Self {
        self.task_context = ctx.into();
        self
    }
}

#[async_trait]
impl AsyncBranchExpander<String> for LlmExpander {
    async fn expand(
        &self,
        node: &ThoughtNode<String>,
        branching_factor: usize,
    ) -> Result<Vec<(String, bool)>> {
        let prompt = format!(
            "{header}Current planning state:\n{state}\n\n\
             Propose exactly {k} distinct next steps that advance the plan. \
             Output one step per line, no bullet markers, no numbering. \
             End a line with the literal token 'DONE.' if that step would \
             represent a COMPLETE plan (no further steps needed).",
            header = if self.task_context.is_empty() {
                String::new()
            } else {
                format!("Goal: {}\n\n", self.task_context)
            },
            state = node.thought,
            k = branching_factor,
        );

        let req = ChatRequest {
            model: caduceus_core::ModelId::new(&self.model),
            messages: vec![Message::user(prompt)],
            system: Some(
                "You are a planning module. Be concise and concrete. \
                 Each step you produce should be actionable in one tool call."
                    .into(),
            ),
            tools: vec![],
            max_tokens: 512,
            temperature: Some(0.7),
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            logprobs: None,
        };
        let resp = self.adapter.chat(req).await?;
        Ok(parse_expansion(&resp.content, branching_factor))
    }
}

fn parse_expansion(text: &str, k: usize) -> Vec<(String, bool)> {
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(k)
        .map(|l| {
            let (body, terminal) = if let Some(stripped) = l.strip_suffix("DONE.") {
                (stripped.trim().to_string(), true)
            } else {
                (l.to_string(), false)
            };
            (body, terminal)
        })
        .collect()
}

/// LLM‑backed scorer — asks the model to rate the candidate plan
/// state on `[0.0, 1.0]`. Non‑numeric responses fall back to `0.0`
/// rather than failing the whole search.
pub struct LlmScorer {
    pub adapter: Arc<dyn LlmAdapter>,
    pub model: String,
    pub task_context: String,
}

impl LlmScorer {
    pub fn new(adapter: Arc<dyn LlmAdapter>, model: impl Into<String>) -> Self {
        Self {
            adapter,
            model: model.into(),
            task_context: String::new(),
        }
    }

    pub fn with_task_context(mut self, ctx: impl Into<String>) -> Self {
        self.task_context = ctx.into();
        self
    }
}

#[async_trait]
impl AsyncBranchScorer<String> for LlmScorer {
    async fn score(&self, node: &ThoughtNode<String>) -> Result<f32> {
        let prompt = format!(
            "{header}Rate the following planning step on a scale of 0.0 (useless) \
             to 1.0 (perfect). Output ONLY the number, nothing else.\n\nStep:\n{thought}",
            header = if self.task_context.is_empty() {
                String::new()
            } else {
                format!("Goal: {}\n\n", self.task_context)
            },
            thought = node.thought,
        );
        let req = ChatRequest {
            model: caduceus_core::ModelId::new(&self.model),
            messages: vec![Message::user(prompt)],
            system: Some(
                "You are a plan critic. Output a single decimal number in [0.0, 1.0].".into(),
            ),
            tools: vec![],
            max_tokens: 16,
            temperature: Some(0.0),
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            logprobs: None,
        };
        let resp = self.adapter.chat(req).await?;
        Ok(parse_score(&resp.content))
    }
}

fn parse_score(text: &str) -> f32 {
    let t = text.trim();
    let cleaned: String = t
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    cleaned.parse::<f32>().unwrap_or(0.0).clamp(0.0, 1.0)
}

// ── Async beam search driver ──────────────────────────────────────────

struct Ranked<T> {
    score: f32,
    depth: usize,
    node: ThoughtNode<T>,
}

impl<T> PartialEq for Ranked<T> {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score && self.depth == other.depth
    }
}
impl<T> Eq for Ranked<T> {}
impl<T> Ord for Ranked<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| other.depth.cmp(&self.depth))
    }
}
impl<T> PartialOrd for Ranked<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Async twin of [`crate::branching_planner::TreeOfThoughts::search`].
/// Same beam‑search algorithm, but awaits expander+scorer calls.
pub async fn search_async<T, E, S>(
    cfg: PlannerConfig,
    root: T,
    expander: &E,
    scorer: &S,
) -> Result<PlanResult<T>>
where
    T: Clone + Send + Sync,
    E: AsyncBranchExpander<T> + ?Sized,
    S: AsyncBranchScorer<T> + ?Sized,
{
    let mut nodes_expanded = 0_usize;
    let root_node = ThoughtNode::new(0, root, 0.0, false);
    let root_score = scorer.score(&root_node).await?;
    let mut frontier: Vec<ThoughtNode<T>> = vec![ThoughtNode {
        score: root_score,
        ..root_node
    }];

    let mut best: Option<ThoughtNode<T>> = None;
    let mut depth_reached = 0;

    for d in 0..cfg.max_depth {
        depth_reached = d;
        let mut next_heap: BinaryHeap<Ranked<T>> = BinaryHeap::new();
        for parent in &frontier {
            if parent.terminal {
                if best
                    .as_ref()
                    .map(|b| parent.score > b.score)
                    .unwrap_or(true)
                {
                    best = Some(parent.clone());
                }
                continue;
            }
            let kids = expander.expand(parent, cfg.branching_factor).await?;
            nodes_expanded += 1;
            for (thought, terminal) in kids {
                let mut child = ThoughtNode::new(parent.depth + 1, thought, 0.0, terminal);
                child.score = scorer.score(&child).await?;
                next_heap.push(Ranked {
                    score: child.score,
                    depth: child.depth,
                    node: child,
                });
            }
        }
        if next_heap.is_empty() {
            break;
        }
        let mut new_frontier = Vec::with_capacity(cfg.beam_width);
        for _ in 0..cfg.beam_width {
            if let Some(r) = next_heap.pop() {
                new_frontier.push(r.node);
            } else {
                break;
            }
        }
        for child in &new_frontier {
            if child.terminal
                && best
                    .as_ref()
                    .map(|b| child.score > b.score)
                    .unwrap_or(true)
            {
                best = Some(child.clone());
            }
        }
        frontier = new_frontier;
        if frontier.iter().all(|n| n.terminal) {
            break;
        }
    }

    if best.is_none() {
        if let Some(top) = frontier.iter().max_by(|a, b| {
            a.score.partial_cmp(&b.score).unwrap_or(Ordering::Equal)
        }) {
            best = Some(top.clone());
        }
    }

    Ok(PlanResult {
        best,
        frontier_at_termination: frontier,
        depth_reached,
        nodes_expanded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_expansion_takes_k_lines_and_marks_terminal() {
        let txt = "step one\nstep two DONE.\nstep three\nstep four";
        let v = parse_expansion(txt, 3);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], ("step one".into(), false));
        assert_eq!(v[1], ("step two".into(), true));
        assert_eq!(v[2], ("step three".into(), false));
    }

    #[test]
    fn parse_expansion_skips_blank_lines() {
        let v = parse_expansion("\n\nfoo\n\nbar\n", 5);
        assert_eq!(v, vec![("foo".into(), false), ("bar".into(), false)]);
    }

    #[test]
    fn parse_score_handles_clean_decimal() {
        assert_eq!(parse_score("0.7"), 0.7);
        assert_eq!(parse_score("  0.42 "), 0.42);
    }

    #[test]
    fn parse_score_clamps_out_of_range() {
        assert_eq!(parse_score("1.5"), 1.0);
        assert_eq!(parse_score("-0.3"), 0.0);
    }

    #[test]
    fn parse_score_falls_back_to_zero_on_garbage() {
        assert_eq!(parse_score("not a number"), 0.0);
        assert_eq!(parse_score(""), 0.0);
    }

    // ── End‑to‑end search with mock adapter ──────────────────────────

    fn final_resp(text: &str) -> caduceus_providers::ChatResponse {
        caduceus_providers::ChatResponse {
            content: text.into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: caduceus_core::StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
        }
    }

    #[tokio::test]
    async fn p13_3_search_async_terminates_on_done_marker() {
        // First call: scorer for root → 0.1
        // Then alternating expand/score pairs until terminal hit.
        let responses = vec![
            final_resp("0.1"),               // root score
            final_resp("a\nb DONE.\nc"),     // expand root → 3 children
            final_resp("0.3"),               // score 'a'
            final_resp("0.9"),               // score 'b' (terminal)
            final_resp("0.2"),               // score 'c'
        ];
        let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(responses));
        let exp = LlmExpander::new(adapter.clone(), "mock");
        let scr = LlmScorer::new(adapter.clone(), "mock");
        let cfg = PlannerConfig {
            branching_factor: 3,
            beam_width: 1,
            max_depth: 3,
        };
        let result = search_async(cfg, "root".to_string(), &exp, &scr).await.unwrap();
        let best = result.best.expect("must find a terminal");
        assert_eq!(best.thought, "b");
        assert!(best.terminal);
        assert!(best.score > 0.5);
    }
}
