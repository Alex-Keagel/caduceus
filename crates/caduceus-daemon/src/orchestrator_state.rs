//! Orchestrator state types + helper functions
//! (or01 + or02 + or03 + or04 + or05 + or06 + or07 + or08 + or09).
//!
//! Per the implementation DAG, this module ships the data types and
//! pure-function helpers that spec #1 §3 / §4 require.  The
//! `OrchestratorState` aggregates them and is the single mutable
//! locus the dispatch loop drives.
//!
//! Spec cross-references:
//!
//! - **§3 / §4** — state types: `Run`, `RunAttempt`, `RetryEntry`,
//!   `RunHistory`, `OrchestratorState`.
//! - **§3.5 / iter-28 #1-3** — `RetryToken` is per-daemon-process
//!   monotonic; on_retry_timer requires exact equality.
//! - **§4 ring invariant + iter-28 #1-5** — `recent_history_ring_size`
//!   in `Config` (already in f02-config-loader); ring invariant: bounded.
//! - **§3.2 step 4 / iter-28 #1-4** — `eligible_for_dispatch` is a pure
//!   defensive pre-filter; the authoritative gate is `revalidate` at
//!   spawn time.
//! - **§3.5 / iter-28 #1-2** — RunAttempt monotonicity caveat: numbering
//!   is monotonic ONLY while represented in active state or retained
//!   history.  Documented on the type.
//! - **§0 / iter-28 #1-1** — trust boundary enforcement is already at
//!   the type level via `crate::mailbox` capability-scoped senders.

use crate::mailbox::{RetryToken as MailboxRetryToken, RunId};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

// ───────────────────────── or01 — Run state types ─────────────────────

/// Run identifier (re-exported from mailbox so consumers don't need to
/// know about the mailbox layer).
pub type RunIdentity = RunId;

/// A single dispatch attempt of a Run.  Iter-28 #1-2: attempt numbering
/// is monotonic only while the Run is represented in active state or
/// retained history.  A fully-drained Run whose prior summaries have
/// been evicted from the bounded ring MAY restart at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunAttempt(pub u32);

/// A Run currently in active state (running, retrying, or disconnected).
#[derive(Debug, Clone)]
pub struct Run {
    pub id: RunId,
    pub attempt: RunAttempt,
    pub session_id: Option<crate::mailbox::SessionId>,
    pub runner_seq_high_water: u64,
    /// Monotonic instant the run entered its current state.
    pub state_since: Instant,
    /// Disconnect generation counter; bumped on each disconnect cycle.
    /// Iter-28 #1-6: on_reattach MUST NOT mutate this.
    pub disconnect_generation: u64,
}

/// A retry entry waiting for its `RetryToken` to fire.  Iter-28 #1-3:
/// `token` is EXACT-equality-checked at on_retry_timer; mismatched
/// tokens drop the message as stale.
#[derive(Debug, Clone)]
pub struct RetryEntry {
    pub run_id: RunId,
    pub token: MailboxRetryToken,
    pub deadline: Instant,
    pub attempt: RunAttempt,
}

/// A retained history record for a completed Run.  Stored in
/// `recent_history_ring` with bounded eviction.
#[derive(Debug, Clone)]
pub struct RunHistory {
    pub run_id: RunId,
    pub final_attempt: RunAttempt,
    pub completed_at: Instant,
    pub exit_code: Option<i32>,
}

// ───────────────────────── or04 — RetryToken counter ──────────────────

/// Per-daemon-process monotonic counter (iter-28 #1-3).  on_retry_timer
/// requires EXACT equality with the current entry's token; mismatched
/// values are dropped as stale.
#[derive(Debug, Default, Clone)]
pub struct RetryTokenIssuer {
    inner: Arc<AtomicU64>,
}

impl RetryTokenIssuer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue the next token.  1-indexed; first token is 1.  May restart
    /// at 0 on daemon restart (in-flight retry timers do not survive
    /// process restart per iter-28 #1-3).
    pub fn issue(&self) -> MailboxRetryToken {
        let v = self.inner.fetch_add(1, Ordering::AcqRel) + 1;
        MailboxRetryToken(v)
    }
}

// ───────────────────────── or05 — recent_history_ring ─────────────────

