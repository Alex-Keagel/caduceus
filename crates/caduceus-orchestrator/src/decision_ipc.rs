//! IPC handlers for the DecisionRegister.
//!
//! Per `spec-decision-register` §4.5 (Z8-D45..D46). Four methods:
//!
//! * `list_decisions` — read-only enumeration.
//! * `lock_decision` — emit a `DecisionLocked` event onto the orchestrator
//!   stream, return the post-apply `DecisionEntry`.
//! * `amend_decision` — emit a `DecisionAmended`.
//! * `unlock_decision` — emit a `DecisionUnlocked`.
//!
//! **Z8-D45 invariant:** IPC handlers MUST enqueue events on the
//! orchestrator's event stream. They DO NOT mutate reducer state directly.
//! This module implements that contract via [`IpcEnqueuer`] — a small
//! trait the orchestrator's event-bus implements; the handler returns
//! an event for the caller to fan out, plus the post-apply state.
//!
//! In tests we use [`InMemoryEnqueuer`] which applies events directly to
//! a `DecisionRegister` and records them; in production the daemon's
//! broadcast bus is the implementor.

use crate::decision_register::{
    apply_event, ApplyOutcome, DecisionRegister, ReducerError as DecisionReducerError,
};
use anyhow::Result;
use caduceus_core::decision_register::{
    DecisionEntry, DecisionId, DecisionRegisterErrorCode, DecisionSource, DecisionValue, ProducerId,
};
use caduceus_core::{AgentEvent, RequestId};

/// The orchestrator-side enqueuer. Production wires this to the broadcast
/// bus; tests use [`InMemoryEnqueuer`] which mutates a register directly.
///
/// `apply` is synchronous from the caller's POV (Z8-D46): it returns once
/// the event has been applied to the reducer state. The orchestrator's
/// implementation is responsible for serializing concurrent enqueues
/// through the single-authority event stream (I-1).
pub trait IpcEnqueuer {
    /// Enqueue + synchronously apply one event. Returns the entry as it
    /// stands after application, or the reducer error.
    fn enqueue(&mut self, event: AgentEvent) -> Result<IpcApply, DecisionReducerError>;
}

/// Outcome returned to IPC callers.
#[derive(Debug, Clone)]
pub struct IpcApply {
    pub outcome: ApplyOutcome,
    /// Post-apply snapshot of the entry, when one exists.
    pub entry: Option<DecisionEntry>,
}

/// Test-friendly enqueuer that owns a register and applies events.
pub struct InMemoryEnqueuer {
    pub register: DecisionRegister,
    pub event_seq: u64,
    pub history: Vec<AgentEvent>,
}

impl InMemoryEnqueuer {
    pub fn new(register: DecisionRegister) -> Self {
        let seq = register.last_event_seq;
        Self {
            register,
            event_seq: seq,
            history: Vec::new(),
        }
    }
}

impl IpcEnqueuer for InMemoryEnqueuer {
    fn enqueue(&mut self, event: AgentEvent) -> Result<IpcApply, DecisionReducerError> {
        self.event_seq += 1;
        let outcome = apply_event(&mut self.register, &event, self.event_seq)?;
        let id = match &event {
            AgentEvent::DecisionLocked { id, .. }
            | AgentEvent::DecisionAmended { id, .. }
            | AgentEvent::DecisionUnlocked { id, .. }
            | AgentEvent::DecisionLockDenied { id, .. } => Some(id.clone()),
            _ => None,
        };
        let entry = id.and_then(|id| self.register.entries.get(&id).cloned());
        self.history.push(event);
        Ok(IpcApply { outcome, entry })
    }
}

// ── IPC handlers ─────────────────────────────────────────────────────────────

/// `list_decisions(thread_id) -> Vec<DecisionEntry>` (spec §4.5).
///
/// Pure read against an in-memory register snapshot. The caller is
/// responsible for ensuring the snapshot is consistent (e.g. by calling
/// this from the orchestrator's single-authority loop).
pub fn list_decisions(register: &DecisionRegister) -> Vec<DecisionEntry> {
    register.entries.values().cloned().collect()
}

