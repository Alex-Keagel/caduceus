//! `forward_to_daemon` + token reconciliation + heartbeat tracker
//! (ru08 + ru10 + ru11 + ru12 + ru13).
//!
//! Per the implementation DAG, this module wires the runner side's
//! parsed `Frame` into the daemon's mailbox and tracks per-Run
//! accounting state.  Three concerns share this module because they
//! all read/write the same `RunAccounting` struct:
//!
//! - **`ru08`**: `forward_to_daemon` is the canonical Z-23 stamp call
//!   site.  It validates + wraps + enqueues a `Frame` and stamps
//!   `runner_seq` only on `Ok`.  Iter-28 #2-7 absorbed.
//! - **`ru10`**: token reconciliation in **delta** mode.
//! - **`ru11`**: token reconciliation in **absolute** mode.  Iter-28
//!   #2-1: `turn_end.tokens_at_turn_end` and `exit.final_tokens` ALSO
//!   reconcile in absolute mode (was previously only `token_update`).
//! - **`ru12`**: heartbeat emit cadence (runner-side; we model the
//!   timer here so tests can drive it).
//! - **`ru13`**: heartbeat-receipt timeout tracker.  Iter-28 #2-3:
//!   if no heartbeat is observed within `heartbeat_timeout_ms`,
//!   trigger `stop_cascade(reason = "heartbeat_timeout")`.
//!
//! Spec cross-references:
//!
//! - **`spec-caduceus-agent-runner-contract.md` §3.2** — forward path.
//! - **`spec-caduceus-agent-runner-contract.md` §4.1** — heartbeat
//!   cadence + timeout policy.
//! - **`spec-caduceus-agent-runner-contract.md` iter-28 #2-1** —
//!   absolute-mode reconcile on turn_end + exit.

use crate::clock::Clock;
use crate::inbound_queue::{InboundQueue, RunnerSeqCounter};
use crate::runner_process::{RunnerProcess, StopReason};
use crate::wire_codec::{DropReason, Frame, FramePayload, TokenMode, TokensAbsolute};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Per-Run token totals.  Reconciled by the absolute/delta logic.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RunTokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

/// Per-Run accounting state mutated by reconciliation.
#[derive(Debug, Default)]
pub struct RunAccounting {
    pub tokens: Mutex<RunTokens>,
    /// Set by `Exit` frames; the cascade reaper consumes this.
    pub advertised_exit: Mutex<Option<TokensAbsolute>>,
}

/// Reconcile a `token_update` frame in delta mode.  Iter-28 #2-1
/// scope: also called by absolute-mode wrapping below for consistency.
pub async fn reconcile_delta(
    accounting: &RunAccounting,
    input_tokens: u64,
    output_tokens: u64,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
) {
    let mut g = accounting.tokens.lock().await;
    g.input_tokens = g.input_tokens.saturating_add(input_tokens);
    g.output_tokens = g.output_tokens.saturating_add(output_tokens);
    if let Some(c) = cache_read {
        g.cache_read_tokens = g.cache_read_tokens.saturating_add(c);
    }
    if let Some(c) = cache_write {
        g.cache_write_tokens = g.cache_write_tokens.saturating_add(c);
    }
}

/// Reconcile in absolute mode.  Spec #2 §4.1 + iter-28 #2-1: absolute
/// MUST win over delta.  This function unconditionally **replaces**
/// the totals (it does not max them).  Callers MUST only invoke this
/// for token_update.absolute, turn_end.tokens_at_turn_end, and
/// exit.final_tokens.
pub async fn reconcile_absolute(accounting: &RunAccounting, abs: TokensAbsolute) {
    let mut g = accounting.tokens.lock().await;
    g.input_tokens = abs.input_tokens;
    g.output_tokens = abs.output_tokens;
    if let Some(c) = abs.cache_read_tokens {
        g.cache_read_tokens = c;
    }
    if let Some(c) = abs.cache_write_tokens {
        g.cache_write_tokens = c;
    }
}

/// Stamped frame: a `Frame` with its post-Ok runner_seq attached.
/// Carried over the daemon-side queue for downstream consumers
/// (snapshot RPC, dispatch loop).
#[derive(Debug, Clone)]
pub struct StampedFrame {
    pub frame: Frame,
    pub runner_seq: u64,
}

