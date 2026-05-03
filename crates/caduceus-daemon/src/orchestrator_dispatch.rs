//! `dispatch_run` 4-phase pipeline (or11a + or11b + or11c + or11d).
//!
//! Per the implementation DAG, this is the keystone dispatch sequence
//! that takes a `Run` from "WorkSource says do this" to "running entry
//! recorded; runner has its first turn".  The pipeline is split into 4
//! sub-phases per the iter-28 review:
//!
//! 1. **`or11a-dispatch-run-preflight`** — revalidate + claim +
//!    trust-boundary check.  Atomic.  Rolls back claimed on failure.
//! 2. **`or11b-dispatch-run-workspace`** — `create_workspace` (§3.5)
//!    with hooks + env exports.  Rolls back claimed + workspace on
//!    failure.
//! 3. **`or11c-dispatch-run-spawn`** — `shell_wrap` validation +
//!    runner spawn + wire codec + ACP negotiation.  Rolls back
//!    claimed + workspace + cleanup partial process on failure.
//! 4. **`or11d-dispatch-run-commit`** — record running entry + reset
//!    Z-9 livelock counter + emit `dispatch_succeeded`.  Terminal.
//!    Dependents wire here, not on or11a/b/c.
//!
//! Iter-28 backlog absorbed:
//!
//! - **#1-1** (trust boundary) — type-level via mailbox senders; we
//!   accept the result of that gate via the `Cmd` we're dispatching for.
//! - **#1-2** (RunAttempt monotonicity) — preserved by reading from
//!   `recent_history_ring` to set the next attempt number.
//! - **#1-4** (eligible_for_dispatch) — used as defensive pre-filter
//!   in or11a; revalidate is the authoritative gate.
//! - **#3-2** (placeholder ordering) — `create_workspace` already
//!   pre-computes safe_run_id / target / workspace_id before the
//!   placeholder row is inserted.

use crate::create_workspace::{create_workspace, CreateWorkspaceArgs, Workspace};
use crate::error::WorkspaceError;
use crate::hooks::{HookExecutor, HookSpec};
use crate::leaf_ownership::RunnerIdentity;
use crate::locks::WorkspaceLocks;
use crate::mailbox::SessionId;
use crate::orchestrator_state::{
    eligible_for_dispatch, revalidate, ClaimEntry, OrchestratorState, RevalidateOutcome, Run,
    RunAttempt, WorkSource,
};
use crate::registry::RepoCoordinate;
use crate::registry_store::RegistryStore;
use crate::runner_process::{RunnerProcess, SpawnSpec};
use crate::workspace::WorkspaceIdKey;
use std::sync::Arc;
use std::time::Instant;

/// Outcome of a `dispatch_run` invocation.  Spec #1 §3.3.
#[derive(Debug)]
pub enum DispatchResult {
    /// Spawn complete; running entry recorded; runner is producing.
    Spawned {
        run_id: crate::mailbox::RunId,
        attempt: RunAttempt,
        workspace: Workspace,
        runner: Arc<RunnerProcess>,
    },
    /// Run was no longer eligible at revalidate time; do NOT retry.
    Skipped,
    /// Recoverable spawn-time failure (e.g., `WorkspaceUnavailable`).
    /// Caller increments `dispatch_defer_attempts` and may reschedule.
    /// Z-9 livelock guard fires at threshold.
    Deferred(DispatchDeferReason),
    /// Non-recoverable spawn-time failure.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchDeferReason {
    WorkspaceUnavailable(String),
    SpawnFailed(String),
    RevalidateRaced,
    ConcurrencyCap,
}

impl std::fmt::Display for DispatchDeferReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchDeferReason::WorkspaceUnavailable(s) => {
                write!(f, "workspace_unavailable: {s}")
            }
            DispatchDeferReason::SpawnFailed(s) => write!(f, "spawn_failed: {s}"),
            DispatchDeferReason::RevalidateRaced => write!(f, "revalidate_raced"),
            DispatchDeferReason::ConcurrencyCap => write!(f, "concurrency_cap"),
        }
    }
}

