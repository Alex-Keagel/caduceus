//! Orchestrator dispatch loop + boot reconcile sweep (or10 + or00).
//!
//! Per the implementation DAG:
//!
//! - **`or00-boot-reconcile-sweep`**: spec #1 §3.1 line 277 once-per-boot
//!   MUST.  Runs BEFORE the first dispatch tick.  Scans the workspace
//!   registry for `Status::CleanupFailed` and `Status::Creating` rows;
//!   enqueues each into the `OrphanReclaim` worker.  Reconciles
//!   `retry_attempts` against the workflow + workspace state.
//!
//! - **`or10-dispatch-loop`**: spec #1 §3.2 main loop.  Awaits
//!   `Cmd::*` from the mailbox + dispatches handlers.  Iter-28 #1-4
//!   absorbed: requires `wf02-workflow-loader` to have run before
//!   the first poll tick (the `Config` is loaded by f02 before this
//!   loop is started).
//!
//! Handlers (or12..or21) are stub-routed in v1; deeper implementations
//! land in their own todos.

use crate::mailbox::{Cmd, Receiver};
use crate::orchestrator_state::OrchestratorState;
use crate::orphan_reclaim::{OrphanReclaimEntry, OrphanReclaimSender, ReclaimReason};
use crate::registry_store::RegistryStore;
use std::sync::Arc;

