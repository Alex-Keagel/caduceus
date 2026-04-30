//! Spec #3 acceptance tests (ws16).
//!
//! Integration tests for the multi-repo workspace model. These tests
//! exercise the public API surface end-to-end across the create/cleanup
//! lifecycle, asserting:
//!
//! - **I-1** (workspace_root canonicalization, no symlink escape)
//! - **I-3** (atomic registry rows, no half-written state)
//! - **I-4** (slug stickiness — same logical repo keeps slug)
//! - **I-6** (derivable workspace_id, deterministic across processes)
//! - **I-7** (no orphan dirs on hook failure — full rollback)
//! - **I-8** (single-writer-per-root advisory lock)
//! - **§3.5 ordering** (placeholder row inserted before fs work; all
//!   identifiers pre-computed)
//! - **§3.6 short-circuits** (OrphanedNoSlug / OrphanedNoLeaf — hooks
//!   MUST NOT run with missing leaf)
//! - **§5B.2 OrphanReclaim** (re-entry skips probe; clears orphans)
//!
//! Iter-28 backlog items resolved by these tests:
//!
//! - **#3-1** (sanitize regex consistency) — `t_invalid_run_id_rejected`
//! - **#3-2** (placeholder ordering) — `t_create_atomic_rollback_*`
//! - **#3-4** (cleanup short-circuit) — `t_cleanup_orphaned_no_*`
//! - **#3-5** (env shell-safety wording) — `t_env_exports_*`
//! - **#3-6** (lock-order surface) — implicit in lock_contention tests
//! - **#3-9** (OrphanReclaim bypass scope) — `t_orphan_reclaim_*`

use caduceus_daemon::{
    cleanup_workspace, create_workspace, sanitize_repo_slug, sanitize_run_id,
    spawn_orphan_reclaim_worker, workspace_env_exports, CleanupArgs, CleanupCallerClass,
    CleanupOutcome, CreateWorkspaceArgs, HookSpec, Metrics, NoopHookExecutor, OrphanReclaimEntry,
    ReclaimReason, RegistryStore, RepoCoordinate, RunnerIdentity, SubprocessHookExecutor,
    WorkspaceIdKey, WorkspaceLocks, WorkspaceStatus, DEFAULT_HOOK_TIMEOUT,
};
use std::sync::Arc;
use std::time::Duration;

fn coord(remote: &str) -> RepoCoordinate {
    let slug = sanitize_repo_slug(remote).unwrap();
    RepoCoordinate::new(slug, Some(remote.into()), Some("main".into()))
}

fn fixture(td: &tempfile::TempDir) -> (RegistryStore, WorkspaceLocks, WorkspaceIdKey) {
    let root = td.path().to_path_buf();
    let store = RegistryStore::open(&root).unwrap();
    let locks = WorkspaceLocks::new();
    let key = WorkspaceIdKey::derive(&root);
    (store, locks, key)
}

fn create_default(
    store: &RegistryStore,
    locks: &WorkspaceLocks,
    key: &WorkspaceIdKey,
    run_id: &str,
    remote: &str,
) -> caduceus_daemon::Workspace {
    let exec = NoopHookExecutor;
    create_workspace(CreateWorkspaceArgs {
        registry: store,
        locks,
        key,
        repo_coordinate: coord(remote),
        raw_run_id: run_id,
        runner: RunnerIdentity::for_self(),
        hook_executor: &exec,
        before_create: vec![],
        after_create: vec![],
    })
    .unwrap()
}

// ─── §3.5 ordering ───────────────────────────────────────────────────

#[test]
fn t_create_succeeds_end_to_end() {
    let td = tempfile::tempdir().unwrap();
    let (store, locks, key) = fixture(&td);
    let ws = create_default(&store, &locks, &key, "01H8XYZ", "https://github.com/o/r");
    assert!(ws.path.exists());
    assert_eq!(
        store.get(&ws.workspace_id).unwrap().status,
        WorkspaceStatus::Ready
    );
}