/// Bounded ring buffer of completed Run summaries.  Spec #4 §4 + spec #1
/// §4 ring invariant (size MUST be >= 1).  Eviction is FIFO.
///
/// Iter-28 #1-2 absorbed via type-level documentation: once a Run is
/// evicted from this ring AND drained from `running` / `retry_attempts`,
/// its `RunAttempt` numbering MAY restart at 1.
#[derive(Debug)]
pub struct RecentHistoryRing {
    capacity: usize,
    inner: VecDeque<RunHistory>,
}

impl RecentHistoryRing {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 1, "recent_history_ring_size MUST be >= 1");
        Self {
            capacity,
            inner: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, entry: RunHistory) {
        if self.inner.len() >= self.capacity {
            self.inner.pop_front();
        }
        self.inner.push_back(entry);
    }

    pub fn iter(&self) -> impl Iterator<Item = &RunHistory> {
        self.inner.iter()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Look up the most recent history entry for a Run.  Used by
    /// on_runner_exit + on_retry_timer.
    pub fn most_recent(&self, run_id: &RunId) -> Option<&RunHistory> {
        self.inner.iter().rev().find(|r| r.run_id == *run_id)
    }
}

// ───────────────────────── or06 — claimed_map + or07 ──────────────────

/// `claimed_map` tracks Runs that have passed `revalidate` but not yet
/// reached `dispatch_succeeded`.  Bounded by `Config::max_concurrency`.
#[derive(Debug, Default)]
pub struct ClaimedMap {
    inner: HashMap<RunId, ClaimEntry>,
}

#[derive(Debug, Clone, Copy)]
pub struct ClaimEntry {
    pub claimed_at: Instant,
    pub attempt: RunAttempt,
}

impl ClaimedMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn contains(&self, run_id: &RunId) -> bool {
        self.inner.contains_key(run_id)
    }

    /// Insert a claim.  Returns `Err(existing)` if the run is already
    /// claimed (race detection).
    pub fn try_claim(&mut self, run_id: RunId, entry: ClaimEntry) -> Result<(), ClaimEntry> {
        match self.inner.entry(run_id) {
            std::collections::hash_map::Entry::Occupied(o) => Err(*o.get()),
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(entry);
                Ok(())
            }
        }
    }

    pub fn release(&mut self, run_id: &RunId) -> Option<ClaimEntry> {
        self.inner.remove(run_id)
    }

    /// Concurrency gate.  Returns true if a new claim would exceed
    /// `max_concurrency`.
    pub fn would_exceed(&self, max: usize) -> bool {
        self.inner.len() >= max
    }
}

/// `dispatch_defer_attempts` — Z-9 livelock guard counter.  Per Run.
/// Reset on success; incremented on each Deferred outcome.  When it
/// reaches `max_dispatch_defer_attempts`, on_retry_timer abandons the
/// run and emits the livelock-guard diagnostic.
#[derive(Debug, Default)]
pub struct DispatchDeferAttempts {
    inner: HashMap<RunId, u32>,
}

impl DispatchDeferAttempts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the counter for `run_id`.  Returns the new value.
    pub fn incr(&mut self, run_id: &RunId) -> u32 {
        let v = self.inner.entry(run_id.clone()).or_insert(0);
        *v += 1;
        *v
    }

    /// Reset on successful dispatch.
    pub fn reset(&mut self, run_id: &RunId) {
        self.inner.remove(run_id);
    }

    pub fn get(&self, run_id: &RunId) -> u32 {
        self.inner.get(run_id).copied().unwrap_or(0)
    }

    /// Z-9 livelock-guard predicate: did this run reach the abandonment
    /// threshold?
    pub fn at_or_over(&self, run_id: &RunId, threshold: u32) -> bool {
        self.get(run_id) >= threshold
    }
}

// ───────────────────────── or08 — revalidate ──────────────────────────

/// Outcome of `revalidate`.  Spec #1 §3.3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevalidateOutcome {
    /// Run is still active in WorkSource; spawn proceeds.
    Active,
    /// Run was skipped or otherwise ineligible at revalidate time;
    /// dispatch SHOULD NOT spawn.
    Skipped,
    /// Workspace is unavailable (e.g., shared-repo lock contention);
    /// surface as `DispatchResult::Deferred`.  Iter-28 #1-4.
    WorkspaceUnavailable,
    /// Spawn cannot proceed because of a non-recoverable error;
    /// dispatch surface as `DispatchResult::Failed`.
    SpawnFailed(String),
}

/// Pluggable WorkSource classifier.  Real impl lives in P6 workflow
/// integration; here we expose the trait so `dispatch_run` is testable
/// without the workflow loaded.
pub trait WorkSource: Send + Sync {
    fn classify(&self, run: &Run) -> TrackerClass;
}

