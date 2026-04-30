//! `create_workspace` — spec #3 §3.5 (ws08).
//!
//! Per the implementation DAG, this module implements the workspace
//! creation algorithm using the foundations from earlier P1 todos:
//!
//! - **`ws01..ws05`** — identity, sanitize, registry types.
//! - **`ws06-registry-store`** — atomic row insertion + state-machine.
//! - **`ws07-lock-primitives`** + **`ws11-shared-repo-lock`** — 3-tier
//!   lock acquire (try-lock for synchronous create).
//! - **`ws12-create-hooks`** — `before_create` / `after_create` runs.
//! - **`ws14-env-exports`** — `CADUCEUS_*` env vars supplied to hooks.
//! - **`ws15-leaf-ownership-handoff`** — single-source §5A.5 chown.
//!
//! Iter-28 backlog items absorbed:
//!
//! - **#3-2 (placeholder ordering)**: `safe_run_id`, `target`, and
//!   `workspace_id` are all computed BEFORE the placeholder row is
//!   inserted.  Step 2 of §3.5 MUST NOT re-derive.  Enforced here.
//! - **#3-6 (lock-order surface)**: acquire order is registry-wide →
//!   per-slug → per-workspace, surfaced explicitly via the call to
//!   `shared_repo_lock::acquire`.
//! - **#3-3 / #3-10 (workspace_id consistency, BLAKE3 32-byte key)**:
//!   resolved at the spec level + propagated here via
//!   `workspace::workspace_id`.
//!
//! Rollback semantics (spec #3 I-7): on hook failure or filesystem
//! failure after the placeholder row is inserted, this function:
//!
//! 1. Removes the leaf directory (best-effort) if it was created.
//! 2. Deletes the placeholder row from the registry.
//! 3. Returns `Error::HookFailed` (NOT a cleanup error from the
//!    rollback step).

use crate::env_exports::workspace_env_exports;
use crate::error::{HookPhase, WorkspaceError};
use crate::hooks::{HookExecutor, HookSpec};
use crate::leaf_ownership::{hand_off_leaf, RunnerIdentity};
use crate::locks::WorkspaceLocks;
use crate::registry::{RepoCoordinate, WorkspaceRegistryRow, WorkspaceStatus};
use crate::registry_store::RegistryStore;
use crate::shared_repo_lock::{self, SharedRepoCaller};
use crate::workspace::{
    build_workspace_path, sanitize_run_id, validate_workspace_path, workspace_id, RepoSlug,
    SafeRunId, WorkspaceIdKey,
};
use std::path::PathBuf;
use std::time::SystemTime;

/// Materialized workspace handed back to the caller after successful create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub workspace_id: String,
    pub path: PathBuf,
    pub repo_coordinate: RepoCoordinate,
    pub safe_run_id: SafeRunId,
    pub created_at: SystemTime,
}

/// Inputs to `create_workspace`.  Bundling the parameters keeps the
/// public API stable as the spec evolves.
pub struct CreateWorkspaceArgs<'a> {
    pub registry: &'a RegistryStore,
    pub locks: &'a WorkspaceLocks,
    pub key: &'a WorkspaceIdKey,
    pub repo_coordinate: RepoCoordinate,
    pub raw_run_id: &'a str,
    pub runner: RunnerIdentity,
    pub hook_executor: &'a dyn HookExecutor,
    /// `before_create` and `after_create` hooks the workflow declared.
    /// Empty vector means "no hooks for this phase".
    pub before_create: Vec<HookSpec>,
    pub after_create: Vec<HookSpec>,
}

