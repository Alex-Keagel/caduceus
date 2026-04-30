//! Cmd handlers (or12 + or13 + or14 + or15 + or16 + or17 + or18 +
//! or19 + or20 + or21).
//!
//! Per the implementation DAG, each handler is a small pure-ish
//! function over `&mut OrchestratorState` that the dispatch loop
//! (or10) routes a `Cmd` variant into.  Splitting the loop body into
//! these functions keeps each handler unit-testable in isolation.
//!
//! Iter-28 backlog absorbed:
//!
//! - **#1-3** RetryToken EXACT equality check (or14).
//! - **#1-6** on_reattach MUST NOT mutate `attempt` or
//!   `disconnect_generation` (or16).
//! - **§3.5 / iter-28 #1-1** EngineDisconnected is daemon-observed
//!   subsystem (or19), NOT authenticated-engine (which lands at
//!   or16/or21 via Cmd::Reattach).

use crate::error::DaemonResult;
use crate::mailbox::{RetryToken, RunId, SessionId};
use crate::orchestrator_state::{OrchestratorState, RunAttempt, RunHistory};
use std::time::Instant;

/// or12 — `on_runner_exit`.  Spec #1 §3.5 cascade: append to history,
/// drop running entry + claim, decide retry (v1 v0: just record).
pub fn on_runner_exit(state: &mut OrchestratorState, run_id: RunId, exit_code: Option<i32>) {
    if let Some(run) = state.running.remove(&run_id) {
        state.recent_history_ring.push(RunHistory {
            run_id: run.id.clone(),
            final_attempt: run.attempt,
            completed_at: Instant::now(),
            exit_code,
        });
    }
    state.claimed.release(&run_id);
}

/// or13 — `on_token_update`.  V1: routed to the runner's accounting
/// via `forward_to_daemon` (P2); orchestrator state itself does not
/// duplicate per-run tokens (those live on the running entry's
/// associated RunnerProcess accounting).
///
/// This handler exists for API symmetry; in v1 it is a no-op because
/// the runner's `forward_to_daemon` already reconciled tokens before
/// the frame reached the orchestrator's queue.  Snapshot RPC (P4)
/// reads tokens directly from RunnerProcess accounting.
pub fn on_token_update(_state: &mut OrchestratorState, _run_id: &RunId) {
    // Intentional no-op (see module doc).
}

