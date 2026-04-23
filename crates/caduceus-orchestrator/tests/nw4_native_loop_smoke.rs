//! NW-4 — native-loop dogfood smoke (backend half).
//!
//! The "native loop" is simply `AgentHarness::run`: when Zed's
//! `caduceus_native_loop` setting is ON, the agent crate dispatches
//! turns directly into this loop instead of the legacy mode-switch UI
//! path. This file exercises the backend half of NW-4 non-interactively
//! via `MockLlmAdapter` + `ToolRegistry`.
//!
//! GUI-only steps still require manual verification in a Zed dev build:
//!   • S3 destructive-tool approval prompt (Allow / Deny paths)
//!   • S6 mid-stream cancel → abort without zombie spawns
//!
//! Run: `cargo test -p caduceus-orchestrator --test nw4_native_loop_smoke`

use async_trait::async_trait;
use caduceus_core::{ModelId, ProviderId, SessionState, StopReason, ToolResult, ToolSpec, ToolUse};
use caduceus_orchestrator::{AgentHarness, ConversationHistory};
use caduceus_providers::mock::MockLlmAdapter;
use caduceus_providers::ChatResponse;
use caduceus_tools::{Tool, ToolRegistry};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// ── helpers ─────────────────────────────────────────────────────────────────

fn session() -> SessionState {
    SessionState::new(
        ".",
        ProviderId::new("mock"),
        ModelId::new("mock-model"),
    )
}

fn text_response(text: &str) -> ChatResponse {
    ChatResponse {
        content: text.to_string(),
        input_tokens: 10,
        output_tokens: 20,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        stop_reason: StopReason::EndTurn,
        tool_calls: vec![],
        logprobs: None,
        thinking: String::new(),
    }
}

fn tool_call_response(id: &str, name: &str, input: Value) -> ChatResponse {
    ChatResponse {
        content: String::new(),
        input_tokens: 10,
        output_tokens: 20,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        stop_reason: StopReason::ToolUse,
        tool_calls: vec![ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        }],
        logprobs: None,
        thinking: String::new(),
    }
}

/// A dead-simple read-only tool that records its invocations so the
/// smoke can assert on multi-round flow without touching the filesystem.
struct EchoTool {
    name: String,
    call_count: Arc<AtomicUsize>,
}

impl EchoTool {
    fn new(name: &str) -> (Arc<Self>, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        let tool = Arc::new(Self {
            name: name.to_string(),
            call_count: counter.clone(),
        });
        (tool, counter)
    }
}

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "Echoes its input back (NW-4 smoke tool).".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            }),
            required_capability: None,
        }
    }

    async fn call(&self, input: Value) -> caduceus_core::Result<ToolResult> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let text = input
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Ok(ToolResult {
            content: format!("echo: {text}"),
            is_error: false,
            tool_use_id: None,
        })
    }
}

// ── S1 — plain prompt, single turn ──────────────────────────────────────────

#[tokio::test]
async fn nw4_s1_plain_prompt_completes_single_turn() {
    let provider = Arc::new(MockLlmAdapter::new(vec![text_response("README summary here.")]));
    let harness = AgentHarness::new(provider, ToolRegistry::new(), 8192, "system");
    let mut state = session();
    let mut history = ConversationHistory::new();

    let out = harness
        .run(&mut state, &mut history, "summarize README")
        .await
        .expect("plain prompt should succeed");

    assert_eq!(out, "README summary here.");
    assert_eq!(state.token_budget.used_input, 10);
    assert_eq!(state.token_budget.used_output, 20);
}

// ── S2 — read-only tool executes and feeds back into the model ──────────────

#[tokio::test]
async fn nw4_s2_read_only_tool_round_trip() {
    let provider = Arc::new(MockLlmAdapter::new(vec![
        tool_call_response("call_1", "echo", json!({ "text": "crate root" })),
        text_response("Saw: echo: crate root"),
    ]));
    let (tool, counter) = EchoTool::new("echo");
    let mut registry = ToolRegistry::new();
    registry.register(tool);

    let harness = AgentHarness::new(provider, registry, 8192, "system");
    let mut state = session();
    let mut history = ConversationHistory::new();

    let out = harness
        .run(&mut state, &mut history, "show crate root")
        .await
        .expect("tool round-trip should succeed");

    assert_eq!(counter.load(Ordering::SeqCst), 1, "tool should fire exactly once");
    assert!(out.contains("echo: crate root"), "final text should include tool output: {out}");
}

// ── S4 — "web fetch" shape: same single-tool path, no mode-switch loop ──────

#[tokio::test]
async fn nw4_s4_fetch_shaped_tool_no_mode_switch() {
    let provider = Arc::new(MockLlmAdapter::new(vec![
        tool_call_response("call_1", "web_fetch", json!({ "text": "https://example.com" })),
        text_response("Fetched: echo: https://example.com"),
    ]));
    let (tool, counter) = EchoTool::new("web_fetch");
    let mut registry = ToolRegistry::new();
    registry.register(tool);

    let harness = AgentHarness::new(provider, registry, 8192, "system");
    let mut state = session();
    let mut history = ConversationHistory::new();

    let out = harness
        .run(&mut state, &mut history, "fetch https://example.com")
        .await
        .expect("fetch-shaped tool should round-trip");

    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert!(out.contains("https://example.com"));
}

// ── S5 — multi-round tool chain: ≥3 tool calls before final text ────────────

#[tokio::test]
async fn nw4_s5_multi_round_tool_chain() {
    let provider = Arc::new(MockLlmAdapter::new(vec![
        tool_call_response("c1", "echo", json!({ "text": "step1" })),
        tool_call_response("c2", "echo", json!({ "text": "step2" })),
        tool_call_response("c3", "echo", json!({ "text": "step3" })),
        text_response("done: step1/step2/step3"),
    ]));
    let (tool, counter) = EchoTool::new("echo");
    let mut registry = ToolRegistry::new();
    registry.register(tool);

    let harness = AgentHarness::new(provider, registry, 8192, "system");
    let mut state = session();
    let mut history = ConversationHistory::new();

    let out = harness
        .run(&mut state, &mut history, "do three things")
        .await
        .expect("3-round chain should succeed");

    assert_eq!(
        counter.load(Ordering::SeqCst),
        3,
        "tool must fire exactly 3 times across the chain"
    );
    assert!(out.contains("done"), "final text should be the post-chain summary: {out}");

    // History must contain exactly one user turn — no duplicated system
    // preamble as we round-tripped through the tool loop.
    let user_turns = history
        .messages()
        .iter()
        .filter(|m| m.role == "user")
        .count();
    assert_eq!(
        user_turns, 1,
        "multi-round loop must not duplicate the user turn in history"
    );
}
