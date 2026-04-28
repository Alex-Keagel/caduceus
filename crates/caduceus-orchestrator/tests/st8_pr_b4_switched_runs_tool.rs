//! ST8 PR-B4 — `SwitchOutcome::Switched` must actually re-execute the
//! originally-denied tool call under a per-call widened envelope.
//!
//! This is the missing-half of PR-B2: PR-B2 wired the
//! `ProfileSwitchPending` / `ProfileSwitchResolved` event surface and made
//! `submit_profile_switch(Switched)` deliver, but the harness arm for
//! `Switched` was a stub that synth-denied the call. PR-B4 replaces the
//! stub with the validate→widen→recheck→dispatch sequence cloned from the
//! grant flow's `Granted` arm.
//!
//! Semantics: **B-per-call** — the harness envelope is *not* mutated;
//! widening lasts for exactly one tool dispatch and the next tool call
//! sees the original envelope. This mirrors the grant flow exactly.

use caduceus_core::{AgentEvent, StopReason};
use caduceus_orchestrator::{AgentEventEmitter, AgentHarness, ConversationHistory, SwitchOutcome};
use caduceus_permissions::PermissionEnvelope;
use caduceus_providers::mock::MockLlmAdapter;
use caduceus_providers::ChatResponse;
use caduceus_tools::{BashTool, ToolRegistry};
use std::sync::Arc;
use std::time::Duration;

fn make_session() -> caduceus_core::SessionState {
    caduceus_core::SessionState::new(
        ".",
        caduceus_core::ProviderId::new("mock"),
        caduceus_core::ModelId::new("mock-model"),
    )
}

fn final_response() -> ChatResponse {
    ChatResponse {
        content: "ok".into(),
        input_tokens: 1,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        stop_reason: StopReason::EndTurn,
        tool_calls: vec![],
        logprobs: None,
        thinking: String::new(),
    }
}

fn tool_then_end(
    tool_use_id: &str,
    tool_name: &str,
    input: serde_json::Value,
) -> Vec<ChatResponse> {
    let r1 = ChatResponse {
        content: String::new(),
        input_tokens: 1,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        stop_reason: StopReason::ToolUse,
        tool_calls: vec![caduceus_core::ToolUse {
            id: tool_use_id.into(),
            name: tool_name.into(),
            input,
        }],
        logprobs: None,
        thinking: String::new(),
    };
    vec![r1, final_response()]
}

/// Drives `harness.run` while a side task watches the broadcast emitter
/// for `ProfileSwitchPending` and submits the requested outcome.
async fn run_with_switch_responder(
    envelope: PermissionEnvelope,
    permission_mode: &str,
    tool_use_id: &str,
    tool_name: &str,
    tool_input: serde_json::Value,
    respond_with: SwitchOutcome,
) -> (Vec<AgentEvent>, String) {
    let provider = Arc::new(MockLlmAdapter::new(tool_then_end(
        tool_use_id,
        tool_name,
        tool_input,
    )));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(BashTool::new(dir.path())));

    let (emitter, mut rx) = AgentEventEmitter::channel(64);
    let mut event_sub = emitter.subscribe();

    let harness = Arc::new(
        AgentHarness::new(provider, registry, 4096, "test")
            .with_emitter(emitter)
            .with_permission_envelope(envelope)
            .with_resume_on_grant(true)
            .with_grant_timeout(Duration::from_secs(2))
            .with_permission_mode(permission_mode),
    );

    let h_sub = harness.clone();
    let switch_task = tokio::spawn(async move {
        loop {
            match event_sub.recv().await {
                Ok(AgentEvent::ProfileSwitchPending {
                    tool_use_id: id, ..
                }) => {
                    let _ = h_sub.submit_profile_switch(&id, respond_with.clone()).await;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let mut state = make_session();
    let mut history = ConversationHistory::new();
    let _ = harness.run(&mut state, &mut history, "go").await;

    // Drain mpsc.
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    // Read assistant's final reply (the second message) for the synth-deny check.
    // The transcript is pinned via state but we only need the events for asserts.
    switch_task.abort();

    // Pull last `ToolResult` content out of the event stream for assertions.
    let last_result = events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::ToolResultEnd { id, content, .. } if id.0.as_str() == tool_use_id => {
                Some(content.clone())
            }
            _ => None,
        })
        .unwrap_or_default();

    (events, last_result)
}

fn find_switch_outcome(events: &[AgentEvent]) -> Option<String> {
    events.iter().find_map(|e| match e {
        AgentEvent::ProfileSwitchResolved { outcome, .. } => Some(outcome.clone()),
        _ => None,
    })
}

// ── tests ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn pr_b4_switched_runs_originally_denied_bash() {
    // Plan envelope denies bash. The classifier maps Plan+Exec → Act, so
    // the harness emits ProfileSwitchPending. The responder submits
    // SwitchOutcome::Switched. Expected: outcome="switched" AND the bash
    // tool actually executes ("ls" on a tempdir produces an empty stdout
    // but a successful, non-error ToolResult).
    let (events, tool_content) = run_with_switch_responder(
        PermissionEnvelope::plan_preset(),
        "plan",
        "tc-b4-switched",
        "bash",
        serde_json::json!({"command": "echo hello-from-switched"}),
        SwitchOutcome::Switched,
    )
    .await;

    let outcome =
        find_switch_outcome(&events).expect("ProfileSwitchResolved must fire on Switched submit");
    assert_eq!(
        outcome, "switched",
        "expected outcome=switched; events: {events:#?}"
    );

    // The tool actually ran: bash echo result must contain our marker.
    // (If the Switched arm were still the synth-deny stub, the content
    // would be the deny string — this is the load-bearing assertion.)
    assert!(
        tool_content.contains("hello-from-switched"),
        "Switched arm did not actually execute the tool. content={tool_content:?}; events: {events:#?}"
    );
}

#[tokio::test]
async fn pr_b4_denied_outcome_still_synth_denies() {
    // Regression: SwitchOutcome::Denied still produces a synth-deny
    // tool result and outcome="denied" — unchanged from PR-B2.
    let (events, tool_content) = run_with_switch_responder(
        PermissionEnvelope::plan_preset(),
        "plan",
        "tc-b4-denied",
        "bash",
        serde_json::json!({"command": "echo should-not-run"}),
        SwitchOutcome::Denied,
    )
    .await;

    let outcome =
        find_switch_outcome(&events).expect("ProfileSwitchResolved must fire on Denied submit");
    assert_eq!(outcome, "denied", "events: {events:#?}");

    // The tool must NOT have executed; ToolResult content is the synth-deny.
    assert!(
        tool_content.contains("PERMISSION_OUT_OF_SCOPE"),
        "Denied path should produce synth-deny; got content={tool_content:?}"
    );
}