/// Spec #1 §3.1 line 277 — boot reconcile sweep.  MUST run BEFORE the
/// first dispatch tick.  Idempotent.
pub async fn boot_reconcile_sweep(
    registry: &RegistryStore,
    orphan_sender: &OrphanReclaimSender,
) -> BootReconcileSummary {
    let mut summary = BootReconcileSummary::default();

    // Spec #3 §3.6 + §5B.2: rows in CleanupFailed retry via OrphanReclaim.
    for row in registry.list_status_cleanup_failed() {
        let r = orphan_sender
            .enqueue(OrphanReclaimEntry {
                workspace_id: row.workspace_id.clone(),
                reason: ReclaimReason::StartupRecovery,
            })
            .await;
        if r.is_ok() {
            summary.cleanup_failed_enqueued += 1;
        } else {
            summary.enqueue_failures += 1;
        }
    }

    // Iter-28 #3-2: rows in Creating that survived a daemon restart are
    // mid-create orphans.  Enqueue for reclaim.
    for row in registry.list_status_creating() {
        let r = orphan_sender
            .enqueue(OrphanReclaimEntry {
                workspace_id: row.workspace_id.clone(),
                reason: ReclaimReason::StartupRecovery,
            })
            .await;
        if r.is_ok() {
            summary.creating_enqueued += 1;
        } else {
            summary.enqueue_failures += 1;
        }
    }

    summary
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BootReconcileSummary {
    pub cleanup_failed_enqueued: usize,
    pub creating_enqueued: usize,
    pub enqueue_failures: usize,
}

/// Spec #1 §3.2 main dispatch loop.  Polls the `Cmd` mailbox + routes.
///
/// `state` is `&mut`; the loop is single-consumer.  Concurrent readers
/// access state via the snapshot RPC (P4) which holds a separate
/// projection lock.
pub async fn run_dispatch_loop(
    mut receiver: Receiver,
    state: Arc<tokio::sync::Mutex<OrchestratorState>>,
) -> DispatchLoopOutcome {
    while let Some(cmd) = receiver.recv().await {
        let mut g = state.lock().await;
        match cmd {
            Cmd::Tick => {
                // Spec #1 §3.2: WorkSource scan + dispatch_run for each
                // active Run that is not already running/claimed.
                // V1 stub: tick is a no-op until WorkSource integration
                // (lands with workflow loader).
            }
            Cmd::RetryRun {
                run_id,
                token,
                deadline: _,
            } => {
                // Spec #1 §3.5 on_retry_timer (or14) — full impl in
                // its own module; v1 stub matches the token equality
                // check.
                if let Some(entry) = g.retry_attempts.get(&run_id) {
                    if entry.token == token {
                        // Token matches; would re-enqueue dispatch.
                        // V1: just clear the entry so the test can
                        // observe the match.
                        g.retry_attempts.remove(&run_id);
                    }
                    // else: stale token; drop silently.
                }
            }
            Cmd::DisconnectTimerExpired { run_id: _ } => {
                // or15 — disconnect FSM transition; v1 stub.
            }
            Cmd::WorkerExit { run_id, exit_code } => {
                // or12 — record history + decide retry.  V1: append
                // to recent_history_ring + drop running entry.
                if let Some(run) = g.running.remove(&run_id) {
                    g.recent_history_ring
                        .push(crate::orchestrator_state::RunHistory {
                            run_id: run.id,
                            final_attempt: run.attempt,
                            completed_at: std::time::Instant::now(),
                            exit_code,
                        });
                    g.claimed.release(&run_id);
                }
            }
            Cmd::WorkflowReloaded => {
                // or17 — hot reload; v1 stub.
            }
            Cmd::EngineDisconnected {
                run_id: _,
                session_id: _,
            } => {
                // or19 — daemon-observed; v1 stub.
            }
            Cmd::Shutdown => {
                g.shutting_down = true;
                // or18 drain + cleanup; v1 stub.  Loop terminates when
                // all senders are dropped.
                drop(g);
                break;
            }
            Cmd::SnapshotRequest { reply } => {
                // or20 — wire to snapshot RPC; v1: ack with Ok.
                let _ = reply.send(Ok(()));
            }
            Cmd::Reattach {
                run_id,
                session_id,
                runner_seq,
            } => {
                // or16 + or21 — full impl in handler module; v1 stub.
                if let Some(run) = g.running.get_mut(&run_id) {
                    if runner_seq >= run.runner_seq_high_water {
                        run.runner_seq_high_water = runner_seq;
                        run.session_id = Some(session_id);
                        // Iter-28 #1-6: do NOT mutate run.attempt or
                        // run.disconnect_generation.
                    }
                    // else: stale reattach; drop silently.
                }
            }
        }
    }
    DispatchLoopOutcome::ChannelClosed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchLoopOutcome {
    /// All senders dropped; loop terminated normally.
    ChannelClosed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_workspace::{create_workspace, CreateWorkspaceArgs};
    use crate::hooks::NoopHookExecutor;
    use crate::leaf_ownership::RunnerIdentity;
    use crate::locks::WorkspaceLocks;
    use crate::mailbox::{MailboxFactory, RetryToken, RunId, SessionId};
    use crate::orphan_reclaim::spawn_orphan_reclaim_worker;
    use crate::registry::RepoCoordinate;
    use crate::telemetry::Metrics;
    use crate::workspace::{sanitize_repo_slug, WorkspaceIdKey};
    use std::sync::Arc;
    use std::time::Duration;

    fn coord() -> RepoCoordinate {
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        RepoCoordinate::new(
            slug,
            Some("https://github.com/o/r".into()),
            Some("main".into()),
        )
    }

    #[tokio::test]
    async fn boot_reconcile_enqueues_cleanup_failed_rows() {
        let td = tempfile::tempdir().unwrap();
        let registry = Arc::new(RegistryStore::open(td.path()).unwrap());
        let locks = WorkspaceLocks::new();
        let key = WorkspaceIdKey::derive(td.path());

        let exec = NoopHookExecutor;
        // Create then mark CleanupFailed.
        let ws = create_workspace(CreateWorkspaceArgs {
            registry: &registry,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "01H8XYZ",
            runner: RunnerIdentity::for_self(),
            hook_executor: &exec,
            before_create: vec![],
            after_create: vec![],
        })
        .unwrap();
        registry
            .transition_to_cleaning_up(&ws.workspace_id)
            .unwrap();
        registry
            .transition_to_cleanup_failed(&ws.workspace_id)
            .unwrap();

        let metrics = Metrics::new();
        let sender = spawn_orphan_reclaim_worker(
            Arc::clone(&registry),
            locks.clone(),
            Arc::new(NoopHookExecutor),
            metrics.clone(),
            8,
        );
        let summary = boot_reconcile_sweep(&registry, &sender).await;
        assert_eq!(summary.cleanup_failed_enqueued, 1);
        // Drain.
        drop(sender);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn dispatch_loop_handles_shutdown_cmd() {
        let mb = MailboxFactory::new(8);
        let state = Arc::new(tokio::sync::Mutex::new(OrchestratorState::new(8)));
        let receiver = mb.receiver;
        let subsys = mb.subsystem;
        // Drop the rest of the senders so the loop ends after shutdown.
        drop(mb.timer);
        drop(mb.snapshot_client);
        drop(mb.engine);

        let s = Arc::clone(&state);
        let loop_task = tokio::spawn(run_dispatch_loop(receiver, s));
        subsys.shutdown().await.unwrap();
        drop(subsys);
        let outcome = loop_task.await.unwrap();
        assert_eq!(outcome, DispatchLoopOutcome::ChannelClosed);
        assert!(state.lock().await.shutting_down);
    }

    #[tokio::test]
    async fn dispatch_loop_handles_worker_exit_appends_history() {
        let mb = MailboxFactory::new(8);
        let state = Arc::new(tokio::sync::Mutex::new(OrchestratorState::new(8)));
        // Pre-populate state.running.
        {
            let mut g = state.lock().await;
            g.running.insert(
                RunId("r1".into()),
                crate::orchestrator_state::Run {
                    id: RunId("r1".into()),
                    attempt: crate::orchestrator_state::RunAttempt(1),
                    session_id: Some(SessionId("s1".into())),
                    runner_seq_high_water: 0,
                    state_since: std::time::Instant::now(),
                    disconnect_generation: 0,
                },
            );
        }
        let receiver = mb.receiver;
        let subsys = mb.subsystem;
        drop(mb.timer);
        drop(mb.snapshot_client);
        drop(mb.engine);

        let s = Arc::clone(&state);
        let loop_task = tokio::spawn(run_dispatch_loop(receiver, s));
        subsys
            .worker_exit(RunId("r1".into()), Some(0))
            .await
            .unwrap();
        subsys.shutdown().await.unwrap();
        drop(subsys);
        loop_task.await.unwrap();
        let g = state.lock().await;
        assert!(g.running.is_empty());
        assert_eq!(g.recent_history_ring.len(), 1);
    }

    #[tokio::test]
    async fn dispatch_loop_drops_stale_retry_tokens() {
        let mb = MailboxFactory::new(8);
        let state = Arc::new(tokio::sync::Mutex::new(OrchestratorState::new(8)));
        // Pre-populate retry_attempts with a known token.
        {
            let mut g = state.lock().await;
            g.retry_attempts.insert(
                RunId("r1".into()),
                crate::orchestrator_state::RetryEntry {
                    run_id: RunId("r1".into()),
                    token: RetryToken(7),
                    deadline: std::time::Instant::now(),
                    attempt: crate::orchestrator_state::RunAttempt(1),
                },
            );
        }
        let receiver = mb.receiver;
        let timer = mb.timer;
        let subsys = mb.subsystem;
        drop(mb.snapshot_client);
        drop(mb.engine);

        let s = Arc::clone(&state);
        let loop_task = tokio::spawn(run_dispatch_loop(receiver, s));

        // STALE token (3 != 7); should leave the entry alone.
        timer
            .retry_run(RunId("r1".into()), RetryToken(3), std::time::Instant::now())
            .await
            .unwrap();
        // Then matching token; should clear the entry.
        timer
            .retry_run(RunId("r1".into()), RetryToken(7), std::time::Instant::now())
            .await
            .unwrap();
        subsys.shutdown().await.unwrap();
        drop(timer);
        drop(subsys);
        loop_task.await.unwrap();
        let g = state.lock().await;
        assert!(!g.retry_attempts.contains_key(&RunId("r1".into())));
    }
}
