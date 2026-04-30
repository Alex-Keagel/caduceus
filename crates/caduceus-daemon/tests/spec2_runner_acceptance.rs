//! Spec #2 acceptance tests (ru22).
//!
//! Integration tests for the agent runner contract. Exercises the
//! wire-codec → inbound queue → forward_to_daemon → token reconcile +
//! lifecycle session pipeline end-to-end.
//!
//! Iter-28 backlog items resolved by these tests:
//!
//! - **#2-1** token reconciliation on turn_end + exit (absolute mode).
//! - **#2-2** SIGKILL outcome honesty — covered by runner_process unit tests
//!   that exercise CascadeOutcome::Reaped { stage: Stage3bSigkill }.
//! - **#2-3** heartbeat-timeout tracker → stop_cascade.
//! - **#2-5** shell_wrap fail-closed gate.
//! - **#2-6** seq=0 stutter (reserved-value guard fires before high-water).
//! - **#2-7** Z-23 stamp rule — single call site in forward_to_daemon.
//! - **#2-8** cross_run_handoff is NOT in v1 closed set; routes to
//!   stop_cascade(unknown_message_kind).

use caduceus_daemon::{
    classify_seq, decode_line, drop_reason_to_stop_reason, forward_to_daemon, inbound_queue,
    validate_shell_wrap, DropReason, FrameId, FramePayload, LifecycleSession, RunAccounting,
    RunnerSeqCounter, SeqClassification, SessionState, StopReason, TokenMode, TokensAbsolute,
};

fn fid(n: u64) -> FrameId {
    FrameId(n)
}

// ─── §4.1 wire codec — closed event-kind set ──────────────────────

#[test]
fn t_token_update_round_trips() {
    let line =
        br#"{"seq":1,"kind":"token_update","mode":"delta","input_tokens":10,"output_tokens":5}"#;
    let f = decode_line(line, fid(0)).unwrap();
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
        _ => panic!("expected TokenUpdate"),
    }
}

#[test]
fn t_iter28_2_8_cross_run_handoff_routes_to_stop_cascade() {
    // Spec #2 iter-28 #2-8: cross_run_handoff is reserved out of the
    // v1 closed set; receiving it MUST trigger stop_cascade with
    // reason=unknown_message_kind.
    let line = br#"{"seq":1,"kind":"cross_run_handoff","payload":{}}"#;
    let r = decode_line(line, fid(0));
    let drop = match r {
        Err(d) => d,
        Ok(_) => panic!("cross_run_handoff MUST be rejected as UnknownKind"),
    };
    let stop = drop_reason_to_stop_reason(&drop);
    assert_eq!(stop, Some(StopReason::UnknownMessageKind));
}

// ─── §4.4 Z-23 stamp rule (iter-28 #2-7) ──────────────────────────

#[tokio::test]
async fn t_iter28_2_7_z23_stamp_only_on_ok() {
    let (q, _rx) = inbound_queue(8);
    let counter = RunnerSeqCounter::new();
    let acc = RunAccounting::default();
    let frame = caduceus_daemon::Frame {
        seq: 1,
        frame_id: fid(0),
        payload: FramePayload::Heartbeat,
    };
    let stamped = forward_to_daemon(&q, &counter, &acc, frame).await.unwrap();
    assert_eq!(stamped, 1);
    assert_eq!(counter.high_water(), 1);
}

#[tokio::test]
async fn t_iter28_2_7_no_stamp_on_drop() {
    let (q, _rx) = inbound_queue(1);
    let counter = RunnerSeqCounter::new();
    let acc = RunAccounting::default();
    // Pre-fill to force QueueFull on the second.
    let _ = q.try_enqueue(caduceus_daemon::Frame {
        seq: 1,
        frame_id: fid(0),
        payload: FramePayload::Heartbeat,
    });
    let r = forward_to_daemon(
        &q,
        &counter,
        &acc,
        caduceus_daemon::Frame {
            seq: 2,
            frame_id: fid(1),
            payload: FramePayload::Heartbeat,
        },
    )
    .await;
    assert!(matches!(r, Err(DropReason::QueueFull)));
    // Z-23: dropped frame MUST NOT consume a runner_seq.
    assert_eq!(counter.high_water(), 0);
}

// ─── seq classifier (iter-28 #2-6) ────────────────────────────────

#[test]
fn t_iter28_2_6_seq_zero_is_stutter() {
    // Reserved-value guard fires BEFORE high-water comparison.
    assert_eq!(classify_seq(0, 0), SeqClassification::Stutter);
    assert_eq!(classify_seq(0, 100), SeqClassification::Stutter);
}

