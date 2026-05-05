//! DecisionRegister reducer + persistence.
//!
//! Per `spec-decision-register` §3.2 + §3.3 (Z8-D10..D20). The reducer is a
//! pure function `(state, event) → Result<state, error>`; persistence is a
//! separate concern wired to the same in-memory state via [`persist`] and
//! [`load`].
//!
//! Wired into the orchestrator's event stream by P5 (open-question pool +
//! workspace-mutation handler isolation) and P6 (IPC handlers). This module
//! is intentionally standalone: it can be tested without touching IPC or
//! event-bus plumbing.

use anyhow::{Context, Result};
use caduceus_core::decision_register::{
    DecisionEntry, DecisionId, DecisionOp, DecisionRecord, DecisionRegisterErrorCode,
    DecisionSource, DecisionState, DecisionValue, ProducerId,
};
use caduceus_core::AgentEvent;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::thread_id::ThreadIdEnv;
use caduceus_core::ThreadId;

/// On-disk schema version. Bumped per `spec-cross-cutting-wiring.md` §3.10.
pub const SCHEMA_VERSION: u16 = 1;

/// In-memory + on-disk register projection.
///
/// `entries` is a `BTreeMap` so iteration is deterministic — important
/// for the lex-sort rendering in [`super`]'s ReconciliationMessage code
/// (Z8-D28).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRegister {
    pub schema_version: u16,
    pub thread_id: ThreadId,
    pub last_event_seq: u64,
    /// Wall-clock time of the most recent applied mutation.
    pub last_mutation: String,
    pub entries: BTreeMap<DecisionId, DecisionEntry>,
}

impl DecisionRegister {
    pub fn new(thread_id: ThreadId) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            thread_id,
            last_event_seq: 0,
            last_mutation: now_iso8601(),
            entries: BTreeMap::new(),
        }
    }

    /// Cardinality of `state == Locked` entries (used by
    /// `DecisionRegisterRestored.count`).
    pub fn locked_count(&self) -> u32 {
        self.entries
            .values()
            .filter(|e| e.state == DecisionState::Locked)
            .count() as u32
    }
}

/// Outcome of applying one event to the register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Mutation committed; the register changed.
    Mutated,
    /// Idempotent re-lock at the same value (Z8-D5). No history append, no
    /// mutation; caller MUST NOT persist on this outcome alone.
    NoOp,
    /// The event is not a decision-related event; the reducer is sparse
    /// (Z8-D13). Caller passes it through to other reducers.
    Passthrough,
    /// Audit-only mutation: `DecisionLockDenied` appended a `LockDenied`
    /// record to the entry's history but did NOT change `state` or
    /// `current` (spec §3.9.3 / Z8-D47). Caller SHOULD persist so the
    /// audit trail survives crash.
    AuditOnly,
}

/// Reducer error — the structured error mirror of
/// `AgentEvent::DecisionRegisterError`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("DecisionRegisterError {{ code: {code}, id: {id:?}, detail: {detail} }}")]
pub struct ReducerError {
    pub code: DecisionRegisterErrorCode,
    pub id: Option<DecisionId>,
    pub detail: String,
}

impl ReducerError {
    fn new(code: DecisionRegisterErrorCode, id: Option<DecisionId>, detail: &str) -> Self {
        Self {
            code,
            id,
            detail: detail.to_string(),
        }
    }
}

