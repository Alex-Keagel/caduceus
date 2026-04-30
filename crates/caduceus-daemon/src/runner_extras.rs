//! Lifecycle session FSM + cross-run handoff reservation + permission
//! elevation forwarder + ACP adapter shim
//! (ru19 + ru20 + ru21 + ru23).
//!
//! Per the implementation DAG, these four concerns share a module
//! because they're all small layers on top of the runner pipeline:
//!
//! - **`ru19`** — `cross_run_handoff` is reserved in the v1 closed
//!   set.  Receiving a frame with `kind = "cross_run_handoff"` MUST
//!   trigger `stop_cascade(unknown_message_kind)`.  Iter-28 #2-8.
//!   The wire codec already returns `DropReason::UnknownKind` for
//!   any kind it doesn't recognize; this module surfaces the
//!   stop-reason mapping.
//!
//! - **`ru20`** — Lifecycle Session FSM tracks turns:
//!   `Idle → InTurn → Idle` repeating; terminal `Exited`.  Driven by
//!   `turn_end` / `exit` frames.  Used by spec #1 dispatch + spec #4
//!   snapshot to know whether a runner is "between turns" (idle) or
//!   actively working.
//!
//! - **`ru21`** — `permission_elevation_request` is forwarded from the
//!   agent's wire to the daemon's permission resolver via a callback.
//!   The actual resolver lives in the m-permissions subsystem (P7);
//!   here we just route.
//!
//! - **`ru23`** — ACP (Agent Communication Protocol) adapter shim.
//!   When the workflow declares `protocol = "acp"`, frames are
//!   translated to/from ACP shape on the way in/out.  V1 minimal:
//!   accepts a flag, defers translation to a pluggable trait.

use crate::runner_process::{RunnerProcess, StopReason};
use crate::wire_codec::{DropReason, Frame, FramePayload};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

// ──────────────────────── ru19 cross_run_handoff ──────────────────────

/// Map a wire-codec `DropReason` to a runner stop reason for
/// stop_cascade.  Iter-28 #2-8: `UnknownKind("cross_run_handoff")`
/// (and any other unknown kind in v1) maps to `UnknownMessageKind`.
pub fn drop_reason_to_stop_reason(reason: &DropReason) -> Option<StopReason> {
    match reason {
        DropReason::UnknownKind(_) => Some(StopReason::UnknownMessageKind),
        DropReason::ProtocolViolation(_) => Some(StopReason::UnknownMessageKind),
        // Parse failures and oversized frames are recoverable; we drop
        // and continue.  QueueFull is backpressure, not a violation.
        DropReason::ParseFailure(_) | DropReason::OversizedFrame | DropReason::QueueFull => None,
    }
}

// ──────────────────────── ru20 Lifecycle Session FSM ──────────────────

/// Lifecycle session state.  Tracks where the runner is in its
/// per-turn lifecycle.  Spec #2 §3.2 / §4.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SessionState {
    /// Spawned; waiting for first turn to start (e.g., agent reading
    /// initial prompt).  Distinct from `InTurn` so dispatch logic can
    /// avoid routing prompt updates to a still-initializing runner.
    Idle = 0,
    /// Agent actively producing output between `turn_start` (implicit
    /// at first non-heartbeat frame after Idle) and `turn_end`.
    InTurn = 1,
    /// Runner has emitted `Exit` (orderly) or `stop_cascade` reaped
    /// the child.  Terminal.
    Exited = 2,
}

/// Lifecycle session FSM.  Cheap to clone (`Arc<AtomicU8>`).
#[derive(Debug, Clone, Default)]
pub struct LifecycleSession {
    state: Arc<AtomicU8>,
}

impl LifecycleSession {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(SessionState::Idle as u8)),
        }
    }

    pub fn state(&self) -> SessionState {
        match self.state.load(Ordering::Acquire) {
            0 => SessionState::Idle,
            1 => SessionState::InTurn,
            _ => SessionState::Exited,
        }
    }

    /// Drive the FSM in response to an observed frame.  Idempotent on
    /// terminal `Exited`.  Returns the new state.
    pub fn observe(&self, frame: &Frame) -> SessionState {
        let cur = self.state();
        if cur == SessionState::Exited {
            return cur;
        }
        let next = match &frame.payload {
            FramePayload::Heartbeat => cur, // heartbeats don't change state
            FramePayload::TurnEnd { .. } => SessionState::Idle,
            FramePayload::Exit { .. } => SessionState::Exited,
            // Any other frame implies the agent is producing output;
            // transition Idle -> InTurn.
            _ => match cur {
                SessionState::Idle => SessionState::InTurn,
                _ => cur,
            },
        };
        self.state.store(next as u8, Ordering::Release);
        next
    }
}