/// `lock_decision(thread_id, id, value, reason?)` (spec §4.5).
///
/// Emits a `DecisionLocked` event with `source = User` (IPC implies a
/// user-driven action). Idempotent on same value (returns existing entry).
pub fn lock_decision<E: IpcEnqueuer>(
    enqueuer: &mut E,
    id: DecisionId,
    value: DecisionValue,
    reason: Option<String>,
    request_id: Option<RequestId>,
    producer_id: ProducerId,
) -> Result<DecisionEntry, DecisionReducerError> {
    let event = AgentEvent::DecisionLocked {
        id: id.clone(),
        value,
        source: DecisionSource::User,
        derived_from: None,
        reason,
        request_id,
        producer_id,
    };
    let applied = enqueuer.enqueue(event)?;
    applied.entry.ok_or_else(|| DecisionReducerError {
        code: DecisionRegisterErrorCode::InvalidValueShape,
        id: Some(id),
        detail: "lock_decision applied but produced no entry — invariant violation".into(),
    })
}

/// `amend_decision(thread_id, id, value, prior_value, reason)` (spec §4.5).
///
/// Emits `DecisionAmended` with `source = User`. Reason MUST be non-empty;
/// `prior_value` MUST match the current state (race detection).
pub fn amend_decision<E: IpcEnqueuer>(
    enqueuer: &mut E,
    id: DecisionId,
    value: DecisionValue,
    prior_value: DecisionValue,
    reason: String,
    request_id: Option<RequestId>,
    producer_id: ProducerId,
) -> Result<DecisionEntry, DecisionReducerError> {
    if reason.is_empty() {
        return Err(DecisionReducerError {
            code: DecisionRegisterErrorCode::AmendWithoutReason,
            id: Some(id),
            detail: "amend_decision: reason MUST be non-empty".into(),
        });
    }
    let event = AgentEvent::DecisionAmended {
        id: id.clone(),
        value,
        prior_value,
        source: DecisionSource::User,
        reason,
        request_id,
        producer_id,
    };
    let applied = enqueuer.enqueue(event)?;
    applied.entry.ok_or_else(|| DecisionReducerError {
        code: DecisionRegisterErrorCode::AmendOfUnlocked,
        id: Some(id),
        detail: "amend_decision applied but produced no entry".into(),
    })
}