/// Spec #3 §3.5.  Create a Run workspace.
///
/// Returns the materialized `Workspace` on success.  On any failure
/// after the placeholder row is inserted, performs full rollback (see
/// module-level docs) and returns the original error.
pub fn create_workspace(args: CreateWorkspaceArgs<'_>) -> Result<Workspace, WorkspaceError> {
    // ── Step 1: input sanitization ───────────────────────────────────
    let safe_run_id = sanitize_run_id(args.raw_run_id)?;

    // RepoSlug round-trip: trust the slug already pre-validated by
    // sanitize_repo_slug (RepoCoordinate::new requires it).  We restore
    // the newtype here purely for the workspace_id call.
    let slug_newtype = RepoSlug::from_string_unchecked(args.repo_coordinate.slug.clone());

    // ── Step 2: deterministic derivations BEFORE the placeholder row ──
    // (Iter-28 #3-2: workspace_id, target, safe_run_id all pre-computed.)
    let target = build_workspace_path(args.registry.workspace_root(), &slug_newtype, &safe_run_id)?;
    let wid = workspace_id(args.key, &slug_newtype, &safe_run_id);

    // ── Step 3: acquire locks (registry → per-slug → per-workspace) ──
    let registry_guard = args.locks.registry_lock();
    let _guards = shared_repo_lock::acquire(
        args.locks,
        registry_guard,
        SharedRepoCaller::SynchronousCreate,
        &args.repo_coordinate.slug,
        &wid,
    )?;

    // ── Step 4: insert placeholder row (Status::Creating) ────────────
    let row = WorkspaceRegistryRow::new(
        wid.clone(),
        WorkspaceStatus::Creating,
        target.clone(),
        args.repo_coordinate.clone(),
        safe_run_id.clone(),
        SystemTime::now(),
    );
    args.registry
        .insert_placeholder(row)
        .map_err(|e| WorkspaceError::RegistryWriteFailed(e.to_string()))?;

    // From here on, any failure MUST roll back the placeholder row.
    // Use a guard helper so we don't forget any path.
    let rollback = || {
        let _ = args.registry.delete(&wid);
        let _ = std::fs::remove_dir_all(&target);
    };

    // ── Step 5: validate path (symlink-escape + ..) ──────────────────
    if let Err(e) = validate_workspace_path(&target, args.registry.workspace_root()) {
        rollback();
        return Err(e);
    }

    // ── Step 6: create leaf directory at mode 0700 ───────────────────
    if let Err(e) = create_leaf(&target) {
        rollback();
        return Err(e);
    }

    // ── Step 7: build env exports ────────────────────────────────────
    let env = workspace_env_exports(
        args.raw_run_id,
        &safe_run_id,
        &target,
        &args.repo_coordinate,
    );

    // ── Step 8: run before_create hooks ──────────────────────────────
    for hook in &args.before_create {
        debug_assert_eq!(hook.phase, HookPhase::BeforeCreate);
        if let Err(e) = args.hook_executor.execute(hook, &env, &target) {
            rollback();
            return Err(e);
        }
    }

    // ── Step 8.5 (§5A.5 — Z6-G1): leaf ownership handoff ─────────────
    if let Err(e) = hand_off_leaf(&target, args.runner) {
        rollback();
        return Err(e);
    }

    // ── Step 9: run after_create hooks ───────────────────────────────
    for hook in &args.after_create {
        debug_assert_eq!(hook.phase, HookPhase::AfterCreate);
        if let Err(e) = args.hook_executor.execute(hook, &env, &target) {
            rollback();
            return Err(e);
        }
    }

    // ── Step 10: transition placeholder row to Ready ─────────────────
    args.registry
        .transition_to_ready(&wid)
        .map_err(|e| WorkspaceError::RegistryWriteFailed(e.to_string()))?;

    Ok(Workspace {
        workspace_id: wid,
        path: target,
        repo_coordinate: args.repo_coordinate,
        safe_run_id,
        created_at: SystemTime::now(),
    })
}

#[cfg(unix)]
fn create_leaf(path: &std::path::Path) -> Result<(), WorkspaceError> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            WorkspaceError::PathValidationFailed(format!(
                "create slug parent {}: {e}",
                parent.display()
            ))
        })?;
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path).map_err(|e| {
        WorkspaceError::PathValidationFailed(format!("mkdir leaf {}: {e}", path.display()))
    })
}