/// Spec #1 glossary `TrackerClass` — coarse classification of a Run's
/// status in the upstream WorkSource (e.g., issue tracker).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerClass {
    /// Run is currently the active focus of work.
    Active,
    /// Run was active but was Skipped / closed / paused upstream.
    Inactive,
    /// Run not present in upstream tracker at all (e.g., deleted).
    Missing,
}

/// Spawn-time gate.  Spec #1 §3.3 — authoritative; `eligible_for_dispatch`
/// (or09) is only a defensive pre-filter.
pub fn revalidate(work_source: &dyn WorkSource, run: &Run) -> RevalidateOutcome {
    match work_source.classify(run) {
        TrackerClass::Active => RevalidateOutcome::Active,
        TrackerClass::Inactive | TrackerClass::Missing => RevalidateOutcome::Skipped,
    }
}

// ───────────────────────── or09 — eligible_for_dispatch ───────────────

/// Pure defensive pre-filter for §3.2 step 4.  MUST NOT mutate state.
/// The authoritative gate is `dispatch_run` revalidate (or08).
/// Iter-28 #1-4 absorbed.
pub fn eligible_for_dispatch(work_source: &dyn WorkSource, run: &Run) -> bool {
    matches!(work_source.classify(run), TrackerClass::Active)
}

// ───────────────────────── or03 — Trust boundary doc ──────────────────

/// Trust-boundary documentation marker.  The actual enforcement is at
/// the type level via `crate::mailbox::*Sender` capability-scoped
/// newtypes (iter-28 #1-1).  This struct exists so callers can express
/// "I have proven the message I'm about to dispatch came from an
/// allowed producer class" via a phantom-typed token, but for v1 we
/// rely on the type system.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrustBoundaryGate;

// ───────────────────────── OrchestratorState aggregate ────────────────

/// The single mutable state struct the dispatch loop owns.  All
/// handlers (or10..or21) take `&mut OrchestratorState`.
#[derive(Debug)]
pub struct OrchestratorState {
    pub running: HashMap<RunId, Run>,
    pub retry_attempts: HashMap<RunId, RetryEntry>,
    pub claimed: ClaimedMap,
    pub dispatch_defer_attempts: DispatchDeferAttempts,
    pub recent_history_ring: RecentHistoryRing,
    pub retry_tokens: RetryTokenIssuer,
    pub shutting_down: bool,
}

