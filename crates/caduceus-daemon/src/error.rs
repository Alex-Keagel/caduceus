//! Canonical daemon error taxonomy.
//!
//! Per the implementation DAG (todo `f03-error-types`), this module defines
//! the exhaustive set of errors that the orchestrator daemon can surface.
//! All errors are non-`anyhow` per repo convention and carry enough context
//! that consumers can route them by variant.
//!
//! Specs cross-referenced:
//!
//! - **`spec-caduceus-orchestrator-algorithm.md`** — `DispatchResult`,
//!   `DispatchDeferred`, `Cmd::*` rejection paths.
//! - **`spec-caduceus-agent-runner-contract.md`** — `SpawnRefused` (incl.
//!   `CreationIdUnavailable`, `ShellWrapUntrustedInput`), wire-codec drops,
//!   stop-cascade outcomes.
//! - **`spec-multi-repo-workspace-model.md`** — `InvalidRunId`,
//!   `InvalidRepoSlug`, lock-acquisition errors, hook failures.
//! - **`spec-orchestrator-status-snapshot.md`** — `SnapshotError::Unavailable`
//!   (non-local transport rejection per local-only gate).

use thiserror::Error;

/// Canonical alias for daemon results.
pub type DaemonResult<T> = std::result::Result<T, DaemonError>;

/// Top-level error type for the orchestrator daemon.
///
/// Variants are exhaustive across the four P0 specs.  Implementations
/// MUST NOT introduce ad-hoc errors that bypass this taxonomy.
#[derive(Debug, Error)]
pub enum DaemonError {
    /// Snapshot RPC could not be served on the calling transport.
    ///
    /// Spec #4 §1.2 — non-locally-trusted transports MUST reject before
    /// snapshot serialization rather than mutate v1 wire shape.
    #[error("snapshot unavailable: {0}")]
    SnapshotUnavailable(SnapshotUnavailableReason),

    /// Runner spawn was refused before the child process was created.
    ///
    /// Spec #2 §3.1 / iter-28 backlog #2-5 — `ShellWrapUntrustedInput`
    /// must be a fail-closed gate, not a runtime check.
    #[error("spawn refused: {0}")]
    SpawnRefused(SpawnRefusedReason),

    /// Workspace operation failed.
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),

    /// Configuration loading or validation failed.
    #[error("config error: {0}")]
    Config(#[from] crate::config::ConfigError),

    /// Trust boundary rejected a `Cmd::*` message at the mailbox.
    ///
    /// Spec #1 §0 — capability-scoped senders MUST reject any
    /// unauthenticated, cross-user, replayed, or wrong-producer message
    /// before it reaches the main loop.
    #[error("trust boundary rejected {kind:?} from producer {producer}")]
    TrustBoundaryRejected { kind: String, producer: String },

    /// Underlying I/O failure, used for paths owned by the daemon's
    /// own filesystem operations (registry rows, replay index, lock
    /// files).  IPC-level I/O errors should be wrapped in a transport-
    /// specific variant by `f06-ipc-transport-local-uds`.
    #[error("daemon i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// Generic invariant breach.  Use sparingly — prefer a typed variant.
    #[error("invariant violation: {0}")]
    InvariantViolation(String),
}

/// Reasons a snapshot RPC may be rejected without serialization.
///
/// Spec #4 §1.2 + iter-28 #4-1 — these are the only two paths permitted.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SnapshotUnavailableReason {
    /// Transport could not be classified as locally-trusted.
    ///
    /// E.g. peer identity could not be established on a UDS, the
    /// connection was over TCP with no peer auth, or the request
    /// originated from a non-local origin.
    #[error("transport not locally trusted")]
    TransportNotLocallyTrusted,

    /// The daemon is in shutdown drain and is no longer serving snapshots.
    #[error("daemon is shutting down")]
    DaemonShuttingDown,

    /// The daemon has not finished boot reconcile (`or00-boot-reconcile-sweep`)
    /// and snapshot would be inconsistent.
    #[error("daemon not yet ready (boot reconcile in progress)")]
    NotReady,
}

/// Reasons a runner spawn may be refused before exec.
///
/// Spec #2 §3.1 — these are NOT post-spawn failures (which surface as
/// runner exit codes).  Spawn refusal is a daemon-side gate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpawnRefusedReason {
    /// `child_creation_id` could not be obtained from the OS.  Spec #2
    /// iter-28 #2 — descendant-walk requires a unique creation token.
    #[error("creation id unavailable from OS")]
    CreationIdUnavailable,

    /// The workflow set `shell_wrap = true` with a `command_string` that
    /// could not be proven to be a static workflow-authored literal.
    /// Spec #2 §3.1 / iter-28 #2-5 — fail-closed.
    #[error(
        "shell_wrap requires a static workflow-authored command_string; runtime input was detected"
    )]
    ShellWrapUntrustedInput,

    /// Workspace was not yet ready when spawn was attempted.
    #[error("workspace unavailable: {0}")]
    WorkspaceUnavailable(String),

    /// Concurrency cap (`max_concurrency`) reached.
    #[error("concurrency cap reached ({0} runs in flight)")]
    ConcurrencyCap(usize),

    /// Run was no longer eligible at spawn-time revalidate gate.
    #[error("run no longer eligible at revalidate gate")]
    RevalidateRaced,
}

