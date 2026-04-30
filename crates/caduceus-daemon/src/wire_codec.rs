//! NDJSON wire codec for the runner ↔ daemon channel (ru05).
//!
//! Per the implementation DAG, this module ships the **frame**-level
//! abstraction over the runner's stdout: line-delimited JSON with a
//! closed set of event kinds, a per-frame size cap, and a drop-reason
//! taxonomy.  Higher layers (runner-seq stamping, forward_to_daemon)
//! consume the parsed `Frame` enum.
//!
//! Spec cross-references:
//!
//! - **`spec-caduceus-agent-runner-contract.md` §4.1** — closed v1 event
//!   kind set; required wire fields per kind.
//! - **`spec-caduceus-agent-runner-contract.md` iter-28 #2-7** — Z-23
//!   stamp rule lives in `runner_seq.rs`; this codec MUST NOT stamp.
//! - **`spec-caduceus-agent-runner-contract.md` iter-28 #2-8** —
//!   `cross_run_handoff` is NOT in the v1 closed set; receiving it
//!   triggers `stop_cascade(unknown_message_kind)` (enforced by callers
//!   that match `Frame::Unknown`).
//!
//! The codec is **stateless** for parsing.  Frame-id correlation,
//! runner_seq stamping, and dropped-frame counting all happen one
//! layer up.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum bytes for a single NDJSON line (one frame).  Spec #2 §4.1
/// places an upper bound; v1 uses 1 MiB.  Frames exceeding this are
/// dropped with `DropReason::OversizedFrame`.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Internal correlation id for a parsed frame, used by the queue's
/// drop-reason diagnostics (Z-23 distinguishes `runner_seq` — which is
/// post-Ok — from `frame_id` — which is per-arrival).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameId(pub u64);

/// Reason a frame was dropped at the codec / queue layer.  Spec #2
/// §3.5.1 enumerates these.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DropReason {
    #[error("frame parse failed: {0}")]
    ParseFailure(String),
    #[error("frame larger than {MAX_FRAME_BYTES} bytes")]
    OversizedFrame,
    #[error("unknown event kind: {0}")]
    UnknownKind(String),
    #[error("protocol violation: {0}")]
    ProtocolViolation(String),
    #[error("queue full")]
    QueueFull,
}

/// Closed v1 event-kind set.  Adding a variant here is a normative spec
/// change.  `Unknown` is NOT a real kind; it's the parsing fallthrough
/// the queue uses to stop_cascade with `unknown_message_kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FramePayload {
    /// Token accounting update (delta or absolute).
    TokenUpdate {
        mode: TokenMode,
        input_tokens: u64,
        output_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_write_tokens: Option<u64>,
    },
    /// Turn boundary; carries absolute tokens at turn end.  Iter-28
    /// #2-1: token_at_turn_end MUST be reconciled in absolute mode.
    TurnEnd { tokens_at_turn_end: TokensAbsolute },
    /// Agent advertises orderly exit; carries final tokens.  Iter-28
    /// #2-1: final_tokens MUST be reconciled in absolute mode.
    Exit {
        exit_kind: ExitKind,
        final_tokens: TokensAbsolute,
    },
    /// Heartbeat from runner.  Spec #2 §4.1 cadence is bounded by
    /// `heartbeat_interval`; iter-28 #2-3 timeout tracker fires after
    /// `heartbeat_timeout_ms` of silence.
    Heartbeat,
    /// Agent requests a privilege elevation; daemon decides.
    PermissionElevationRequest { capability: String, reason: String },
    /// Runner-side protocol violation diagnostic (rare; usually the
    /// daemon emits this in response to malformed frames).
    ProtocolViolation { reason: String, kind_detail: String },
}

/// Token reporting mode.  Spec #2 §4.1; `absolute` MUST win on
/// reconciliation if both modes are observed in the same frame stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenMode {
    Absolute,
    Delta,
}

/// Absolute token totals carried by `turn_end` and `exit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokensAbsolute {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
}