impl OrchestratorState {
    pub fn new(recent_history_ring_size: usize) -> Self {
        Self {
            running: HashMap::new(),
            retry_attempts: HashMap::new(),
            claimed: ClaimedMap::new(),
            dispatch_defer_attempts: DispatchDeferAttempts::new(),
            recent_history_ring: RecentHistoryRing::new(recent_history_ring_size),
            retry_tokens: RetryTokenIssuer::new(),
            shutting_down: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::RunId as MbRunId;
    use std::time::Instant;

    fn rid(s: &str) -> RunId {
        MbRunId(s.to_string())
    }

    fn run(id: &str, attempt: u32) -> Run {
        Run {
            id: rid(id),
            attempt: RunAttempt(attempt),
            session_id: None,
            runner_seq_high_water: 0,
            state_since: Instant::now(),
            disconnect_generation: 0,
        }
    }

    // ─── or04 RetryTokenIssuer ──────────────────────────────────────

    #[test]
    fn retry_token_issuer_starts_at_one_and_increments() {
        let issuer = RetryTokenIssuer::new();
        assert_eq!(issuer.issue().0, 1);
        assert_eq!(issuer.issue().0, 2);
        assert_eq!(issuer.issue().0, 3);
    }

    // ─── or05 RecentHistoryRing ─────────────────────────────────────

    #[test]
    fn ring_evicts_when_capacity_reached() {
        let mut ring = RecentHistoryRing::new(3);
        for i in 0..5 {
            ring.push(RunHistory {
                run_id: rid(&format!("r{i}")),
                final_attempt: RunAttempt(1),
                completed_at: Instant::now(),
                exit_code: Some(0),
            });
        }
        assert_eq!(ring.len(), 3);
        let ids: Vec<&str> = ring.iter().map(|h| h.run_id.0.as_str()).collect();
        assert_eq!(ids, ["r2", "r3", "r4"]);
    }

    #[test]
    fn ring_most_recent_returns_latest_for_run() {
        let mut ring = RecentHistoryRing::new(8);
        ring.push(RunHistory {
            run_id: rid("r1"),
            final_attempt: RunAttempt(1),
            completed_at: Instant::now(),
            exit_code: Some(1),
        });
        ring.push(RunHistory {
            run_id: rid("r1"),
            final_attempt: RunAttempt(2),
            completed_at: Instant::now(),
            exit_code: Some(0),
        });
        let h = ring.most_recent(&rid("r1")).unwrap();
        assert_eq!(h.final_attempt, RunAttempt(2));
        assert_eq!(h.exit_code, Some(0));
    }

    #[test]
    #[should_panic(expected = "recent_history_ring_size MUST be >= 1")]
    fn ring_zero_capacity_panics() {
        let _ = RecentHistoryRing::new(0);
    }

    // ─── or06 ClaimedMap ────────────────────────────────────────────

    #[test]
    fn claimed_map_try_claim_rejects_duplicate() {
        let mut m = ClaimedMap::new();
        let entry = ClaimEntry {
            claimed_at: Instant::now(),
            attempt: RunAttempt(1),
        };
        assert!(m.try_claim(rid("r1"), entry).is_ok());
        assert!(m.try_claim(rid("r1"), entry).is_err());
    }

    #[test]
    fn claimed_map_release_removes() {
        let mut m = ClaimedMap::new();
        let entry = ClaimEntry {
            claimed_at: Instant::now(),
            attempt: RunAttempt(1),
        };
        m.try_claim(rid("r1"), entry).unwrap();
        assert_eq!(m.release(&rid("r1")).unwrap().attempt, RunAttempt(1));
        assert!(!m.contains(&rid("r1")));
    }

    #[test]
    fn claimed_map_concurrency_gate() {
        let mut m = ClaimedMap::new();
        for i in 0..3 {
            let _ = m.try_claim(
                rid(&format!("r{i}")),
                ClaimEntry {
                    claimed_at: Instant::now(),
                    attempt: RunAttempt(1),
                },
            );
        }
        assert!(!m.would_exceed(4));
        assert!(m.would_exceed(3));
    }

    // ─── or07 DispatchDeferAttempts (Z-9 livelock guard) ────────────

    #[test]
    fn dispatch_defer_increment_and_reset() {
        let mut d = DispatchDeferAttempts::new();
        assert_eq!(d.get(&rid("r1")), 0);
        assert_eq!(d.incr(&rid("r1")), 1);
        assert_eq!(d.incr(&rid("r1")), 2);
        d.reset(&rid("r1"));
        assert_eq!(d.get(&rid("r1")), 0);
    }

    #[test]
    fn dispatch_defer_at_or_over_threshold() {
        let mut d = DispatchDeferAttempts::new();
        for _ in 0..8 {
            d.incr(&rid("r1"));
        }
        assert!(d.at_or_over(&rid("r1"), 8));
        assert!(!d.at_or_over(&rid("r2"), 1));
    }

    // ─── or08 + or09 revalidate / eligible_for_dispatch ─────────────

    struct StubWorkSource {
        active: Vec<String>,
    }
    impl WorkSource for StubWorkSource {
        fn classify(&self, run: &Run) -> TrackerClass {
            if self.active.contains(&run.id.0) {
                TrackerClass::Active
            } else {
                TrackerClass::Missing
            }
        }
    }

    #[test]
    fn eligible_for_dispatch_true_when_active() {
        let ws = StubWorkSource {
            active: vec!["r1".into()],
        };
        assert!(eligible_for_dispatch(&ws, &run("r1", 1)));
        assert!(!eligible_for_dispatch(&ws, &run("r2", 1)));
    }

    #[test]
    fn revalidate_active_returns_active() {
        let ws = StubWorkSource {
            active: vec!["r1".into()],
        };
        assert_eq!(revalidate(&ws, &run("r1", 1)), RevalidateOutcome::Active);
    }

    #[test]
    fn revalidate_missing_returns_skipped() {
        let ws = StubWorkSource { active: vec![] };
        assert_eq!(revalidate(&ws, &run("r1", 1)), RevalidateOutcome::Skipped);
    }

    // ─── OrchestratorState aggregate ────────────────────────────────

    #[test]
    fn orchestrator_state_initializes() {
        let s = OrchestratorState::new(32);
        assert!(s.running.is_empty());
        assert!(s.retry_attempts.is_empty());
        assert!(s.claimed.is_empty());
        assert_eq!(s.recent_history_ring.len(), 0);
        assert!(!s.shutting_down);
    }
}