/// Workspace-layer errors, owned by spec #3.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkspaceError {
    /// `run_id` failed `sanitize_run_id` — does not match
    /// `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`, is `.` / `..`, or contains
    /// `..` as a substring.
    #[error("invalid run_id: {0}")]
    InvalidRunId(String),

    /// `repo_slug` failed `sanitize_repo_slug` — see spec #3 §3.1.
    #[error("invalid repo slug: {0}")]
    InvalidRepoSlug(String),

    /// Path-traversal or symlink-escape detected during validate.
    #[error("workspace path validation failed: {0}")]
    PathValidationFailed(String),

    /// Per-slug shared-repo lock could not be acquired in try-lock mode
    /// (v1 strategy a; spec #3 §3.7).
    #[error("shared-repo lock contended for slug: {0}")]
    SharedRepoLocked(String),

    /// `before_create` / `after_create` / `before_cleanup` /
    /// `after_cleanup` returned non-zero.
    ///
    /// Per spec #3 I-7, `Error::HookFailed` is the canonical error to
    /// surface — NOT the cleanup error from any rollback.
    #[error("workspace hook failed: phase={phase}, exit_code={exit_code:?}")]
    HookFailed {
        phase: HookPhase,
        exit_code: Option<i32>,
    },

    /// Registry row could not be persisted atomically.
    #[error("registry write failed: {0}")]
    RegistryWriteFailed(String),

    /// Workspace already cleared (terminal state) — dependent calls
    /// MUST treat this as a no-op.
    #[error("workspace already cleared")]
    AlreadyCleared,
}

/// Workspace lifecycle hook phase identifier.
///
/// Spec #3 §3.5 / §3.6 enumerates these four phases.  The `OrphanedNoLeaf`
/// short-circuit in §3.6 mandates that none of these run if the leaf is
/// gone — see iter-28 backlog #3-4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    BeforeCreate,
    AfterCreate,
    BeforeCleanup,
    AfterCleanup,
}

impl std::fmt::Display for HookPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            HookPhase::BeforeCreate => "before_create",
            HookPhase::AfterCreate => "after_create",
            HookPhase::BeforeCleanup => "before_cleanup",
            HookPhase::AfterCleanup => "after_cleanup",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_unavailable_displays_reason() {
        let err =
            DaemonError::SnapshotUnavailable(SnapshotUnavailableReason::TransportNotLocallyTrusted);
        let s = err.to_string();
        assert!(s.contains("snapshot unavailable"));
        assert!(s.contains("transport not locally trusted"));
    }

    #[test]
    fn spawn_refused_shell_wrap_includes_directive() {
        let err = DaemonError::SpawnRefused(SpawnRefusedReason::ShellWrapUntrustedInput);
        let s = err.to_string();
        assert!(s.contains("static workflow-authored"));
    }

    #[test]
    fn workspace_error_into_daemon_error_via_from() {
        let inner = WorkspaceError::InvalidRunId("..".into());
        let outer: DaemonError = inner.into();
        match outer {
            DaemonError::Workspace(WorkspaceError::InvalidRunId(s)) => {
                assert_eq!(s, "..");
            }
            _ => panic!("expected DaemonError::Workspace(InvalidRunId(..))"),
        }
    }

    #[test]
    fn trust_boundary_rejection_records_kind_and_producer() {
        let err = DaemonError::TrustBoundaryRejected {
            kind: "Cmd::Reattach".into(),
            producer: "snapshot-client".into(),
        };
        let s = err.to_string();
        assert!(s.contains("Cmd::Reattach"));
        assert!(s.contains("snapshot-client"));
    }

    #[test]
    fn hook_phase_display_matches_spec_names() {
        assert_eq!(HookPhase::BeforeCreate.to_string(), "before_create");
        assert_eq!(HookPhase::AfterCreate.to_string(), "after_create");
        assert_eq!(HookPhase::BeforeCleanup.to_string(), "before_cleanup");
        assert_eq!(HookPhase::AfterCleanup.to_string(), "after_cleanup");
    }

    #[test]
    fn workspace_hook_failed_renders_phase_and_exit_code() {
        let err = WorkspaceError::HookFailed {
            phase: HookPhase::BeforeCreate,
            exit_code: Some(1),
        };
        let s = err.to_string();
        assert!(s.contains("before_create"));
        assert!(s.contains("Some(1)"));
    }

    #[test]
    fn io_error_converts_into_daemon_error() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: DaemonError = io.into();
        matches!(err, DaemonError::Io(_));
    }
}