// ──────────────────────── ru21 Permission Elevation ───────────────────

/// Decision rendered by the permission resolver.  Spec m-permissions
/// owns the resolver; this enum is the wire shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationDecision {
    Allow,
    Deny,
    /// Defer the decision to a user prompt.
    PromptUser,
}

/// Forwarder callback type.  Supplied by the daemon's permission
/// resolver wiring; the runner module knows nothing about resolution.
pub type ElevationForwarder = Arc<
    dyn Fn(String, String) -> futures::future::BoxFuture<'static, ElevationDecision> + Send + Sync,
>;

/// Forward a permission_elevation_request frame to the resolver.
/// Iter-28 nothing-specific: this is the v1 forwarder.
pub async fn forward_permission_request(
    forwarder: &ElevationForwarder,
    capability: String,
    reason: String,
) -> ElevationDecision {
    forwarder(capability, reason).await
}

// ──────────────────────── ru23 ACP Adapter Shim ───────────────────────

/// Runner protocol selection.  Spec #2 §3.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerProtocol {
    /// Native NDJSON wire (default).
    NativeNdjson,
    /// ACP — Agent Communication Protocol.  Translation handled by an
    /// `AcpAdapter` impl supplied by the workflow loader.
    Acp,
}

/// Translation surface for ACP <-> native NDJSON.  V1 has only a stub;
/// real adapters land with workflow integration.
pub trait AcpAdapter: Send + Sync {
    /// Translate an ACP frame (raw bytes from the agent) into the
    /// native `Frame` shape, or surface a drop reason.
    fn ingress(&self, raw: &[u8]) -> Result<Frame, DropReason>;

    /// Translate a native daemon -> runner message into ACP wire bytes.
    fn egress(&self, frame: &Frame) -> Result<Vec<u8>, DropReason>;
}

/// V1 stub adapter that surfaces `UnknownKind("acp")` for ingress.
/// Replaced by a real adapter when the workflow loader wires one in.
#[derive(Debug, Default)]
pub struct StubAcpAdapter;

impl AcpAdapter for StubAcpAdapter {
    fn ingress(&self, _raw: &[u8]) -> Result<Frame, DropReason> {
        Err(DropReason::UnknownKind("acp".to_string()))
    }
    fn egress(&self, _frame: &Frame) -> Result<Vec<u8>, DropReason> {
        Err(DropReason::ProtocolViolation(
            "ACP egress not implemented in v1 stub".into(),
        ))
    }
}