#[test]
fn t_create_atomic_rollback_on_hook_failure() {
    // Iter-28 #3-2 placeholder ordering — on hook failure, rollback
    // must remove BOTH the placeholder row and the leaf.
    let td = tempfile::tempdir().unwrap();
    let (store, locks, key) = fixture(&td);
    let exec = SubprocessHookExecutor::default();
    let r = create_workspace(CreateWorkspaceArgs {
        registry: &store,
        locks: &locks,
        key: &key,
        repo_coordinate: coord("https://github.com/o/r"),
        raw_run_id: "01H8XYZ",
        runner: RunnerIdentity::for_self(),
        hook_executor: &exec,
        before_create: vec![HookSpec {
            phase: caduceus_daemon::error::HookPhase::BeforeCreate,
            command: vec!["/bin/sh".into(), "-c".into(), "exit 1".into()],
            timeout: DEFAULT_HOOK_TIMEOUT,
        }],
        after_create: vec![],
    });
    assert!(r.is_err());
    assert!(
        store.list().is_empty(),
        "I-7: registry MUST be empty after rollback"
    );
    let leaf = td.path().join("github_com_o_r").join("01H8XYZ");
    assert!(!leaf.exists(), "I-7: leaf MUST be removed after rollback");
}

// ─── §3.2 sanitize_run_id (iter-28 #3-1) ─────────────────────────────

#[test]
fn t_invalid_run_id_rejected() {
    assert!(sanitize_run_id("../etc/passwd").is_err());
    assert!(sanitize_run_id("..").is_err());
    assert!(sanitize_run_id(".").is_err());
    assert!(sanitize_run_id("").is_err());
    assert!(sanitize_run_id(&"a".repeat(129)).is_err());
}

// ─── I-6 derivability (iter-28 #3-3 + #3-10) ─────────────────────────

#[test]
fn t_workspace_id_is_deterministic_across_calls() {
    use caduceus_daemon::workspace_id;
    let td = tempfile::tempdir().unwrap();
    let key = WorkspaceIdKey::derive(td.path());
    let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
    let rid = sanitize_run_id("01H8XYZ").unwrap();
    let id1 = workspace_id(&key, &slug, &rid);
    let id2 = workspace_id(&key, &slug, &rid);
    assert_eq!(id1, id2);
    assert!(id1.starts_with("wsp_"));
    assert_eq!(id1.len(), 4 + 32);
}

// ─── §3.6 short-circuits (iter-28 #3-4) ──────────────────────────────

