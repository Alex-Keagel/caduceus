//! caduceusd — orchestrator daemon for the caduceus engine.
//!
//! This crate implements the orchestration algorithm specified in
//! `docs/specs/spec-caduceus-orchestrator-algorithm.md`, the agent runner
//! contract from `docs/specs/spec-caduceus-agent-runner-contract.md`, the
//! workspace model from `docs/specs/spec-multi-repo-workspace-model.md`,
//! and the snapshot surface from `docs/specs/spec-orchestrator-status-snapshot.md`.
//!
//! The daemon is a separate OS process from the caduceus engine.  Engine
//! sessions connect to it over a local IPC transport (UDS on POSIX, named
//! pipe on Windows) and exchange `Cmd::*` messages with capability-scoped
//! sender handles per producer class (timer / subsystem / snapshot-client /
//! authenticated-engine).  See `error::DaemonError` for the canonical error
//! taxonomy and `lifecycle` for the boot / drain / shutdown state machine.
//!
//! Implementation status: **P0 foundations** (`f01-daemon-scaffold`,
//! `f02-config-loader`, `f03-error-types`) per the implementation DAG.

pub mod cleanup_workspace;
pub mod clock;
pub mod config;
pub mod create_workspace;
pub mod env_exports;
pub mod error;
pub mod forward;
pub mod hooks;
pub mod inbound_queue;
pub mod ipc;
pub mod leaf_ownership;
pub mod lifecycle;
pub mod locks;
pub mod mailbox;
pub mod orchestrator_dispatch;
pub mod orchestrator_handlers;
pub mod orchestrator_loop;
pub mod orchestrator_state;
pub mod orphan_reclaim;
pub mod registry;
pub mod registry_store;
pub mod runner_extras;
pub mod runner_process;
pub mod shared_repo_lock;
pub mod storage;
pub mod telemetry;
pub mod test_harness;
pub mod wire_codec;
pub mod workspace;

pub use cleanup_workspace::{cleanup_workspace, CleanupArgs, CleanupCallerClass, CleanupOutcome};
pub use clock::{Clock, RealClock, SharedClock, VirtualClock};
pub use config::{Config, ConfigError};
pub use create_workspace::{create_workspace, CreateWorkspaceArgs, Workspace};
pub use env_exports::workspace_env_exports;
pub use error::{DaemonError, DaemonResult};
pub use forward::{
    forward_to_daemon, observe_heartbeat, reconcile_absolute, reconcile_delta,
    spawn_heartbeat_emit, spawn_heartbeat_timeout_tracker, RunAccounting, RunTokens, StampedFrame,
};
pub use hooks::{
    HookExecutor, HookOutcome, HookSpec, NoopHookExecutor, SubprocessHookExecutor,
    DEFAULT_HOOK_TIMEOUT,
};
pub use inbound_queue::{
    classify_seq, inbound_queue, FrameIdAllocator, InboundQueue, InboundReceiver, RunnerSeqCounter,
    SeqClassification,
};
pub use leaf_ownership::{hand_off_leaf, RunnerIdentity};
pub use lifecycle::{Lifecycle, LifecycleState, ShutdownReason};
pub use locks::{CreateGuards, RegistryGuard, WorkspaceLocks};
pub use mailbox::{
    Cmd, EngineSender, MailboxError, MailboxFactory, Receiver, RetryToken, RunId, SessionId,
    SnapshotClientSender, SubsystemSender, TimerSender,
};
pub use orchestrator_dispatch::{
    dispatch_run, DispatchDeferReason, DispatchResult, DispatchRunArgs,
};
pub use orchestrator_handlers::{
    cmd_reattach, on_disconnect_timer_expired, on_engine_disconnected, on_reattach, on_retry_timer,
    on_runner_exit, on_shutdown, on_snapshot_request, on_token_update, on_workflow_reloaded,
    ReattachOutcome, RetryFireOutcome,
};
pub use orchestrator_loop::{
    boot_reconcile_sweep, run_dispatch_loop, BootReconcileSummary, DispatchLoopOutcome,
};
pub use orchestrator_state::{
    eligible_for_dispatch, revalidate, ClaimEntry, ClaimedMap, DispatchDeferAttempts,
    OrchestratorState, RecentHistoryRing, RetryEntry, RetryTokenIssuer, RevalidateOutcome, Run,
    RunAttempt, RunHistory, RunIdentity, TrackerClass, TrustBoundaryGate, WorkSource,
};
pub use orphan_reclaim::{
    spawn_orphan_reclaim_worker, OrphanReclaimEntry, OrphanReclaimSender, ReclaimReason,
};
pub use registry::{RepoCoordinate, WorkspaceRegistryRow, WorkspaceStatus};
pub use registry_store::{RegistryError, RegistryStore};
pub use runner_extras::{
    cascade_for_drop, drop_reason_to_stop_reason, forward_permission_request, AcpAdapter,
    ElevationDecision, ElevationForwarder, LifecycleSession, RunnerProtocol, SessionState,
    StubAcpAdapter,
};
pub use runner_process::{
    validate_shell_wrap, CascadeOutcome, CascadeStage, RunnerError, RunnerProcess, RunnerState,
    SpawnSpec, StopReason,
};
pub use shared_repo_lock::{SharedRepoCaller, SharedRepoLockStrategy};
pub use storage::{atomic_write, JsonRowStore, Row, StorageError, StorageResult};
pub use telemetry::{init_tracing, Counter, Metrics};
pub use wire_codec::{
    decode_line, encode_frame, DropReason, ExitKind, Frame, FrameId, FramePayload, TokenMode,
    TokensAbsolute, MAX_FRAME_BYTES,
};
pub use workspace::{
    build_workspace_path, sanitize_repo_slug, sanitize_run_id, validate_workspace_path,
    workspace_id, RepoSlug, SafeRunId, WorkspaceIdKey,
};

#[cfg(unix)]
pub use ipc::{IpcConfig, IpcConnection, IpcError, IpcListener, PeerCreds};