/// Reason the runner advertised an exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitKind {
    Completed,
    Aborted,
    Error,
}

/// A frame parsed from the wire.  Iter-28 #2-6: `seq == 0` is the
/// reserved-value guard; `Frame` carries the raw seq the runner
/// emitted.  The seq-regression classifier (`seq_classifier.rs`)
/// decides what to do with it; the codec does not enforce.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub seq: u64,
    pub payload: FramePayload,
    /// Internal correlation id, assigned by the parser.  Used by the
    /// queue to log drops; NEVER conflated with `runner_seq` (Z-23).
    pub frame_id: FrameId,
}

/// Decode a single NDJSON line into a `Frame`.  Returns
/// `Err(DropReason::*)` on malformed input.  The caller is responsible
/// for assigning a `FrameId` (we accept it as input so callers can
/// keep their own monotonic counter).
pub fn decode_line(line: &[u8], frame_id: FrameId) -> Result<Frame, DropReason> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(DropReason::OversizedFrame);
    }
    // Parse to a generic Value first so we can extract `seq` separately
    // from the payload tag.
    #[derive(Deserialize)]
    struct Outer {
        seq: u64,
        #[serde(flatten)]
        payload: serde_json::Value,
    }
    let outer: Outer =
        serde_json::from_slice(line).map_err(|e| DropReason::ParseFailure(e.to_string()))?;

    // Try to deserialize the payload portion as a known FramePayload.
    let payload: FramePayload = match serde_json::from_value(outer.payload.clone()) {
        Ok(p) => p,
        Err(_) => {
            // Fall through to "unknown kind" for diagnostic.
            let kind = outer
                .payload
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("<missing>");
            return Err(DropReason::UnknownKind(kind.to_string()));
        }
    };

    Ok(Frame {
        seq: outer.seq,
        payload,
        frame_id,
    })
}

