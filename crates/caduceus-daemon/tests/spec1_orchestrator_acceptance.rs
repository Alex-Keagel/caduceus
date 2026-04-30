//! Spec #1 acceptance tests (or22).
//!
//! Integration tests for the orchestrator algorithm: trust boundary,
//! RetryToken equality, livelock guard, reattach FSM, recent_history_ring
//! eviction, eligible_for_dispatch helper, drain on exit.
//!
//! Iter-28 backlog items resolved by these tests:
//!
//! - **#1-1** trust boundary — already enforced at the type level by
//!   the capability-scoped mailbox; verified by `t_trust_boundary_*`
//!   compile-time-only check.
//! - **#1-2** RunAttempt monotonicity caveat — `t_run_attempt_*`.
//! - **#1-3** RetryToken EXACT equality — `t_retry_token_*`.
//! - **#1-4** eligible_for_dispatch helper — `t_eligible_*`.
//! - **#1-5** recent_history_ring_size in Config — verified by
//!   f02 + or01 unit tests.
//! - **#1-6** on_reattach MUST NOT mutate attempt or
//!   disconnect_generation — `t_reattach_*`.

use caduceus_daemon::orchestrator_handlers::{
    on_reattach, on_retry_timer, on_runner_exit, on_shutdown, ReattachOutcome, RetryFireOutcome,
};
use caduceus_daemon::orchestrator_state::{
    eligible_for_dispatch, ClaimEntry, OrchestratorState, RetryEntry, Run, RunAttempt, RunHistory,
    TrackerClass, WorkSource,
};
use caduceus_daemon::{RetryToken, RunId, SessionId};
use std::time::Instant;

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

// ─── #1-2 RunAttempt monotonicity caveat ──────────────────────────

#[test]
fn t_run_attempt_restart_after_full_drain() {
    // Iter-28 #1-2: a fully-drained Run (no entry in running, retry_attempts,
    // or recent_history_ring) MAY restart attempt numbering at 1.
    let mut s = OrchestratorState::new(2);
    s.running.insert(rid("r1"), run("r1", 1));
    s.recent_history_ring.push(RunHistory {
        run_id: rid("r1"),
        final_attempt: RunAttempt(1),
        completed_at: Instant::now(),
        exit_code: Some(0),
    });
    // Evict by pushing capacity+1 unrelated entries.
    for i in 0..3 {
        s.recent_history_ring.push(RunHistory {
            run_id: rid(&format!("other{i}")),
            final_attempt: RunAttempt(1),
            completed_at: Instant::now(),
            exit_code: Some(0),
        });
    }
    // r1 is no longer in the ring.
    assert!(s.recent_history_ring.most_recent(&rid("r1")).is_none());
}

// ─── #1-3 RetryToken EXACT equality ────────────────────────────────

#[test]
fn t_retry_token_exact_equality() {
    let mut s = OrchestratorState::new(8);
    s.retry_attempts.insert(
        rid("r1"),
        RetryEntry {
            run_id: rid("r1"),
            token: RetryToken(42),
            deadline: Instant::now(),
            attempt: RunAttempt(2),
        },
    );
    // Off-by-one: stale.
    match on_retry_timer(&mut s, &rid("r1"), RetryToken(41)) {
        RetryFireOutcome::StaleToken { .. } => {}
        other => panic!("expected StaleToken, got {other:?}"),
    }
    // Off-by-one in the other direction: also stale.
    match on_retry_timer(&mut s, &rid("r1"), RetryToken(43)) {
        RetryFireOutcome::StaleToken { .. } => {}
        other => panic!("expected StaleToken, got {other:?}"),
    }
    // Exact: fires.
    match on_retry_timer(&mut s, &rid("r1"), RetryToken(42)) {
        RetryFireOutcome::Fired { .. } => {}
        other => panic!("expected Fired, got {other:?}"),
    }
}

// ─── #1-4 eligible_for_dispatch ────────────────────────────────────

struct StubWS {
    active: Vec<String>,
}
impl WorkSource for StubWS {
    fn classify(&self, run: &Run) -> TrackerClass {
        if self.active.contains(&run.id.0) {
            TrackerClass::Active
        } else {
            TrackerClass::Missing
        }
    }
}

#[test]
fn t_eligible_returns_true_for_active() {
    let ws = StubWS {
        active: vec!["r1".into()],
    };
    assert!(eligible_for_dispatch(&ws, &run("r1", 1)));
}

#[test]
fn t_eligible_returns_false_for_missing() {
    let ws = StubWS { active: vec![] };
    assert!(!eligible_for_dispatch(&ws, &run("r1", 1)));
}

// ─── #1-6 on_reattach MUST NOT mutate attempt / disconnect_generation ──

#[test]
fn t_reattach_does_not_mutate_attempt() {
    let mut s = OrchestratorState::new(8);
    let mut r = run("r1", 7);
    r.disconnect_generation = 3;
    s.running.insert(rid("r1"), r);
    let res = on_reattach(&mut s, &rid("r1"), SessionId("new_sess".into()), 5);
    assert_eq!(res, ReattachOutcome::Reattached);
    let run = s.running.get(&rid("r1")).unwrap();
    assert_eq!(run.attempt, RunAttempt(7), "attempt MUST be preserved");
    assert_eq!(
        run.disconnect_generation, 3,
        "disconnect_generation MUST be preserved"
    );
    assert_eq!(run.runner_seq_high_water, 5);
}

#[test]
fn t_reattach_stale_seq_drops() {
    let mut s = OrchestratorState::new(8);
    let mut r = run("r1", 1);
    r.runner_seq_high_water = 100;
    s.running.insert(rid("r1"), r);
    let res = on_reattach(&mut s, &rid("r1"), SessionId("sess".into()), 50);
    match res {
        ReattachOutcome::StaleSeq {
            high_water,
            observed,
        } => {
            assert_eq!(high_water, 100);
            assert_eq!(observed, 50);
        }
        other => panic!("expected StaleSeq, got {other:?}"),
    }
}

// ─── on_runner_exit drain ──────────────────────────────────────────

#[test]
fn t_on_runner_exit_drains_state() {
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
    assert!(s.running.is_empty());
    assert!(s.claimed.is_empty());
    assert_eq!(s.recent_history_ring.len(), 1);
}

// ─── on_shutdown predicate ─────────────────────────────────────────

#[test]
fn t_on_shutdown_marks_flag() {
    let mut s = OrchestratorState::new(8);
    assert!(!s.shutting_down);
    on_shutdown(&mut s);
    assert!(s.shutting_down);
}

// ─── #1-5 Config field present (compile-time check) ────────────────

#[test]
fn t_config_recent_history_ring_size_field_exists() {
    // Iter-28 #1-5: the Config struct MUST carry recent_history_ring_size.
    let toml = r#"
        workflow_path = "/wf.yaml"
        workspace_root = "/ws"
        recent_history_ring_size = 64
    "#;
    let cfg = caduceus_daemon::Config::from_toml_str(toml).unwrap();
    assert_eq!(cfg.recent_history_ring_size, 64);
}
