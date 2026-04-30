//! Capability-scoped command mailbox.
//!
//! Per the implementation DAG (todo `f05-cmd-mailbox`), this module
//! implements the canonical `Cmd` enum + a bounded MPSC mailbox where each
//! producer holds a sender newtype that can ONLY emit its allowed variants.
//! This is the type-level enforcement of spec #1's trust boundary
//! (iter-28 backlog #1-1) — no runtime check is required because an
//! unauthorised producer cannot construct a forbidden `Cmd`.
//!
//! Producer classes (spec #1 §0):
//!
//! 1. **Timer** — `Tick`, `RetryRun`, `DisconnectTimerExpired`.
//! 2. **Subsystem** — `WorkerExit`, `WorkflowReloaded`, `EngineDisconnected`,
//!    `Shutdown`.
//! 3. **Snapshot client** — `SnapshotRequest`.
//! 4. **Authenticated engine** — `Reattach` only.
//!
//! Each class has a `*Sender` newtype.  `MailboxFactory::build()` returns a
//! `Receiver` (consumed by the dispatch loop) plus exactly four senders,
//! one per class.  The senders are `Clone`, so each subsystem instance
//! holds its own clone.
//!
//! Spec cross-references:
//!
//! - **`spec-caduceus-orchestrator-algorithm.md` §0** — trust boundary.
//! - **`spec-caduceus-orchestrator-algorithm.md` §3.5** — Cmd::Reattach
//!   handler (`or16-on-reattach`) requires authenticated-engine producer.
//! - **`spec-caduceus-orchestrator-algorithm.md` §3.5 EngineDisconnected**
//!   — daemon-observed; producer class is **subsystem**, not authenticated
//!   engine (iter-28 #1-1 absorbed in Cmd::EngineDisconnected variant).

use std::time::Instant;
use tokio::sync::mpsc;

/// Run identifier carried by most `Cmd` variants.  Newtyped to prevent
/// accidental swap with `runner_seq` (u64) or `attempt` (u32).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunId(pub String);

/// Opaque session identifier issued by the runner on first frame.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

/// Per-process monotonic counter (spec #1 §3.5; iter-28 #1-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RetryToken(pub u64);

/// Top-level command type consumed by the orchestrator main loop.
///
/// The variants are stable across the four P0 specs; producer-class
/// gating is enforced by which `*Sender` newtype is allowed to construct
/// each variant.
#[derive(Debug)]
pub enum Cmd {
    /// Periodic poll trigger.  Producer: timer.
    Tick,

    /// Retry timer fired for the run carrying `token`.  Producer: timer.
    RetryRun {
        run_id: RunId,
        token: RetryToken,
        deadline: Instant,
    },

    /// Disconnect timer fired for the run.  Producer: timer.
    DisconnectTimerExpired { run_id: RunId },

    /// Worker process exited.  Producer: subsystem.
    WorkerExit {
        run_id: RunId,
        exit_code: Option<i32>,
    },

    /// Workflow file reloaded.  Producer: subsystem.
    WorkflowReloaded,

    /// Engine RPC channel observed closed.  Producer: subsystem (NOT
    /// authenticated-engine — iter-28 backlog #1-1).
    EngineDisconnected {
        run_id: RunId,
        session_id: SessionId,
    },

    /// Supervisor-issued shutdown.  Producer: subsystem.
    Shutdown,

    /// Snapshot request from a snapshot client.  Producer: snapshot client.
    SnapshotRequest {
        reply: tokio::sync::oneshot::Sender<crate::error::DaemonResult<()>>,
    },

    /// Reattach request from an authenticated engine session.  Producer:
    /// authenticated-engine ONLY.
    Reattach {
        run_id: RunId,
        session_id: SessionId,
        runner_seq: u64,
    },
}

/// Errors surfaced by the mailbox.
#[derive(Debug, thiserror::Error)]
pub enum MailboxError {
    /// The mailbox was already closed (typically because the daemon is
    /// in the `Halted` state and the receiver has been dropped).
    #[error("mailbox closed; daemon is shutting down or halted")]
    Closed,

    /// The mailbox is at capacity AND the sender chose `try_send` over
    /// the awaiting `send`.  Producers that hold time-sensitive frames
    /// (timer fan-outs, signal handlers) use `try_send` to avoid hangs.
    #[error("mailbox full; producer chose try_send")]
    Full,
}

impl<T> From<mpsc::error::SendError<T>> for MailboxError {
    fn from(_: mpsc::error::SendError<T>) -> Self {
        MailboxError::Closed
    }
}

impl<T> From<mpsc::error::TrySendError<T>> for MailboxError {
    fn from(e: mpsc::error::TrySendError<T>) -> Self {
        match e {
            mpsc::error::TrySendError::Full(_) => MailboxError::Full,
            mpsc::error::TrySendError::Closed(_) => MailboxError::Closed,
        }
    }
}

/// Receiver side, held by the dispatch loop.  Single owner.
pub struct Receiver {
    inner: mpsc::Receiver<Cmd>,
}

