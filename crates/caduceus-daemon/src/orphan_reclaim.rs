//! `OrphanReclaim` worker — spec #3 §5B.2 (ws10).
//!
//! Per the implementation DAG, this module ships the background queue
//! that re-enters `cleanup_workspace` for orphaned workspaces.  Sources
//! of enqueue (spec #3 §5B.2 + §3.5 step 5b + startup recovery):
//!
//! 1. **Create-time rollback** — spec #3 §3.5 step 5b: a partial-create
//!    aborted before placeholder-row deletion enqueues for reclaim.
//! 2. **Cleanup retry** — `cleanup_workspace` returning
//!    `WorkspaceError::HookFailed` transitions the row to
//!    `CleanupFailed` (recorded by `cleanup_workspace.rs`); a separate
//!    bookkeeping task enqueues those rows.
//! 3. **Startup recovery** — `or00-boot-reconcile-sweep` enumerates
//!    rows with status `CleanupFailed` and `Creating` and enqueues
//!    them.
//!
//! Iter-28 #3-9 absorbed: the **canonical bypass scope** statement.
//! `OrphanReclaim` re-entry into `cleanup_workspace` skips ONLY step 4
//! (the layered liveness probe — already a stub in v1) regardless of
//! enqueue source.  All other §3.6 steps run unchanged.  The bypass
//! is implemented via `CleanupCallerClass::OrphanReclaim`.
//!
//! Worker model (v1):
//!
//! - Bounded MPSC queue of `(slug, workspace_id)` entries.
//! - Single drainer task.  Each entry is processed sequentially
//!   (per-slug serialization is enforced by `WorkspaceLocks` blocking
//!   on contention; the worker MAY appear to make no progress but the
//!   contention is bounded by the duration of the holder).
//! - Outcomes are logged + counted via the `Metrics` registry.

use crate::cleanup_workspace::{
    cleanup_workspace, CleanupArgs, CleanupCallerClass, CleanupOutcome,
};
use crate::error::WorkspaceError;
use crate::hooks::HookExecutor;
use crate::locks::WorkspaceLocks;
use crate::registry_store::RegistryStore;
use crate::telemetry::Metrics;
use std::sync::Arc;

/// An entry on the orphan-reclaim queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanReclaimEntry {
    pub workspace_id: String,
    /// Why this entry was enqueued, for diagnostics.
    pub reason: ReclaimReason,
}

/// Why an entry was enqueued.  Diagnostic only; semantics are identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimReason {
    CreateRollback,
    CleanupFailed,
    StartupRecovery,
}

impl std::fmt::Display for ReclaimReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ReclaimReason::CreateRollback => "create_rollback",
            ReclaimReason::CleanupFailed => "cleanup_failed",
            ReclaimReason::StartupRecovery => "startup_recovery",
        };
        f.write_str(s)
    }
}

/// Producer side of the queue.  Cheap to clone; multiple call sites
/// can enqueue concurrently.
#[derive(Debug, Clone)]
pub struct OrphanReclaimSender {
    inner: tokio::sync::mpsc::Sender<OrphanReclaimEntry>,
}

impl OrphanReclaimSender {
    pub async fn enqueue(&self, entry: OrphanReclaimEntry) -> Result<(), WorkspaceError> {
        self.inner
            .send(entry)
            .await
            .map_err(|_| WorkspaceError::PathValidationFailed("orphan-reclaim queue closed".into()))
    }

    /// Try-send variant for synchronous code paths (e.g. create-time
    /// rollback) that must not block.  Drops the entry if the queue
    /// is full; `or00-boot-reconcile-sweep` will pick it up on next
    /// startup.
    pub fn try_enqueue(&self, entry: OrphanReclaimEntry) -> bool {
        self.inner.try_send(entry).is_ok()
    }
}

/// Spawn the orphan-reclaim worker.  Returns the sender side; the
/// worker task runs in the background until the daemon shuts down.
pub fn spawn_orphan_reclaim_worker(
    registry: Arc<RegistryStore>,
    locks: WorkspaceLocks,
    hook_executor: Arc<dyn HookExecutor>,
    metrics: Metrics,
    queue_capacity: usize,
) -> OrphanReclaimSender {
    let (tx, rx) = tokio::sync::mpsc::channel::<OrphanReclaimEntry>(queue_capacity);
    tokio::spawn(async move {
        run_worker(registry, locks, hook_executor, metrics, rx).await;
    });
    OrphanReclaimSender { inner: tx }
}

async fn run_worker(
    registry: Arc<RegistryStore>,
    locks: WorkspaceLocks,
    hook_executor: Arc<dyn HookExecutor>,
    metrics: Metrics,
    mut rx: tokio::sync::mpsc::Receiver<OrphanReclaimEntry>,
) {
    let success = metrics.counter("orphan_reclaim.success");
    let failure = metrics.counter("orphan_reclaim.failure");
    let no_leaf = metrics.counter("orphan_reclaim.no_leaf");
    let no_slug = metrics.counter("orphan_reclaim.no_slug");

    while let Some(entry) = rx.recv().await {
        let outcome = process_entry(&registry, &locks, hook_executor.as_ref(), &entry);
        match outcome {
            Ok(CleanupOutcome::Cleared) => {
                success.incr();
                tracing::info!(
                    workspace_id = %entry.workspace_id,
                    reason = %entry.reason,
                    "orphan_reclaim.cleared"
                );
            }
            Ok(CleanupOutcome::OrphanedNoLeaf) => {
                no_leaf.incr();
                tracing::info!(
                    workspace_id = %entry.workspace_id,
                    reason = %entry.reason,
                    "orphan_reclaim.no_leaf"
                );
            }
            Ok(CleanupOutcome::OrphanedNoSlug) => {
                no_slug.incr();
                tracing::info!(
                    workspace_id = %entry.workspace_id,
                    reason = %entry.reason,
                    "orphan_reclaim.no_slug"
                );
            }
            Err(e) => {
                failure.incr();
                tracing::warn!(
                    workspace_id = %entry.workspace_id,
                    reason = %entry.reason,
                    error = %e,
                    "orphan_reclaim.failure"
                );
            }
        }
    }
}

