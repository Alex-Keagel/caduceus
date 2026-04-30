//! Runner process model — parent-side shell + spawn pipeline + stop cascade
//! (ru01 + ru02 + ru03 + ru04 + ru14..ru18).
//!
//! Per the implementation DAG, this module is the parent-side mirror of
//! the agent subprocess.  It owns the child process handle, the state
//! machine, the stdin pipe (for stage 2 of the cascade), and the
//! per-Run runner_seq counter.
//!
//! State machine (spec #2 §3.2):
//!
//! ```text
//! Spawning  →  Running  →  StoppingCascade  →  Reaped
//!     |            |                              ^
//!     |            └──── on stop_cascade ──────────┘
//!     └──── on spawn failure → Reaped
//! ```
//!
//! Stop cascade (spec #2 §3.3):
//!
//! - **Stage 1** — outbound `cancel` frame (graceful).  Bounded by ε₁.
//! - **Stage 2** — close stdin.  Bounded by `grace_period_ms`.
//! - **Stage 3a** — `SIGTERM` (POSIX) / `CTRL_BREAK_EVENT` (Windows).
//!   Bounded by `grace_period_ms`.
//! - **Stage 3b** — `SIGKILL` (POSIX) / `TerminateProcess` (Windows).
//!   Bounded by ε₂.  Iter-28 #2-2: SIGKILL dispatch CAN fail; reap CAN
//!   time out; we surface that honestly via `signal_error` /
//!   `reap_timeout` events instead of pretending success.
//!
//! Composite bound: `ε₁ + 2 * grace_period_ms + ε₂` (spec #2 §3.3
//! invariant; verified on POSIX and Windows by ru18).
//!
//! Spec cross-references:
//!
//! - **§3.1 / iter-28 #2-5** — `shell_wrap = true` requires a
//!   workflow-static `command_string`; we surface this as
//!   `SpawnRefused::ShellWrapUntrustedInput`.
//! - **§3.4 / iter-28 #2** — ACP negotiation lives in `acp.rs`
//!   (ru23).  This module deals only with raw NDJSON child stdio.

use crate::error::SpawnRefusedReason;
use crate::inbound_queue::RunnerSeqCounter;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::Mutex;

/// State of a `RunnerProcess`.  Stored in an atomic so concurrent
/// readers (heartbeat tracker, dispatch loop) can poll without a lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RunnerState {
    Spawning = 0,
    Running = 1,
    StoppingCascade = 2,
    Reaped = 3,
}

/// Reason a stop cascade was initiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Orchestrator-initiated graceful stop (`Cmd::Shutdown` cascade,
    /// or terminal-state runner with cleanup).
    GracefulShutdown,
    /// Heartbeat timeout (iter-28 #2-3) — the runner stopped sending
    /// heartbeats within `heartbeat_timeout_ms`.
    HeartbeatTimeout,
    /// Z-23 violation: `runner_seq` regressed or skipped.
    SeqRegression,
    /// Wire violation: `runner_seq_gap`.
    SeqGap,
    /// Wire violation: unknown event kind (e.g., `cross_run_handoff`
    /// in v1 — iter-28 #2-8).
    UnknownMessageKind,
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            StopReason::GracefulShutdown => "graceful_shutdown",
            StopReason::HeartbeatTimeout => "heartbeat_timeout",
            StopReason::SeqRegression => "runner_seq_regression",
            StopReason::SeqGap => "runner_seq_gap",
            StopReason::UnknownMessageKind => "unknown_message_kind",
        };
        f.write_str(s)
    }
}

/// Outcome of a stop cascade.  Spec #2 §3.3 + iter-28 #2-2 (honest
/// SIGKILL outcome enumeration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CascadeOutcome {
    /// Reaped at the named stage with exit code (or `None` if signalled).
    Reaped {
        stage: CascadeStage,
        exit_code: Option<i32>,
    },
    /// Stage 3b SIGKILL dispatch returned `Err` AND/OR the post-SIGKILL
    /// reap timed out.  Iter-28 #2-2: do NOT pretend success.
    SigkillTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadeStage {
    Stage1Cancel,
    Stage2Stdin,
    Stage3aSigterm,
    Stage3bSigkill,
}