/// Encode a frame into a single NDJSON line (no trailing newline).
/// Used by the runner side for synthesizing test fixtures.  Production
/// runner code emits frames natively; this exists for symmetric tests
/// and for the in-process runner.
pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>, DropReason> {
    #[derive(Serialize)]
    struct Outer<'a> {
        seq: u64,
        #[serde(flatten)]
        payload: &'a FramePayload,
    }
    serde_json::to_vec(&Outer {
        seq: frame.seq,
        payload: &frame.payload,
    })
    .map_err(|e| DropReason::ParseFailure(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fid(n: u64) -> FrameId {
        FrameId(n)
    }

    #[test]
    fn decode_token_update_delta() {
        let line = br#"{"seq":1,"kind":"token_update","mode":"delta","input_tokens":10,"output_tokens":5}"#;
        let f = decode_line(line, fid(0)).unwrap();
        assert_eq!(f.seq, 1);
        match f.payload {
            FramePayload::TokenUpdate {
                mode,
                input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(mode, TokenMode::Delta);
                assert_eq!(input_tokens, 10);
                assert_eq!(output_tokens, 5);
            }
            other => panic!("expected TokenUpdate, got {other:?}"),
        }
    }

    #[test]
    fn decode_token_update_absolute_with_cache() {
        let line = br#"{"seq":2,"kind":"token_update","mode":"absolute","input_tokens":100,"output_tokens":50,"cache_read_tokens":20}"#;
        let f = decode_line(line, fid(1)).unwrap();
        match f.payload {
            FramePayload::TokenUpdate {
                mode,
                cache_read_tokens,
                ..
            } => {
                assert_eq!(mode, TokenMode::Absolute);
                assert_eq!(cache_read_tokens, Some(20));
            }
            other => panic!("expected TokenUpdate, got {other:?}"),
        }
    }

    #[test]
    fn decode_turn_end_with_absolute_tokens() {
        let line = br#"{"seq":7,"kind":"turn_end","tokens_at_turn_end":{"input_tokens":1000,"output_tokens":500}}"#;
        let f = decode_line(line, fid(0)).unwrap();
        match f.payload {
            FramePayload::TurnEnd { tokens_at_turn_end } => {
                assert_eq!(tokens_at_turn_end.input_tokens, 1000);
                assert_eq!(tokens_at_turn_end.output_tokens, 500);
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn decode_exit_with_final_tokens() {
        let line = br#"{"seq":99,"kind":"exit","exit_kind":"completed","final_tokens":{"input_tokens":2000,"output_tokens":1000}}"#;
        let f = decode_line(line, fid(0)).unwrap();
        match f.payload {
            FramePayload::Exit {
                exit_kind,
                final_tokens,
            } => {
                assert_eq!(exit_kind, ExitKind::Completed);
                assert_eq!(final_tokens.input_tokens, 2000);
            }
            other => panic!("expected Exit, got {other:?}"),
        }
    }

    #[test]
    fn decode_heartbeat() {
        let line = br#"{"seq":3,"kind":"heartbeat"}"#;
        let f = decode_line(line, fid(0)).unwrap();
        assert!(matches!(f.payload, FramePayload::Heartbeat));
    }

    #[test]
    fn decode_permission_elevation() {
        let line = br#"{"seq":42,"kind":"permission_elevation_request","capability":"network.write","reason":"git push"}"#;
        let f = decode_line(line, fid(0)).unwrap();
        match f.payload {
            FramePayload::PermissionElevationRequest { capability, reason } => {
                assert_eq!(capability, "network.write");
                assert_eq!(reason, "git push");
            }
            other => panic!("expected PermissionElevationRequest, got {other:?}"),
        }
    }

    #[test]
    fn decode_unknown_kind_returns_unknown_kind_drop() {
        // Iter-28 #2-8: cross_run_handoff is NOT in v1 closed set.
        let line = br#"{"seq":5,"kind":"cross_run_handoff","payload":{}}"#;
        let r = decode_line(line, fid(0));
        match r {
            Err(DropReason::UnknownKind(k)) => assert_eq!(k, "cross_run_handoff"),
            other => panic!("expected UnknownKind, got {other:?}"),
        }
    }

    #[test]
    fn decode_oversized_returns_oversized_frame() {
        let big = b"a".repeat(MAX_FRAME_BYTES + 1);
        let r = decode_line(&big, fid(0));
        assert!(matches!(r, Err(DropReason::OversizedFrame)));
    }

    #[test]
    fn decode_malformed_json_returns_parse_failure() {
        let line = br#"{"seq":1,"kind":not-json"#;
        match decode_line(line, fid(0)) {
            Err(DropReason::ParseFailure(_)) => {}
            other => panic!("expected ParseFailure, got {other:?}"),
        }
    }

    #[test]
    fn decode_missing_seq_is_parse_failure() {
        let line = br#"{"kind":"heartbeat"}"#;
        assert!(matches!(
            decode_line(line, fid(0)),
            Err(DropReason::ParseFailure(_))
        ));
    }

    #[test]
    fn decode_seq_zero_is_passed_through_for_classifier() {
        // Iter-28 #2-6: seq=0 is the reserved-value guard, but the codec
        // does NOT classify; it returns the frame and the regression
        // classifier handles it.
        let line = br#"{"seq":0,"kind":"heartbeat"}"#;
        let f = decode_line(line, fid(0)).unwrap();
        assert_eq!(f.seq, 0);
    }

    #[test]
    fn encode_then_decode_round_trips() {
        let original = Frame {
            seq: 17,
            frame_id: fid(99),
            payload: FramePayload::TokenUpdate {
                mode: TokenMode::Delta,
                input_tokens: 7,
                output_tokens: 3,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        };
        let bytes = encode_frame(&original).unwrap();
        let back = decode_line(&bytes, fid(99)).unwrap();
        assert_eq!(back.seq, original.seq);
        assert_eq!(back.payload, original.payload);
    }
}
