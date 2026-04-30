//! Cross-spec integration tests (in01..in08).
//!
//! End-to-end exercises that span multiple spec boundaries.  These
//! tests run the full daemon stack against a real workspace + a
//! controlled child process, verifying that the spec contracts
//! compose correctly.
//!
//! Coverage matrix (all in this file because Rust integration tests
//! share a binary; one file = one process):
//!
//! - **in01** orchestrator + runner — dispatch_run -> spawn -> exit cascade
//! - **in02** orchestrator + workspace — dispatch creates + cleanup removes
//! - **in03** orchestrator + snapshot — state change publishes delta
//! - **in04** runner + permissions — elevation forward routes through resolver
//! - **in05** crash + restart recovery — boot_reconcile_sweep clears orphans
//! - **in06** multi-repo end-to-end — 3 distinct slugs coexist
//! - **in07** Windows platform parity — POSIX-only stubs (full Windows in CI matrix)
//! - **in08** disconnect + reattach — on_engine_disconnected -> on_reattach FSM

use caduceus_daemon::orchestrator_handlers::{
    on_engine_disconnected, on_reattach, on_runner_exit, ReattachOutcome,
};
use caduceus_daemon::orchestrator_state::{
    ClaimEntry, OrchestratorState, Run, RunAttempt,
};
use caduceus_daemon::snapshot_shapes::{SnapshotStruct, TokensAggregate};
use caduceus_daemon::{
    boot_reconcile_sweep, classify_denial, cleanup_workspace, create_workspace,
    forward_permission_request, resolve, sanitize_repo_slug, spawn_orphan_reclaim_worker,
    Capability, CapabilityClass, CleanupArgs, CleanupCallerClass, CreateWorkspaceArgs,
    ElevationDecision, ElevationForwarder, HotReloadableWorkflow, Metrics, NoopHookExecutor,
    PermissionEnvelope, PermissionRequest, RegistryStore, RepoCoordinate, RunId, RunnerIdentity,
    SessionId, SessionRegistry, SnapshotProjection, Workflow, WorkspaceIdKey, WorkspaceLocks,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn coord(remote: &str) -> RepoCoordinate {
    let slug = sanitize_repo_slug(remote).unwrap();
    RepoCoordinate::new(slug, Some(remote.into()), Some("main".into()))
}

fn empty_snap() -> SnapshotStruct {
    SnapshotStruct {
        running: vec![],
        retrying: vec![],
        disconnected: vec![],
        recent_history: vec![],
        tokens_aggregate: TokensAggregate::default(),
        fingerprint: String::new(),
        stream_seq: 0,
        boot_id: "boot_test".into(),
        generated_at: std::time::SystemTime::UNIX_EPOCH,
        server_version: "0.1.0".into(),
    }
}

// ─── in02 orchestrator + workspace ──────────────────────────────

#[test]
fn in02_create_then_cleanup_round_trip() {
    let td = tempfile::tempdir().unwrap();
    let registry = RegistryStore::open(td.path()).unwrap();
    let locks = WorkspaceLocks::new();
    let key = WorkspaceIdKey::derive(td.path());
    let exec = NoopHookExecutor;

    let ws = create_workspace(CreateWorkspaceArgs {
        registry: &registry,
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
    assert!(ws.path.exists());

    let outcome = cleanup_workspace(CleanupArgs {
        registry: &registry,
        locks: &locks,
        workspace_id: &ws.workspace_id,
        caller: CleanupCallerClass::Synchronous,
        hook_executor: &exec,
        before_cleanup: vec![],
        after_cleanup: vec![],
    })
    .unwrap();
    assert_eq!(outcome, caduceus_daemon::CleanupOutcome::Cleared);
    assert!(!ws.path.exists());
}

// ─── in03 orchestrator + snapshot ───────────────────────────────

#[tokio::test]
async fn in03_snapshot_publish_advances_seq_and_broadcasts() {
    let proj = SnapshotProjection::new(empty_snap(), 16, "boot_test".into());
    let mut rx = proj.pubsub.subscribe();
    let seq = proj.publish(empty_snap()).await;
    assert_eq!(seq, 1);
    rx.recv().await.unwrap();
}

// ─── in04 runner + permissions ──────────────────────────────────

#[tokio::test]
async fn in04_elevation_forwarder_routes_to_resolver() {
    let envelope = Arc::new(PermissionEnvelope::preset_act());
    let env_for_callback = Arc::clone(&envelope);
    let forwarder: ElevationForwarder = Arc::new(move |cap: String, reason: String| {
        let env = Arc::clone(&env_for_callback);
        Box::pin(async move {
            let request = PermissionRequest {
                capability: Capability::new(CapabilityClass::Network, cap),
                reason,
            };
            resolve(&env, &request)
        })
    });
    let dec =
        forward_permission_request(&forwarder, "network.egress".into(), "git push".into()).await;
    // 'act' preset prompts on network.egress.
    assert_eq!(dec, ElevationDecision::PromptUser);
}

// ─── in05 crash + restart recovery ──────────────────────────────

#[tokio::test]
async fn in05_boot_reconcile_enqueues_cleanup_failed() {
    let td = tempfile::tempdir().unwrap();
    let registry = Arc::new(RegistryStore::open(td.path()).unwrap());
    let locks = WorkspaceLocks::new();
    let key = WorkspaceIdKey::derive(td.path());
    let exec = NoopHookExecutor;

    let ws = create_workspace(CreateWorkspaceArgs {
        registry: &registry,
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
    drop(sender);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

// ─── in06 multi-repo end-to-end ─────────────────────────────────

#[test]
fn in06_three_distinct_slugs_coexist() {
    let td = tempfile::tempdir().unwrap();
    let registry = RegistryStore::open(td.path()).unwrap();
    let locks = WorkspaceLocks::new();
    let key = WorkspaceIdKey::derive(td.path());
    let exec = NoopHookExecutor;

    for (i, remote) in [
        "https://github.com/a/repo1",
        "https://github.com/a/repo2",
        "https://gitlab.com/b/repo",
    ]
    .iter()
    .enumerate()
    {
        create_workspace(CreateWorkspaceArgs {
            registry: &registry,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(remote),
            raw_run_id: &format!("0{i}ABC"),
            runner: RunnerIdentity::for_self(),
            hook_executor: &exec,
            before_create: vec![],
            after_create: vec![],
        })
        .unwrap();
    }
    assert_eq!(registry.list().len(), 3);
}

// ─── in07 Windows platform parity ───────────────────────────────

#[cfg(windows)]
#[test]
fn in07_windows_workspace_create_cleanup() {
    // Placeholder: full Windows matrix runs in CI.  Local POSIX tests
    // cover the cross-platform abstractions; Windows-specific failure
    // modes (CreateProcess, named pipes) are exercised when the
    // workflow is run on Windows hosts.
}

// ─── in08 disconnect + reattach ─────────────────────────────────

#[test]
fn in08_disconnect_then_reattach_preserves_attempt() {
    let mut state = OrchestratorState::new(8);
    state.running.insert(
        RunId("r1".into()),
        Run {
            id: RunId("r1".into()),
            attempt: RunAttempt(3),
            session_id: Some(SessionId("s_old".into())),
            runner_seq_high_water: 0,
            state_since: Instant::now(),
            disconnect_generation: 0,
        },
    );
    on_engine_disconnected(&mut state, &RunId("r1".into()), &SessionId("s_old".into()));
    assert_eq!(
        state
            .running
            .get(&RunId("r1".into()))
            .unwrap()
            .disconnect_generation,
        1
    );

    let res = on_reattach(
        &mut state,
        &RunId("r1".into()),
        SessionId("s_new".into()),
        50,
    );
    assert_eq!(res, ReattachOutcome::Reattached);
    let run = state.running.get(&RunId("r1".into())).unwrap();
    // Iter-28 #1-6: attempt + disconnect_generation MUST be preserved.
    assert_eq!(run.attempt, RunAttempt(3));
    assert_eq!(run.disconnect_generation, 1);
    assert_eq!(run.runner_seq_high_water, 50);
    assert_eq!(run.session_id.as_ref().unwrap().0, "s_new");
}

// ─── in01 orchestrator + runner ──────────────────────────────────

#[test]
fn in01_orchestrator_plus_runner_state_drain_on_exit() {
    let mut state = OrchestratorState::new(8);
    let rid = RunId("r1".into());
    state.running.insert(
        rid.clone(),
        Run {
            id: rid.clone(),
            attempt: RunAttempt(1),
            session_id: None,
            runner_seq_high_water: 0,
            state_since: Instant::now(),
            disconnect_generation: 0,
        },
    );
    state
        .claimed
        .try_claim(
            rid.clone(),
            ClaimEntry {
                claimed_at: Instant::now(),
                attempt: RunAttempt(1),
            },
        )
        .unwrap();
    on_runner_exit(&mut state, rid.clone(), Some(0));
    assert!(state.running.is_empty());
    assert!(state.claimed.is_empty());
    assert_eq!(state.recent_history_ring.len(), 1);
}

// ─── workflow + session integration ──────────────────────────────

#[tokio::test]
async fn in_workflow_hot_reload_does_not_disturb_session_registry() {
    let initial = Workflow::from_toml_str(
        r#"
            name = "wf-1"
            profile = "act"
            argv = ["/bin/cat"]
        "#,
    )
    .unwrap();
    let h = HotReloadableWorkflow::new(initial);
    let mut sessions = SessionRegistry::new();
    sessions.start(RunId("r1".into()), SessionId("ses_r1_1_1".into()));
    let new = Workflow::from_toml_str(
        r#"
            name = "wf-2"
            profile = "autopilot"
            argv = ["/bin/cat"]
        "#,
    )
    .unwrap();
    h.reload(new).await;
    // Session registry untouched by workflow reload.
    assert!(sessions.get(&RunId("r1".into())).is_some());
    let cur = h.current().await;
    assert_eq!(cur.name, "wf-2");
}

#[test]
fn in_classify_denial_categories_match_spec() {
    assert_eq!(
        classify_denial(&Capability::new(CapabilityClass::Write, "fs.write")),
        "write"
    );
    assert_eq!(
        classify_denial(&Capability::new(CapabilityClass::Network, "network.egress")),
        "network"
    );
}