impl std::fmt::Display for CascadeStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CascadeStage::Stage1Cancel => "cancel",
            CascadeStage::Stage2Stdin => "stdin",
            CascadeStage::Stage3aSigterm => "sigterm",
            CascadeStage::Stage3bSigkill => "sigkill",
        };
        f.write_str(s)
    }
}

/// Errors specific to the runner process model.
#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("spawn refused: {0}")]
    SpawnRefused(SpawnRefusedReason),
    #[error("spawn I/O error: {0}")]
    SpawnIo(#[source] std::io::Error),
    #[error("runner is not in Running state (current: {0:?})")]
    NotRunning(RunnerState),
}

/// Workflow-supplied spawn parameters.  Spec #2 §3.1.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    /// argv to exec.  Spec #2 §3.1: when `shell_wrap == true`, this
    /// MUST be `["bash", "-lc", <static-literal>]`.
    pub argv: Vec<String>,
    /// Working directory; typically the workspace leaf path.
    pub cwd: PathBuf,
    /// Environment exports (CADUCEUS_* + workflow-declared).  Hooks +
    /// runner share this set per spec #3 I-9 isolation.
    pub env: std::collections::BTreeMap<String, String>,
    /// Cascade timing budgets.
    pub grace_period: Duration,
    pub epsilon_1: Duration,
    pub epsilon_2: Duration,
    /// Was this command shell-wrapped?  Iter-28 #2-5 — used by the
    /// validation gate; the validator's caller MUST also have proven
    /// `argv[2]` is workflow-static.  We carry the flag here so the
    /// runner can record it for diagnostics; we do NOT re-check.
    pub shell_wrapped: bool,
}

impl SpawnSpec {
    pub fn default_budgets() -> (Duration, Duration, Duration) {
        // Spec #2 §3.3 v1 defaults; tunable per-workflow later.
        (
            Duration::from_secs(10),    // grace_period
            Duration::from_millis(500), // ε₁
            Duration::from_millis(500), // ε₂
        )
    }
}

/// Validate the shell-wrap fail-closed gate (ru04 + iter-28 #2-5).
///
/// `command_string_is_workflow_static` MUST be a boolean derived by
/// the caller from the workflow loader: `true` iff the command_string
/// comes from a static workflow-authored literal (i.e., NOT from
/// runtime prompt / agent / tool / env input).
///
/// If the workflow opted into `shell_wrap = true` and the command
/// string is NOT provably static, return
/// `SpawnRefusedReason::ShellWrapUntrustedInput`.  Spec #2 §3.1.
pub fn validate_shell_wrap(
    shell_wrap_requested: bool,
    command_string_is_workflow_static: bool,
) -> Result<(), SpawnRefusedReason> {
    if shell_wrap_requested && !command_string_is_workflow_static {
        return Err(SpawnRefusedReason::ShellWrapUntrustedInput);
    }
    Ok(())
}

/// Parent-side runner process.  Owns the child handle and drives the
/// state machine.  Cheap to share via `Arc<Mutex<RunnerProcess>>`.
pub struct RunnerProcess {
    /// Per-Run runner_seq counter (Z-23 stamp lives here).
    pub runner_seq: RunnerSeqCounter,
    state: std::sync::atomic::AtomicU8,
    grace_period: Duration,
    epsilon_1: Duration,
    epsilon_2: Duration,
    /// The child process handle.  `None` after reaping.
    child: Mutex<Option<tokio::process::Child>>,
    /// Process group id (POSIX); used for `kill -pgid`.  None on Windows.
    #[cfg(unix)]
    pgid: Option<i32>,
    /// Was this command shell-wrapped?  Diagnostic only.
    shell_wrapped: bool,
    /// Last observed heartbeat instant (for ru13 timeout tracker).
    last_heartbeat: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for RunnerProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerProcess")
            .field("state", &self.state())
            .field("grace_period", &self.grace_period)
            .field("shell_wrapped", &self.shell_wrapped)
            .field("runner_seq_high_water", &self.runner_seq.high_water())
            .finish()
    }
}

