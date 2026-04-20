//! # caduceus-eval
//!
//! Lightweight benchmark harness for measuring `AgentHarness::run` on a
//! deterministic suite of tasks. Acts as the baseline rig for SWE-Bench-Lite
//! parity work and the regression budget for later phases (verification
//! strategies, learned compaction, memory blocks, etc.).
//!
//! ## Design
//!
//! - [`EvalTask`] is a trait, not a struct, so real SWE-Bench tasks (which
//!   require git checkout + pytest execution) and bundled fixture tasks
//!   (which run in-memory against [`MockLlmAdapter`]) implement the same
//!   surface and feed the same runner.
//! - [`EvalRunner`] runs each task end-to-end through `AgentHarness::run`
//!   and records pass/fail, token usage, and wall-clock to an
//!   [`EvalReport`] that serialises to JSON for diffing across commits.
//! - The bundled [`fixtures::bundled_tasks`] returns 10 deterministic
//!   tasks so CI has a baseline number to ratchet without internet
//!   access or model-vendor flakiness.
//!
//! ## SWE-Bench integration
//!
//! Production SWE-Bench-Lite integration is intentionally out of scope for
//! the initial harness and is tracked separately. The `EvalTask` trait is
//! the integration seam: a `SweBenchTask` impl will instantiate a real
//! provider, clone the target repo, run the agent, and shell out to
//! `pytest` in `judge`.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use caduceus_core::{ModelId, ProviderId, SessionState};
use caduceus_orchestrator::{AgentHarness, ConversationHistory};
use caduceus_providers::{mock::MockLlmAdapter, ChatResponse, LlmAdapter};
use caduceus_tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub mod fixtures;
pub mod locomo;
pub mod trajectory;
pub use locomo::{
    LocomoRunner, MultiTurnTask, RecallProbe, RecallReport, TurnOutcome,
};
pub use trajectory::{
    RecordingLlmAdapter, ReplayingLlmAdapter, Trajectory, TrajectoryEntry, TrajectoryRecorder,
    TRAJECTORY_SCHEMA_VERSION,
};

/// A single benchmark task. Implementations build their own provider /
/// tools / acceptance criteria; the runner only orchestrates.
#[async_trait]
pub trait EvalTask: Send + Sync {
    /// Stable identifier — used as the JSON record key. Must be unique
    /// across the suite or the runner returns an error.
    fn name(&self) -> &str;

    /// User input passed to `AgentHarness::run`.
    fn prompt(&self) -> String;

    /// Build the LLM adapter (typically a [`MockLlmAdapter`] with scripted
    /// responses for deterministic CI; real tasks return a live provider).
    fn provider(&self) -> Arc<dyn LlmAdapter>;

    /// Build the tool registry the agent has access to.
    fn tools(&self) -> ToolRegistry {
        ToolRegistry::new()
    }

    /// System prompt; defaults to a minimal directive.
    fn system_prompt(&self) -> String {
        "You are a benchmark agent. Answer concisely.".to_string()
    }

    /// Project root used by the harness. Defaults to a tempdir per task.
    fn project_root(&self) -> Option<&Path> {
        None
    }

    /// Acceptance check. Returning `Ok(true)` marks the task PASSED;
    /// `Ok(false)` marks FAILED with no error; `Err(_)` is a hard error
    /// (infrastructure failure, e.g. judge couldn't read output).
    fn judge(&self, output: &str, state: &SessionState) -> Result<bool>;
}

/// Per-task outcome serialised into the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub task: String,
    pub passed: bool,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub wall_clock_ms: u64,
    /// Populated when `judge` returned `Err` or the harness panicked.
    /// Distinct from `passed=false` (which is a clean failure).
    pub error: Option<String>,
    /// Truncated agent output for debugging. Capped at 4 KiB to keep
    /// reports diffable.
    pub output_preview: String,
}

const PREVIEW_CAP: usize = 4096;