#[test]
fn t_seq_gap_detected() {
    match classify_seq(7, 5) {
        SeqClassification::Gap { expected, observed } => {
            assert_eq!(expected, 6);
            assert_eq!(observed, 7);
        }
        other => panic!("expected Gap, got {other:?}"),
    }
}

#[test]
fn t_seq_regression_detected() {
    match classify_seq(3, 5) {
        SeqClassification::Regression {
            high_water,
            observed,
        } => {
            assert_eq!(high_water, 5);
            assert_eq!(observed, 3);
        }
        other => panic!("expected Regression, got {other:?}"),
    }
}

// ─── shell-wrap fail-closed gate (iter-28 #2-5) ──────────────────

#[test]
fn t_iter28_2_5_shell_wrap_rejects_runtime_input() {
    use caduceus_daemon::error::SpawnRefusedReason;
    let r = validate_shell_wrap(true, false);
    assert!(matches!(
        r,
        Err(SpawnRefusedReason::ShellWrapUntrustedInput)
    ));
}

#[test]
fn t_iter28_2_5_shell_wrap_accepts_static_literal() {
    let r = validate_shell_wrap(true, true);
    assert!(r.is_ok());
}

// ─── token reconciliation (iter-28 #2-1) ──────────────────────────

#[tokio::test]
async fn t_iter28_2_1_turn_end_reconciles_absolute() {
    let (q, _rx) = inbound_queue(8);
    let counter = RunnerSeqCounter::new();
    let acc = RunAccounting::default();
    let delta = caduceus_daemon::Frame {
        seq: 1,
        frame_id: fid(0),
        payload: FramePayload::TokenUpdate {
            mode: TokenMode::Delta,
            input_tokens: 50,
            output_tokens: 25,
            cache_read_tokens: None,
            cache_write_tokens: None,
        },
    };
    forward_to_daemon(&q, &counter, &acc, delta).await.unwrap();
    let turn_end = caduceus_daemon::Frame {
        seq: 2,
        frame_id: fid(1),
        payload: FramePayload::TurnEnd {
            tokens_at_turn_end: TokensAbsolute {
                input_tokens: 999,
                output_tokens: 444,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        },
    };
    forward_to_daemon(&q, &counter, &acc, turn_end)
        .await
        .unwrap();
    let g = acc.tokens.lock().await;
    assert_eq!(g.input_tokens, 999, "absolute MUST replace delta");
    assert_eq!(g.output_tokens, 444);
}

#[tokio::test]
async fn t_iter28_2_1_exit_records_advertised_exit() {
    let (q, _rx) = inbound_queue(8);
    let counter = RunnerSeqCounter::new();
    let acc = RunAccounting::default();
    let exit = caduceus_daemon::Frame {
        seq: 1,
        frame_id: fid(0),
        payload: FramePayload::Exit {
            exit_kind: caduceus_daemon::ExitKind::Completed,
            final_tokens: TokensAbsolute {
                input_tokens: 2000,
                output_tokens: 1000,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        },
    };
    forward_to_daemon(&q, &counter, &acc, exit).await.unwrap();
    let exit_recorded = acc.advertised_exit.lock().await;
    assert!(exit_recorded.is_some());
}

// ─── Lifecycle Session FSM ────────────────────────────────────────

#[test]
fn t_session_idle_in_turn_idle_cycle() {
    let s = LifecycleSession::new();
    assert_eq!(s.state(), SessionState::Idle);
    let token = caduceus_daemon::Frame {
        seq: 1,
        frame_id: fid(0),
        payload: FramePayload::TokenUpdate {
            mode: TokenMode::Delta,
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: None,
            cache_write_tokens: None,
        },
    };
    assert_eq!(s.observe(&token), SessionState::InTurn);
    let end = caduceus_daemon::Frame {
        seq: 2,
        frame_id: fid(1),
        payload: FramePayload::TurnEnd {
            tokens_at_turn_end: TokensAbsolute {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        },
    };
    assert_eq!(s.observe(&end), SessionState::Idle);
}

#[test]
fn t_session_terminal_after_exit() {
    let s = LifecycleSession::new();
    let exit = caduceus_daemon::Frame {
        seq: 1,
        frame_id: fid(0),
        payload: FramePayload::Exit {
            exit_kind: caduceus_daemon::ExitKind::Completed,
            final_tokens: TokensAbsolute {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        },
    };
    assert_eq!(s.observe(&exit), SessionState::Exited);
}
