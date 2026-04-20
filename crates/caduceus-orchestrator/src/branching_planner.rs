//! P12.3 — Tree-of-Thoughts (ToT) style branching planner.
//!
//! Implements the scaffolding from Yao et al. 2023, "Tree of Thoughts:
//! Deliberate Problem Solving with Large Language Models"
//! (arXiv:2305.10601). Replaces the chain-of-thought "single linear
//! plan" mode with an explicit search tree where:
//!
//! 1. The planner expands a frontier into K candidate continuations
//!    per node ("thoughts").
//! 2. Each candidate is scored by a [`BranchScorer`] (the LLM, a
//!    verifier, or a heuristic).
//! 3. The top-N continuations are kept; the rest are pruned (beam
//!    search, beam width = N).
//! 4. The loop terminates when a node is marked terminal OR the
//!    depth budget is hit.
//!
//! The data structures are LLM-agnostic — callers wire whatever
//! expander/scorer they want. This module is the search engine, not
//! the prompt template.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// One node in the search tree. `depth = 0` is the root prompt;
/// children represent one elaboration step further.
#[derive(Debug, Clone)]
pub struct ThoughtNode<T> {
    pub depth: usize,
    pub thought: T,
    pub score: f32,
    /// Whether the expander declared this node a leaf — typically
    /// "the plan is complete" or "we've hit a dead end".
    pub terminal: bool,
}

impl<T> ThoughtNode<T> {
    pub fn new(depth: usize, thought: T, score: f32, terminal: bool) -> Self {
        Self {
            depth,
            thought,
            score,
            terminal,
        }
    }
}

/// Expand one frontier node into up to K candidate continuations.
/// Pure trait, no I/O — async wrappers are caller's responsibility
/// (the engine is sync to keep the search loop deterministic).
pub trait BranchExpander<T> {
    /// Generate up to `branching_factor` continuations of `node`.
    /// Each returned tuple is `(thought, terminal_flag)`. The engine
    /// will run the scorer over them.
    fn expand(&self, node: &ThoughtNode<T>, branching_factor: usize) -> Vec<(T, bool)>;
}

/// Score a candidate thought. Higher is better. Scores are NOT
/// required to be in any particular range; the planner only uses
/// them for relative ordering.
pub trait BranchScorer<T> {
    fn score(&self, node: &ThoughtNode<T>) -> f32;
}

/// Configuration for one search.
#[derive(Debug, Clone, Copy)]
pub struct PlannerConfig {
    /// Children per parent at every expansion. Yao et al. recommend
    /// 3–5; keep this small to bound cost.
    pub branching_factor: usize,
    /// Beam width: how many surviving frontier nodes per depth.
    /// 1 collapses to greedy; > 1 explores in parallel.
    pub beam_width: usize,
    /// Maximum tree depth. Hard stop to bound runtime even when no
    /// node is terminal.
    pub max_depth: usize,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            branching_factor: 3,
            beam_width: 2,
            max_depth: 6,
        }
    }
}

/// Result of one search. `best` is the highest-scoring terminal node
/// encountered; `frontier_at_termination` is the live beam at the
/// last expansion (useful for diagnostics / partial-resume).
#[derive(Debug, Clone)]
pub struct PlanResult<T> {
    pub best: Option<ThoughtNode<T>>,
    pub frontier_at_termination: Vec<ThoughtNode<T>>,
    pub depth_reached: usize,
    pub nodes_expanded: usize,
}

// Heap helpers — `BinaryHeap` is a max-heap by `Ord`. We wrap nodes
// so the heap orders by score descending and breaks ties by depth
// ascending (prefer shallower wins).
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
        // NaN-safe: NaN compares equal to itself here, sorts to end.
        match self.score.partial_cmp(&other.score) {
            Some(Ordering::Equal) | None => other.depth.cmp(&self.depth),
            Some(o) => o,
        }
    }
}
impl<T> PartialOrd for Ranked<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The search engine. Stateless aside from the `cfg` it carries.
pub struct TreeOfThoughts<T, E, S>
where
    E: BranchExpander<T>,
    S: BranchScorer<T>,
{
    pub cfg: PlannerConfig,
    pub expander: E,
    pub scorer: S,
    _ph: std::marker::PhantomData<T>,
}