/// or14 — `on_retry_timer`.  Iter-28 #1-3: EXACT equality check on
/// `RetryToken`.  Stale tokens dropped silently.
pub fn on_retry_timer(
    state: &mut OrchestratorState,
    run_id: &RunId,
    token: RetryToken,
) -> RetryFireOutcome {
    let entry = match state.retry_attempts.get(run_id) {
        Some(e) => e.clone(),
        None => return RetryFireOutcome::NoEntry,
    };
    if entry.token != token {
        return RetryFireOutcome::StaleToken {
            expected: entry.token,
            observed: token,
        };
    }
    state.retry_attempts.remove(run_id);
    RetryFireOutcome::Fired {
        attempt: entry.attempt,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryFireOutcome {
    Fired {
        attempt: RunAttempt,
    },
    StaleToken {
        expected: RetryToken,
        observed: RetryToken,
    },
    NoEntry,
}

/// or15 — `on_disconnect_timer_expired`.  Spec #1 §8.7.  V1: bumps the
/// disconnect_generation on the run if still running (so any subsequent
/// reattach observes a higher generation and can reset disconnect
/// markers).
pub fn on_disconnect_timer_expired(state: &mut OrchestratorState, run_id: &RunId) {
    if let Some(run) = state.running.get_mut(run_id) {
        run.disconnect_generation = run.disconnect_generation.saturating_add(1);
        run.state_since = Instant::now();
    }
}

/// or16 — `on_reattach`.  Iter-28 #1-6: MUST NOT mutate `attempt` or
/// `disconnect_generation`.  Advances `runner_seq_high_water` only if
/// the incoming `runner_seq` is non-regressing.  Stale reattaches drop.
pub fn on_reattach(
    state: &mut OrchestratorState,
    run_id: &RunId,
    session_id: SessionId,
    runner_seq: u64,
) -> ReattachOutcome {
    let run = match state.running.get_mut(run_id) {
        Some(r) => r,
        None => return ReattachOutcome::NoRun,
    };
    if runner_seq < run.runner_seq_high_water {
        return ReattachOutcome::StaleSeq {
            high_water: run.runner_seq_high_water,
            observed: runner_seq,
        };
    }
    run.runner_seq_high_water = runner_seq;
    run.session_id = Some(session_id);
    // Iter-28 #1-6: attempt + disconnect_generation are NOT touched.
    ReattachOutcome::Reattached
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReattachOutcome {
    Reattached,
    StaleSeq { high_water: u64, observed: u64 },
    NoRun,
}

/// or17 — `on_workflow_reloaded`.  V1: stub — workflow loader (P6)
/// will hot-swap the in-memory workflow pointer; this handler is the
/// observation point for the orchestrator to notice that happened.
pub fn on_workflow_reloaded(_state: &mut OrchestratorState) {
    // V1 stub.
}

/// or18 — `on_shutdown`.  Sets `state.shutting_down = true` (executable,
/// not comment-only — iter-28 backlog absorbed).  Caller invokes
/// stop_cascade across all running runners; we just mark the flag.
pub fn on_shutdown(state: &mut OrchestratorState) {
    state.shutting_down = true;
}

/// or19 — `on_engine_disconnected`.  Daemon-observed event (iter-28
/// #1-1: producer class is subsystem, NOT authenticated-engine).
/// Bumps the disconnect_generation; downstream disconnect timer
/// scheduling is the dispatch loop's concern.
pub fn on_engine_disconnected(
    state: &mut OrchestratorState,
    run_id: &RunId,
    _session_id: &SessionId,
) {
    if let Some(run) = state.running.get_mut(run_id) {
        run.disconnect_generation = run.disconnect_generation.saturating_add(1);
    }
}

/// or20 — `on_snapshot_request`.  V1: returns Ok; full snapshot
/// projection lands in P4 (sn01-snapshot-projection).
pub fn on_snapshot_request(_state: &OrchestratorState) -> DaemonResult<()> {
    Ok(())
}

/// or21 — `Cmd::Reattach` router.  Trust-boundary gate is at the
/// mailbox type level (only `EngineSender` can construct
/// `Cmd::Reattach` per iter-28 #1-1); this handler routes to or16.
pub fn cmd_reattach(
    state: &mut OrchestratorState,
    run_id: RunId,
    session_id: SessionId,
    runner_seq: u64,
) -> ReattachOutcome {
    on_reattach(state, &run_id, session_id, runner_seq)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator_state::{ClaimEntry, RetryEntry, Run};

    fn rid(s: &str) -> RunId {
        RunId(s.to_string())
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

    #[test]
    fn on_runner_exit_appends_history_and_releases_claim() {
        let mut s = OrchestratorState::new(8);
        s.running.insert(rid("r1"), run("r1", 1));
        s.claimed
            .try_claim(
                rid("r1"),
                ClaimEntry {
                    claimed_at: Instant::now(),
                    attempt: RunAttempt(1),
                },
            )
            .unwrap();
        on_runner_exit(&mut s, rid("r1"), Some(0));
        assert!(!s.running.contains_key(&rid("r1")));
        assert!(!s.claimed.contains(&rid("r1")));
        assert_eq!(s.recent_history_ring.len(), 1);
    }

    #[test]
    fn on_retry_timer_exact_token_match_fires() {
        let mut s = OrchestratorState::new(8);
        let token = RetryToken(42);
        s.retry_attempts.insert(
            rid("r1"),
            RetryEntry {
                run_id: rid("r1"),
                token,
                deadline: Instant::now(),
                attempt: RunAttempt(2),
            },
        );
        match on_retry_timer(&mut s, &rid("r1"), token) {
            RetryFireOutcome::Fired { attempt } => assert_eq!(attempt, RunAttempt(2)),
            other => panic!("expected Fired, got {other:?}"),
        }
        assert!(!s.retry_attempts.contains_key(&rid("r1")));
    }

    #[test]
    fn on_retry_timer_stale_token_drops_silently() {
        let mut s = OrchestratorState::new(8);
        s.retry_attempts.insert(
            rid("r1"),
            RetryEntry {
                run_id: rid("r1"),
                token: RetryToken(7),
                deadline: Instant::now(),
                attempt: RunAttempt(1),
            },
        );
        match on_retry_timer(&mut s, &rid("r1"), RetryToken(3)) {
            RetryFireOutcome::StaleToken { expected, observed } => {
                assert_eq!(expected, RetryToken(7));
                assert_eq!(observed, RetryToken(3));
            }
            other => panic!("expected StaleToken, got {other:?}"),
        }
        // Entry MUST remain.
        assert!(s.retry_attempts.contains_key(&rid("r1")));
    }

    #[test]
    fn on_retry_timer_no_entry() {
        let mut s = OrchestratorState::new(8);
        let r = on_retry_timer(&mut s, &rid("r1"), RetryToken(1));
        assert_eq!(r, RetryFireOutcome::NoEntry);
    }

    #[test]
    fn on_reattach_advances_high_water_and_sets_session() {
        let mut s = OrchestratorState::new(8);
        s.running.insert(rid("r1"), run("r1", 1));
        let r = on_reattach(&mut s, &rid("r1"), SessionId("sess1".into()), 5);
        assert_eq!(r, ReattachOutcome::Reattached);
        let run = s.running.get(&rid("r1")).unwrap();
        assert_eq!(run.runner_seq_high_water, 5);
        assert_eq!(run.session_id.as_ref().unwrap().0, "sess1");
        // Iter-28 #1-6: attempt + disconnect_generation untouched.
        assert_eq!(run.attempt, RunAttempt(1));
        assert_eq!(run.disconnect_generation, 0);
    }

    #[test]
    fn on_reattach_drops_stale_seq() {
        let mut s = OrchestratorState::new(8);
        let mut r = run("r1", 1);
        r.runner_seq_high_water = 10;
        s.running.insert(rid("r1"), r);
        let result = on_reattach(&mut s, &rid("r1"), SessionId("sess".into()), 7);
        match result {
            ReattachOutcome::StaleSeq {
                high_water,
                observed,
            } => {
                assert_eq!(high_water, 10);
                assert_eq!(observed, 7);
            }
            other => panic!("expected StaleSeq, got {other:?}"),
        }
    }

    #[test]
    fn on_reattach_no_run() {
        let mut s = OrchestratorState::new(8);
        let r = on_reattach(&mut s, &rid("nope"), SessionId("x".into()), 1);
        assert_eq!(r, ReattachOutcome::NoRun);
    }

    #[test]
    fn on_disconnect_timer_expired_bumps_generation() {
        let mut s = OrchestratorState::new(8);
        s.running.insert(rid("r1"), run("r1", 1));
        on_disconnect_timer_expired(&mut s, &rid("r1"));
        assert_eq!(s.running.get(&rid("r1")).unwrap().disconnect_generation, 1);
        on_disconnect_timer_expired(&mut s, &rid("r1"));
        assert_eq!(s.running.get(&rid("r1")).unwrap().disconnect_generation, 2);
    }

    #[test]
    fn on_engine_disconnected_bumps_generation() {
        let mut s = OrchestratorState::new(8);
        s.running.insert(rid("r1"), run("r1", 1));
        on_engine_disconnected(&mut s, &rid("r1"), &SessionId("s".into()));
        assert_eq!(s.running.get(&rid("r1")).unwrap().disconnect_generation, 1);
    }

    #[test]
    fn on_shutdown_sets_flag() {
        let mut s = OrchestratorState::new(8);
        assert!(!s.shutting_down);
        on_shutdown(&mut s);
        assert!(s.shutting_down);
    }

    #[test]
    fn on_snapshot_request_returns_ok() {
        let s = OrchestratorState::new(8);
        assert!(on_snapshot_request(&s).is_ok());
    }
}
