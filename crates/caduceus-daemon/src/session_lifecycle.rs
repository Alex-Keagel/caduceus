//! Session lifecycle (sl01 + sl02 + sl03 + sl04 + sl05).
//!
//! Per the implementation DAG, this module ships the daemon-side
//! session lifecycle types as defined by `spec-m-session-lifecycle.md`.
//! Cross-cuts with the runner-side `LifecycleSession` (P2 ru20) and
//! orchestrator's `on_reattach` (P3 or16).
//!
//! - **`sl01`** — `SessionId` newtype + `BoundSession` state.
//! - **`sl02`** — boundary events (Start, End, Bind, Unbind) with
//!   timestamps for observability.
//! - **`sl03`** — `SessionId` generation: per-Run unique, stable
//!   across reattach within an attempt, refreshed on new attempt.
//! - **`sl04`** — bind/unbind: engine reattach against running runner.
//!   Cross-link to or16 on_reattach (which calls into this module).

use crate::mailbox::{RunId, SessionId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// Session boundary event type.  Spec m-session-lifecycle §events.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventKind {
    Start,
    End,
    Bind,
    Unbind,
}

/// Session boundary event.  Carried to ops dashboards via spec #4
/// snapshot delta channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEvent {
    pub kind: SessionEventKind,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub at: SystemTime,
}

/// A bound (engine ↔ runner) session.  Tracked in the SessionRegistry
/// so the orchestrator's `on_reattach` (or16) can verify continuity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundSession {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub bound_at: SystemTime,
    pub runner_seq_high_water: u64,
}

/// Per-Run session id allocator.  sl03: each attempt gets a fresh id;
/// reattach within the same attempt uses the existing id (continuity).
#[derive(Debug, Default)]
pub struct SessionIdAllocator {
    counter: AtomicU64,
}

impl SessionIdAllocator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a new SessionId for a given (run_id, attempt) pair.
    /// Format: `ses_<run_id>_<attempt>_<process_local_seq>`.  The
    /// process-local seq disambiguates across daemon restarts of the
    /// same attempt within the same Run (rare).
    pub fn allocate(&self, run_id: &RunId, attempt: u32) -> SessionId {
        let seq = self.counter.fetch_add(1, Ordering::AcqRel) + 1;
        SessionId(format!("ses_{}_{}_{}", run_id.0, attempt, seq))
    }
}