/// **Z-23 STAMP RULE** — single authoritative call site.
///
/// `forward_to_daemon` validates, enqueues, and stamps a frame in one
/// atomic step.  Iter-28 #2-1 absorbed: dispatch token reconciliation
/// before stamping for `turn_end` and `exit` so the snapshot delta
/// emission (downstream) reads consistent values.
///
/// Returns the stamped runner_seq on Ok.  On `Dropped`, no runner_seq
/// is consumed; the `frame_id` carried inside the dropped frame is
/// the diagnostic correlation id.
pub async fn forward_to_daemon(
    queue: &InboundQueue,
    counter: &RunnerSeqCounter,
    accounting: &RunAccounting,
    frame: Frame,
) -> Result<u64, DropReason> {
    // Iter-28 #2-1: reconcile tokens BEFORE stamping/forwarding so
    // downstream readers observe consistent state when they receive
    // the stamped frame.
    match &frame.payload {
        FramePayload::TokenUpdate {
            mode,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
        } => match mode {
            TokenMode::Delta => {
                reconcile_delta(
                    accounting,
                    *input_tokens,
                    *output_tokens,
                    *cache_read_tokens,
                    *cache_write_tokens,
                )
                .await;
            }
            TokenMode::Absolute => {
                reconcile_absolute(
                    accounting,
                    TokensAbsolute {
                        input_tokens: *input_tokens,
                        output_tokens: *output_tokens,
                        cache_read_tokens: *cache_read_tokens,
                        cache_write_tokens: *cache_write_tokens,
                    },
                )
                .await;
            }
        },
        FramePayload::TurnEnd { tokens_at_turn_end } => {
            reconcile_absolute(accounting, *tokens_at_turn_end).await;
        }
        FramePayload::Exit { final_tokens, .. } => {
            reconcile_absolute(accounting, *final_tokens).await;
            let mut g = accounting.advertised_exit.lock().await;
            *g = Some(*final_tokens);
        }
        _ => {}
    }

    // Enqueue the frame.  On Dropped, do NOT stamp.
    queue.try_enqueue(frame)?;
    let stamped = counter.stamp_after_ok();
    Ok(stamped)
}

/// Heartbeat emit timer.  Spec #2 §4.1 cadence — once every
/// `interval`.  Returns a `JoinHandle` so callers can stop the timer
/// at runner shutdown.
pub fn spawn_heartbeat_emit(
    interval: Duration,
    on_tick: impl Fn() + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut t = tokio::time::interval(interval);
        // First tick fires immediately by default; we want a steady
        // cadence starting `interval` after spawn.
        t.tick().await;
        loop {
            t.tick().await;
            on_tick();
        }
    })
}

/// Heartbeat timeout tracker.  Spec #2 §4.1 + iter-28 #2-3.
///
/// Polls `runner.last_heartbeat_at()` against `clock.now_monotonic()`
/// at `poll_interval` cadence.  If the gap exceeds `timeout`, invokes
/// `stop_cascade(reason = HeartbeatTimeout)`.
///
/// Returns a `JoinHandle`; the caller owns it for the runner's lifetime.
pub fn spawn_heartbeat_timeout_tracker(
    runner: Arc<RunnerProcess>,
    clock: Arc<dyn Clock>,
    timeout: Duration,
    poll_interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(poll_interval).await;
            let last = runner.last_heartbeat_at().await;
            let now = clock.now_monotonic();
            let breached = match last {
                Some(t) => now.saturating_duration_since(t) > timeout,
                None => false,
            };
            if breached {
                let _ = runner.stop_cascade(StopReason::HeartbeatTimeout).await;
                break;
            }
        }
    })
}