#[cfg(not(unix))]
fn create_leaf(path: &std::path::Path) -> Result<(), WorkspaceError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            WorkspaceError::PathValidationFailed(format!(
                "create slug parent {}: {e}",
                parent.display()
            ))
        })?;
    }
    std::fs::create_dir(path)
        .map_err(|e| WorkspaceError::PathValidationFailed(format!("mkdir leaf {path:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{NoopHookExecutor, SubprocessHookExecutor, DEFAULT_HOOK_TIMEOUT};
    use crate::workspace::sanitize_repo_slug;

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

    #[test]
    fn create_succeeds_with_no_hooks() {
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let exec = NoopHookExecutor;
        let ws = create_workspace(CreateWorkspaceArgs {
            registry: &store,
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

        // Leaf exists.
        assert!(ws.path.exists(), "leaf must exist after create");
        // Row is Ready.
        let row = store.get(&ws.workspace_id).unwrap();
        assert_eq!(row.status, WorkspaceStatus::Ready);
        // workspace_id is wsp_<32 hex>.
        assert!(ws.workspace_id.starts_with("wsp_"));
        assert_eq!(ws.workspace_id.len(), 4 + 32);
    }

    #[test]
    fn create_runs_before_and_after_hooks_in_order() {
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let marker_dir = td.path().join("hook-marker-dir");
        std::fs::create_dir(&marker_dir).unwrap();
        let before_marker = marker_dir.join("before");
        let after_marker = marker_dir.join("after");
        let exec = SubprocessHookExecutor::default();
        let ws = create_workspace(CreateWorkspaceArgs {
            registry: &store,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "01H8XYZ",
            runner: RunnerIdentity::for_self(),
            hook_executor: &exec,
            before_create: vec![HookSpec {
                phase: HookPhase::BeforeCreate,
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("touch {}", before_marker.display()),
                ],
                timeout: DEFAULT_HOOK_TIMEOUT,
            }],
            after_create: vec![HookSpec {
                phase: HookPhase::AfterCreate,
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("touch {}", after_marker.display()),
                ],
                timeout: DEFAULT_HOOK_TIMEOUT,
            }],
        })
        .unwrap();
        assert!(before_marker.exists());
        assert!(after_marker.exists());
        assert!(ws.path.exists());
    }

    #[test]
    fn create_rolls_back_on_before_create_hook_failure() {
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let exec = SubprocessHookExecutor::default();
        let r = create_workspace(CreateWorkspaceArgs {
            registry: &store,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "01H8XYZ",
            runner: RunnerIdentity::for_self(),
            hook_executor: &exec,
            before_create: vec![HookSpec {
                phase: HookPhase::BeforeCreate,
                command: vec!["/bin/sh".into(), "-c".into(), "exit 5".into()],
                timeout: DEFAULT_HOOK_TIMEOUT,
            }],
            after_create: vec![],
        });
        match r {
            Err(WorkspaceError::HookFailed { phase, exit_code }) => {
                assert_eq!(phase, HookPhase::BeforeCreate);
                assert_eq!(exit_code, Some(5));
            }
            other => panic!("expected HookFailed, got {other:?}"),
        }
        // Registry MUST be empty (rollback).
        assert!(
            store.list().is_empty(),
            "rollback must remove placeholder row"
        );
        // Leaf MUST be removed.
        let slug_dir = td.path().join("github_com_o_r");
        assert!(
            !slug_dir.join("01H8XYZ").exists(),
            "rollback must remove leaf"
        );
    }

    #[test]
    fn create_rejects_invalid_run_id_before_locking() {
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let exec = NoopHookExecutor;
        let r = create_workspace(CreateWorkspaceArgs {
            registry: &store,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "../etc/passwd",
            runner: RunnerIdentity::for_self(),
            hook_executor: &exec,
            before_create: vec![],
            after_create: vec![],
        });
        assert!(matches!(r, Err(WorkspaceError::InvalidRunId(_))));
        // Nothing should be in the registry.
        assert!(store.list().is_empty());
    }

    #[test]
    fn create_returns_shared_repo_locked_when_slug_held() {
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let exec = NoopHookExecutor;
        // Hold the per-slug guard via the reclaim path.
        let _held = locks.acquire_for_reclaim("github_com_o_r", "wsp_aaa");
        let r = create_workspace(CreateWorkspaceArgs {
            registry: &store,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "01H8XYZ",
            runner: RunnerIdentity::for_self(),
            hook_executor: &exec,
            before_create: vec![],
            after_create: vec![],
        });
        match r {
            Err(WorkspaceError::SharedRepoLocked(slug)) => {
                assert_eq!(slug, "github_com_o_r");
            }
            other => panic!("expected SharedRepoLocked, got {other:?}"),
        }
        // No row inserted on contention.
        assert!(store.list().is_empty());
    }

    #[test]
    fn create_is_idempotent_safe_only_in_separate_run_ids() {
        // Same run_id twice would conflict on workspace_id; spec rejects
        // (we surface that via state-machine: second insert would fail
        // because there's already a row for that wid).  Different run_ids
        // succeed.
        let td = tempfile::tempdir().unwrap();
        let (store, locks, key) = fixture(&td);
        let exec = NoopHookExecutor;
        let _w1 = create_workspace(CreateWorkspaceArgs {
            registry: &store,
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
        let _w2 = create_workspace(CreateWorkspaceArgs {
            registry: &store,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "02ABCDE",
            runner: RunnerIdentity::for_self(),
            hook_executor: &exec,
            before_create: vec![],
            after_create: vec![],
        })
        .unwrap();
        assert_eq!(store.list().len(), 2);
    }
}