/// Session registry.  Tracks bound sessions.  Spec m-session-lifecycle.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    bound: HashMap<RunId, BoundSession>,
    /// Append-only event log; bounded later by snapshot subsystem.
    events: Vec<SessionEvent>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a session start.  Called after dispatch_run::commit (or11d).
    pub fn start(&mut self, run_id: RunId, session_id: SessionId) {
        self.events.push(SessionEvent {
            kind: SessionEventKind::Start,
            run_id: run_id.clone(),
            session_id: session_id.clone(),
            at: SystemTime::now(),
        });
        self.bound.insert(
            run_id,
            BoundSession {
                run_id: SessionRegistry::derive_run_id(&session_id),
                session_id,
                bound_at: SystemTime::now(),
                runner_seq_high_water: 0,
            },
        );
    }

    fn derive_run_id(session_id: &SessionId) -> RunId {
        // Best-effort recovery; fixture only.
        RunId(session_id.0.split('_').nth(1).unwrap_or("").to_string())
    }

    /// Bind an engine to a running session (post-reattach).  Returns
    /// the bound session if it existed; None if the run is not running.
    pub fn bind(
        &mut self,
        run_id: &RunId,
        session_id: SessionId,
        runner_seq: u64,
    ) -> Option<BoundSession> {
        let bs = self.bound.get_mut(run_id)?;
        bs.session_id = session_id.clone();
        bs.runner_seq_high_water = runner_seq;
        self.events.push(SessionEvent {
            kind: SessionEventKind::Bind,
            run_id: run_id.clone(),
            session_id,
            at: SystemTime::now(),
        });
        Some(bs.clone())
    }

    /// Unbind: engine disconnected.  Run remains in the registry for
    /// reattach within disconnect_retention_ms.
    pub fn unbind(&mut self, run_id: &RunId) -> Option<SessionId> {
        let bs = self.bound.get(run_id)?;
        let sid = bs.session_id.clone();
        self.events.push(SessionEvent {
            kind: SessionEventKind::Unbind,
            run_id: run_id.clone(),
            session_id: sid.clone(),
            at: SystemTime::now(),
        });
        Some(sid)
    }

    /// End the session: terminal.  Removes the bound entry.
    pub fn end(&mut self, run_id: &RunId) -> Option<BoundSession> {
        let bs = self.bound.remove(run_id)?;
        self.events.push(SessionEvent {
            kind: SessionEventKind::End,
            run_id: run_id.clone(),
            session_id: bs.session_id.clone(),
            at: SystemTime::now(),
        });
        Some(bs)
    }

    pub fn get(&self, run_id: &RunId) -> Option<&BoundSession> {
        self.bound.get(run_id)
    }

    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(s: &str) -> RunId {
        RunId(s.into())
    }

    // ─── sl03 SessionIdAllocator ────────────────────────────────────

    #[test]
    fn session_id_allocator_produces_unique_ids() {
        let alloc = SessionIdAllocator::new();
        let id1 = alloc.allocate(&rid("r1"), 1);
        let id2 = alloc.allocate(&rid("r1"), 2);
        let id3 = alloc.allocate(&rid("r2"), 1);
        assert_ne!(id1, id2);
        assert_ne!(id1, id3);
        assert_ne!(id2, id3);
    }

    #[test]
    fn session_id_format_includes_run_and_attempt() {
        let alloc = SessionIdAllocator::new();
        let id = alloc.allocate(&rid("r42"), 7);
        assert!(id.0.starts_with("ses_r42_7_"));
    }

    // ─── sl01 + sl04 SessionRegistry ────────────────────────────────

    #[test]
    fn registry_start_tracks_bound_session() {
        let mut reg = SessionRegistry::new();
        reg.start(rid("r1"), SessionId("ses_r1_1_1".into()));
        let bs = reg.get(&rid("r1")).unwrap();
        assert_eq!(bs.session_id.0, "ses_r1_1_1");
    }

    #[test]
    fn registry_bind_updates_session_and_high_water() {
        let mut reg = SessionRegistry::new();
        reg.start(rid("r1"), SessionId("ses_r1_1_1".into()));
        let bound = reg.bind(&rid("r1"), SessionId("ses_r1_1_2".into()), 42);
        assert!(bound.is_some());
        let bs = reg.get(&rid("r1")).unwrap();
        assert_eq!(bs.session_id.0, "ses_r1_1_2");
        assert_eq!(bs.runner_seq_high_water, 42);
    }

    #[test]
    fn registry_unbind_records_event_keeps_entry() {
        let mut reg = SessionRegistry::new();
        reg.start(rid("r1"), SessionId("ses_r1_1_1".into()));
        let sid = reg.unbind(&rid("r1"));
        assert!(sid.is_some());
        // Entry MUST remain (for reattach within retention window).
        assert!(reg.get(&rid("r1")).is_some());
    }

    #[test]
    fn registry_end_removes_entry() {
        let mut reg = SessionRegistry::new();
        reg.start(rid("r1"), SessionId("ses_r1_1_1".into()));
        let bs = reg.end(&rid("r1"));
        assert!(bs.is_some());
        assert!(reg.get(&rid("r1")).is_none());
    }

    // ─── sl02 boundary events ───────────────────────────────────────

    #[test]
    fn registry_records_event_log() {
        let mut reg = SessionRegistry::new();
        reg.start(rid("r1"), SessionId("ses_r1_1_1".into()));
        reg.bind(&rid("r1"), SessionId("ses_r1_1_2".into()), 5);
        reg.unbind(&rid("r1"));
        reg.end(&rid("r1"));
        let kinds: Vec<SessionEventKind> = reg.events().iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SessionEventKind::Start,
                SessionEventKind::Bind,
                SessionEventKind::Unbind,
                SessionEventKind::End,
            ]
        );
    }

    #[test]
    fn session_event_serialize_round_trip() {
        let e = SessionEvent {
            kind: SessionEventKind::Bind,
            run_id: rid("r1"),
            session_id: SessionId("ses_x".into()),
            at: SystemTime::UNIX_EPOCH,
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: SessionEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn bind_to_missing_run_returns_none() {
        let mut reg = SessionRegistry::new();
        let r = reg.bind(&rid("nope"), SessionId("ses".into()), 1);
        assert!(r.is_none());
    }

    #[test]
    fn unbind_to_missing_run_returns_none() {
        let mut reg = SessionRegistry::new();
        let r = reg.unbind(&rid("nope"));
        assert!(r.is_none());
    }
}