/// Inputs to `dispatch_run`.
pub struct DispatchRunArgs<'a> {
    pub state: &'a mut OrchestratorState,
    pub work_source: &'a dyn WorkSource,
    pub registry: &'a RegistryStore,
    pub locks: &'a WorkspaceLocks,
    pub key: &'a WorkspaceIdKey,
    pub repo_coordinate: RepoCoordinate,
    pub raw_run_id: String,
    pub run: Run,
    pub runner: RunnerIdentity,
    pub spawn_spec: SpawnSpec,
    pub hook_executor: &'a dyn HookExecutor,
    pub before_create: Vec<HookSpec>,
    pub after_create: Vec<HookSpec>,
    /// Configured `max_concurrency` (orchestrator config).
    pub max_concurrency: usize,
}

/// Spec #1 §3.3 — dispatch_run pipeline.  Returns at the first failure.
pub async fn dispatch_run(args: DispatchRunArgs<'_>) -> DispatchResult {
    // ── or11a: preflight (revalidate + claim) ────────────────────────
    let preflight = preflight(
        args.state,
        args.work_source,
        &args.run,
        args.max_concurrency,
    );
    let attempt = match preflight {
        PreflightOutcome::Ok(attempt) => attempt,
        PreflightOutcome::Skipped => return DispatchResult::Skipped,
        PreflightOutcome::Deferred(r) => return DispatchResult::Deferred(r),
    };

    // ── or11b: workspace acquisition ──────────────────────────────────
    let workspace = match acquire_workspace(
        args.registry,
        args.locks,
        args.key,
        args.repo_coordinate.clone(),
        &args.raw_run_id,
        args.runner,
        args.hook_executor,
        args.before_create,
        args.after_create,
    ) {
        Ok(ws) => ws,
        Err(e) => {
            // Roll back the claim from or11a.
            args.state.claimed.release(&args.run.id);
            return DispatchResult::Deferred(map_workspace_err(e));
        }
    };

    // ── or11c: runner spawn ───────────────────────────────────────────
    let runner = match spawn_runner(args.spawn_spec).await {
        Ok(r) => r,
        Err(e) => {
            // Roll back workspace + claim.
            let _ = std::fs::remove_dir_all(&workspace.path);
            let _ = args.registry.delete(&workspace.workspace_id);
            args.state.claimed.release(&args.run.id);
            return DispatchResult::Deferred(DispatchDeferReason::SpawnFailed(e));
        }
    };

    // ── or11d: commit (record running entry + reset livelock counter) ─
    commit(args.state, &args.run, attempt, &runner);

    DispatchResult::Spawned {
        run_id: args.run.id,
        attempt,
        workspace,
        runner,
    }
}

// ──────────────────────────── or11a preflight ─────────────────────────

#[derive(Debug)]
enum PreflightOutcome {
    Ok(RunAttempt),
    Skipped,
    Deferred(DispatchDeferReason),
}

fn preflight(
    state: &mut OrchestratorState,
    work_source: &dyn WorkSource,
    run: &Run,
    max_concurrency: usize,
) -> PreflightOutcome {
    // Iter-28 #1-4: defensive pre-filter (eligible_for_dispatch).
    if !eligible_for_dispatch(work_source, run) {
        return PreflightOutcome::Skipped;
    }
    // Authoritative gate: revalidate at spawn time.
    match revalidate(work_source, run) {
        RevalidateOutcome::Active => {}
        RevalidateOutcome::Skipped => return PreflightOutcome::Skipped,
        RevalidateOutcome::WorkspaceUnavailable => {
            return PreflightOutcome::Deferred(DispatchDeferReason::WorkspaceUnavailable(
                "revalidate".into(),
            ));
        }
        RevalidateOutcome::SpawnFailed(e) => {
            return PreflightOutcome::Deferred(DispatchDeferReason::SpawnFailed(e));
        }
    }
    // Concurrency gate.
    if state.claimed.would_exceed(max_concurrency) {
        return PreflightOutcome::Deferred(DispatchDeferReason::ConcurrencyCap);
    }
    // Compute next attempt: prior history's final_attempt + 1, or 1
    // if no retained history (iter-28 #1-2 monotonicity caveat).
    let next_attempt = match state.recent_history_ring.most_recent(&run.id) {
        Some(h) => RunAttempt(h.final_attempt.0 + 1),
        None => match state.retry_attempts.get(&run.id) {
            Some(r) => RunAttempt(r.attempt.0 + 1),
            None => RunAttempt(1),
        },
    };
    let claim_entry = ClaimEntry {
        claimed_at: Instant::now(),
        attempt: next_attempt,
    };
    if state
        .claimed
        .try_claim(run.id.clone(), claim_entry)
        .is_err()
    {
        // Race: another tick already claimed this Run.
        return PreflightOutcome::Deferred(DispatchDeferReason::RevalidateRaced);
    }
    PreflightOutcome::Ok(next_attempt)
}

