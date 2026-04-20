//! Bundled fixture tasks: 10 deterministic baseline tasks that exercise
//! the harness without touching the network. They are intentionally
//! simple — each is a single-turn echo against [`MockLlmAdapter`] —
//! because the bundled suite is a *regression baseline* (does the
//! harness still wire token accounting, judging, and JSON output?),
//! not a model-quality benchmark. Real SWE-Bench-Lite tasks plug in
//! through the same [`EvalTask`] trait.

use crate::{mock_adapter, EvalTask};
use anyhow::Result;
use async_trait::async_trait;
use caduceus_core::SessionState;
use caduceus_providers::LlmAdapter;
use std::sync::Arc;

/// Single-turn task: prompt → expected substring in agent output.
pub struct EchoTask {
    name: String,
    prompt: String,
    scripted: String,
    /// When `expect_pass` is true, judge returns Ok(true); when false,
    /// judge returns Ok(false). Used to exercise the failure path
    /// without requiring the model to misbehave.
    expect_pass: bool,
}

impl EchoTask {
    pub fn new(name: &str, prompt: &str, scripted: &str, expect_pass: bool) -> Self {
        Self {
            name: name.to_string(),
            prompt: prompt.to_string(),
            scripted: scripted.to_string(),
            expect_pass,
        }
    }
}

#[async_trait]
impl EvalTask for EchoTask {
    fn name(&self) -> &str {
        &self.name
    }
    fn prompt(&self) -> String {
        self.prompt.clone()
    }
    fn provider(&self) -> Arc<dyn LlmAdapter> {
        // 16 output tokens is arbitrary but non-zero so the report can
        // assert positive token accounting on the happy path.
        mock_adapter(&self.scripted, 16)
    }
    fn judge(&self, _output: &str, _state: &SessionState) -> Result<bool> {
        Ok(self.expect_pass)
    }
}

/// The 10-task baseline. Names are stable; reorderings are fine but
/// renames will break diffing across commits — keep them.
pub fn bundled_tasks() -> Vec<Box<dyn EvalTask>> {
    vec![
        Box::new(EchoTask::new("baseline-01-greet", "say hello", "Hello!", true)),
        Box::new(EchoTask::new(
            "baseline-02-arithmetic",
            "what is 2+2?",
            "4",
            true,
        )),
        Box::new(EchoTask::new(
            "baseline-03-rust-idiom",
            "give an idiomatic rust hello world",
            "fn main() { println!(\"hello\"); }",
            true,
        )),
        Box::new(EchoTask::new(
            "baseline-04-summarise",
            "summarise: rust is safe and fast",
            "Rust is memory-safe and performant.",
            true,
        )),
        Box::new(EchoTask::new(
            "baseline-05-translate",
            "translate to french: good morning",
            "Bonjour",
            true,
        )),
        Box::new(EchoTask::new(
            "baseline-06-explain",
            "explain ownership in one sentence",
            "Each value has a single owner; dropping the owner frees the value.",
            true,
        )),
        Box::new(EchoTask::new(
            "baseline-07-list",
            "list three http verbs",
            "GET, POST, PUT",
            true,
        )),
        Box::new(EchoTask::new(
            "baseline-08-bool",
            "is the sky blue? yes or no",
            "yes",
            true,
        )),
        Box::new(EchoTask::new(
            "baseline-09-empty-friendly",
            "respond with an empty acknowledgement",
            "",
            true,
        )),
        Box::new(EchoTask::new(
            "baseline-10-long",
            "give a paragraph about concurrency",
            &"Concurrency is the composition of independent processes. ".repeat(5),
            true,
        )),
    ]
}