impl<T: Clone, E: BranchExpander<T>, S: BranchScorer<T>> TreeOfThoughts<T, E, S> {
    pub fn new(cfg: PlannerConfig, expander: E, scorer: S) -> Self {
        Self {
            cfg,
            expander,
            scorer,
            _ph: std::marker::PhantomData,
        }
    }

    /// Run beam search starting from one root thought. The root is
    /// scored as-is (depth 0). The loop expands the beam, scores all
    /// candidates, keeps the top `beam_width`, and stops when a
    /// terminal node enters the beam OR depth budget exhausts.
    pub fn search(&self, root: T) -> PlanResult<T> {
        let mut nodes_expanded = 0_usize;
        let root_node = ThoughtNode::new(0, root, 0.0, false);
        let root_score = self.scorer.score(&root_node);
        let mut frontier: Vec<ThoughtNode<T>> = vec![ThoughtNode {
            score: root_score,
            ..root_node
        }];

        let mut best: Option<ThoughtNode<T>> = None;
        let mut depth_reached = 0;

        for d in 0..self.cfg.max_depth {
            depth_reached = d;
            // Expand every live (non-terminal) node in the beam.
            let mut next_heap: BinaryHeap<Ranked<T>> = BinaryHeap::new();
            for parent in &frontier {
                if parent.terminal {
                    // Terminal nodes are best-candidates; skip expansion.
                    if best
                        .as_ref()
                        .map(|b| parent.score > b.score)
                        .unwrap_or(true)
                    {
                        best = Some(parent.clone());
                    }
                    continue;
                }
                let kids = self.expander.expand(parent, self.cfg.branching_factor);
                nodes_expanded += 1;
                for (thought, terminal) in kids {
                    let mut child =
                        ThoughtNode::new(parent.depth + 1, thought, 0.0, terminal);
                    child.score = self.scorer.score(&child);
                    next_heap.push(Ranked {
                        score: child.score,
                        depth: child.depth,
                        node: child,
                    });
                }
            }
            if next_heap.is_empty() {
                // Nothing to expand — frontier was all-terminal or
                // expander returned empty. Done.
                break;
            }
            // Take top-`beam_width` for the next frontier.
            let mut new_frontier = Vec::with_capacity(self.cfg.beam_width);
            for _ in 0..self.cfg.beam_width {
                if let Some(r) = next_heap.pop() {
                    new_frontier.push(r.node);
                } else {
                    break;
                }
            }
            // Promote any terminal child seen this round into `best`.
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
            // Early-exit if every survivor is terminal — no useful
            // expansion left to do.
            if frontier.iter().all(|n| n.terminal) {
                break;
            }
        }

        // If we never saw a terminal node, the best non-terminal in
        // the final frontier wins (caller can decide to keep / drop).
        if best.is_none() {
            best = frontier
                .iter()
                .max_by(|a, b| {
                    a.score
                        .partial_cmp(&b.score)
                        .unwrap_or(Ordering::Equal)
                })
                .cloned();
        }

        PlanResult {
            best,
            frontier_at_termination: frontier,
            depth_reached,
            nodes_expanded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A toy expander that walks a numeric path: each child appends
    /// "+i" for i in 1..=branching_factor. Terminal at depth 3.
    struct PathExpander;
    impl BranchExpander<String> for PathExpander {
        fn expand(&self, node: &ThoughtNode<String>, k: usize) -> Vec<(String, bool)> {
            (1..=k)
                .map(|i| {
                    let next = format!("{}+{i}", node.thought);
                    let terminal = node.depth + 1 >= 3;
                    (next, terminal)
                })
                .collect()
        }
    }

    /// Score = numeric value of the last "+N" suffix. Bigger = better.
    struct SuffixScorer;
    impl BranchScorer<String> for SuffixScorer {
        fn score(&self, node: &ThoughtNode<String>) -> f32 {
            node.thought
                .rsplit('+')
                .next()
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(0.0)
        }
    }

    #[test]
    fn p12_3_search_terminates_and_returns_best() {
        let cfg = PlannerConfig {
            branching_factor: 3,
            beam_width: 2,
            max_depth: 5,
        };
        let tot = TreeOfThoughts::new(cfg, PathExpander, SuffixScorer);
        let result = tot.search("root".into());
        assert!(result.best.is_some());
        let best = result.best.unwrap();
        // Best should end in "+3" (highest score per level).
        assert!(best.thought.ends_with("+3"), "got {:?}", best.thought);
        assert!(best.terminal, "best should be terminal at depth 3");
    }

    #[test]
    fn p12_3_beam_width_one_is_greedy() {
        let cfg = PlannerConfig {
            branching_factor: 3,
            beam_width: 1,
            max_depth: 5,
        };
        let tot = TreeOfThoughts::new(cfg, PathExpander, SuffixScorer);
        let result = tot.search("r".into());
        // Greedy with branching=3 expands exactly 1 node per depth
        // until depth 3 (terminal). So 3 expansions total.
        assert_eq!(result.nodes_expanded, 3);
    }

    #[test]
    fn p12_3_max_depth_caps_search() {
        let cfg = PlannerConfig {
            branching_factor: 2,
            beam_width: 1,
            max_depth: 1,
        };
        let tot = TreeOfThoughts::new(cfg, PathExpander, SuffixScorer);
        let result = tot.search("r".into());
        // Hard-cap at depth 1: exactly one expansion of the root.
        assert_eq!(result.nodes_expanded, 1);
        assert_eq!(result.depth_reached, 0);
    }

    /// Expander that always returns no children — verifies engine
    /// terminates cleanly when nothing can be expanded.
    struct EmptyExpander;
    impl BranchExpander<String> for EmptyExpander {
        fn expand(&self, _: &ThoughtNode<String>, _: usize) -> Vec<(String, bool)> {
            vec![]
        }
    }

    #[test]
    fn p12_3_empty_expansion_returns_root_as_best() {
        let cfg = PlannerConfig::default();
        let tot = TreeOfThoughts::new(cfg, EmptyExpander, SuffixScorer);
        let result = tot.search("root".into());
        assert!(result.best.is_some());
        assert_eq!(result.best.unwrap().thought, "root");
        // Engine attempted exactly one expansion of the root, which
        // returned no children — counted as one attempt.
        assert_eq!(result.nodes_expanded, 1);
    }

    /// Expander that immediately marks the first child terminal.
    /// Verifies terminal-child promotion into `best`.
    struct EarlyTerminalExpander;
    impl BranchExpander<String> for EarlyTerminalExpander {
        fn expand(&self, node: &ThoughtNode<String>, k: usize) -> Vec<(String, bool)> {
            (1..=k)
                .map(|i| (format!("{}+{i}", node.thought), i == 1))
                .collect()
        }
    }

    #[test]
    fn p12_3_terminal_child_seen_round_one_is_promoted() {
        let cfg = PlannerConfig {
            branching_factor: 3,
            beam_width: 3,
            max_depth: 5,
        };
        let tot = TreeOfThoughts::new(cfg, EarlyTerminalExpander, SuffixScorer);
        let result = tot.search("r".into());
        // Best non-terminal child has +3 (score 3); terminal child
        // has +1 (score 1). Best (per score) wins → "+3" non-terminal
        // until it becomes terminal at deeper depth (it never does
        // here since EarlyTerminalExpander only marks i==1 terminal).
        // The PROMOTED `best` must have terminal=true (the +1 child)
        // since that's the only node that ever got the terminal flag
        // and we promote on terminal.
        let best = result.best.expect("must have a best");
        assert!(best.terminal, "best should be terminal");
        assert!(best.thought.ends_with("+1"));
    }
}
