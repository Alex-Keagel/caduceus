//! Workspace lifecycle hooks (ws12, ws13).
//!
//! Per the implementation DAG, this module defines the `HookExecutor`
//! trait and two implementations:
//!
//! - **`NoopHookExecutor`** — drops all hooks unconditionally.  Default
//!   for tests and for workflows that declare no hooks.
//! - **`SubprocessHookExecutor`** — runs each hook as a child process,
//!   honoring the workflow's command + timeout + env extension.
//!
//! Hook *binding* (mapping workflow YAML configuration to executor
//! state) belongs to `wf04-workflow-hooks-config`.  Here we ship the
//! plumbing.
//!
//! Spec cross-references:
//!
//! - **`spec-multi-repo-workspace-model.md` §3.5 / §3.6** — `before_create`
//!   / `after_create` / `before_cleanup` / `after_cleanup` ordering and
//!   abort-on-failure semantics.
//! - **`spec-multi-repo-workspace-model.md` I-7** — non-zero hook exit
//!   surfaces as `WorkspaceError::HookFailed`; rollback is the workspace
//!   algorithm's responsibility, not the hook's.
//! - **`spec-multi-repo-workspace-model.md` I-9** — hook isolation:
//!   hooks observe ONLY the env supplied here; no daemon-env inheritance
//!   by default.
//! - **`spec-multi-repo-workspace-model.md` §3.6 short-circuit** —
//!   `before_cleanup` / `after_cleanup` MUST NOT run when the leaf is
//!   gone (`OrphanedNoLeaf`/`OrphanedNoSlug`); enforced by the cleanup
//!   algorithm, not here.

