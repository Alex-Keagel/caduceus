//! Cross-run collab patterns — handoff protocol + state merge +
//! target admission (co01 + co02 + co03 + co04).
//!
//! Per the implementation DAG, this module promotes
//! `cross_run_handoff` from a v1 stop-cascade reservation (ru19) into
//! a real protocol once a future spec wave ships.  V1 commits to
//! **closed-set rejection** in the runner (ru19); this module supplies
//! the types and merge logic so spec #5 can promote in a follow-up
//! without rewriting the runner pipeline.
//!
//! Spec cross-reference: `spec-caduceus-collab-patterns.md` §3 patterns
//! 1+2 (normative), pattern 3 (explicit non-support).

use crate::mailbox::RunId;
use crate::snapshot_shapes::TokensAggregate;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A cross-run handoff frame payload.  Spec #5 §3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandoffPayload {
    pub source_run_id: RunId,
    pub target_run_id: RunId,
    /// State to merge into the target.  V1: tokens only; richer
    /// merges (workspace cwd, prompt history) land with future spec.
    pub merge_state: HandoffMergeState,
}

/// State carried from source to target across a handoff.  V1 minimal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct HandoffMergeState {
    pub tokens: TokensAggregate,
}

/// Errors from the handoff admission gate.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HandoffError {
    #[error("target run not admissible: {0}")]
    TargetNotAdmissible(String),
    #[error("source and target are the same run: {0:?}")]
    SourceEqualsTarget(RunId),
    #[error("merge state contradicts target state: {0}")]
    MergeConflict(String),
}

/// co03 — Target Run admission gate.  Returns Ok if the target is
/// admissible (i.e., currently running and not already in a terminal
/// state).  V1 takes pre-computed booleans so callers can supply
/// state without coupling this module to OrchestratorState.
pub fn admit_target(
    source: &RunId,
    target: &RunId,
    target_is_running: bool,
    target_is_terminal: bool,
) -> Result<(), HandoffError> {
    if source == target {
        return Err(HandoffError::SourceEqualsTarget(target.clone()));
    }
    if !target_is_running {
        return Err(HandoffError::TargetNotAdmissible(format!(
            "target {} not running",
            target.0
        )));
    }
    if target_is_terminal {
        return Err(HandoffError::TargetNotAdmissible(format!(
            "target {} is in terminal state",
            target.0
        )));
    }
    Ok(())
}

/// co02 — Merge `source_state` into `target_state`.  Per spec #5 v1
/// merge rules:
///
/// - Tokens: SUM (source + target).  Conflict-free.
/// - Future fields: TBD; v1 only has tokens.
pub fn merge_state(
    target_state: HandoffMergeState,
    source_state: HandoffMergeState,
) -> HandoffMergeState {
    HandoffMergeState {
        tokens: TokensAggregate {
            input_tokens: target_state
                .tokens
                .input_tokens
                .saturating_add(source_state.tokens.input_tokens),
            output_tokens: target_state
                .tokens
                .output_tokens
                .saturating_add(source_state.tokens.output_tokens),
            cache_read_tokens: target_state
                .tokens
                .cache_read_tokens
                .saturating_add(source_state.tokens.cache_read_tokens),
            cache_write_tokens: target_state
                .tokens
                .cache_write_tokens
                .saturating_add(source_state.tokens.cache_write_tokens),
        },
    }
}

/// co01 — Apply a handoff: admit + merge.  Returns the merged state
/// for the target on success.
pub fn apply_handoff(
    payload: &HandoffPayload,
    target_state: HandoffMergeState,
    target_is_running: bool,
    target_is_terminal: bool,
) -> Result<HandoffMergeState, HandoffError> {
    admit_target(
        &payload.source_run_id,
        &payload.target_run_id,
        target_is_running,
        target_is_terminal,
    )?;
    Ok(merge_state(target_state, payload.merge_state.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(s: &str) -> RunId {
        RunId(s.into())
    }

    fn payload(src: &str, tgt: &str, input: u64, output: u64) -> HandoffPayload {
        HandoffPayload {
            source_run_id: rid(src),
            target_run_id: rid(tgt),
            merge_state: HandoffMergeState {
                tokens: TokensAggregate {
                    input_tokens: input,
                    output_tokens: output,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
            },
        }
    }

    fn state(input: u64, output: u64) -> HandoffMergeState {
        HandoffMergeState {
            tokens: TokensAggregate {
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
        }
    }

    // ─── co03 admit_target ──────────────────────────────────────────

    #[test]
    fn admit_target_rejects_self_handoff() {
        let r = admit_target(&rid("r1"), &rid("r1"), true, false);
        assert!(matches!(r, Err(HandoffError::SourceEqualsTarget(_))));
    }

    #[test]
    fn admit_target_rejects_non_running_target() {
        let r = admit_target(&rid("r1"), &rid("r2"), false, false);
        assert!(matches!(r, Err(HandoffError::TargetNotAdmissible(_))));
    }

    #[test]
    fn admit_target_rejects_terminal_target() {
        let r = admit_target(&rid("r1"), &rid("r2"), true, true);
        assert!(matches!(r, Err(HandoffError::TargetNotAdmissible(_))));
    }

    #[test]
    fn admit_target_accepts_running_distinct_target() {
        let r = admit_target(&rid("r1"), &rid("r2"), true, false);
        assert!(r.is_ok());
    }

    // ─── co02 merge_state ───────────────────────────────────────────

    #[test]
    fn merge_state_sums_tokens() {
        let merged = merge_state(state(100, 50), state(20, 10));
        assert_eq!(merged.tokens.input_tokens, 120);
        assert_eq!(merged.tokens.output_tokens, 60);
    }

    #[test]
    fn merge_state_handles_overflow_via_saturating() {
        let merged = merge_state(state(u64::MAX, 0), state(1, 0));
        assert_eq!(merged.tokens.input_tokens, u64::MAX);
    }

    // ─── co01 apply_handoff end-to-end ──────────────────────────────

    #[test]
    fn apply_handoff_succeeds_for_valid_target() {
        let p = payload("r1", "r2", 50, 25);
        let merged = apply_handoff(&p, state(100, 50), true, false).unwrap();
        assert_eq!(merged.tokens.input_tokens, 150);
        assert_eq!(merged.tokens.output_tokens, 75);
    }

    #[test]
    fn apply_handoff_rejects_self_handoff() {
        let p = payload("r1", "r1", 0, 0);
        let r = apply_handoff(&p, state(0, 0), true, false);
        assert!(matches!(r, Err(HandoffError::SourceEqualsTarget(_))));
    }

    #[test]
    fn handoff_payload_serialize_round_trip() {
        let p = payload("r1", "r2", 10, 5);
        let s = serde_json::to_string(&p).unwrap();
        let back: HandoffPayload = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }
}
