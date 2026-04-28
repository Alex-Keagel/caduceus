//! ST8 PR-B2 — denial classifier orchestrator wiring (integration).
//!
//! Drives a full `harness.run()` and inspects the events stream, so we
//! pin the cross-cutting wire-format contract:
//!
//!  - `permission_mode` + `classical_fit` => emit
//!    `ProfileSwitchPending` (and **only** that — not
//!    `GrantPending` / `ScopeExpansionRequested`).
//!  - No `permission_mode` => fall through to grant flow as before.
//!  - `resume_on_grant_enabled == false` => no pending events at all.
//!  - Wait task without a switch decision => emits
//!    `ProfileSwitchResolved { outcome: "timeout" }`.

use caduceus_core::{AgentEvent, StopReason};
use caduceus_orchestrator::{AgentEventEmitter, AgentHarness, ConversationHistory};
use caduceus_permissions::PermissionEnvelope;
use caduceus_providers::mock::MockLlmAdapter;
use caduceus_providers::ChatResponse;
use caduceus_tools::{BashTool, ToolRegistry};
use std::sync::Arc;

// ── helpers ─────────────────────────────────────────────────────────────

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

/// One-tool response then a final assistant turn so `harness.run`
/// terminates after the synth-deny lands.
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

async fn drain(rx: &mut tokio::sync::mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

async fn run_and_collect(
    envelope: PermissionEnvelope,
    permission_mode: Option<&str>,
    resume_on_grant: bool,
    tool_use_id: &str,
    tool_name: &str,
    tool_input: serde_json::Value,
) -> Vec<AgentEvent> {
    let provider = Arc::new(MockLlmAdapter::new(tool_then_end(
        tool_use_id,
        tool_name,
        tool_input,
    )));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(BashTool::new(dir.path())));

    let (emitter, mut rx) = AgentEventEmitter::channel(64);
    let mut h = AgentHarness::new(provider, registry, 4096, "test")
        .with_emitter(emitter)
        .with_permission_envelope(envelope)
        .with_resume_on_grant(resume_on_grant)
        .with_grant_timeout(std::time::Duration::from_millis(80));
    if let Some(m) = permission_mode {
        h = h.with_permission_mode(m);
    }
    let mut state = make_session();
    let mut history = ConversationHistory::new();
    let _ = h.run(&mut state, &mut history, "go").await;
    drain(&mut rx).await
}

fn has_profile_switch_pending(events: &[AgentEvent], expected_target: &str) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::ProfileSwitchPending { target_mode, .. }
                if target_mode == expected_target
        )
    })
}

fn has_grant_pending(events: &[AgentEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, AgentEvent::GrantPending { .. }))
}

fn has_scope_expansion(events: &[AgentEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, AgentEvent::ScopeExpansionRequested { .. }))
}

// ── classifier wiring (full run) ────────────────────────────────────────

#[tokio::test]
async fn plan_mode_exec_deny_emits_profile_switch_not_grant() {
    // Plan's exec list is closed → `bash` → Deny. classical_fit
    // maps Plan + Exec → Act, so the harness emits
    // ProfileSwitchPending and NOT GrantPending /
    // ScopeExpansionRequested (the latter is grant-flow-specific).
    let events = run_and_collect(
        PermissionEnvelope::plan_preset(),
        Some("plan"),
        true,
        "tc-plan-exec",
        "bash",
        serde_json::json!({"command": "ls"}),
    )
    .await;

    assert!(
        has_profile_switch_pending(&events, "act"),
        "expected ProfileSwitchPending{{target_mode:act}}; got {events:#?}"
    );
    assert!(
        !has_grant_pending(&events),
        "must NOT emit GrantPending on a switch-routed denial; got {events:#?}"
    );
    assert!(
        !has_scope_expansion(&events),
        "must NOT emit ScopeExpansionRequested on a switch-routed denial \
         (it is grant-flow-specific and would leak as a stale grant); got {events:#?}"
    );
}

#[tokio::test]
async fn act_mode_write_outside_allow_emits_grant_not_switch() {
    // Act's classical_fit row is empty: even a write denial
    // routes through GrantRequired and the legacy grant path.
    // (Act allows exec by default, so we use write_file to a path
    // outside the write allowlist instead.)
    let env = PermissionEnvelope::act_preset(vec!["src/**".into()], vec![/* no extra deny */]);
    let events = run_and_collect(
        env,
        Some("act"),
        true,
        "tc-act-write",
        "write_file",
        serde_json::json!({"path": "tools/x.sh", "content": "echo hi"}),
    )
    .await;

    assert!(
        has_grant_pending(&events),
        "expected GrantPending on Act write denial; got {events:#?}"
    );
    assert!(
        !has_profile_switch_pending(&events, "act"),
        "must NOT emit ProfileSwitchPending when classical_fit returns None; got {events:#?}"
    );
    // Grant flow continues to emit ScopeExpansionRequested.
    assert!(
        has_scope_expansion(&events),
        "grant flow must keep emitting ScopeExpansionRequested; got {events:#?}"
    );
}

#[tokio::test]
async fn mode_unset_falls_through_to_grant() {
    // plan-preset envelope, but no `with_permission_mode` →
    // ModeKind::Custom → classical_fit returns None →
    // GrantRequired → grant flow.
    let events = run_and_collect(
        PermissionEnvelope::plan_preset(),
        None,
        true,
        "tc-no-mode",
        "bash",
        serde_json::json!({"command": "ls"}),
    )
    .await;

    assert!(
        has_grant_pending(&events),
        "permission_mode=None must fall through to grant flow; got {events:#?}"
    );
    assert!(
        !has_profile_switch_pending(&events, "act"),
        "permission_mode=None must NOT emit ProfileSwitchPending; got {events:#?}"
    );
}

#[tokio::test]
async fn resume_on_grant_off_does_not_emit_switch_event() {
    // Classifier still returns SuggestSwitch, but with
    // resume_on_grant=false the SuggestSwitch arm synth-denies
    // immediately without emitting any new event — parity with
    // the grant flow's flag-off behaviour.
    let events = run_and_collect(
        PermissionEnvelope::plan_preset(),
        Some("plan"),
        false,
        "tc-flag-off",
        "bash",
        serde_json::json!({"command": "ls"}),
    )
    .await;

    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::ProfileSwitchPending { .. } | AgentEvent::GrantPending { .. }
        )),
        "resume_on_grant=false must NOT emit pending-events; got {events:#?}"
    );
}

#[tokio::test]
async fn switch_timeout_emits_resolved_with_outcome_timeout() {
    // No bridge will respond, so the switch wait task times out
    // (grant_timeout=80ms in run_and_collect). Expected:
    // ProfileSwitchResolved{outcome:"timeout"}.
    let events = run_and_collect(
        PermissionEnvelope::plan_preset(),
        Some("plan"),
        true,
        "tc-timeout",
        "bash",
        serde_json::json!({"command": "ls"}),
    )
    .await;

    let resolved = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ProfileSwitchResolved { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .expect("ProfileSwitchResolved must be emitted on timeout");
    assert_eq!(resolved, "timeout", "events: {events:#?}");
}