// ──────────────────────────── or11b workspace ─────────────────────────

#[allow(clippy::too_many_arguments)]
fn acquire_workspace(
    registry: &RegistryStore,
    locks: &WorkspaceLocks,
    key: &WorkspaceIdKey,
    repo_coordinate: RepoCoordinate,
    raw_run_id: &str,
    runner: RunnerIdentity,
    hook_executor: &dyn HookExecutor,
    before_create: Vec<HookSpec>,
    after_create: Vec<HookSpec>,
) -> Result<Workspace, WorkspaceError> {
    create_workspace(CreateWorkspaceArgs {
        registry,
        locks,
        key,
        repo_coordinate,
        raw_run_id,
        runner,
        hook_executor,
        before_create,
        after_create,
    })
}

fn map_workspace_err(e: WorkspaceError) -> DispatchDeferReason {
    match e {
        WorkspaceError::SharedRepoLocked(slug) => {
            DispatchDeferReason::WorkspaceUnavailable(format!("shared_repo_locked: {slug}"))
        }
        other => DispatchDeferReason::WorkspaceUnavailable(other.to_string()),
    }
}

// ──────────────────────────── or11c spawn ─────────────────────────────

async fn spawn_runner(spec: SpawnSpec) -> Result<Arc<RunnerProcess>, String> {
    RunnerProcess::spawn(spec).await.map_err(|e| e.to_string())
}

// ──────────────────────────── or11d commit ────────────────────────────