/// Apply one [`AgentEvent`] to the register. Pure: no side effects.
///
/// * Returns `Ok(ApplyOutcome::Mutated)` on a state-changing event.
/// * Returns `Ok(ApplyOutcome::NoOp)` on an idempotent re-lock (Z8-D5).
/// * Returns `Ok(ApplyOutcome::AuditOnly)` on `DecisionLockDenied`.
/// * Returns `Ok(ApplyOutcome::Passthrough)` on non-decision events.
/// * Returns `Err(ReducerError)` on any rejected event (Z8-D6/7/8/9 etc.).
///   The register is unchanged on `Err`.
pub fn apply_event(
    state: &mut DecisionRegister,
    event: &AgentEvent,
    event_seq: u64,
) -> Result<ApplyOutcome, ReducerError> {
    match event {
        AgentEvent::DecisionLocked {
            id,
            value,
            source,
            derived_from,
            reason,
            request_id,
            producer_id,
        } => apply_lock(
            state,
            event_seq,
            id,
            value,
            *source,
            derived_from.as_ref(),
            reason.as_deref(),
            request_id.as_ref(),
            producer_id,
        ),
        AgentEvent::DecisionAmended {
            id,
            value,
            prior_value,
            source,
            reason,
            request_id,
            producer_id,
        } => apply_amend(
            state,
            event_seq,
            id,
            value,
            prior_value,
            *source,
            reason,
            request_id.as_ref(),
            producer_id,
        ),
        AgentEvent::DecisionUnlocked {
            id,
            prior_value,
            source,
            reason,
            request_id,
            producer_id,
        } => apply_unlock(
            state,
            event_seq,
            id,
            prior_value,
            *source,
            reason,
            request_id.as_ref(),
            producer_id,
        ),
        AgentEvent::DecisionLockDenied {
            id,
            attempted_value,
            denied_by,
            reason,
            source,
            request_id,
        } => apply_lock_denied(
            state,
            event_seq,
            id,
            attempted_value,
            denied_by,
            reason,
            *source,
            request_id.as_ref(),
        ),
        // Non-decision events: sparse pass-through (Z8-D13).
        _ => Ok(ApplyOutcome::Passthrough),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_lock(
    state: &mut DecisionRegister,
    event_seq: u64,
    id: &DecisionId,
    value: &DecisionValue,
    source: DecisionSource,
    derived_from: Option<&DecisionId>,
    reason: Option<&str>,
    request_id: Option<&caduceus_core::RequestId>,
    producer_id: &ProducerId,
) -> Result<ApplyOutcome, ReducerError> {
    if let Err(code) = value.validate_shape() {
        return Err(ReducerError::new(code, Some(id.clone()), "value shape"));
    }
    let now = now_iso8601();

    if let Some(entry) = state.entries.get_mut(id) {
        match (entry.state, &entry.current) {
            (DecisionState::Locked, Some(current)) if current == value => {
                // Idempotent re-lock (Z8-D5).
                Ok(ApplyOutcome::NoOp)
            }
            (DecisionState::Locked, Some(_current)) => {
                // Re-lock with different value. Two cases per spec §3.9.5:
                //   (a) Last authoring record was User AND new event is
                //       Agent → AgentOverrodeUser (Z8-D38).
                //   (b) Otherwise → ImplicitAmendForbidden (Z8-D6).
                let last_authored_was_user = entry
                    .history
                    .iter()
                    .rev()
                    .find(|r| matches!(r.op, DecisionOp::Lock | DecisionOp::Amend))
                    .map(|r| r.source == DecisionSource::User)
                    .unwrap_or(false);
                if last_authored_was_user && source == DecisionSource::Agent {
                    Err(ReducerError::new(
                        DecisionRegisterErrorCode::AgentOverrodeUser,
                        Some(id.clone()),
                        "agent re-locked a User-sourced decision with a different value; \
                         emit DecisionAmended with a reason naming the override",
                    ))
                } else {
                    Err(ReducerError::new(
                        DecisionRegisterErrorCode::ImplicitAmendForbidden,
                        Some(id.clone()),
                        "re-lock with different value; emit DecisionAmended explicitly",
                    ))
                }
            }
            (DecisionState::Locked, None) => {
                // Should be impossible (Z8-D17 invariant). Treat as
                // corruption surfaced as a fatal-class reducer error for
                // visibility.
                Err(ReducerError::new(
                    DecisionRegisterErrorCode::InvalidValueShape,
                    Some(id.clone()),
                    "Locked entry with current=None violates Z8-D17",
                ))
            }
            (DecisionState::Unlocked, _) => {
                // Re-lock of an Unlocked entry (Z8-D5b). Append history.
                // Agents are allowed to lock unlocked entries (the chain
                // User-Lock → Anything-Unlock → Agent-Lock is legitimate
                // because the unlock signals user retraction; Z8-D38
                // doesn't apply once the entry is Unlocked).
                entry.history.push(DecisionRecord {
                    op: DecisionOp::Lock,
                    value: Some(value.clone()),
                    source,
                    locked_at: now.clone(),
                    request_id: request_id.cloned(),
                    reason: reason.map(|s| s.to_string()),
                    producer_id: producer_id.clone(),
                });
                entry.state = DecisionState::Locked;
                entry.current = Some(value.clone());
                if entry.derived_from.is_none() {
                    entry.derived_from = derived_from.cloned();
                }
                state.last_event_seq = event_seq;
                state.last_mutation = now;
                Ok(ApplyOutcome::Mutated)
            }
        }
    } else {
        // Brand new entry.
        let record = DecisionRecord {
            op: DecisionOp::Lock,
            value: Some(value.clone()),
            source,
            locked_at: now.clone(),
            request_id: request_id.cloned(),
            reason: reason.map(|s| s.to_string()),
            producer_id: producer_id.clone(),
        };
        let entry = DecisionEntry {
            id: id.clone(),
            state: DecisionState::Locked,
            current: Some(value.clone()),
            history: vec![record],
            derived_from: derived_from.cloned(),
            locked_at: now.clone(),
            last_amended_at: None,
        };
        state.entries.insert(id.clone(), entry);
        state.last_event_seq = event_seq;
        state.last_mutation = now;
        Ok(ApplyOutcome::Mutated)
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_amend(
    state: &mut DecisionRegister,
    event_seq: u64,
    id: &DecisionId,
    value: &DecisionValue,
    prior_value: &DecisionValue,
    source: DecisionSource,
    reason: &str,
    request_id: Option<&caduceus_core::RequestId>,
    producer_id: &ProducerId,
) -> Result<ApplyOutcome, ReducerError> {
    if reason.is_empty() {
        return Err(ReducerError::new(
            DecisionRegisterErrorCode::AmendWithoutReason,
            Some(id.clone()),
            "DecisionAmended.reason MUST be non-empty",
        ));
    }
    if let Err(code) = value.validate_shape() {
        return Err(ReducerError::new(code, Some(id.clone()), "value shape"));
    }
    let entry = state.entries.get_mut(id).ok_or_else(|| {
        ReducerError::new(
            DecisionRegisterErrorCode::AmendOfUnlocked,
            Some(id.clone()),
            "amend of unknown id",
        )
    })?;
    if entry.state != DecisionState::Locked {
        return Err(ReducerError::new(
            DecisionRegisterErrorCode::AmendOfUnlocked,
            Some(id.clone()),
            "amend of an Unlocked entry; emit DecisionLocked first",
        ));
    }
    if entry.current.as_ref() != Some(prior_value) {
        return Err(ReducerError::new(
            DecisionRegisterErrorCode::StaleAmend,
            Some(id.clone()),
            "DecisionAmended.prior_value does not match current; concurrent amend race",
        ));
    }
    // Z8-D38: agent overriding a User-sourced lock without explicit
    // amend-naming-the-override is ALSO blocked here at the amend level
    // when source=Agent and the last record was User-sourced.
    if source == DecisionSource::Agent {
        if let Some(record) = entry.history.iter().next_back() {
            if record.source == DecisionSource::User
                && record.value.as_ref() == Some(prior_value)
                && reason.trim().is_empty()
            {
                return Err(ReducerError::new(
                    DecisionRegisterErrorCode::AgentOverrodeUser,
                    Some(id.clone()),
                    "agent amended a User-sourced decision without a reason naming the override",
                ));
            }
        }
    }
    let now = now_iso8601();
    entry.history.push(DecisionRecord {
        op: DecisionOp::Amend,
        value: Some(value.clone()),
        source,
        locked_at: now.clone(),
        request_id: request_id.cloned(),
        reason: Some(reason.to_string()),
        producer_id: producer_id.clone(),
    });
    entry.current = Some(value.clone());
    entry.last_amended_at = Some(now.clone());
    state.last_event_seq = event_seq;
    state.last_mutation = now;
    Ok(ApplyOutcome::Mutated)
}

#[allow(clippy::too_many_arguments)]
fn apply_unlock(
    state: &mut DecisionRegister,
    event_seq: u64,
    id: &DecisionId,
    prior_value: &DecisionValue,
    source: DecisionSource,
    reason: &str,
    request_id: Option<&caduceus_core::RequestId>,
    producer_id: &ProducerId,
) -> Result<ApplyOutcome, ReducerError> {
    if reason.is_empty() {
        return Err(ReducerError::new(
            DecisionRegisterErrorCode::AmendWithoutReason,
            Some(id.clone()),
            "DecisionUnlocked.reason MUST be non-empty",
        ));
    }
    let entry = state.entries.get_mut(id).ok_or_else(|| {
        ReducerError::new(
            DecisionRegisterErrorCode::UnlockOfUnlocked,
            Some(id.clone()),
            "unlock of unknown id",
        )
    })?;
    if entry.state != DecisionState::Locked {
        return Err(ReducerError::new(
            DecisionRegisterErrorCode::UnlockOfUnlocked,
            Some(id.clone()),
            "unlock of an already-Unlocked entry",
        ));
    }
    if entry.current.as_ref() != Some(prior_value) {
        return Err(ReducerError::new(
            DecisionRegisterErrorCode::StaleAmend,
            Some(id.clone()),
            "DecisionUnlocked.prior_value does not match current",
        ));
    }
    let now = now_iso8601();
    entry.history.push(DecisionRecord {
        op: DecisionOp::Unlock,
        value: None,
        source,
        locked_at: now.clone(),
        request_id: request_id.cloned(),
        reason: Some(reason.to_string()),
        producer_id: producer_id.clone(),
    });
    entry.state = DecisionState::Unlocked;
    entry.current = None;
    entry.last_amended_at = Some(now.clone());
    state.last_event_seq = event_seq;
    state.last_mutation = now;
    Ok(ApplyOutcome::Mutated)
}

#[allow(clippy::too_many_arguments)]
fn apply_lock_denied(
    state: &mut DecisionRegister,
    event_seq: u64,
    id: &DecisionId,
    attempted_value: &DecisionValue,
    denied_by: &str,
    reason: &str,
    source: DecisionSource,
    request_id: Option<&caduceus_core::RequestId>,
) -> Result<ApplyOutcome, ReducerError> {
    if reason.is_empty() {
        return Err(ReducerError::new(
            DecisionRegisterErrorCode::AmendWithoutReason,
            Some(id.clone()),
            "DecisionLockDenied.reason MUST be non-empty",
        ));
    }
    let now = now_iso8601();
    let producer_id = ProducerId::new(format!("permission-envelope:{denied_by}"));
    let record = DecisionRecord {
        op: DecisionOp::LockDenied,
        value: Some(attempted_value.clone()),
        source,
        locked_at: now.clone(),
        request_id: request_id.cloned(),
        reason: Some(reason.to_string()),
        producer_id,
    };
    if let Some(entry) = state.entries.get_mut(id) {
        entry.history.push(record);
    } else {
        let entry = DecisionEntry {
            id: id.clone(),
            state: DecisionState::Unlocked,
            current: None,
            history: vec![record],
            derived_from: None,
            locked_at: now.clone(),
            last_amended_at: None,
        };
        state.entries.insert(id.clone(), entry);
    }
    state.last_event_seq = event_seq;
    state.last_mutation = now;
    Ok(ApplyOutcome::AuditOnly)
}

// ── Persistence ──────────────────────────────────────────────────────────────

/// File name under `<thread_dir>/`.
const REGISTER_FILE: &str = "decision_register.json";

/// Path to the durable register file for `tid`.
pub fn register_path(env: &ThreadIdEnv, tid: &ThreadId) -> PathBuf {
    env.thread_dir(tid).join(REGISTER_FILE)
}

/// Atomic-rename write with fsync discipline (spec §3.3.5 / Z8-D19a..e).
///
/// Steps:
/// 1. Write to `<file>.tmp.<rand>` in the SAME directory.
/// 2. `fsync` the temp file.
/// 3. `rename` to the target path.
/// 4. `fsync` the parent directory (best-effort on macOS where `F_FULLFSYNC`
///    is preferred but `fsync` of dir suffices for ordering guarantees).
///
/// Returns `Ok(())` on success; on failure the temp file is left in place
/// for crash recovery (Z8-D19e: "prefer the non-temp file when both exist"
/// is enforced by [`load`]).
pub fn persist(env: &ThreadIdEnv, register: &DecisionRegister) -> Result<()> {
    let dir = env.thread_dir(&register.thread_id);
    fs::create_dir_all(&dir).with_context(|| format!("mkdir -p {}", dir.display()))?;
    let target = register_path(env, &register.thread_id);

    let bytes = serde_json::to_vec_pretty(register).context("serialize DecisionRegister")?;

    let tmp = dir.join(format!("{}.tmp.{:08x}", REGISTER_FILE, rand_suffix()));
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("create tmp {}", tmp.display()))?;
        f.write_all(&bytes)
            .with_context(|| format!("write tmp {}", tmp.display()))?;
        // fsync the temp file (Z8-D19b).
        f.sync_all()
            .with_context(|| format!("fsync tmp {}", tmp.display()))?;
    }

    fs::rename(&tmp, &target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;

    // fsync the parent directory (Z8-D19d) — best-effort: not all platforms
    // expose this, errors here are logged but not fatal.
    if let Ok(parent) = fs::File::open(&dir) {
        let _ = parent.sync_all();
    }

    Ok(())
}

/// Load the register from disk. Returns `Ok(None)` if no register exists
/// yet (truly new thread) and `Ok(Some(_))` otherwise.
///
/// Crash recovery (Z8-D19e): if both `decision_register.json` AND a
/// `decision_register.json.tmp.*` file exist, prefer the non-temp file.
/// Stale `.tmp.*` files older than 1 hour are swept (logged-not-fatal).
pub fn load(env: &ThreadIdEnv, tid: &ThreadId) -> Result<Option<DecisionRegister>> {
    let target = register_path(env, tid);

    // Sweep stale temp files before reading.
    sweep_stale_tmp(env.thread_dir(tid).as_path()).ok();

    if !target.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&target).with_context(|| format!("read {}", target.display()))?;
    let register: DecisionRegister =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", target.display()))?;
    if register.thread_id != *tid {
        anyhow::bail!(
            "register at {} stores thread_id={} but lookup was for {}",
            target.display(),
            register.thread_id,
            tid,
        );
    }
    if register.schema_version > SCHEMA_VERSION {
        anyhow::bail!(
            "register at {} has schema_version={} but this build supports up to {}",
            target.display(),
            register.schema_version,
            SCHEMA_VERSION
        );
    }
    Ok(Some(register))
}

fn sweep_stale_tmp(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let now = std::time::SystemTime::now();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if !name.starts_with(&format!("{}.tmp.", REGISTER_FILE)) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let age = now.duration_since(mtime).unwrap_or_default();
        if age.as_secs() > 3600 {
            let _ = fs::remove_file(&path);
        }
    }
    Ok(())
}

fn rand_suffix() -> u32 {
    Uuid::new_v4().as_u128() as u32
}

fn now_iso8601() -> String {
    let now: DateTime<Utc> = Utc::now();
    now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::AgentEvent;
    use tempfile::TempDir;

    fn pid() -> ProducerId {
        ProducerId::new("agent-runner-claude")
    }

    fn dval_str(s: &str) -> DecisionValue {
        DecisionValue::String(s.to_string())
    }

    fn id(s: &str) -> DecisionId {
        DecisionId::new(s).unwrap()
    }

    fn lock_event(id_s: &str, value: &str, src: DecisionSource) -> AgentEvent {
        AgentEvent::DecisionLocked {
            id: id(id_s),
            value: dval_str(value),
            source: src,
            derived_from: None,
            reason: None,
            request_id: None,
            producer_id: pid(),
        }
    }

    fn amend_event(id_s: &str, value: &str, prior: &str, reason: &str) -> AgentEvent {
        AgentEvent::DecisionAmended {
            id: id(id_s),
            value: dval_str(value),
            prior_value: dval_str(prior),
            source: DecisionSource::Agent,
            reason: reason.to_string(),
            request_id: None,
            producer_id: pid(),
        }
    }

    fn unlock_event(id_s: &str, prior: &str, reason: &str) -> AgentEvent {
        AgentEvent::DecisionUnlocked {
            id: id(id_s),
            prior_value: dval_str(prior),
            source: DecisionSource::Agent,
            reason: reason.to_string(),
            request_id: None,
            producer_id: pid(),
        }
    }

    // ── Reducer rule tests (§6.1) ──

    #[test]
    fn z8_d1_lock_creates_entry() {
        let mut r = DecisionRegister::new(ThreadId::new());
        let outcome = apply_event(
            &mut r,
            &lock_event("naming/framework", "Aletheia", DecisionSource::User),
            1,
        )
        .unwrap();
        assert_eq!(outcome, ApplyOutcome::Mutated);
        let e = &r.entries[&id("naming/framework")];
        assert_eq!(e.state, DecisionState::Locked);
        assert_eq!(e.current.as_ref().unwrap(), &dval_str("Aletheia"));
        assert_eq!(e.history.len(), 1);
        assert_eq!(e.history[0].op, DecisionOp::Lock);
        assert_eq!(r.last_event_seq, 1);
    }

    #[test]
    fn z8_d5_idempotent_relock_is_noop() {
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "v", DecisionSource::User), 1).unwrap();
        let outcome = apply_event(&mut r, &lock_event("a", "v", DecisionSource::Agent), 2).unwrap();
        assert_eq!(outcome, ApplyOutcome::NoOp);
        // No history append.
        assert_eq!(r.entries[&id("a")].history.len(), 1);
        // last_event_seq unchanged on no-op (caller holds responsibility for
        // event_seq monotonicity).
        assert_eq!(r.last_event_seq, 1);
    }

    #[test]
    fn z8_d6_implicit_amend_forbidden() {
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "v1", DecisionSource::User), 1).unwrap();
        let err = apply_event(&mut r, &lock_event("a", "v2", DecisionSource::User), 2)
            .expect_err("must be ImplicitAmendForbidden");
        assert_eq!(err.code, DecisionRegisterErrorCode::ImplicitAmendForbidden);
        // Register state unchanged.
        assert_eq!(
            r.entries[&id("a")].current.as_ref().unwrap(),
            &dval_str("v1")
        );
        assert_eq!(r.entries[&id("a")].history.len(), 1);
    }

    #[test]
    fn z8_d7_amend_unknown_is_error() {
        let mut r = DecisionRegister::new(ThreadId::new());
        let err = apply_event(&mut r, &amend_event("a", "v2", "v1", "x"), 1).unwrap_err();
        assert_eq!(err.code, DecisionRegisterErrorCode::AmendOfUnlocked);
    }

    #[test]
    fn z8_d9_amend_without_reason_is_error() {
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "v1", DecisionSource::User), 1).unwrap();
        let err = apply_event(&mut r, &amend_event("a", "v2", "v1", ""), 2).unwrap_err();
        assert_eq!(err.code, DecisionRegisterErrorCode::AmendWithoutReason);
    }

    #[test]
    fn z8_d9a_stale_amend_is_error() {
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "v1", DecisionSource::User), 1).unwrap();
        let err = apply_event(&mut r, &amend_event("a", "v3", "v2", "wrong prior"), 2).unwrap_err();
        assert_eq!(err.code, DecisionRegisterErrorCode::StaleAmend);
    }

    #[test]
    fn amend_appends_history_and_updates_current() {
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "v1", DecisionSource::User), 1).unwrap();
        apply_event(&mut r, &amend_event("a", "v2", "v1", "refined"), 2).unwrap();
        apply_event(&mut r, &amend_event("a", "v3", "v2", "refined again"), 3).unwrap();
        let e = &r.entries[&id("a")];
        assert_eq!(e.history.len(), 3);
        assert_eq!(e.current.as_ref().unwrap(), &dval_str("v3"));
        assert!(e.last_amended_at.is_some());
    }

    #[test]
    fn unlock_then_relock_restarts_locked_state() {
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "v", DecisionSource::User), 1).unwrap();
        apply_event(&mut r, &unlock_event("a", "v", "user retracted"), 2).unwrap();
        let e = &r.entries[&id("a")];
        assert_eq!(e.state, DecisionState::Unlocked);
        assert_eq!(e.current, None);
        // Re-lock with new value.
        apply_event(&mut r, &lock_event("a", "v_new", DecisionSource::Agent), 3).unwrap();
        let e = &r.entries[&id("a")];
        assert_eq!(e.state, DecisionState::Locked);
        assert_eq!(e.current.as_ref().unwrap(), &dval_str("v_new"));
        assert_eq!(e.history.len(), 3);
    }

    #[test]
    fn z8_d8_unlock_unknown_is_error() {
        let mut r = DecisionRegister::new(ThreadId::new());
        let err = apply_event(&mut r, &unlock_event("a", "v", "x"), 1).unwrap_err();
        assert_eq!(err.code, DecisionRegisterErrorCode::UnlockOfUnlocked);
    }

    #[test]
    fn z8_d17_invariant_unlocked_implies_no_current() {
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "v", DecisionSource::User), 1).unwrap();
        apply_event(&mut r, &unlock_event("a", "v", "x"), 2).unwrap();
        let e = &r.entries[&id("a")];
        assert_eq!(e.state, DecisionState::Unlocked);
        assert!(e.current.is_none());
    }

    #[test]
    fn z8_d38_agent_overrode_user_via_lock_relock() {
        // Spec §3.9.5: when User has locked a value AND the entry is still
        // Locked AND Agent attempts to re-lock with a different value, the
        // reducer rejects with AgentOverrodeUser (a more specific code than
        // ImplicitAmendForbidden, surfaced because the audit trail wants to
        // distinguish "agent stepped on user" from generic "implicit amend").
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "user_v", DecisionSource::User), 1).unwrap();
        let err = apply_event(
            &mut r,
            &lock_event("a", "agent_guess", DecisionSource::Agent),
            2,
        )
        .unwrap_err();
        assert_eq!(err.code, DecisionRegisterErrorCode::AgentOverrodeUser);
        // Register state unchanged.
        assert_eq!(
            r.entries[&id("a")].current.as_ref().unwrap(),
            &dval_str("user_v")
        );
    }

    #[test]
    fn agent_can_lock_after_unlock_chain() {
        // The legitimate chain: User locks; some actor unlocks (signaling
        // retraction); Agent re-locks with new value. Z8-D38 does NOT
        // fire here because the entry is Unlocked at the moment of the
        // Agent's lock.
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "user_v", DecisionSource::User), 1).unwrap();
        apply_event(&mut r, &unlock_event("a", "user_v", "user said rethink"), 2).unwrap();
        let outcome = apply_event(
            &mut r,
            &lock_event("a", "agent_v", DecisionSource::Agent),
            3,
        )
        .unwrap();
        assert_eq!(outcome, ApplyOutcome::Mutated);
        let e = &r.entries[&id("a")];
        assert_eq!(e.state, DecisionState::Locked);
        assert_eq!(e.current.as_ref().unwrap(), &dval_str("agent_v"));
        assert_eq!(e.history.len(), 3);
    }

    #[test]
    fn passthrough_for_non_decision_event() {
        let mut r = DecisionRegister::new(ThreadId::new());
        let e = AgentEvent::TextDelta {
            text: "hello".to_string(),
        };
        let outcome = apply_event(&mut r, &e, 1).unwrap();
        assert_eq!(outcome, ApplyOutcome::Passthrough);
        assert!(r.entries.is_empty());
    }

    #[test]
    fn z8_d47_lock_denied_records_audit_does_not_change_state() {
        let mut r = DecisionRegister::new(ThreadId::new());
        let e = AgentEvent::DecisionLockDenied {
            id: id("a"),
            attempted_value: dval_str("forbidden"),
            denied_by: "envelope/no-write-private".to_string(),
            reason: "private/** is sensitive".to_string(),
            source: DecisionSource::Agent,
            request_id: None,
        };
        let outcome = apply_event(&mut r, &e, 1).unwrap();
        assert_eq!(outcome, ApplyOutcome::AuditOnly);
        let entry = &r.entries[&id("a")];
        assert_eq!(entry.state, DecisionState::Unlocked);
        assert!(entry.current.is_none());
        assert_eq!(entry.history.len(), 1);
        assert_eq!(entry.history[0].op, DecisionOp::LockDenied);
    }

    // ── Persistence tests (§6.2) ──

    fn env_in(td: &TempDir) -> ThreadIdEnv {
        ThreadIdEnv::with_base(td.path().to_path_buf())
    }

    #[test]
    fn z8_d18_persists_full_history() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "v1", DecisionSource::User), 1).unwrap();
        apply_event(&mut r, &amend_event("a", "v2", "v1", "r1"), 2).unwrap();
        apply_event(&mut r, &amend_event("a", "v3", "v2", "r2"), 3).unwrap();
        persist(&env, &r).unwrap();
        let loaded = load(&env, &r.thread_id).unwrap().expect("must load");
        assert_eq!(loaded, r);
        assert_eq!(loaded.entries[&id("a")].history.len(), 3);
    }

    #[test]
    fn z8_d19a_atomic_rename_no_stale_temp() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let r = {
            let mut r = DecisionRegister::new(ThreadId::new());
            apply_event(&mut r, &lock_event("a", "v", DecisionSource::User), 1).unwrap();
            r
        };
        persist(&env, &r).unwrap();
        let dir = env.thread_dir(&r.thread_id);
        let entries: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        assert!(entries.contains(&REGISTER_FILE.to_string()));
        assert!(
            !entries
                .iter()
                .any(|n| n.starts_with(&format!("{}.tmp.", REGISTER_FILE))),
            "unexpected stale temp: {entries:?}"
        );
    }

    #[test]
    fn z8_d19e_load_prefers_target_when_temp_present() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "real", DecisionSource::User), 1).unwrap();
        persist(&env, &r).unwrap();

        // Now drop a bogus tmp file alongside the real one — load() must
        // ignore it. The crash-recovery rule says "prefer non-temp".
        let dir = env.thread_dir(&r.thread_id);
        let stale = dir.join(format!("{}.tmp.deadbeef", REGISTER_FILE));
        fs::write(&stale, b"{}").unwrap();
        let loaded = load(&env, &r.thread_id).unwrap().expect("must load");
        assert_eq!(
            loaded.entries[&id("a")].current.as_ref().unwrap(),
            &dval_str("real")
        );
    }

    #[test]
    fn load_returns_none_when_register_absent() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let tid = ThreadId::new();
        assert!(load(&env, &tid).unwrap().is_none());
    }

    #[test]
    fn load_rejects_thread_id_mismatch() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let tid_a = ThreadId::new();
        let tid_b = ThreadId::new();
        let mut r = DecisionRegister::new(tid_a.clone());
        apply_event(&mut r, &lock_event("x", "y", DecisionSource::User), 1).unwrap();
        persist(&env, &r).unwrap();
        // Move the file under tid_b's path, then try to load.
        fs::create_dir_all(env.thread_dir(&tid_b)).unwrap();
        fs::rename(register_path(&env, &tid_a), register_path(&env, &tid_b)).unwrap();
        let err = load(&env, &tid_b).unwrap_err();
        assert!(err.to_string().contains("thread_id"), "got: {err}");
    }

    #[test]
    fn load_rejects_future_schema_version() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let tid = ThreadId::new();
        fs::create_dir_all(env.thread_dir(&tid)).unwrap();
        let path = register_path(&env, &tid);
        let mut r = DecisionRegister::new(tid.clone());
        r.schema_version = SCHEMA_VERSION + 99;
        let bytes = serde_json::to_vec_pretty(&r).unwrap();
        fs::write(&path, bytes).unwrap();
        let err = load(&env, &tid).unwrap_err();
        assert!(err.to_string().contains("schema_version"), "got: {err}");
    }

    #[test]
    fn z8_d14_compaction_does_not_undo_register_state() {
        // The reducer is sparse (Z8-D13) — compaction events pass through.
        let mut r = DecisionRegister::new(ThreadId::new());
        apply_event(&mut r, &lock_event("a", "v", DecisionSource::User), 1).unwrap();
        let comp = AgentEvent::ContextCompacted {
            freed_tokens: 1000,
            before: 5000,
            after: 4000,
        };
        let outcome = apply_event(&mut r, &comp, 2).unwrap();
        assert_eq!(outcome, ApplyOutcome::Passthrough);
        // Register intact.
        let e = &r.entries[&id("a")];
        assert_eq!(e.state, DecisionState::Locked);
        assert_eq!(e.current.as_ref().unwrap(), &dval_str("v"));
    }
}