use crate::error::{HookPhase, WorkspaceError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// A hook to invoke at a workspace lifecycle phase.
///
/// `command` is a list of argv entries; the first is the executable.
/// `timeout` bounds the child process; on timeout the executor MUST
/// terminate the child and surface `HookFailed { exit_code: None }`.
#[derive(Debug, Clone)]
pub struct HookSpec {
    pub phase: HookPhase,
    pub command: Vec<String>,
    pub timeout: Duration,
}

/// Default per-phase timeout budgets.  Spec #3 §3.5 / §3.6 expect
/// hooks to be short; long-running setup belongs to the runner.
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(120);

/// Outcome of a successful hook execution.  Failure paths surface as
/// `WorkspaceError::HookFailed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookOutcome {
    pub phase: HookPhase,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Abstract hook executor.  Plugged into `create_workspace` /
/// `cleanup_workspace` so tests can stub hooks without forking a real
/// child process.
pub trait HookExecutor: Send + Sync {
    /// Execute `hook` with the given `env` exports and `cwd`.  Returns
    /// `HookOutcome` on success (exit 0).  Non-zero exit or timeout
    /// surfaces as `WorkspaceError::HookFailed`.
    fn execute(
        &self,
        hook: &HookSpec,
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> Result<HookOutcome, WorkspaceError>;
}

/// Hook executor that drops all hooks.  Use when the workflow declares
/// no hooks, or in tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopHookExecutor;

impl HookExecutor for NoopHookExecutor {
    fn execute(
        &self,
        hook: &HookSpec,
        _env: &BTreeMap<String, String>,
        _cwd: &Path,
    ) -> Result<HookOutcome, WorkspaceError> {
        Ok(HookOutcome {
            phase: hook.phase,
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

/// Hook executor that runs each hook as a child process.
#[derive(Debug, Default, Clone)]
pub struct SubprocessHookExecutor {
    /// Optional override of the system PATH. None inherits from daemon's
    /// own PATH at startup. Hooks otherwise see only the supplied env.
    pub path_env: Option<PathBuf>,
}

impl HookExecutor for SubprocessHookExecutor {
    fn execute(
        &self,
        hook: &HookSpec,
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> Result<HookOutcome, WorkspaceError> {
        if hook.command.is_empty() {
            return Err(WorkspaceError::HookFailed {
                phase: hook.phase,
                exit_code: None,
            });
        }
        let (program, args) = (&hook.command[0], &hook.command[1..]);

        let mut cmd = std::process::Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(p) = &self.path_env {
            cmd.env("PATH", p);
        } else if let Ok(p) = std::env::var("PATH") {
            cmd.env("PATH", p);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }

        let child = cmd.spawn().map_err(|_| WorkspaceError::HookFailed {
            phase: hook.phase,
            exit_code: None,
        })?;
        wait_with_timeout(child, hook.timeout, hook.phase)
    }
}

fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
    phase: HookPhase,
) -> Result<HookOutcome, WorkspaceError> {
    let start = std::time::Instant::now();
    let poll = Duration::from_millis(50);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut s) = child.stdout.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut stdout);
                }
                if let Some(mut s) = child.stderr.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut stderr);
                }
                let code = status.code();
                if code == Some(0) {
                    return Ok(HookOutcome {
                        phase,
                        exit_code: 0,
                        stdout,
                        stderr,
                    });
                }
                return Err(WorkspaceError::HookFailed {
                    phase,
                    exit_code: code,
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(WorkspaceError::HookFailed {
                        phase,
                        exit_code: None,
                    });
                }
                std::thread::sleep(poll);
            }
            Err(_) => {
                return Err(WorkspaceError::HookFailed {
                    phase,
                    exit_code: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_executor_returns_zero_exit() {
        let exec = NoopHookExecutor;
        let hook = HookSpec {
            phase: HookPhase::BeforeCreate,
            command: vec!["unused".into()],
            timeout: Duration::from_secs(1),
        };
        let env = BTreeMap::new();
        let outcome = exec.execute(&hook, &env, Path::new("/tmp")).unwrap();
        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.phase, HookPhase::BeforeCreate);
    }

    #[test]
    fn subprocess_executor_runs_true_command_successfully() {
        // /bin/true is universally present on POSIX with exit 0.
        let exec = SubprocessHookExecutor::default();
        let hook = HookSpec {
            phase: HookPhase::AfterCreate,
            command: vec!["/usr/bin/true".into()],
            timeout: Duration::from_secs(5),
        };
        let env = BTreeMap::new();
        let r = exec.execute(&hook, &env, Path::new("/"));
        if r.is_err() {
            // Some macOS layouts have /bin/true instead of /usr/bin/true.
            let hook2 = HookSpec {
                phase: HookPhase::AfterCreate,
                command: vec!["/bin/true".into()],
                timeout: Duration::from_secs(5),
            };
            assert!(exec.execute(&hook2, &env, Path::new("/")).is_ok());
        } else {
            assert!(r.is_ok());
        }
    }

    #[test]
    fn subprocess_executor_surfaces_nonzero_exit() {
        let exec = SubprocessHookExecutor::default();
        let hook = HookSpec {
            phase: HookPhase::BeforeCreate,
            command: vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
            timeout: Duration::from_secs(5),
        };
        let env = BTreeMap::new();
        match exec.execute(&hook, &env, Path::new("/tmp")) {
            Err(WorkspaceError::HookFailed { exit_code, .. }) => assert_eq!(exit_code, Some(7)),
            other => panic!("expected HookFailed(Some(7)), got {other:?}"),
        }
    }

    #[test]
    fn subprocess_executor_enforces_timeout() {
        let exec = SubprocessHookExecutor::default();
        let hook = HookSpec {
            phase: HookPhase::BeforeCreate,
            command: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            timeout: Duration::from_millis(100),
        };
        let env = BTreeMap::new();
        match exec.execute(&hook, &env, Path::new("/tmp")) {
            Err(WorkspaceError::HookFailed { exit_code, .. }) => assert!(exit_code.is_none()),
            other => panic!("expected HookFailed(None), got {other:?}"),
        }
    }

    #[test]
    fn subprocess_executor_passes_env_vars() {
        let exec = SubprocessHookExecutor::default();
        let hook = HookSpec {
            phase: HookPhase::AfterCreate,
            command: vec![
                "/bin/sh".into(),
                "-c".into(),
                "test \"$CADUCEUS_TEST_VAR\" = \"value42\"".into(),
            ],
            timeout: Duration::from_secs(5),
        };
        let mut env = BTreeMap::new();
        env.insert("CADUCEUS_TEST_VAR".into(), "value42".into());
        let r = exec.execute(&hook, &env, Path::new("/tmp"));
        assert!(r.is_ok(), "env var not received: {r:?}");
    }

    #[test]
    fn subprocess_executor_rejects_empty_command() {
        let exec = SubprocessHookExecutor::default();
        let hook = HookSpec {
            phase: HookPhase::BeforeCreate,
            command: vec![],
            timeout: Duration::from_secs(1),
        };
        let env = BTreeMap::new();
        assert!(exec.execute(&hook, &env, Path::new("/tmp")).is_err());
    }
}