/// Aggregate report for a full eval run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub task_count: usize,
    pub passed: usize,
    pub failed: usize,
    pub errored: usize,
    pub pass_rate: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_wall_clock_ms: u64,
    pub outcomes: Vec<TaskOutcome>,
}

impl EvalReport {
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Append-only JSONL line for time-series tracking. One report per line.
    pub fn to_jsonl_line(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// The runner. Stateless; `run` is the only entry point.
pub struct EvalRunner;

impl EvalRunner {
    /// Run every task sequentially. Sequential by design — the harness
    /// holds tokio::Mutex on the approval channel and many tasks share
    /// process-wide rate limits when wired to real providers.
    pub async fn run(tasks: Vec<Box<dyn EvalTask>>) -> Result<EvalReport> {
        // Reject duplicate task names up front: the report is keyed by
        // `task` and silent overwrites would mask regressions.
        let mut seen = std::collections::HashSet::new();
        for t in &tasks {
            if !seen.insert(t.name().to_string()) {
                return Err(anyhow!("duplicate eval task name: {}", t.name()));
            }
        }

        let mut outcomes = Vec::with_capacity(tasks.len());
        for task in tasks {
            outcomes.push(Self::run_one(task.as_ref()).await);
        }

        let passed = outcomes.iter().filter(|o| o.passed && o.error.is_none()).count();
        let errored = outcomes.iter().filter(|o| o.error.is_some()).count();
        let failed = outcomes.len() - passed - errored;
        let total_input_tokens: u64 =
            outcomes.iter().map(|o| o.input_tokens as u64).sum();
        let total_output_tokens: u64 =
            outcomes.iter().map(|o| o.output_tokens as u64).sum();
        let total_wall_clock_ms: u64 = outcomes.iter().map(|o| o.wall_clock_ms).sum();
        let task_count = outcomes.len();
        let pass_rate = if task_count > 0 {
            passed as f64 / task_count as f64
        } else {
            0.0
        };

        Ok(EvalReport {
            generated_at: chrono::Utc::now(),
            task_count,
            passed,
            failed,
            errored,
            pass_rate,
            total_input_tokens,
            total_output_tokens,
            total_wall_clock_ms,
            outcomes,
        })
    }

    async fn run_one(task: &dyn EvalTask) -> TaskOutcome {
        let name = task.name().to_string();
        // Owned tempdir kept alive for the duration of this call. When
        // the task supplies its own project_root we defer to it; otherwise
        // we mint a per-task scratch dir that is auto-cleaned on Drop.
        let owned_tmp;
        let project_root: &Path = match task.project_root() {
            Some(p) => p,
            None => {
                owned_tmp = match tempfile::tempdir() {
                    Ok(d) => d,
                    Err(e) => return error_outcome(&name, format!("tempdir: {e}")),
                };
                owned_tmp.path()
            }
        };

        let provider = task.provider();
        let tools = task.tools();
        let system_prompt = task.system_prompt();
        let prompt = task.prompt();

        let harness = AgentHarness::new(provider, tools, 200_000, &system_prompt);
        let mut state = SessionState::new(
            project_root,
            ProviderId::new("eval"),
            ModelId::new("eval-task"),
        );
        let mut history = ConversationHistory::new();

        let started = Instant::now();
        let run_result = harness.run(&mut state, &mut history, &prompt).await;
        let wall_clock_ms = started.elapsed().as_millis() as u64;

        let output = match run_result {
            Ok(s) => s,
            Err(e) => {
                return TaskOutcome {
                    task: name,
                    passed: false,
                    input_tokens: state.token_budget.used_input,
                    output_tokens: state.token_budget.used_output,
                    total_tokens: state.token_budget.used_input
                        + state.token_budget.used_output,
                    wall_clock_ms,
                    error: Some(format!("harness: {e}")),
                    output_preview: String::new(),
                };
            }
        };

        let (passed, error) = match task.judge(&output, &state) {
            Ok(p) => (p, None),
            Err(e) => (false, Some(format!("judge: {e}"))),
        };

        TaskOutcome {
            task: name,
            passed,
            input_tokens: state.token_budget.used_input,
            output_tokens: state.token_budget.used_output,
            total_tokens: state.token_budget.used_input + state.token_budget.used_output,
            wall_clock_ms,
            error,
            output_preview: truncate(&output, PREVIEW_CAP),
        }
    }
}

fn error_outcome(name: &str, msg: String) -> TaskOutcome {
    TaskOutcome {
        task: name.to_string(),
        passed: false,
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        wall_clock_ms: 0,
        error: Some(msg),
        output_preview: String::new(),
    }
}

pub(crate) fn truncate(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let mut end = cap;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Helper used by fixtures to build a one-shot scripted ChatResponse.
pub fn scripted(content: &str, output_tokens: u32) -> ChatResponse {
    ChatResponse {
        content: content.to_string(),
        input_tokens: 8,
        output_tokens,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        stop_reason: caduceus_core::StopReason::EndTurn,
        tool_calls: vec![],
        logprobs: None,
    }
}

/// Helper: build a MockLlmAdapter with a single scripted response.
pub fn mock_adapter(content: &str, output_tokens: u32) -> Arc<dyn LlmAdapter> {
    Arc::new(MockLlmAdapter::new(vec![scripted(content, output_tokens)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::EchoTask;

    #[tokio::test]
    async fn runner_executes_single_task_and_records_metrics() {
        let task: Box<dyn EvalTask> =
            Box::new(EchoTask::new("echo-1", "say hello", "hello world", true));
        let report = EvalRunner::run(vec![task]).await.unwrap();
        assert_eq!(report.task_count, 1);
        assert_eq!(report.passed, 1);
        assert_eq!(report.failed, 0);
        assert!(report.outcomes[0].output_tokens > 0);
        assert!(report.outcomes[0].error.is_none());
    }

    #[tokio::test]
    async fn runner_distinguishes_failed_from_errored() {
        let pass: Box<dyn EvalTask> =
            Box::new(EchoTask::new("p", "go", "hi", true));
        let fail: Box<dyn EvalTask> =
            Box::new(EchoTask::new("f", "go", "hi", false)); // judge -> false
        let report = EvalRunner::run(vec![pass, fail]).await.unwrap();
        assert_eq!(report.task_count, 2);
        assert_eq!(report.passed, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.errored, 0);
        assert!((report.pass_rate - 0.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn runner_rejects_duplicate_task_names() {
        let a: Box<dyn EvalTask> = Box::new(EchoTask::new("dup", "x", "x", true));
        let b: Box<dyn EvalTask> = Box::new(EchoTask::new("dup", "x", "x", true));
        let err = EvalRunner::run(vec![a, b]).await.unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[tokio::test]
    async fn bundled_suite_runs_and_serialises() {
        let tasks = fixtures::bundled_tasks();
        assert_eq!(tasks.len(), 10, "baseline suite is exactly 10 tasks");
        let report = EvalRunner::run(tasks).await.unwrap();
        assert_eq!(report.task_count, 10);
        // We expect the baseline to fully pass against deterministic mocks.
        // Failures here mean the harness or fixture mocks regressed.
        assert_eq!(
            report.errored, 0,
            "no infrastructure errors expected; outcomes={:?}",
            report.outcomes
        );
        assert_eq!(report.passed, 10, "all baseline tasks must pass");
        // JSON round-trips.
        let json = report.to_json().unwrap();
        let back: EvalReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.task_count, 10);
        // JSONL is a single line.
        let line = report.to_jsonl_line().unwrap();
        assert!(!line.contains('\n'));
    }

    #[test]
    fn truncate_handles_multibyte_boundary() {
        let s = "héllo wörld";
        let t = truncate(s, 4);
        // No panic; result is valid utf8 with the marker appended.
        assert!(t.ends_with('…'));
    }
}
