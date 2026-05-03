//! Bounded inbound frame queue + Z-23 runner_seq stamping
//! (ru06 + ru07 + ru09).
//!
//! Per the implementation DAG, this module ships:
//!
//! - **`ru06`**: bounded MPSC queue of `Frame` arrivals; backpressure
//!   surfaces as `DropReason::QueueFull`.
//! - **`ru07`**: Z-23 stamp rule.  `runner_seq` is stamped **only**
//!   after `forward_to_daemon` returns `Ok`.  No pre-delivery, no
//!   read-time, no daemon-side lazy stamping.  On `Dropped`, no
//!   `runner_seq` is consumed; `frame_id` is the diagnostic id.
//!   ALL OTHER SITES MUST cite `§4.4 Z-23 stamp rule` and call
//!   [`RunnerSeqCounter::stamp_after_ok`] — they MUST NOT restate
//!   the rule or stamp at any other point.  Iter-28 #2-7 absorbed.
//! - **`ru09`**: seq-regression classifier.  Iter-28 #2-6 absorbed:
//!   `seq == 0` reserved-value guard fires BEFORE the high-water
//!   comparison; classified as `Stutter` and dropped (caller does
//!   NOT `stop_cascade`).
//!
//! These three concerns are tightly coupled so they live in one
//! module.

use crate::wire_codec::{DropReason, Frame, FrameId};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Per-Run runner_seq counter.  Owned by the runner-side shell (one
/// per Run); cheap to clone for use inside the post-Ok stamp call site.
///
/// The counter is **per-Run**, not per-process — different Runs MUST
/// have independent counters.  Spec #2 §4.4 invariant.
#[derive(Debug, Default, Clone)]
pub struct RunnerSeqCounter {
    inner: Arc<AtomicU64>,
}

impl RunnerSeqCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the current high-water mark without consuming.  Used by the
    /// classifier for regression checks.
    pub fn high_water(&self) -> u64 {
        self.inner.load(Ordering::Acquire)
    }

    /// **Z-23 STAMP RULE** — single authoritative call site.
    ///
    /// Increment the per-Run runner_seq AFTER `forward_to_daemon`
    /// returns `Ok`.  Returns the new value (i.e., the value the
    /// frame is stamped with).  On `Dropped`, this MUST NOT be called.
    ///
    /// All other call sites in the codebase MUST cite "§4.4 Z-23 stamp
    /// rule" and invoke this method; stamping at any other point is
    /// FORBIDDEN.
    pub fn stamp_after_ok(&self) -> u64 {
        // fetch_add returns previous value; we add 1 first then return
        // post-add, matching "1-indexed; first stamp is 1".
        self.inner.fetch_add(1, Ordering::AcqRel) + 1
    }
}

/// Classification of a frame's `seq` field relative to the prior
/// observed runner_seq high-water.
///
/// Spec #2 §4.4 + iter-28 #2-6.  Note that this classifier looks at
/// the RAW seq the runner emitted; the Z-23 stamp is a separate
/// concern (post-Ok).  `Stutter` exists because zero is reserved as
/// a sentinel; `seq=0` MUST be dropped without forwarding and MUST NOT
/// trigger `stop_cascade`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqClassification {
    /// Strictly increasing by 1 from the prior high-water.  Forward.
    Ok,
    /// `seq == 0`: reserved-value guard.  Iter-28 #2-6 — DROP, do NOT
    /// `stop_cascade`.  Diagnostic key: `kind_detail = "stutter"`.
    Stutter,
    /// `seq` skipped values (e.g., high-water=5, frame seq=7).  Per
    /// spec #2 §3.2 this triggers `stop_cascade(reason="runner_seq_gap")`.
    Gap { expected: u64, observed: u64 },
    /// `seq` regressed (e.g., high-water=5, frame seq=4).  Triggers
    /// `stop_cascade(reason="runner_seq_regression")`.
    Regression { high_water: u64, observed: u64 },
}

/// Classify a single observed frame's seq against a high-water counter.
///
/// IMPORTANT: this function does NOT mutate the counter.  Mutation
/// happens only at the Z-23 post-Ok stamp call site.  This separation
/// is critical: a `Gap`/`Regression` MUST NOT advance the counter.
pub fn classify_seq(frame_seq: u64, high_water: u64) -> SeqClassification {
    // Iter-28 #2-6: explicit seq==0 guard fires BEFORE the high-water
    // comparison.
    if frame_seq == 0 {
        return SeqClassification::Stutter;
    }
    let expected = high_water + 1;
    if frame_seq == expected {
        SeqClassification::Ok
    } else if frame_seq > expected {
        SeqClassification::Gap {
            expected,
            observed: frame_seq,
        }
    } else {
        SeqClassification::Regression {
            high_water,
            observed: frame_seq,
        }
    }
}