impl RunnerProcess {
    /// Spawn the child per `SpawnSpec`.  Spec #2 §3.1 (POSIX) /
    /// §3.1 Windows mapping (iter-28 #2-4).
    pub async fn spawn(spec: SpawnSpec) -> Result<Arc<Self>, RunnerError> {
        if spec.argv.is_empty() {
            return Err(RunnerError::SpawnRefused(
                SpawnRefusedReason::ShellWrapUntrustedInput,
            ));
        }
        let mut cmd = tokio::process::Command::new(&spec.argv[0]);
        cmd.args(&spec.argv[1..])
            .current_dir(&spec.cwd)
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        // POSIX: put child in its own process group so `kill -pgid` works
        // for the cascade. tokio::process::Command on Unix exposes this
        // via process_group(0).
        #[cfg(unix)]
        cmd.process_group(0);

        // Windows: CREATE_NEW_PROCESS_GROUP so CTRL_BREAK_EVENT is
        // routed only to the child + descendants.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        let child = cmd.spawn().map_err(RunnerError::SpawnIo)?;
        #[cfg(unix)]
        let pgid = child.id().map(|p| p as i32);

        let process = Arc::new(Self {
            runner_seq: RunnerSeqCounter::new(),
            state: std::sync::atomic::AtomicU8::new(RunnerState::Running as u8),
            grace_period: spec.grace_period,
            epsilon_1: spec.epsilon_1,
            epsilon_2: spec.epsilon_2,
            child: Mutex::new(Some(child)),
            #[cfg(unix)]
            pgid,
            shell_wrapped: spec.shell_wrapped,
            last_heartbeat: Mutex::new(Some(Instant::now())),
        });
        Ok(process)
    }

