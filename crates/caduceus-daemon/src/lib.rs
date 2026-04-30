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

pub mod clock;
pub mod config;
pub mod env_exports;
pub mod error;
pub mod ipc;
pub mod lifecycle;
pub mod locks;
pub mod mailbox;
pub mod registry;
pub mod registry_store;
pub mod shared_repo_lock;
pub mod storage;
pub mod telemetry;
pub mod test_harness;
pub mod workspace;

pub use clock::{Clock, RealClock, SharedClock, VirtualClock};
pub use config::{Config, ConfigError};
pub use env_exports::workspace_env_exports;
pub use error::{DaemonError, DaemonResult};
pub use lifecycle::{Lifecycle, LifecycleState, ShutdownReason};
pub use locks::{CreateGuards, RegistryGuard, WorkspaceLocks};
pub use mailbox::{
    Cmd, EngineSender, MailboxError, MailboxFactory, Receiver, RetryToken, RunId, SessionId,
    SnapshotClientSender, SubsystemSender, TimerSender,
};
pub use registry::{RepoCoordinate, WorkspaceRegistryRow, WorkspaceStatus};
pub use registry_store::{RegistryError, RegistryStore};
pub use shared_repo_lock::{SharedRepoCaller, SharedRepoLockStrategy};
pub use storage::{atomic_write, JsonRowStore, Row, StorageError, StorageResult};
pub use telemetry::{init_tracing, Counter, Metrics};
pub use workspace::{
    build_workspace_path, sanitize_repo_slug, sanitize_run_id, validate_workspace_path,
    workspace_id, RepoSlug, SafeRunId, WorkspaceIdKey,
};

#[cfg(unix)]
pub use ipc::{IpcConfig, IpcConnection, IpcError, IpcListener, PeerCreds};