fn process_entry(
    registry: &RegistryStore,
    locks: &WorkspaceLocks,
    hook_executor: &dyn HookExecutor,
    entry: &OrphanReclaimEntry,
) -> Result<CleanupOutcome, WorkspaceError> {
    cleanup_workspace(CleanupArgs {
        registry,
        locks,
        workspace_id: &entry.workspace_id,
        caller: CleanupCallerClass::OrphanReclaim,
        hook_executor,
        before_cleanup: vec![],
        after_cleanup: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_workspace::{create_workspace, CreateWorkspaceArgs};
    use crate::hooks::NoopHookExecutor;
    use crate::leaf_ownership::RunnerIdentity;
    use crate::registry::RepoCoordinate;
    use crate::workspace::{sanitize_repo_slug, WorkspaceIdKey};

    fn coord() -> RepoCoordinate {
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        RepoCoordinate::new(
            slug,
            Some("https://github.com/o/r".into()),
            Some("main".into()),
        )
    }

    #[tokio::test]
    async fn worker_clears_orphaned_workspace() {
        let td = tempfile::tempdir().unwrap();
        let store = Arc::new(RegistryStore::open(td.path()).unwrap());
        let locks = WorkspaceLocks::new();
        let key = WorkspaceIdKey::derive(td.path());

        let exec_create = NoopHookExecutor;
        let ws = create_workspace(CreateWorkspaceArgs {
            registry: &store,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "01H8XYZ",
            runner: RunnerIdentity::for_self(),
            hook_executor: &exec_create,
            before_create: vec![],
            after_create: vec![],
        })
        .unwrap();

        let metrics = Metrics::new();
        let sender = spawn_orphan_reclaim_worker(
            Arc::clone(&store),
            locks.clone(),
            Arc::new(NoopHookExecutor),
            metrics.clone(),
            8,
        );
        sender
            .enqueue(OrphanReclaimEntry {
                workspace_id: ws.workspace_id.clone(),
                reason: ReclaimReason::CreateRollback,
            })
            .await
            .unwrap();
        // Drain: drop sender and wait for the queue to empty.
        drop(sender);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(store.get(&ws.workspace_id).is_none());
        assert!(!ws.path.exists());
        assert_eq!(metrics.counter("orphan_reclaim.success").get(), 1);
    }

    #[tokio::test]
    async fn worker_records_no_leaf_when_leaf_already_gone() {
        let td = tempfile::tempdir().unwrap();
        let store = Arc::new(RegistryStore::open(td.path()).unwrap());
        let locks = WorkspaceLocks::new();
        let key = WorkspaceIdKey::derive(td.path());
        let exec_create = NoopHookExecutor;
        let ws = create_workspace(CreateWorkspaceArgs {
            registry: &store,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "01H8XYZ",
            runner: RunnerIdentity::for_self(),
            hook_executor: &exec_create,
            before_create: vec![],
            after_create: vec![],
        })
        .unwrap();
        std::fs::remove_dir_all(&ws.path).unwrap();

        let metrics = Metrics::new();
        let sender = spawn_orphan_reclaim_worker(
            Arc::clone(&store),
            locks.clone(),
            Arc::new(NoopHookExecutor),
            metrics.clone(),
            8,
        );
        sender
            .enqueue(OrphanReclaimEntry {
                workspace_id: ws.workspace_id.clone(),
                reason: ReclaimReason::StartupRecovery,
            })
            .await
            .unwrap();
        drop(sender);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(store.get(&ws.workspace_id).is_none());
        assert_eq!(metrics.counter("orphan_reclaim.no_leaf").get(), 1);
    }

    #[test]
    fn try_enqueue_drops_silently_when_full() {
        // Build a sender backed by a tiny channel; fill it; verify
        // try_enqueue returns false after capacity reached.
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let sender = OrphanReclaimSender { inner: tx };
        let e = OrphanReclaimEntry {
            workspace_id: "wsp_x".into(),
            reason: ReclaimReason::CreateRollback,
        };
        // First fits, second overflows (channel capacity 1; nothing draining).
        assert!(sender.try_enqueue(e.clone()));
        assert!(!sender.try_enqueue(e));
    }

    #[test]
    fn reclaim_reason_display_is_snake_case() {
        assert_eq!(ReclaimReason::CreateRollback.to_string(), "create_rollback");
        assert_eq!(ReclaimReason::CleanupFailed.to_string(), "cleanup_failed");
        assert_eq!(
            ReclaimReason::StartupRecovery.to_string(),
            "startup_recovery"
        );
    }
}