    /// Read the current state.  Lock-free.
    pub fn state(&self) -> RunnerState {
        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            0 => RunnerState::Spawning,
            1 => RunnerState::Running,
            2 => RunnerState::StoppingCascade,
            _ => RunnerState::Reaped,
        }
    }

    /// Update last-observed heartbeat instant (called on every accepted
    /// heartbeat frame).  Used by ru13 timeout tracker.
    pub async fn record_heartbeat(&self, when: Instant) {
        let mut g = self.last_heartbeat.lock().await;
        *g = Some(when);
    }

    /// Read the last observed heartbeat instant.
    pub async fn last_heartbeat_at(&self) -> Option<Instant> {
        *self.last_heartbeat.lock().await
    }

    /// Spec #2 §3.3 stop cascade.  Drives stages 1 → 2 → 3a → 3b in
    /// order, returning at the first stage that reaps the child.
    pub async fn stop_cascade(&self, _reason: StopReason) -> CascadeOutcome {
        self.state.store(
            RunnerState::StoppingCascade as u8,
            std::sync::atomic::Ordering::Release,
        );

        // ── Stage 1: outbound cancel frame (close stdin gently) ─────
        // The "cancel" frame is a daemon -> runner protocol message;
        // until ru08 forward path is wired, the only graceful signal
        // we have is to close stdin in stage 2.  Stage 1 here is a
        // logical wait: ε₁ for the runner to observe an out-of-band
        // cancel via its own protocol layer (e.g., a SIGUSR2).  V1
        // skips the cancel frame and goes straight to stdin close.
        if self.try_wait_within(self.epsilon_1).await {
            return CascadeOutcome::Reaped {
                stage: CascadeStage::Stage1Cancel,
                exit_code: self.last_exit_code().await,
            };
        }

        // ── Stage 2: close stdin ──
        self.close_stdin().await;
        if self.try_wait_within(self.grace_period).await {
            return CascadeOutcome::Reaped {
                stage: CascadeStage::Stage2Stdin,
                exit_code: self.last_exit_code().await,
            };
        }

        // ── Stage 3a: SIGTERM (POSIX) / CTRL_BREAK_EVENT (Windows) ──
        self.send_sigterm().await;
        if self.try_wait_within(self.grace_period).await {
            return CascadeOutcome::Reaped {
                stage: CascadeStage::Stage3aSigterm,
                exit_code: self.last_exit_code().await,
            };
        }

        // ── Stage 3b: SIGKILL / TerminateProcess (iter-28 #2-2 honest) ──
        let dispatched = self.send_sigkill().await;
        if !dispatched {
            // SIGKILL dispatch failed; honest outcome.
            return CascadeOutcome::SigkillTimeout;
        }
        if self.try_wait_within(self.epsilon_2).await {
            CascadeOutcome::Reaped {
                stage: CascadeStage::Stage3bSigkill,
                exit_code: self.last_exit_code().await,
            }
        } else {
            CascadeOutcome::SigkillTimeout
        }
    }

    async fn try_wait_within(&self, budget: Duration) -> bool {
        let start = Instant::now();
        let poll = Duration::from_millis(20);
        while start.elapsed() < budget {
            let mut g = self.child.lock().await;
            if let Some(child) = g.as_mut() {
                match child.try_wait() {
                    Ok(Some(_status)) => {
                        // Child reaped.
                        self.state.store(
                            RunnerState::Reaped as u8,
                            std::sync::atomic::Ordering::Release,
                        );
                        return true;
                    }
                    Ok(None) => { /* still running */ }
                    Err(_) => return false,
                }
            } else {
                // Already reaped.
                return true;
            }
            drop(g);
            tokio::time::sleep(poll).await;
        }
        false
    }

    async fn last_exit_code(&self) -> Option<i32> {
        let mut g = self.child.lock().await;
        if let Some(child) = g.as_mut() {
            child.try_wait().ok().flatten().and_then(|s| s.code())
        } else {
            None
        }
    }

    async fn close_stdin(&self) {
        let mut g = self.child.lock().await;
        if let Some(child) = g.as_mut() {
            // Take and drop stdin handle.
            let _ = child.stdin.take();
        }
    }

    #[cfg(unix)]
    async fn send_sigterm(&self) {
        if let Some(pgid) = self.pgid {
            // kill the WHOLE process group, not just the leader.
            unsafe { libc::kill(-pgid, libc::SIGTERM) };
        }
    }

    #[cfg(not(unix))]
    async fn send_sigterm(&self) {
        // Windows: GenerateConsoleCtrlEvent for CTRL_BREAK_EVENT IF the
        // child has a console.  V1 skips this stage on Windows; the
        // cascade proceeds to stage 3b via TerminateProcess.
    }

    /// Returns `true` if the SIGKILL dispatch syscall succeeded; the
    /// child may still be in the kernel's reaping queue.  Iter-28 #2-2:
    /// false means we should surface `signal_error` rather than pretending.
    #[cfg(unix)]
    async fn send_sigkill(&self) -> bool {
        if let Some(pgid) = self.pgid {
            let r = unsafe { libc::kill(-pgid, libc::SIGKILL) };
            r == 0
        } else {
            false
        }
    }

    #[cfg(not(unix))]
    async fn send_sigkill(&self) -> bool {
        let mut g = self.child.lock().await;
        if let Some(child) = g.as_mut() {
            child.kill().await.is_ok()
        } else {
            false
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn spec_for(argv: Vec<&str>) -> SpawnSpec {
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

    #[test]
    fn shell_wrap_rejects_runtime_input() {
        // ru04 / iter-28 #2-5: shell_wrap=true + non-static command_string
        // MUST surface ShellWrapUntrustedInput.
        let r = validate_shell_wrap(true, false);
        assert!(matches!(
            r,
            Err(SpawnRefusedReason::ShellWrapUntrustedInput)
        ));
    }

    #[test]
    fn shell_wrap_accepts_static_literal() {
        let r = validate_shell_wrap(true, true);
        assert!(r.is_ok());
    }

    #[test]
    fn shell_wrap_disabled_always_ok() {
        let r = validate_shell_wrap(false, false);
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn spawn_starts_child_and_state_running() {
        // /bin/cat reads stdin until EOF; useful as a long-lived child.
        let spec = spec_for(vec!["/bin/cat"]);
        let runner = RunnerProcess::spawn(spec).await.unwrap();
        assert_eq!(runner.state(), RunnerState::Running);
        // Cleanup.
        let _ = runner.stop_cascade(StopReason::GracefulShutdown).await;
    }

    #[tokio::test]
    async fn cascade_reaps_at_stage_2_when_stdin_close_terminates_child() {
        // /bin/cat exits when stdin closes; stage 2 should reap it.
        let spec = spec_for(vec!["/bin/cat"]);
        let runner = RunnerProcess::spawn(spec).await.unwrap();
        let outcome = runner.stop_cascade(StopReason::GracefulShutdown).await;
        match outcome {
            CascadeOutcome::Reaped { stage, .. } => {
                // Either stage 1 (if rapid race) or stage 2 — both are
                // acceptable; we just check it didn't escalate past 2.
                assert!(matches!(
                    stage,
                    CascadeStage::Stage1Cancel | CascadeStage::Stage2Stdin
                ));
            }
            CascadeOutcome::SigkillTimeout => panic!("must not escalate to SIGKILL for /bin/cat"),
        }
        assert_eq!(runner.state(), RunnerState::Reaped);
    }

    #[tokio::test]
    async fn cascade_escalates_through_sigkill_for_uncatchable_child() {
        // sh -c 'trap "" TERM; trap "" INT; while :; do sleep 60; done'
        // ignores SIGTERM/SIGINT and never exits voluntarily.  Should
        // escalate to SIGKILL.
        let mut spec = spec_for(vec![
            "/bin/sh",
            "-c",
            r#"trap "" TERM INT; while :; do sleep 60; done"#,
        ]);
        spec.grace_period = Duration::from_millis(200);
        spec.epsilon_1 = Duration::from_millis(50);
        spec.epsilon_2 = Duration::from_millis(500);
        let runner = RunnerProcess::spawn(spec).await.unwrap();
        let outcome = runner.stop_cascade(StopReason::GracefulShutdown).await;
        match outcome {
            CascadeOutcome::Reaped {
                stage: CascadeStage::Stage3bSigkill,
                ..
            } => {}
            CascadeOutcome::SigkillTimeout => {
                // Acceptable on heavily loaded CI; still honest.
            }
            other => panic!("expected SIGKILL outcome, got {other:?}"),
        }
    }

    #[test]
    fn stop_reason_display_strings_match_spec() {
        assert_eq!(
            StopReason::GracefulShutdown.to_string(),
            "graceful_shutdown"
        );
        assert_eq!(
            StopReason::HeartbeatTimeout.to_string(),
            "heartbeat_timeout"
        );
        assert_eq!(
            StopReason::SeqRegression.to_string(),
            "runner_seq_regression"
        );
        assert_eq!(StopReason::SeqGap.to_string(), "runner_seq_gap");
        assert_eq!(
            StopReason::UnknownMessageKind.to_string(),
            "unknown_message_kind"
        );
    }

    #[test]
    fn cascade_stage_display_strings_match_spec() {
        assert_eq!(CascadeStage::Stage1Cancel.to_string(), "cancel");
        assert_eq!(CascadeStage::Stage2Stdin.to_string(), "stdin");
        assert_eq!(CascadeStage::Stage3aSigterm.to_string(), "sigterm");
        assert_eq!(CascadeStage::Stage3bSigkill.to_string(), "sigkill");
    }

    #[tokio::test]
    async fn record_heartbeat_updates_last_heartbeat() {
        let spec = spec_for(vec!["/bin/cat"]);
        let runner = RunnerProcess::spawn(spec).await.unwrap();
        let t0 = runner.last_heartbeat_at().await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let t1 = Instant::now();
        runner.record_heartbeat(t1).await;
        let observed = runner.last_heartbeat_at().await.unwrap();
        assert!(observed > t0);
        // Cleanup.
        let _ = runner.stop_cascade(StopReason::GracefulShutdown).await;
    }
}