/// Bounded inbound queue.  Producers enqueue parsed `Frame`s; the
/// dispatch loop on the consumer side drains via [`InboundReceiver::recv`].
///
/// Spec #2 §3.5.1: capacity is fixed; a full queue surfaces
/// `DropReason::QueueFull` at the producer side.
#[derive(Debug)]
pub struct InboundQueue {
    sender: tokio::sync::mpsc::Sender<Frame>,
}

impl Clone for InboundQueue {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

/// Single-consumer receiver.  Held by `forward_to_daemon` (when it
/// lands as ru08); for the v1 foundation we expose it directly so the
/// classifier and tests can drive the pipeline.
#[derive(Debug)]
pub struct InboundReceiver {
    inner: tokio::sync::mpsc::Receiver<Frame>,
}

impl InboundReceiver {
    pub async fn recv(&mut self) -> Option<Frame> {
        self.inner.recv().await
    }
}

/// Build a bounded inbound queue with the given capacity.  Spec #2
/// §3.5.1 recommended capacity: 1024.
pub fn inbound_queue(capacity: usize) -> (InboundQueue, InboundReceiver) {
    assert!(capacity >= 1, "inbound queue capacity MUST be >= 1");
    let (tx, rx) = tokio::sync::mpsc::channel(capacity);
    (InboundQueue { sender: tx }, InboundReceiver { inner: rx })
}

impl InboundQueue {
    /// Try to enqueue a frame without blocking.  Returns
    /// `DropReason::QueueFull` if the buffer is full.  Does NOT stamp
    /// runner_seq (Z-23: stamp is post-Ok at forward_to_daemon).
    pub fn try_enqueue(&self, frame: Frame) -> Result<(), DropReason> {
        self.sender.try_send(frame).map_err(|e| match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => DropReason::QueueFull,
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                DropReason::ProtocolViolation("inbound queue closed; daemon shutting down".into())
            }
        })
    }

    /// Enqueue a frame, awaiting capacity.  For producers that can
    /// afford to wait (most frames in the steady state).
    pub async fn enqueue(&self, frame: Frame) -> Result<(), DropReason> {
        self.sender
            .send(frame)
            .await
            .map_err(|_| DropReason::ProtocolViolation("inbound queue closed".into()))
    }
}

/// `frame_id` allocator.  Per-Run, independent of runner_seq (Z-23).
/// Mutated on every parse, regardless of whether the frame is dropped.
#[derive(Debug, Default, Clone)]
pub struct FrameIdAllocator {
    inner: Arc<AtomicU64>,
}