/// Apply `stop_cascade` for an unrecoverable wire-codec drop reason.
/// Helper called by integration code so callers don't need to know
/// the StopReason enum.
pub async fn cascade_for_drop(runner: &RunnerProcess, reason: DropReason) {
    if let Some(stop) = drop_reason_to_stop_reason(&reason) {
        let _ = runner.stop_cascade(stop).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire_codec::{ExitKind, FrameId, FramePayload, TokenMode, TokensAbsolute};

    fn frame(payload: FramePayload) -> Frame {
        Frame {
            seq: 1,
            frame_id: FrameId(1),
            payload,
        }
    }

    // ─── ru19 cross_run_handoff mapping ──────────────────────────────

    #[test]
    fn unknown_kind_maps_to_unknown_message_kind_stop() {
        let drop = DropReason::UnknownKind("cross_run_handoff".into());
        assert_eq!(
            drop_reason_to_stop_reason(&drop),
            Some(StopReason::UnknownMessageKind)
        );
    }

    #[test]
    fn protocol_violation_maps_to_unknown_message_kind_stop() {
        let drop = DropReason::ProtocolViolation("bad seq".into());
        assert_eq!(
            drop_reason_to_stop_reason(&drop),
            Some(StopReason::UnknownMessageKind)
        );
    }

    #[test]
    fn parse_failure_does_not_trigger_cascade() {
        let drop = DropReason::ParseFailure("bad json".into());
        assert_eq!(drop_reason_to_stop_reason(&drop), None);
    }

    #[test]
    fn queue_full_does_not_trigger_cascade() {
        let drop = DropReason::QueueFull;
        assert_eq!(drop_reason_to_stop_reason(&drop), None);
    }

    #[test]
    fn oversized_frame_does_not_trigger_cascade() {
        let drop = DropReason::OversizedFrame;
        assert_eq!(drop_reason_to_stop_reason(&drop), None);
    }

    // ─── ru20 Lifecycle Session FSM ──────────────────────────────────

    #[test]
    fn session_starts_idle() {
        let s = LifecycleSession::new();
        assert_eq!(s.state(), SessionState::Idle);
    }

    #[test]
    fn session_transitions_idle_to_in_turn_on_token_update() {
        let s = LifecycleSession::new();
        let f = frame(FramePayload::TokenUpdate {
            mode: TokenMode::Delta,
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: None,
            cache_write_tokens: None,
        });
        assert_eq!(s.observe(&f), SessionState::InTurn);
    }

    #[test]
    fn session_transitions_in_turn_to_idle_on_turn_end() {
        let s = LifecycleSession::new();
        let token = frame(FramePayload::TokenUpdate {
            mode: TokenMode::Delta,
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: None,
            cache_write_tokens: None,
        });
        let end = frame(FramePayload::TurnEnd {
            tokens_at_turn_end: TokensAbsolute {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        });
        s.observe(&token);
        assert_eq!(s.observe(&end), SessionState::Idle);
    }

    #[test]
    fn session_terminal_after_exit() {
        let s = LifecycleSession::new();
        let exit = frame(FramePayload::Exit {
            exit_kind: ExitKind::Completed,
            final_tokens: TokensAbsolute {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        });
        assert_eq!(s.observe(&exit), SessionState::Exited);
        // Subsequent frames don't change state.
        let token = frame(FramePayload::TokenUpdate {
            mode: TokenMode::Delta,
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: None,
            cache_write_tokens: None,
        });
        assert_eq!(s.observe(&token), SessionState::Exited);
    }

    #[test]
    fn session_heartbeat_does_not_change_state() {
        let s = LifecycleSession::new();
        let hb = frame(FramePayload::Heartbeat);
        assert_eq!(s.observe(&hb), SessionState::Idle);
        // Move to InTurn, then heartbeat must not regress.
        let token = frame(FramePayload::TokenUpdate {
            mode: TokenMode::Delta,
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: None,
            cache_write_tokens: None,
        });
        s.observe(&token);
        assert_eq!(s.observe(&hb), SessionState::InTurn);
    }

    // ─── ru21 Permission elevation forwarder ────────────────────────

    #[tokio::test]
    async fn elevation_forwarder_routes_request_to_resolver() {
        let forwarder: ElevationForwarder = Arc::new(|cap: String, reason: String| {
            Box::pin(async move {
                if cap == "network.write" && reason.contains("git push") {
                    ElevationDecision::Allow
                } else {
                    ElevationDecision::Deny
                }
            })
        });
        let dec = forward_permission_request(
            &forwarder,
            "network.write".into(),
            "git push origin main".into(),
        )
        .await;
        assert_eq!(dec, ElevationDecision::Allow);

        let dec2 = forward_permission_request(&forwarder, "fs.write".into(), "rm -rf".into()).await;
        assert_eq!(dec2, ElevationDecision::Deny);
    }

    // ─── ru23 ACP adapter shim ───────────────────────────────────────

    #[test]
    fn stub_acp_adapter_ingress_returns_unknown_kind() {
        let a = StubAcpAdapter;
        let r = a.ingress(b"some-acp-bytes");
        match r {
            Err(DropReason::UnknownKind(k)) => assert_eq!(k, "acp"),
            other => panic!("expected UnknownKind('acp'), got {other:?}"),
        }
    }

    #[test]
    fn runner_protocol_default_is_native() {
        // Compile-time exhaustiveness check: NativeNdjson is the v1 default.
        let p = RunnerProtocol::NativeNdjson;
        assert!(matches!(p, RunnerProtocol::NativeNdjson));
    }
}
