//! `cleanup_workspace` — spec #3 §3.6 (ws09 + ws13).
//!
//! Per the implementation DAG, this module implements the workspace
//! cleanup algorithm with the explicit short-circuit semantics that
//! iter-28 backlog #3-4 surfaces:
//!
//! - **`OrphanedNoSlug`**: `ENOENT` opening the slug parent → skip
//!   steps 4–8 entirely (no probe, no hooks, no `unlinkat`).  Proceed
//!   directly to step 9 (delete registry row).
//! - **`OrphanedNoLeaf`**: `ENOENT` / `ELOOP` opening the leaf →
//!   skip steps 4–8 entirely.  Hooks MUST NOT run because the cwd for
//!   `before_cleanup` / `after_cleanup` does not exist.
//!
//! Iter-28 #3-7 absorbed: phase-1 row claim is performed under a brief
//! registry-wide mutex (transition to `CleaningUp`) BEFORE the longer
//! per-slug + per-workspace locks are acquired.
//!
//! Iter-28 #3-9 absorbed: `OrphanReclaim` re-entry skips ONLY step 4
//! (the layered liveness probe).  All other steps remain unchanged
//! regardless of enqueue source.  Surfaced via the
//! [`CleanupCallerClass`] enum.

use crate::env_exports::workspace_env_exports;
use crate::error::{HookPhase, WorkspaceError};
use crate::hooks::{HookExecutor, HookSpec};
use crate::locks::WorkspaceLocks;
use crate::registry::WorkspaceStatus;
use crate::registry_store::RegistryStore;
use crate::shared_repo_lock::{self, SharedRepoCaller};

/// Caller class invoking cleanup.  Determines whether the layered
/// liveness probe runs (step 6 of §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupCallerClass {
    /// Synchronous cleanup from spec #1 dispatch on terminal-state
    /// runner exit.  Liveness probe runs.
    Synchronous,
    /// `OrphanReclaim` re-entry (spec #3 §5B.2).  Liveness probe is
    /// skipped — the run is known dead by virtue of being orphaned.
    OrphanReclaim,
}

/// Outcome of a `cleanup_workspace` call.  Returned for diagnostics
/// and to drive `OrphanReclaim` retry decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// Full cleanup ran (probe + hooks + unlinkat + row delete).
    Cleared,
    /// Slug parent was missing; skipped steps 4-8; row was deleted if
    /// it existed.
    OrphanedNoSlug,
    /// Leaf was missing; skipped steps 4-8; row was deleted if it
    /// existed.
    OrphanedNoLeaf,
}

/// Inputs to `cleanup_workspace`.
pub struct CleanupArgs<'a> {
    pub registry: &'a RegistryStore,
    pub locks: &'a WorkspaceLocks,
    pub workspace_id: &'a str,
    pub caller: CleanupCallerClass,
    pub hook_executor: &'a dyn HookExecutor,
    pub before_cleanup: Vec<HookSpec>,
    pub after_cleanup: Vec<HookSpec>,
}

