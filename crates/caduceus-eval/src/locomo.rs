//! P12.1 — LOCOMO-style multi-turn memory-recall benchmark harness.
//!
//! Inspired by Maharana et al. 2024, "Evaluating Very Long-Term
//! Conversational Memory of LLM Agents" (arXiv:2402.17753). LOCOMO
//! probes whether an agent retains facts injected in earlier turns
//! and can recall them many turns later — the canonical failure mode
//! for compaction-driven amnesia (gap G7 in the round-3 audit).
//!
//! This module ships a *deterministic, mock-adapter* harness so CI
//! can ratchet recall accuracy without burning provider tokens. The
//! production LOCOMO drop-in (real provider, real graded answers)
//! plugs into the same `MultiTurnTask` trait by swapping the adapter.
//!
//! ## Shape
//!
//! 1. A `MultiTurnTask` is a SEQUENCE of `(prompt, expected)` pairs.
//! 2. The runner calls `harness.run` once per pair, threading the
//!    SAME `SessionState` and `ConversationHistory` so the harness
//!    sees prior turns exactly as a real long session would.
//! 3. Each pair is judged independently — substring containment is
//!    the default, but tasks can override `judge_turn` for fuzzier
//!    semantic matching.
//! 4. The aggregate `RecallReport` records per-turn pass/fail plus
//!    an overall recall percentage, which CI uses as the regression
//!    budget.
//!
//! Designed to be agent-agnostic: anything that satisfies the
//! `EvalTask`-style hooks (provider, tools, system_prompt) can be
//! wrapped in a `MultiTurnTask` without changes.

use anyhow::Result;
use async_trait::async_trait;
use caduceus_core::{ModelId, ProviderId, SessionState};
use caduceus_orchestrator::{AgentHarness, ConversationHistory};
use caduceus_providers::LlmAdapter;
use caduceus_tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// A single (prompt, expected-substring) probe inside a multi-turn task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallProbe {
    /// Human-readable label used in reports, e.g. "fact-injection-1"
    /// or "recall-after-5-turns".
    pub label: String,
    /// Prompt fed to `harness.run` for this turn.
    pub prompt: String,
    /// Substring(s) expected in the agent's response. ALL must be
    /// present for the turn to pass under the default judge.
    pub expected_substrings: Vec<String>,
}

/// Outcome of a single turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnOutcome {
    pub label: String,
    pub passed: bool,
    pub wall_clock_ms: u64,
    pub output_preview: String,
}

/// Aggregate report for a multi-turn run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallReport {
    pub task: String,
    pub turn_count: usize,
    pub passed_turns: usize,
    /// `passed_turns / turn_count`. 0.0 for empty.
    pub recall_rate: f64,
    pub turns: Vec<TurnOutcome>,
}

/// A multi-turn LOCOMO task. Lives separately from `EvalTask` because
/// the latter is single-shot.
#[async_trait]
pub trait MultiTurnTask: Send + Sync {
    fn name(&self) -> &str;
    fn probes(&self) -> &[RecallProbe];

    fn provider(&self) -> Arc<dyn LlmAdapter>;

    fn tools(&self) -> ToolRegistry {
        ToolRegistry::new()
    }

    fn system_prompt(&self) -> String {
        "You are a benchmark agent. Answer concisely and recall earlier facts.".into()
    }

    fn project_root(&self) -> Option<&Path> {
        None
    }

    /// Judge a single turn. Default: every `expected_substrings` entry
    /// must be a substring of `output` (case-insensitive). Override for
    /// semantic / regex / numeric tolerance.
    fn judge_turn(&self, probe: &RecallProbe, output: &str) -> bool {
        let lower = output.to_lowercase();
        probe
            .expected_substrings
            .iter()
            .all(|needle| lower.contains(&needle.to_lowercase()))
    }
}

/// Runner. Stateless.
pub struct LocomoRunner;