fn commit(
    state: &mut OrchestratorState,
    run: &Run,
    attempt: RunAttempt,
    _runner: &Arc<RunnerProcess>,
) {
    // Reset Z-9 livelock counter on successful dispatch.
    state.dispatch_defer_attempts.reset(&run.id);
    // Insert into running map.
    let mut running_run = run.clone();
    running_run.attempt = attempt;
    running_run.state_since = Instant::now();
    running_run.session_id = Some(SessionId(format!("ses_{}_{}", run.id.0, attempt.0)));
    state.running.insert(run.id.clone(), running_run);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::RealClock;
    use crate::hooks::NoopHookExecutor;
    use crate::orchestrator_state::TrackerClass;
    use crate::workspace::sanitize_repo_slug;

    struct AlwaysActive;
    impl WorkSource for AlwaysActive {
        fn classify(&self, _run: &Run) -> TrackerClass {
            TrackerClass::Active
        }
    }

    struct AlwaysMissing;
    impl WorkSource for AlwaysMissing {
        fn classify(&self, _run: &Run) -> TrackerClass {
            TrackerClass::Missing
        }
    }

    fn fixture_run(id: &str) -> Run {
        Run {
            id: crate::mailbox::RunId(id.to_string()),
            attempt: RunAttempt(1),
            session_id: None,
            runner_seq_high_water: 0,
            state_since: Instant::now(),
            disconnect_generation: 0,
        }
    }

    fn coord() -> RepoCoordinate {
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        RepoCoordinate::new(
            slug,
            Some("https://github.com/o/r".into()),
            Some("main".into()),
        )
    }

    fn make_spec(argv: Vec<&str>) -> SpawnSpec {
        let (g, e1, e2) = SpawnSpec::default_budgets();
        SpawnSpec {
            argv: argv.into_iter().map(String::from).collect(),
            cwd: std::env::temp_dir(),
            env: Default::default(),
            grace_period: g,
            epsilon_1: e1,
            epsilon_2: e2,
            shell_wrapped: false,
        }
    }

    #[tokio::test]
    async fn dispatch_skipped_when_revalidate_returns_missing() {
        let td = tempfile::tempdir().unwrap();
        let registry = RegistryStore::open(td.path()).unwrap();
        let locks = WorkspaceLocks::new();
        let key = WorkspaceIdKey::derive(td.path());
        let mut state = OrchestratorState::new(8);
        let exec = NoopHookExecutor;
        let r = dispatch_run(DispatchRunArgs {
            state: &mut state,
            work_source: &AlwaysMissing,
            registry: &registry,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "01H8XYZ".into(),
            run: fixture_run("r1"),
            runner: RunnerIdentity::for_self(),
            spawn_spec: make_spec(vec!["/bin/cat"]),
            hook_executor: &exec,
            before_create: vec![],
            after_create: vec![],
            max_concurrency: 4,
        })
        .await;
        assert!(matches!(r, DispatchResult::Skipped));
        // No state mutation on skip.
        assert!(state.claimed.is_empty());
    }

    #[tokio::test]
    async fn dispatch_deferred_on_concurrency_cap() {
        let td = tempfile::tempdir().unwrap();
        let registry = RegistryStore::open(td.path()).unwrap();
        let locks = WorkspaceLocks::new();
        let key = WorkspaceIdKey::derive(td.path());
        let mut state = OrchestratorState::new(8);
        // Pre-fill the claimed map up to capacity.
        for i in 0..4 {
            let _ = state.claimed.try_claim(
                crate::mailbox::RunId(format!("pre{i}")),
                ClaimEntry {
                    claimed_at: Instant::now(),
                    attempt: RunAttempt(1),
                },
            );
        }
        let exec = NoopHookExecutor;
        let r = dispatch_run(DispatchRunArgs {
            state: &mut state,
            work_source: &AlwaysActive,
            registry: &registry,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "01H8XYZ".into(),
            run: fixture_run("r1"),
            runner: RunnerIdentity::for_self(),
            spawn_spec: make_spec(vec!["/bin/cat"]),
            hook_executor: &exec,
            before_create: vec![],
            after_create: vec![],
            max_concurrency: 4,
        })
        .await;
        match r {
            DispatchResult::Deferred(DispatchDeferReason::ConcurrencyCap) => {}
            other => panic!("expected Deferred(ConcurrencyCap), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_spawned_records_running_entry_and_resets_livelock() {
        let td = tempfile::tempdir().unwrap();
        let registry = RegistryStore::open(td.path()).unwrap();
        let locks = WorkspaceLocks::new();
        let key = WorkspaceIdKey::derive(td.path());
        let mut state = OrchestratorState::new(8);
        // Pre-set a livelock counter for this run; commit MUST reset it.
        let rid = crate::mailbox::RunId("r1".into());
        state.dispatch_defer_attempts.incr(&rid);
        state.dispatch_defer_attempts.incr(&rid);

        let exec = NoopHookExecutor;
        let _clock = RealClock;
        let r = dispatch_run(DispatchRunArgs {
            state: &mut state,
            work_source: &AlwaysActive,
            registry: &registry,
            locks: &locks,
            key: &key,
            repo_coordinate: coord(),
            raw_run_id: "01H8XYZ".into(),
            run: fixture_run("r1"),
            runner: RunnerIdentity::for_self(),
            spawn_spec: make_spec(vec!["/bin/cat"]),
            hook_executor: &exec,
            before_create: vec![],
            after_create: vec![],
            max_concurrency: 4,
        })
        .await;
        let spawned = match r {
            DispatchResult::Spawned {
                run_id,
                attempt,
                runner,
                ..
            } => {
                assert_eq!(run_id.0, "r1");
                assert_eq!(attempt, RunAttempt(1));
                runner
            }
            other => panic!("expected Spawned, got {other:?}"),
        };
        // Running entry recorded.
        assert!(state.running.contains_key(&rid));
        // Livelock counter reset.
        assert_eq!(state.dispatch_defer_attempts.get(&rid), 0);
        // Cleanup.
        let _ = spawned
            .stop_cascade(crate::runner_process::StopReason::GracefulShutdown)
            .await;
    }

    #[test]
    fn dispatch_defer_reason_display_strings() {
        assert_eq!(
            DispatchDeferReason::ConcurrencyCap.to_string(),
            "concurrency_cap"
        );
        assert_eq!(
            DispatchDeferReason::RevalidateRaced.to_string(),
            "revalidate_raced"
        );
        assert!(DispatchDeferReason::SpawnFailed("oom".into())
            .to_string()
            .starts_with("spawn_failed"));
    }
}