impl FrameIdAllocator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the next frame_id.  1-indexed; first id is 1.  Used as
    /// diagnostic correlation in drop logs.
    pub fn next(&self) -> FrameId {
        FrameId(self.inner.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire_codec::{FramePayload, TokenMode};

    fn frame(seq: u64) -> Frame {
        Frame {
            seq,
            frame_id: FrameId(0),
            payload: FramePayload::Heartbeat,
        }
    }

    fn token_frame(seq: u64) -> Frame {
        Frame {
            seq,
            frame_id: FrameId(0),
            payload: FramePayload::TokenUpdate {
                mode: TokenMode::Delta,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        }
    }

    // ─── runner_seq stamp ─────────────────────────────────────────

    #[test]
    fn z23_stamp_starts_at_one_and_increments_strictly() {
        let counter = RunnerSeqCounter::new();
        assert_eq!(counter.high_water(), 0);
        assert_eq!(counter.stamp_after_ok(), 1);
        assert_eq!(counter.stamp_after_ok(), 2);
        assert_eq!(counter.stamp_after_ok(), 3);
        assert_eq!(counter.high_water(), 3);
    }

    #[test]
    fn z23_stamp_is_per_counter_independent() {
        let c1 = RunnerSeqCounter::new();
        let c2 = RunnerSeqCounter::new();
        c1.stamp_after_ok();
        c1.stamp_after_ok();
        assert_eq!(c1.high_water(), 2);
        assert_eq!(c2.high_water(), 0);
    }

    #[test]
    fn z23_stamp_is_thread_safe() {
        let counter = RunnerSeqCounter::new();
        let mut handles = Vec::new();
        for _ in 0..16 {
            let c = counter.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    c.stamp_after_ok();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(counter.high_water(), 1600);
    }

    // ─── seq classifier (iter-28 #2-6) ────────────────────────────

    #[test]
    fn classify_seq_ok_strictly_increases() {
        assert_eq!(classify_seq(1, 0), SeqClassification::Ok);
        assert_eq!(classify_seq(2, 1), SeqClassification::Ok);
        assert_eq!(classify_seq(100, 99), SeqClassification::Ok);
    }

    #[test]
    fn classify_seq_zero_is_stutter_before_high_water_check() {
        // Iter-28 #2-6: explicit seq==0 guard MUST fire FIRST.
        assert_eq!(classify_seq(0, 5), SeqClassification::Stutter);
        assert_eq!(classify_seq(0, 0), SeqClassification::Stutter);
        // Even when seq=0 would be a "regression" (high_water=5 -> 0),
        // we surface it as Stutter, NOT Regression.
    }

    #[test]
    fn classify_seq_gap_when_skipping_values() {
        match classify_seq(7, 5) {
            SeqClassification::Gap { expected, observed } => {
                assert_eq!(expected, 6);
                assert_eq!(observed, 7);
            }
            other => panic!("expected Gap, got {other:?}"),
        }
    }

    #[test]
    fn classify_seq_regression_when_lower_than_high_water() {
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

    #[test]
    fn classify_seq_does_not_mutate_high_water() {
        // The classifier is a pure function; mutation only happens at
        // the Z-23 stamp call site.
        let counter = RunnerSeqCounter::new();
        let _ = classify_seq(7, counter.high_water());
        assert_eq!(counter.high_water(), 0);
    }

    // ─── inbound queue ─────────────────────────────────────────────

    #[tokio::test]
    async fn enqueue_then_recv() {
        let (tx, mut rx) = inbound_queue(8);
        tx.try_enqueue(frame(1)).unwrap();
        let f = rx.recv().await.unwrap();
        assert_eq!(f.seq, 1);
    }

    #[tokio::test]
    async fn try_enqueue_full_returns_queue_full_drop_reason() {
        let (tx, _rx) = inbound_queue(1);
        tx.try_enqueue(frame(1)).unwrap();
        let r = tx.try_enqueue(frame(2));
        assert!(matches!(r, Err(DropReason::QueueFull)));
    }

    #[tokio::test]
    async fn dropping_receiver_closes_queue() {
        let (tx, rx) = inbound_queue(8);
        drop(rx);
        let r = tx.try_enqueue(frame(1));
        assert!(matches!(r, Err(DropReason::ProtocolViolation(_))));
    }

    #[tokio::test]
    async fn enqueue_blocks_when_full_until_drained() {
        let (tx, mut rx) = inbound_queue(1);
        tx.try_enqueue(token_frame(1)).unwrap();
        let tx2 = tx.clone();
        let send_task = tokio::spawn(async move {
            tx2.enqueue(token_frame(2)).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!send_task.is_finished(), "enqueue must block when full");
        let _ = rx.recv().await;
        send_task.await.unwrap();
    }

    // ─── frame_id allocator ────────────────────────────────────────

    #[test]
    fn frame_id_allocator_starts_at_one_and_increments() {
        let alloc = FrameIdAllocator::new();
        assert_eq!(alloc.next(), FrameId(1));
        assert_eq!(alloc.next(), FrameId(2));
        assert_eq!(alloc.next(), FrameId(3));
    }

    #[test]
    fn frame_id_allocator_is_independent_of_runner_seq() {
        // Z-23: frame_id and runner_seq are SEPARATE counters.  The
        // queue allocates frame_id on every parse; runner_seq advances
        // only on post-Ok forward.
        let frame_alloc = FrameIdAllocator::new();
        let counter = RunnerSeqCounter::new();
        let _f1 = frame_alloc.next(); // FrameId(1)
        let _f2 = frame_alloc.next(); // FrameId(2)
        let _f3 = frame_alloc.next(); // FrameId(3) -- e.g., 3 frames parsed
        assert_eq!(counter.high_water(), 0); // no stamps yet
        counter.stamp_after_ok(); // first frame succeeded
        assert_eq!(counter.high_water(), 1);
    }

    #[test]
    #[should_panic(expected = "inbound queue capacity MUST be >= 1")]
    fn zero_capacity_panics() {
        let _ = inbound_queue(0);
    }
}