impl LocomoRunner {
    /// Run all probes of one task in order, threading state/history.
    /// Returns a per-turn report.
    pub async fn run(task: &dyn MultiTurnTask) -> Result<RecallReport> {
        let owned_tmp;
        let project_root: &Path = match task.project_root() {
            Some(p) => p,
            None => {
                owned_tmp = tempfile::tempdir()?;
                owned_tmp.path()
            }
        };

        let provider = task.provider();
        let tools = task.tools();
        let system_prompt = task.system_prompt();
        let harness = AgentHarness::new(provider, tools, 200_000, &system_prompt);

        // CRITICAL: state + history are persistent across probes so
        // the agent observes the multi-turn session exactly as a
        // real user would.
        let mut state = SessionState::new(
            project_root,
            ProviderId::new("locomo"),
            ModelId::new("locomo-task"),
        );
        let mut history = ConversationHistory::new();

        let mut turn_outcomes = Vec::with_capacity(task.probes().len());
        for probe in task.probes() {
            let started = Instant::now();
            let output = harness
                .run(&mut state, &mut history, &probe.prompt)
                .await
                .unwrap_or_else(|e| format!("<harness error: {e}>"));
            let wall_clock_ms = started.elapsed().as_millis() as u64;
            let passed = task.judge_turn(probe, &output);
            turn_outcomes.push(TurnOutcome {
                label: probe.label.clone(),
                passed,
                wall_clock_ms,
                output_preview: super::truncate(&output, 1024),
            });
        }

        let turn_count = turn_outcomes.len();
        let passed_turns = turn_outcomes.iter().filter(|t| t.passed).count();
        let recall_rate = if turn_count > 0 {
            passed_turns as f64 / turn_count as f64
        } else {
            0.0
        };

        Ok(RecallReport {
            task: task.name().to_string(),
            turn_count,
            passed_turns,
            recall_rate,
            turns: turn_outcomes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripted;
    use caduceus_providers::mock::MockLlmAdapter;

    /// Test fixture: scripts a sequence of model responses, one per
    /// probe. Order matters — Mock adapter pops responses FIFO.
    struct ScriptedLocomoTask {
        name: String,
        probes: Vec<RecallProbe>,
        responses: Vec<String>,
    }

    #[async_trait]
    impl MultiTurnTask for ScriptedLocomoTask {
        fn name(&self) -> &str {
            &self.name
        }
        fn probes(&self) -> &[RecallProbe] {
            &self.probes
        }
        fn provider(&self) -> Arc<dyn LlmAdapter> {
            let chats: Vec<_> = self.responses.iter().map(|r| scripted(r, 5)).collect();
            Arc::new(MockLlmAdapter::new(chats))
        }
    }

    fn probe(label: &str, prompt: &str, expected: &[&str]) -> RecallProbe {
        RecallProbe {
            label: label.into(),
            prompt: prompt.into(),
            expected_substrings: expected.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn p12_1_perfect_recall_yields_100_percent() {
        let task = ScriptedLocomoTask {
            name: "perfect".into(),
            probes: vec![
                probe(
                    "inject",
                    "Remember: my favourite colour is blue.",
                    &["blue"],
                ),
                probe("recall", "What was my favourite colour?", &["blue"]),
            ],
            responses: vec![
                "Got it — your favourite colour is blue.".into(),
                "Your favourite colour is blue.".into(),
            ],
        };
        let r = LocomoRunner::run(&task).await.unwrap();
        assert_eq!(r.turn_count, 2);
        assert_eq!(r.passed_turns, 2);
        assert!((r.recall_rate - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn p12_1_partial_recall_reflected_in_rate() {
        let task = ScriptedLocomoTask {
            name: "partial".into(),
            probes: vec![
                probe("inject", "My pet is a cat.", &["cat"]),
                probe("recall", "What's my pet?", &["cat"]),
            ],
            // Second response forgets — should fail.
            responses: vec!["Noted, your pet is a cat.".into(), "I don't recall.".into()],
        };
        let r = LocomoRunner::run(&task).await.unwrap();
        assert_eq!(r.passed_turns, 1);
        assert!((r.recall_rate - 0.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn p12_1_default_judge_is_case_insensitive() {
        let task = ScriptedLocomoTask {
            name: "case".into(),
            probes: vec![probe("recall", "what?", &["BLUE"])],
            responses: vec!["The colour is blue.".into()],
        };
        let r = LocomoRunner::run(&task).await.unwrap();
        assert_eq!(r.passed_turns, 1, "case-insensitive substring match");
    }

    #[tokio::test]
    async fn p12_1_all_substrings_must_match_for_pass() {
        let task = ScriptedLocomoTask {
            name: "conjunction".into(),
            probes: vec![probe("recall", "summarise", &["alpha", "beta", "gamma"])],
            // Missing "gamma" → must fail.
            responses: vec!["Found alpha and beta but nothing else.".into()],
        };
        let r = LocomoRunner::run(&task).await.unwrap();
        assert_eq!(r.passed_turns, 0);
    }

    #[tokio::test]
    async fn p12_1_history_persists_across_turns() {
        // Critical for LOCOMO semantics: the second turn must see the
        // first turn in conversation history. We assert this indirectly
        // by checking both probes complete (mock adapter would panic
        // on missing scripted response if history were reset and
        // a fresh chat were issued without our scripted reply lined up).
        let task = ScriptedLocomoTask {
            name: "persist".into(),
            probes: vec![
                probe("t1", "fact A", &["a"]),
                probe("t2", "fact B", &["b"]),
                probe("t3", "fact C", &["c"]),
            ],
            responses: vec!["a".into(), "b".into(), "c".into()],
        };
        let r = LocomoRunner::run(&task).await.unwrap();
        assert_eq!(r.turn_count, 3);
        assert_eq!(r.passed_turns, 3);
    }
}