/// Spec #3 §3.6.  Tear down a Run workspace.
///
/// On `OrphanedNoLeaf`/`OrphanedNoSlug`, hooks are NOT invoked even if
/// the workflow declared them — the cwd for the hook does not exist.
/// Spec rationale: hook authors rely on `CADUCEUS_WORKSPACE_PATH`
/// being a usable directory; running them with a missing cwd is
/// surprising and unsafe.
pub fn cleanup_workspace(args: CleanupArgs<'_>) -> Result<CleanupOutcome, WorkspaceError> {
    // ── Phase-1 row claim (iter-28 #3-7): brief registry-wide mutex ──
    let row = args
        .registry
        .get(args.workspace_id)
        .ok_or(WorkspaceError::AlreadyCleared)?;
    // Transition Ready -> CleaningUp (or accept already-CleaningUp on
    // OrphanReclaim retry).  No-op if status is already terminal.
    match row.status {
        WorkspaceStatus::Ready => {
            args.registry
                .transition_to_cleaning_up(args.workspace_id)
                .map_err(|e| WorkspaceError::RegistryWriteFailed(e.to_string()))?;
        }
        WorkspaceStatus::CleaningUp | WorkspaceStatus::CleanupFailed => {
            // OrphanReclaim or retry path; proceed with cleanup.
        }
        WorkspaceStatus::Creating => {
            // Caller is recovering an aborted create; treat as cleanup
            // proceed, but ensure status is moved to CleaningUp first.
            // (Direct Creating -> CleaningUp is not allowed by the
            // strict state machine; we transition to Ready briefly to
            // satisfy the FSM, then to CleaningUp.  Document as a
            // recovery edge.)
            args.registry
                .transition_to_ready(args.workspace_id)
                .map_err(|e| WorkspaceError::RegistryWriteFailed(e.to_string()))?;
            args.registry
                .transition_to_cleaning_up(args.workspace_id)
                .map_err(|e| WorkspaceError::RegistryWriteFailed(e.to_string()))?;
        }
    }

    // ── Acquire per-slug + per-workspace locks (blocking) ────────────
    let registry_guard = args.locks.registry_lock();
    let _guards = shared_repo_lock::acquire(
        args.locks,
        registry_guard,
        match args.caller {
            CleanupCallerClass::Synchronous => SharedRepoCaller::Cleanup,
            CleanupCallerClass::OrphanReclaim => SharedRepoCaller::OrphanReclaim,
        },
        &row.repo_coordinate.slug,
        args.workspace_id,
    )?;

    // ── Step 3 / 4: probe slug + leaf existence (short-circuit) ──────
    let leaf = &row.path;
    let slug_dir = leaf
        .parent()
        .ok_or_else(|| {
            WorkspaceError::PathValidationFailed(format!("leaf {} has no parent", leaf.display()))
        })?
        .to_path_buf();

    if !slug_dir.exists() {
        // OrphanedNoSlug: skip 4-8, proceed to step 9.
        let _ = args
            .registry
            .delete(args.workspace_id)
            .map_err(|e| WorkspaceError::RegistryWriteFailed(e.to_string()))?;
        return Ok(CleanupOutcome::OrphanedNoSlug);
    }

    if !leaf.exists()
        || leaf
            .symlink_metadata()
            .map(|m| m.is_symlink())
            .unwrap_or(false)
    {
        // OrphanedNoLeaf: skip 4-8, proceed to step 9.  Hooks MUST NOT run.
        let _ = args
            .registry
            .delete(args.workspace_id)
            .map_err(|e| WorkspaceError::RegistryWriteFailed(e.to_string()))?;
        return Ok(CleanupOutcome::OrphanedNoLeaf);
    }

    // ── Step 5: before_cleanup hooks (only if leaf exists) ───────────
    let env = workspace_env_exports(
        &row.safe_run_id,
        &crate::workspace::SafeRunId::from_string_unchecked(row.safe_run_id.clone()),
        leaf,
        &row.repo_coordinate,
    );
    for hook in &args.before_cleanup {
        debug_assert_eq!(hook.phase, HookPhase::BeforeCleanup);
        // On hook failure we transition to CleanupFailed and return —
        // I-7 says rollback errors do not eclipse the original failure.
        if let Err(e) = args.hook_executor.execute(hook, &env, leaf) {
            let _ = args
                .registry
                .transition_to_cleanup_failed(args.workspace_id);
            return Err(e);
        }
    }

    // ── Step 6: layered liveness probe (skip on OrphanReclaim) ──────
    // Iter-28 #3-9: §5B.2 canonical bypass scope — re-entry skips ONLY
    // step 4 (probe) regardless of enqueue source.  V1: probe is a
    // stub that always returns "dead" (workspace is presumed dead at
    // this point — runner has exited).  Real probe lands with the
    // runner subsystem (P2).
    if args.caller == CleanupCallerClass::Synchronous {
        // probe stub: noop in v1.
    }

    // ── Step 7: recursive remove ─────────────────────────────────────
    if let Err(e) = std::fs::remove_dir_all(leaf) {
        let _ = args
            .registry
            .transition_to_cleanup_failed(args.workspace_id);
        return Err(WorkspaceError::PathValidationFailed(format!(
            "remove_dir_all({}): {e}",
            leaf.display()
        )));
    }

    // ── Step 8: after_cleanup hooks ──────────────────────────────────
    // After unlinkat, the leaf cwd is gone; run hooks against the slug
    // directory instead.  Hooks should NOT depend on the leaf path
    // existing post-cleanup.
    for hook in &args.after_cleanup {
        debug_assert_eq!(hook.phase, HookPhase::AfterCleanup);
        if let Err(e) = args.hook_executor.execute(hook, &env, &slug_dir) {
            let _ = args
                .registry
                .transition_to_cleanup_failed(args.workspace_id);
            return Err(e);
        }
    }

    // ── Step 9: delete registry row ──────────────────────────────────
    args.registry
        .delete(args.workspace_id)
        .map_err(|e| WorkspaceError::RegistryWriteFailed(e.to_string()))?;
    Ok(CleanupOutcome::Cleared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_workspace::{create_workspace, CreateWorkspaceArgs};
    use crate::hooks::{NoopHookExecutor, SubprocessHookExecutor, DEFAULT_HOOK_TIMEOUT};
    use crate::leaf_ownership::RunnerIdentity;
    use crate::registry::RepoCoordinate;
    use crate::workspace::{sanitize_repo_slug, WorkspaceIdKey};

    fn fixture(td: &tempfile::TempDir) -> (RegistryStore, WorkspaceLocks, WorkspaceIdKey) {
        let root = td.path().to_path_buf();
        let store = RegistryStore::open(&root).unwrap();
        let locks = WorkspaceLocks::new();
        let key = WorkspaceIdKey::derive(&root);
        (store, locks, key)
    }

    fn coord() -> RepoCoordinate {
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        RepoCoordinate::new(
            slug,
            Some("https://github.com/o/r".into()),
            Some("main".into()),
        )
    }

    fn create(
        store: &RegistryStore,
        locks: &WorkspaceLocks,
        key: &WorkspaceIdKey,
        run_id: &str,
    ) -> crate::create_workspace::Workspace {
        let exec = NoopHookExecutor;
        create_workspace(CreateWorkspaceArgs {
            registry: store,
            locks,
            key,
            repo_coordinate: coord(),
            raw_run_id: run_id,
            runner: RunnerIdentity::for_self(),
            hook_executor: &exec,
            before_create: vec![],
            after_create: vec![],
        })
        .unwrap()
    }

    #[test]
    fn cleanup_full_path_removes_leaf_and_row() {
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let ws = create(&store, &locks, &key, "01H8XYZ");
        let exec = NoopHookExecutor;
        let outcome = cleanup_workspace(CleanupArgs {
            registry: &store,
            locks: &locks,
            workspace_id: &ws.workspace_id,
            caller: CleanupCallerClass::Synchronous,
            hook_executor: &exec,
            before_cleanup: vec![],
            after_cleanup: vec![],
        })
        .unwrap();
        assert_eq!(outcome, CleanupOutcome::Cleared);
        assert!(!ws.path.exists());
        assert!(store.get(&ws.workspace_id).is_none());
    }

    #[test]
    fn cleanup_orphaned_no_leaf_skips_hooks() {
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let ws = create(&store, &locks, &key, "01H8XYZ");
        // Manually remove the leaf BEFORE cleanup (simulate post-crash state).
        std::fs::remove_dir_all(&ws.path).unwrap();
        let marker = td.path().join("hook-marker");
        // before_cleanup hook would create the marker file IF it ran;
        // OrphanedNoLeaf MUST skip it.
        let exec = SubprocessHookExecutor::default();
        let outcome = cleanup_workspace(CleanupArgs {
            registry: &store,
            locks: &locks,
            workspace_id: &ws.workspace_id,
            caller: CleanupCallerClass::Synchronous,
            hook_executor: &exec,
            before_cleanup: vec![HookSpec {
                phase: HookPhase::BeforeCleanup,
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("touch {}", marker.display()),
                ],
                timeout: DEFAULT_HOOK_TIMEOUT,
            }],
            after_cleanup: vec![],
        })
        .unwrap();
        assert_eq!(outcome, CleanupOutcome::OrphanedNoLeaf);
        assert!(!marker.exists(), "hooks MUST NOT run on OrphanedNoLeaf");
        assert!(store.get(&ws.workspace_id).is_none());
    }

    #[test]
    fn cleanup_orphaned_no_slug_skips_hooks() {
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let ws = create(&store, &locks, &key, "01H8XYZ");
        // Remove the entire slug parent.
        let slug_dir = ws.path.parent().unwrap().to_path_buf();
        std::fs::remove_dir_all(&slug_dir).unwrap();
        let marker = td.path().join("hook-marker");
        let exec = SubprocessHookExecutor::default();
        let outcome = cleanup_workspace(CleanupArgs {
            registry: &store,
            locks: &locks,
            workspace_id: &ws.workspace_id,
            caller: CleanupCallerClass::Synchronous,
            hook_executor: &exec,
            before_cleanup: vec![HookSpec {
                phase: HookPhase::BeforeCleanup,
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("touch {}", marker.display()),
                ],
                timeout: DEFAULT_HOOK_TIMEOUT,
            }],
            after_cleanup: vec![],
        })
        .unwrap();
        assert_eq!(outcome, CleanupOutcome::OrphanedNoSlug);
        assert!(!marker.exists(), "hooks MUST NOT run on OrphanedNoSlug");
        assert!(store.get(&ws.workspace_id).is_none());
    }

    #[test]
    fn cleanup_runs_before_and_after_hooks() {
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let ws = create(&store, &locks, &key, "01H8XYZ");
        let marker_dir = td.path().join("hook-markers");
        std::fs::create_dir(&marker_dir).unwrap();
        let before = marker_dir.join("before");
        let after = marker_dir.join("after");
        let exec = SubprocessHookExecutor::default();
        let outcome = cleanup_workspace(CleanupArgs {
            registry: &store,
            locks: &locks,
            workspace_id: &ws.workspace_id,
            caller: CleanupCallerClass::Synchronous,
            hook_executor: &exec,
            before_cleanup: vec![HookSpec {
                phase: HookPhase::BeforeCleanup,
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("touch {}", before.display()),
                ],
                timeout: DEFAULT_HOOK_TIMEOUT,
            }],
            after_cleanup: vec![HookSpec {
                phase: HookPhase::AfterCleanup,
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("touch {}", after.display()),
                ],
                timeout: DEFAULT_HOOK_TIMEOUT,
            }],
        })
        .unwrap();
        assert_eq!(outcome, CleanupOutcome::Cleared);
        assert!(before.exists());
        assert!(after.exists());
    }

    #[test]
    fn cleanup_returns_already_cleared_when_workspace_missing() {
        let td = tempfile::tempdir().unwrap();
        let (store, locks, _key) = fixture(&td);
        let exec = NoopHookExecutor;
        let r = cleanup_workspace(CleanupArgs {
            registry: &store,
            locks: &locks,
            workspace_id: "wsp_nonexistent",
            caller: CleanupCallerClass::Synchronous,
            hook_executor: &exec,
            before_cleanup: vec![],
            after_cleanup: vec![],
        });
        assert!(matches!(r, Err(WorkspaceError::AlreadyCleared)));
    }

    #[test]
    fn cleanup_orphan_reclaim_class_succeeds_on_ready_row() {
        // OrphanReclaim caller class skips the liveness probe (v1 noop)
        // and uses the blocking lock acquisition path.
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let ws = create(&store, &locks, &key, "01H8XYZ");
        let exec = NoopHookExecutor;
        let outcome = cleanup_workspace(CleanupArgs {
            registry: &store,
            locks: &locks,
            workspace_id: &ws.workspace_id,
            caller: CleanupCallerClass::OrphanReclaim,
            hook_executor: &exec,
            before_cleanup: vec![],
            after_cleanup: vec![],
        })
        .unwrap();
        assert_eq!(outcome, CleanupOutcome::Cleared);
    }
}