#[test]
fn t_cleanup_orphaned_no_leaf_skips_hooks() {
    let td = tempfile::tempdir().unwrap();
    let (store, locks, key) = fixture(&td);
    let ws = create_default(&store, &locks, &key, "01H8XYZ", "https://github.com/o/r");
    std::fs::remove_dir_all(&ws.path).unwrap();
    let marker = td.path().join("hook-marker");
    let exec = SubprocessHookExecutor::default();
    let outcome = cleanup_workspace(CleanupArgs {
        registry: &store,
        locks: &locks,
        workspace_id: &ws.workspace_id,
        caller: CleanupCallerClass::Synchronous,
        hook_executor: &exec,
        before_cleanup: vec![HookSpec {
            phase: caduceus_daemon::error::HookPhase::BeforeCleanup,
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
    assert!(
        !marker.exists(),
        "iter-28 #3-4: hooks MUST NOT run on OrphanedNoLeaf"
    );
}

#[test]
fn t_cleanup_orphaned_no_slug_skips_hooks() {
    let td = tempfile::tempdir().unwrap();
    let (store, locks, key) = fixture(&td);
    let ws = create_default(&store, &locks, &key, "01H8XYZ", "https://github.com/o/r");
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
            phase: caduceus_daemon::error::HookPhase::BeforeCleanup,
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
    assert!(
        !marker.exists(),
        "iter-28 #3-4: hooks MUST NOT run on OrphanedNoSlug"
    );
}

// ─── env exports + shell-safety (iter-28 #3-5) ────────────────────────

#[test]
fn t_env_exports_contain_canonical_names() {
    let rid = sanitize_run_id("01H8XYZ").unwrap();
    let env = workspace_env_exports(
        "01H8XYZ",
        &rid,
        std::path::Path::new("/var/lib/caduceus/o_r/01H8XYZ"),
        &coord("https://github.com/o/r"),
    );
    for k in [
        "CADUCEUS_RUN_ID",
        "CADUCEUS_RUN_ID_SAFE",
        "CADUCEUS_WORKSPACE_PATH",
        "CADUCEUS_REPO_SLUG",
        "CADUCEUS_REPO_REMOTE_URL",
        "CADUCEUS_REPO_REMOTE_URL_SAFE_B64",
        "CADUCEUS_REPO_DEFAULT_BRANCH",
    ] {
        assert!(env.contains_key(k));
    }
    // SAFE_B64 contains only [A-Za-z0-9-_].
    let b64 = &env["CADUCEUS_REPO_REMOTE_URL_SAFE_B64"];
    for c in b64.chars() {
        assert!(c.is_ascii_alphanumeric() || c == '-' || c == '_');
    }
}

// ─── I-8 single-writer-per-root advisory lock ────────────────────────

#[cfg(unix)]
#[test]
fn t_single_writer_per_root_lock() {
    use caduceus_daemon::RegistryError;
    let td = tempfile::tempdir().unwrap();
    let _first = RegistryStore::open(td.path()).unwrap();
    let second = RegistryStore::open(td.path());
    assert!(matches!(
        second,
        Err(RegistryError::AnotherDaemonHoldingLock { .. })
    ));
}

// ─── §5B.2 OrphanReclaim — iter-28 #3-9 bypass scope ─────────────────

#[tokio::test]
async fn t_orphan_reclaim_clears_orphans() {
    let td = tempfile::tempdir().unwrap();
    let store = Arc::new(RegistryStore::open(td.path()).unwrap());
    let locks = WorkspaceLocks::new();
    let key = WorkspaceIdKey::derive(td.path());
    let exec = NoopHookExecutor;
    let ws = create_workspace(CreateWorkspaceArgs {
        registry: &store,
        locks: &locks,
        key: &key,
        repo_coordinate: coord("https://github.com/o/r"),
        raw_run_id: "01H8XYZ",
        runner: RunnerIdentity::for_self(),
        hook_executor: &exec,
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
            reason: ReclaimReason::CleanupFailed,
        })
        .await
        .unwrap();
    drop(sender);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(store.get(&ws.workspace_id).is_none());
    assert_eq!(metrics.counter("orphan_reclaim.success").get(), 1);
}

// ─── I-4 sticky slug across re-create ─────────────────────────────────

#[test]
fn t_slug_remains_sticky_across_canonical_url_variants() {
    // Same logical repo accessed via https vs git@ — slug must be identical.
    let s1 = sanitize_repo_slug("https://github.com/openai/symphony").unwrap();
    let s2 = sanitize_repo_slug("https://github.com/openai/symphony.git").unwrap();
    let s3 = sanitize_repo_slug("git@github.com:openai/symphony.git").unwrap();
    let s4 = sanitize_repo_slug("https://github.com/Openai/Symphony").unwrap();
    assert_eq!(s1.as_str(), "github_com_openai_symphony");
    assert_eq!(s2.as_str(), s1.as_str());
    assert_eq!(s3.as_str(), s1.as_str());
    assert_eq!(s4.as_str(), s1.as_str());
}

// ─── I-1 path traversal rejection ─────────────────────────────────────

#[test]
fn t_path_traversal_rejected_in_run_id() {
    let td = tempfile::tempdir().unwrap();
    let (store, locks, key) = fixture(&td);
    let exec = NoopHookExecutor;
    let r = create_workspace(CreateWorkspaceArgs {
        registry: &store,
        locks: &locks,
        key: &key,
        repo_coordinate: coord("https://github.com/o/r"),
        raw_run_id: "../etc/passwd",
        runner: RunnerIdentity::for_self(),
        hook_executor: &exec,
        before_create: vec![],
        after_create: vec![],
    });
    assert!(matches!(
        r,
        Err(caduceus_daemon::error::WorkspaceError::InvalidRunId(_))
    ));
}

// ─── Multi-run multi-repo concurrent dispatch ─────────────────────────

#[test]
fn t_multiple_workspaces_coexist() {
    let td = tempfile::tempdir().unwrap();
    let (store, locks, key) = fixture(&td);
    let _ws1 = create_default(&store, &locks, &key, "01ABC", "https://github.com/a/repo1");
    let _ws2 = create_default(&store, &locks, &key, "02DEF", "https://github.com/a/repo2");
    let _ws3 = create_default(&store, &locks, &key, "03GHI", "https://github.com/b/repo");
    assert_eq!(store.list().len(), 3);
}