impl Receiver {
    /// Await the next `Cmd`.  Returns `None` if all senders have been
    /// dropped (shutdown signal).
    pub async fn recv(&mut self) -> Option<Cmd> {
        self.inner.recv().await
    }
}

/// Sender for **timer-class** commands.  Spec #1 §0.
#[derive(Clone)]
pub struct TimerSender {
    inner: mpsc::Sender<Cmd>,
}

impl TimerSender {
    pub async fn tick(&self) -> Result<(), MailboxError> {
        Ok(self.inner.send(Cmd::Tick).await?)
    }
    pub async fn retry_run(
        &self,
        run_id: RunId,
        token: RetryToken,
        deadline: Instant,
    ) -> Result<(), MailboxError> {
        Ok(self
            .inner
            .send(Cmd::RetryRun {
                run_id,
                token,
                deadline,
            })
            .await?)
    }
    pub async fn disconnect_timer_expired(&self, run_id: RunId) -> Result<(), MailboxError> {
        Ok(self
            .inner
            .send(Cmd::DisconnectTimerExpired { run_id })
            .await?)
    }
}

/// Sender for **subsystem-class** commands (worker exit, workflow reload,
/// engine-disconnected diagnostic, supervisor shutdown).  Spec #1 §0.
#[derive(Clone)]
pub struct SubsystemSender {
    inner: mpsc::Sender<Cmd>,
}

impl SubsystemSender {
    pub async fn worker_exit(
        &self,
        run_id: RunId,
        exit_code: Option<i32>,
    ) -> Result<(), MailboxError> {
        Ok(self
            .inner
            .send(Cmd::WorkerExit { run_id, exit_code })
            .await?)
    }
    pub async fn workflow_reloaded(&self) -> Result<(), MailboxError> {
        Ok(self.inner.send(Cmd::WorkflowReloaded).await?)
    }
    pub async fn engine_disconnected(
        &self,
        run_id: RunId,
        session_id: SessionId,
    ) -> Result<(), MailboxError> {
        Ok(self
            .inner
            .send(Cmd::EngineDisconnected { run_id, session_id })
            .await?)
    }
    /// Try-send variant for signal handlers that must not block.
    pub fn try_shutdown(&self) -> Result<(), MailboxError> {
        Ok(self.inner.try_send(Cmd::Shutdown)?)
    }
    pub async fn shutdown(&self) -> Result<(), MailboxError> {
        Ok(self.inner.send(Cmd::Shutdown).await?)
    }
}

/// Sender for **snapshot-client-class** commands.  Spec #1 §0; spec #4 §3.
#[derive(Clone)]
pub struct SnapshotClientSender {
    inner: mpsc::Sender<Cmd>,
}

impl SnapshotClientSender {
    pub async fn request(
        &self,
        reply: tokio::sync::oneshot::Sender<crate::error::DaemonResult<()>>,
    ) -> Result<(), MailboxError> {
        Ok(self.inner.send(Cmd::SnapshotRequest { reply }).await?)
    }
}

/// Sender for **authenticated-engine-class** commands.  Spec #1 §0; spec
/// #2 §0; iter-28 #1-1 (only `Reattach` may be emitted from this class).
#[derive(Clone)]
pub struct EngineSender {
    inner: mpsc::Sender<Cmd>,
}

impl EngineSender {
    pub async fn reattach(
        &self,
        run_id: RunId,
        session_id: SessionId,
        runner_seq: u64,
    ) -> Result<(), MailboxError> {
        Ok(self
            .inner
            .send(Cmd::Reattach {
                run_id,
                session_id,
                runner_seq,
            })
            .await?)
    }
}

/// Mailbox factory bundling one receiver and the four senders.  The
/// receiver is single-consumer (the dispatch loop); the senders are
/// per-class but Clone-able to fan out.
pub struct MailboxFactory {
    pub receiver: Receiver,
    pub timer: TimerSender,
    pub subsystem: SubsystemSender,
    pub snapshot_client: SnapshotClientSender,
    pub engine: EngineSender,
}

