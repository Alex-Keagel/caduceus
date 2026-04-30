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
pub mod error;
pub mod lifecycle;
pub mod mailbox;
pub mod telemetry;
pub mod test_harness;

pub use clock::{Clock, RealClock, SharedClock, VirtualClock};
pub use config::{Config, ConfigError};
pub use error::{DaemonError, DaemonResult};
pub use lifecycle::{Lifecycle, LifecycleState, ShutdownReason};
pub use mailbox::{
    Cmd, EngineSender, MailboxError, MailboxFactory, Receiver, RetryToken, RunId, SessionId,
    SnapshotClientSender, SubsystemSender, TimerSender,
};
pub use telemetry::{init_tracing, Counter, Metrics};