/// `unlock_decision(thread_id, id, reason)` (spec §4.5).
///
/// Emits `DecisionUnlocked` with `source = User`. Reason MUST be non-empty.
/// Caller MUST supply `prior_value` since the spec event variant carries
/// it (the orchestrator-side wrapper resolves `current` and passes it).
pub fn unlock_decision<E: IpcEnqueuer>(
    enqueuer: &mut E,
    register_for_prior_lookup: &DecisionRegister,
    id: DecisionId,
    reason: String,
    request_id: Option<RequestId>,
    producer_id: ProducerId,
) -> Result<DecisionEntry, DecisionReducerError> {
    if reason.is_empty() {
        return Err(DecisionReducerError {
            code: DecisionRegisterErrorCode::AmendWithoutReason,
            id: Some(id),
            detail: "unlock_decision: reason MUST be non-empty".into(),
        });
    }
    let prior_value = register_for_prior_lookup
        .entries
        .get(&id)
        .and_then(|e| e.current.clone())
        .ok_or_else(|| DecisionReducerError {
            code: DecisionRegisterErrorCode::UnlockOfUnlocked,
            id: Some(id.clone()),
            detail: "unlock_decision: id is not currently Locked".into(),
        })?;
    let event = AgentEvent::DecisionUnlocked {
        id: id.clone(),
        prior_value,
        source: DecisionSource::User,
        reason,
        request_id,
        producer_id,
    };
    let applied = enqueuer.enqueue(event)?;
    applied.entry.ok_or_else(|| DecisionReducerError {
        code: DecisionRegisterErrorCode::UnlockOfUnlocked,
        id: Some(id),
        detail: "unlock_decision applied but produced no entry".into(),
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::decision_register::DecisionState;
    use caduceus_core::ThreadId;

    fn id(s: &str) -> DecisionId {
        DecisionId::new(s).unwrap()
    }

    fn pid() -> ProducerId {
        ProducerId::new("zed-cli")
    }

    fn fresh() -> InMemoryEnqueuer {
        InMemoryEnqueuer::new(DecisionRegister::new(ThreadId::new()))
    }

    // ── §6.9 IPC tests ──

    #[test]
    fn z8_d45_lock_decision_via_ipc_enqueues_event() {
        let mut e = fresh();
        let entry = lock_decision(
            &mut e,
            id("naming/framework"),
            DecisionValue::String("Aletheia".into()),
            Some("user picked".into()),
            None,
            pid(),
        )
        .unwrap();
        // Z8-D45: the IPC produced an event on the stream, not a direct
        // mutation. The InMemoryEnqueuer records every event it applied;
        // assert exactly one was enqueued.
        assert_eq!(e.history.len(), 1);
        assert!(matches!(e.history[0], AgentEvent::DecisionLocked { .. }));
        // Post-apply state observable.
        assert_eq!(entry.state, DecisionState::Locked);
        assert_eq!(
            entry.current.as_ref().unwrap(),
            &DecisionValue::String("Aletheia".into())
        );
        // Z8-D46: entry returned reflects post-apply state.
        let stored = &e.register.entries[&id("naming/framework")];
        assert_eq!(stored, &entry);
    }

    #[test]
    fn lock_decision_idempotent_on_same_value() {
        let mut e = fresh();
        let _ = lock_decision(
            &mut e,
            id("a"),
            DecisionValue::String("v".into()),
            None,
            None,
            pid(),
        )
        .unwrap();
        // Same id, same value → idempotent.
        let entry = lock_decision(
            &mut e,
            id("a"),
            DecisionValue::String("v".into()),
            None,
            None,
            pid(),
        )
        .unwrap();
        assert_eq!(
            entry.history.len(),
            1,
            "idempotent re-lock must NOT append history"
        );
        // Two events were enqueued, but the second was a NoOp.
        assert_eq!(e.history.len(), 2);
    }

    #[test]
    fn amend_decision_via_ipc_returns_post_apply() {
        let mut e = fresh();
        lock_decision(
            &mut e,
            id("a"),
            DecisionValue::String("v1".into()),
            None,
            None,
            pid(),
        )
        .unwrap();
        let amended = amend_decision(
            &mut e,
            id("a"),
            DecisionValue::String("v2".into()),
            DecisionValue::String("v1".into()),
            "user refined".into(),
            None,
            pid(),
        )
        .unwrap();
        assert_eq!(
            amended.current.as_ref().unwrap(),
            &DecisionValue::String("v2".into())
        );
        assert_eq!(amended.history.len(), 2);
        assert!(amended.last_amended_at.is_some());
    }

    #[test]
    fn amend_without_reason_rejected_at_handler_layer() {
        let mut e = fresh();
        let err = amend_decision(
            &mut e,
            id("a"),
            DecisionValue::String("v2".into()),
            DecisionValue::String("v1".into()),
            String::new(),
            None,
            pid(),
        )
        .unwrap_err();
        assert_eq!(err.code, DecisionRegisterErrorCode::AmendWithoutReason);
        // No event enqueued — reject happens before reducer.
        assert_eq!(e.history.len(), 0);
    }

    #[test]
    fn amend_with_stale_prior_value_rejected_by_reducer() {
        let mut e = fresh();
        lock_decision(
            &mut e,
            id("a"),
            DecisionValue::String("v1".into()),
            None,
            None,
            pid(),
        )
        .unwrap();
        let err = amend_decision(
            &mut e,
            id("a"),
            DecisionValue::String("v3".into()),
            DecisionValue::String("v2".into()), // wrong prior
            "race".into(),
            None,
            pid(),
        )
        .unwrap_err();
        assert_eq!(err.code, DecisionRegisterErrorCode::StaleAmend);
    }

    #[test]
    fn unlock_decision_via_ipc_marks_unlocked() {
        let mut e = fresh();
        lock_decision(
            &mut e,
            id("a"),
            DecisionValue::String("v".into()),
            None,
            None,
            pid(),
        )
        .unwrap();
        // Snapshot for prior_value lookup happens against the post-lock
        // register the enqueuer holds.
        let reg_snap = e.register.clone();
        let unlocked = unlock_decision(
            &mut e,
            &reg_snap,
            id("a"),
            "user retracted".into(),
            None,
            pid(),
        )
        .unwrap();
        assert_eq!(unlocked.state, DecisionState::Unlocked);
        assert!(unlocked.current.is_none());
        assert_eq!(unlocked.history.len(), 2);
    }

    #[test]
    fn unlock_unknown_id_errors() {
        let mut e = fresh();
        let snap = e.register.clone();
        let err =
            unlock_decision(&mut e, &snap, id("missing"), "x".into(), None, pid()).unwrap_err();
        assert_eq!(err.code, DecisionRegisterErrorCode::UnlockOfUnlocked);
        assert_eq!(e.history.len(), 0);
    }

    #[test]
    fn list_decisions_returns_all_entries() {
        let mut e = fresh();
        for s in ["a", "b", "c"] {
            lock_decision(
                &mut e,
                id(s),
                DecisionValue::String(format!("v_{s}")),
                None,
                None,
                pid(),
            )
            .unwrap();
        }
        let listed = list_decisions(&e.register);
        let mut names: Vec<&str> = listed.iter().map(|x| x.id.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["a", "b", "c"]);
    }
}