impl MailboxFactory {
    /// Build a new mailbox with the given backing capacity.  The cap MUST
    /// be `>= 1`.  Recommended capacity: 1024 (covers worst-case timer
    /// fan-out at high-concurrency).
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 1, "mailbox capacity MUST be >= 1");
        let (tx, rx) = mpsc::channel(capacity);
        Self {
            receiver: Receiver { inner: rx },
            timer: TimerSender { inner: tx.clone() },
            subsystem: SubsystemSender { inner: tx.clone() },
            snapshot_client: SnapshotClientSender { inner: tx.clone() },
            engine: EngineSender { inner: tx },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_id(s: &str) -> RunId {
        RunId(s.to_string())
    }
    fn session_id(s: &str) -> SessionId {
        SessionId(s.to_string())
    }

    #[tokio::test]
    async fn timer_sender_emits_tick() {
        let mut mb = MailboxFactory::new(8);
        mb.timer.tick().await.unwrap();
        match mb.receiver.recv().await {
            Some(Cmd::Tick) => {}
            other => panic!("expected Cmd::Tick, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timer_sender_emits_retry_run_with_token() {
        let mut mb = MailboxFactory::new(8);
        mb.timer
            .retry_run(run_id("r1"), RetryToken(42), Instant::now())
            .await
            .unwrap();
        match mb.receiver.recv().await {
            Some(Cmd::RetryRun {
                run_id: r, token, ..
            }) => {
                assert_eq!(r.0, "r1");
                assert_eq!(token.0, 42);
            }
            other => panic!("expected Cmd::RetryRun, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subsystem_sender_emits_worker_exit() {
        let mut mb = MailboxFactory::new(8);
        mb.subsystem
            .worker_exit(run_id("r1"), Some(0))
            .await
            .unwrap();
        match mb.receiver.recv().await {
            Some(Cmd::WorkerExit {
                run_id: r,
                exit_code,
            }) => {
                assert_eq!(r.0, "r1");
                assert_eq!(exit_code, Some(0));
            }
            other => panic!("expected Cmd::WorkerExit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subsystem_sender_emits_engine_disconnected_not_authenticated_engine() {
        // Iter-28 #1-1: EngineDisconnected MUST be producer-class subsystem,
        // not authenticated-engine. We enforce this by having only
        // SubsystemSender expose `engine_disconnected()`.
        let mut mb = MailboxFactory::new(8);
        mb.subsystem
            .engine_disconnected(run_id("r1"), session_id("s1"))
            .await
            .unwrap();
        match mb.receiver.recv().await {
            Some(Cmd::EngineDisconnected { run_id: r, .. }) => {
                assert_eq!(r.0, "r1");
            }
            other => panic!("expected Cmd::EngineDisconnected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn engine_sender_can_only_emit_reattach() {
        // The EngineSender impl exposes ONLY reattach(); attempting to
        // call any other variant constructor is a compile-time error.
        // (We can't write a runtime test for what doesn't compile, so
        // this test asserts the positive: reattach DOES work.)
        let mut mb = MailboxFactory::new(8);
        mb.engine
            .reattach(run_id("r1"), session_id("s1"), 7)
            .await
            .unwrap();
        match mb.receiver.recv().await {
            Some(Cmd::Reattach {
                run_id: r,
                session_id: s,
                runner_seq,
            }) => {
                assert_eq!(r.0, "r1");
                assert_eq!(s.0, "s1");
                assert_eq!(runner_seq, 7);
            }
            other => panic!("expected Cmd::Reattach, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_client_sender_emits_snapshot_request() {
        let mut mb = MailboxFactory::new(8);
        let (tx, _rx) = tokio::sync::oneshot::channel();
        mb.snapshot_client.request(tx).await.unwrap();
        match mb.receiver.recv().await {
            Some(Cmd::SnapshotRequest { .. }) => {}
            other => panic!("expected Cmd::SnapshotRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn try_shutdown_does_not_block_when_mailbox_full() {
        let mut mb = MailboxFactory::new(1);
        // Fill the buffer with one item.
        mb.timer.tick().await.unwrap();
        // try_shutdown MUST return Full, never block.
        let r = mb.subsystem.try_shutdown();
        assert!(matches!(r, Err(MailboxError::Full)));
        // Drain and retry to confirm correctness.
        mb.receiver.recv().await;
        mb.subsystem.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn dropping_all_senders_closes_receiver() {
        let MailboxFactory {
            mut receiver,
            timer,
            subsystem,
            snapshot_client,
            engine,
        } = MailboxFactory::new(8);
        drop(timer);
        drop(subsystem);
        drop(snapshot_client);
        drop(engine);
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn senders_clone_independently() {
        // Cloning a sender must produce a handle that delivers to the
        // same receiver. Each subsystem instance holds its own clone.
        let mut mb = MailboxFactory::new(8);
        let timer_a = mb.timer.clone();
        let timer_b = mb.timer.clone();
        timer_a.tick().await.unwrap();
        timer_b.tick().await.unwrap();
        let mut count = 0;
        for _ in 0..2 {
            if let Some(Cmd::Tick) = mb.receiver.recv().await {
                count += 1;
            }
        }
        assert_eq!(count, 2);
    }

    #[test]
    #[should_panic(expected = "mailbox capacity MUST be >= 1")]
    fn zero_capacity_panics() {
        let _ = MailboxFactory::new(0);
    }

    #[tokio::test]
    async fn mailbox_full_with_send_blocks_until_drained() {
        // Confirms backpressure works: send().await yields when buffer
        // is full and resumes once a recv() drains an item.
        let mut mb = MailboxFactory::new(1);
        mb.timer.tick().await.unwrap();
        // Spawn a sender that will block on the second tick.
        let timer = mb.timer.clone();
        let send_task = tokio::spawn(async move {
            timer.tick().await.unwrap();
        });
        // Verify the second send is still pending after a moment.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!send_task.is_finished(), "send must block while full");
        // Drain to unblock.
        mb.receiver.recv().await;
        send_task.await.unwrap();
    }
}