/// Convenience wrapper: take an `Instant` to use as the heartbeat
/// observation timestamp.  Used when integration code observes a
/// heartbeat frame and wants to update the runner's tracker without
/// directly importing Instant.
pub async fn observe_heartbeat(runner: &RunnerProcess, at: Instant) {
    runner.record_heartbeat(at).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound_queue::inbound_queue;
    use crate::wire_codec::{ExitKind, FrameId, FramePayload, TokenMode};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn token_frame(seq: u64, mode: TokenMode, input: u64, output: u64) -> Frame {
        Frame {
            seq,
            frame_id: FrameId(seq),
            payload: FramePayload::TokenUpdate {
                mode,
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        }
    }

    fn turn_end(seq: u64, input: u64, output: u64) -> Frame {
        Frame {
            seq,
            frame_id: FrameId(seq),
            payload: FramePayload::TurnEnd {
                tokens_at_turn_end: TokensAbsolute {
                    input_tokens: input,
                    output_tokens: output,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                },
            },
        }
    }

    fn exit_frame(seq: u64, input: u64, output: u64) -> Frame {
        Frame {
            seq,
            frame_id: FrameId(seq),
            payload: FramePayload::Exit {
                exit_kind: ExitKind::Completed,
                final_tokens: TokensAbsolute {
                    input_tokens: input,
                    output_tokens: output,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                },
            },
        }
    }

    #[tokio::test]
    async fn forward_stamps_only_on_ok() {
        let (q, _rx) = inbound_queue(8);
        let counter = RunnerSeqCounter::new();
        let acc = RunAccounting::default();
        let r = forward_to_daemon(&q, &counter, &acc, token_frame(1, TokenMode::Delta, 5, 3)).await;
        assert_eq!(r.unwrap(), 1);
        assert_eq!(counter.high_water(), 1);
    }

    #[tokio::test]
    async fn forward_does_not_stamp_on_drop() {
        let (q, _rx) = inbound_queue(1);
        let counter = RunnerSeqCounter::new();
        let acc = RunAccounting::default();
        // Fill queue so next try_enqueue returns Full.
        let _ = q.try_enqueue(token_frame(1, TokenMode::Delta, 1, 1));
        let r = forward_to_daemon(&q, &counter, &acc, token_frame(2, TokenMode::Delta, 1, 1)).await;
        assert!(matches!(r, Err(DropReason::QueueFull)));
        // Z-23: dropped frame MUST NOT consume a runner_seq.
        assert_eq!(counter.high_water(), 0);
    }

    #[tokio::test]
    async fn delta_mode_accumulates() {
        let (q, _rx) = inbound_queue(8);
        let counter = RunnerSeqCounter::new();
        let acc = RunAccounting::default();
        forward_to_daemon(&q, &counter, &acc, token_frame(1, TokenMode::Delta, 10, 5))
            .await
            .unwrap();
        forward_to_daemon(&q, &counter, &acc, token_frame(2, TokenMode::Delta, 7, 3))
            .await
            .unwrap();
        let g = acc.tokens.lock().await;
        assert_eq!(g.input_tokens, 17);
        assert_eq!(g.output_tokens, 8);
    }

    #[tokio::test]
    async fn absolute_mode_replaces_delta_state() {
        let (q, _rx) = inbound_queue(8);
        let counter = RunnerSeqCounter::new();
        let acc = RunAccounting::default();
        forward_to_daemon(
            &q,
            &counter,
            &acc,
            token_frame(1, TokenMode::Delta, 100, 50),
        )
        .await
        .unwrap();
        forward_to_daemon(
            &q,
            &counter,
            &acc,
            token_frame(2, TokenMode::Absolute, 1000, 500),
        )
        .await
        .unwrap();
        let g = acc.tokens.lock().await;
        // Absolute REPLACES (not adds to) delta totals.
        assert_eq!(g.input_tokens, 1000);
        assert_eq!(g.output_tokens, 500);
    }

    #[tokio::test]
    async fn iter28_2_1_turn_end_reconciles_absolute() {
        // Iter-28 #2-1: turn_end.tokens_at_turn_end MUST be reconciled
        // in absolute mode.  Was previously only token_update.
        let (q, _rx) = inbound_queue(8);
        let counter = RunnerSeqCounter::new();
        let acc = RunAccounting::default();
        forward_to_daemon(&q, &counter, &acc, token_frame(1, TokenMode::Delta, 50, 25))
            .await
            .unwrap();
        forward_to_daemon(&q, &counter, &acc, turn_end(2, 999, 444))
            .await
            .unwrap();
        let g = acc.tokens.lock().await;
        assert_eq!(g.input_tokens, 999);
        assert_eq!(g.output_tokens, 444);
    }

    #[tokio::test]
    async fn iter28_2_1_exit_reconciles_absolute_and_records_advertised_exit() {
        // Iter-28 #2-1: exit.final_tokens MUST be reconciled in
        // absolute mode AND recorded as advertised_exit so the cascade
        // reaper can avoid racing with the runner's own exit path.
        let (q, _rx) = inbound_queue(8);
        let counter = RunnerSeqCounter::new();
        let acc = RunAccounting::default();
        forward_to_daemon(&q, &counter, &acc, exit_frame(1, 2000, 1000))
            .await
            .unwrap();
        let g = acc.tokens.lock().await;
        assert_eq!(g.input_tokens, 2000);
        let exit = acc.advertised_exit.lock().await;
        assert_eq!(
            *exit,
            Some(TokensAbsolute {
                input_tokens: 2000,
                output_tokens: 1000,
                cache_read_tokens: None,
                cache_write_tokens: None,
            })
        );
    }

    #[tokio::test]
    async fn heartbeat_emit_fires_on_cadence() {
        let count = Arc::new(AtomicU64::new(0));
        let c = Arc::clone(&count);
        let handle = spawn_heartbeat_emit(Duration::from_millis(20), move || {
            c.fetch_add(1, Ordering::Relaxed);
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.abort();
        let n = count.load(Ordering::Relaxed);
        // Expect ~3-5 ticks in 100ms with 20ms cadence.
        assert!(n >= 2, "expected at least 2 heartbeats, got {n}");
    }
}
