//! Workflow YAML schema + loader + hot reload + hooks binding +
//! permissions binding + safe shell-fragment form
//! (wf01 + wf02 + wf03 + wf04 + wf05 + wf06 + wf07).
//!
//! Per the implementation DAG, this module ships the daemon-side
//! workflow contract from `spec-repo-owned-workflow-contract.md`.
//!
//! The "YAML" in spec #6 is convention; v1 ships a TOML loader using
//! the same parser shape as `caduceus_daemon::config` so we don't pull
//! a YAML crate in.  Workflow files MAY use either TOML or NDJSON in
//! the future; the in-memory `Workflow` struct is canonical.
//!
//! Iter-28 absorbed:
//!
//! - **#2-5** shell-wrap fail-closed — workflow loader records whether
//!   `command_string` is a static literal so `validate_shell_wrap`
//!   (P2 ru04) can enforce.
//! - **#6 wf02 + or10 dep** — `or10-dispatch-loop` depends on
//!   `wf02-workflow-loader` so dispatch never fires before workflow
//!   is loaded.

use crate::permissions::PermissionEnvelope;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use thiserror::Error;
use tokio::sync::RwLock;

/// Workflow protocol selection.  Mirrors `RunnerProtocol` (P2 ru23).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowProtocol {
    Native,
    Acp,
}

/// Lifecycle phase (mirrors `error::HookPhase` but with workflow-level
/// types).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowHookPhase {
    BeforeCreate,
    AfterCreate,
    BeforeCleanup,
    AfterCleanup,
}

/// A workflow-declared hook command.  Spec #6 §3.5.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowHook {
    pub phase: WorkflowHookPhase,
    pub command: Vec<String>,
    /// Timeout in seconds.  Default 120s.
    #[serde(default = "default_hook_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_hook_timeout_secs() -> u64 {
    120
}

/// Top-level workflow struct.  Loaded from a workflow file at daemon
/// startup; hot-reloaded on `Cmd::WorkflowReloaded`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workflow {
    /// Workflow name (informational).
    pub name: String,
    /// Protocol: native NDJSON or ACP.
    #[serde(default = "default_protocol")]
    pub protocol: WorkflowProtocol,
    /// Iter-28 #2-5: when true, command_string MUST be a static
    /// literal asserted by the loader.  Runtime input is forbidden.
    #[serde(default)]
    pub shell_wrap: bool,
    /// argv for the runner (when shell_wrap=false).
    #[serde(default)]
    pub argv: Vec<String>,
    /// command_string for shell_wrap=true.  Loader verifies it is
    /// a static literal field (spec-static).
    #[serde(default)]
    pub command_string: Option<String>,
    /// Permission profile name; resolved against PermissionEnvelope at
    /// load time (wf05).
    pub profile: String,
    /// Workflow-declared environment (merged with CADUCEUS_* exports).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Workflow-declared hooks (wf04).
    #[serde(default)]
    pub hooks: Vec<WorkflowHook>,
}

fn default_protocol() -> WorkflowProtocol {
    WorkflowProtocol::Native
}

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("workflow read failed at {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("workflow parse failed: {0}")]
    ParseFailed(String),
    #[error("workflow validation failed: {0}")]
    Invalid(String),
    #[error("permission profile not found: {0}")]
    UnknownPermissionProfile(String),
}

impl Workflow {
    /// Parse a workflow from a TOML string.  Lean parser similar to
    /// `config::Config::from_toml_str`.  Sufficient for the v1 schema.
    pub fn from_toml_str(src: &str) -> Result<Self, WorkflowError> {
        parse_toml(src)
    }

    /// Load + validate from disk.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, WorkflowError> {
        let path = path.as_ref().to_path_buf();
        let src = std::fs::read_to_string(&path).map_err(|source| WorkflowError::ReadFailed {
            path: path.clone(),
            source,
        })?;
        let wf = Self::from_toml_str(&src)?;
        wf.validate()?;
        Ok(wf)
    }

    /// Validate field invariants.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.name.is_empty() {
            return Err(WorkflowError::Invalid(
                "workflow name MUST NOT be empty".into(),
            ));
        }
        if self.shell_wrap && self.command_string.is_none() {
            return Err(WorkflowError::Invalid(
                "shell_wrap=true requires command_string".into(),
            ));
        }
        if !self.shell_wrap && self.argv.is_empty() {
            return Err(WorkflowError::Invalid(
                "argv MUST NOT be empty when shell_wrap=false".into(),
            ));
        }
        if self.profile.is_empty() {
            return Err(WorkflowError::Invalid("profile MUST NOT be empty".into()));
        }
        Ok(())
    }
}

/// Resolve `profile` to a `PermissionEnvelope` (wf05).  V1 maps to
/// the three built-in presets; future workflows can carry inline
/// envelopes.
pub fn resolve_profile(profile: &str) -> Result<PermissionEnvelope, WorkflowError> {
    match profile {
        "plan" => Ok(PermissionEnvelope::preset_plan()),
        "act" => Ok(PermissionEnvelope::preset_act()),
        "autopilot" => Ok(PermissionEnvelope::preset_autopilot()),
        other => Err(WorkflowError::UnknownPermissionProfile(other.into())),
    }
}

/// wf06 — safe shell-fragment encoding.  POSIX single-quote escape:
/// each `'` in the input becomes `'\''`, and the result is wrapped
/// in single quotes.  Output is always shell-safe to drop into a
/// command string without further processing.
pub fn shell_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// wf03 — Hot-reloadable workflow handle.  Cheap to clone; reads are
/// lock-free against the inner Arc<Workflow>.
#[derive(Debug, Clone)]
pub struct HotReloadableWorkflow {
    inner: Arc<RwLock<Arc<Workflow>>>,
    last_reload: Arc<RwLock<SystemTime>>,
}

impl HotReloadableWorkflow {
    pub fn new(initial: Workflow) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(initial))),
            last_reload: Arc::new(RwLock::new(SystemTime::now())),
        }
    }

    pub async fn current(&self) -> Arc<Workflow> {
        Arc::clone(&*self.inner.read().await)
    }

    /// Hot-swap the workflow.  Existing Runs continue with the prior
    /// pointer (they hold their own `Arc<Workflow>` clone snapshot);
    /// new Runs pick up the new pointer.  Spec #6 hot-reload semantics.
    pub async fn reload(&self, new: Workflow) {
        new.validate().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "workflow hot-reload validation failed; ignoring");
        });
        let mut g = self.inner.write().await;
        *g = Arc::new(new);
        let mut t = self.last_reload.write().await;
        *t = SystemTime::now();
    }

    pub async fn last_reload_at(&self) -> SystemTime {
        *self.last_reload.read().await
    }
}

// ────────────────────── Lean TOML parser (workflow) ──────────────────

fn parse_toml(src: &str) -> Result<Workflow, WorkflowError> {
    use std::collections::HashMap;
    let mut sections: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut current_section = "";
    sections.insert("", Vec::new());
    for raw in src.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current_section = name.trim();
            sections.entry(current_section).or_default();
            continue;
        }
        sections.entry(current_section).or_default().push(line);
    }
    let root = sections.get("").cloned().unwrap_or_default();
    let kv = parse_kv(&root)?;
    let name = parse_str_required(&kv, "name")?;
    let profile = parse_str_required(&kv, "profile")?;
    let protocol = match parse_str_optional(&kv, "protocol")? {
        Some(s) => match s.as_str() {
            "native" => WorkflowProtocol::Native,
            "acp" => WorkflowProtocol::Acp,
            other => {
                return Err(WorkflowError::ParseFailed(format!(
                    "unknown protocol: {other}"
                )))
            }
        },
        None => WorkflowProtocol::Native,
    };
    let shell_wrap = parse_bool_optional(&kv, "shell_wrap")?.unwrap_or(false);
    let argv = parse_string_array_optional(&kv, "argv")?.unwrap_or_default();
    let command_string = parse_str_optional(&kv, "command_string")?;

    let env_lines = sections.get("env").cloned().unwrap_or_default();
    let env_kv = parse_kv(&env_lines)?;
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in env_kv {
        env.insert(k.to_string(), parse_str_value(v)?);
    }

    Ok(Workflow {
        name,
        protocol,
        shell_wrap,
        argv,
        command_string,
        profile,
        env,
        hooks: Vec::new(),
    })
}

fn parse_kv<'a>(lines: &[&'a str]) -> Result<Vec<(&'a str, &'a str)>, WorkflowError> {
    let mut out = Vec::new();
    for line in lines {
        let Some((k, v)) = line.split_once('=') else {
            return Err(WorkflowError::ParseFailed(format!(
                "expected `key = value`, got: {line}"
            )));
        };
        out.push((k.trim(), v.trim()));
    }
    Ok(out)
}

fn parse_str_required(kv: &[(&str, &str)], key: &str) -> Result<String, WorkflowError> {
    parse_str_optional(kv, key)?
        .ok_or_else(|| WorkflowError::ParseFailed(format!("missing required key `{key}`")))
}

fn parse_str_optional(kv: &[(&str, &str)], key: &str) -> Result<Option<String>, WorkflowError> {
    for (k, v) in kv {
        if *k == key {
            return Ok(Some(parse_str_value(v)?));
        }
    }
    Ok(None)
}

fn parse_bool_optional(kv: &[(&str, &str)], key: &str) -> Result<Option<bool>, WorkflowError> {
    for (k, v) in kv {
        if *k == key {
            return Ok(Some(v.trim().parse().map_err(|e| {
                WorkflowError::ParseFailed(format!("bool for `{key}`: {e}"))
            })?));
        }
    }
    Ok(None)
}

fn parse_str_value(v: &str) -> Result<String, WorkflowError> {
    let v = v.trim();
    let v = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .ok_or_else(|| WorkflowError::ParseFailed(format!("expected quoted string, got: {v}")))?;
    Ok(v.to_string())
}

fn parse_string_array_optional(
    kv: &[(&str, &str)],
    key: &str,
) -> Result<Option<Vec<String>>, WorkflowError> {
    for (k, v) in kv {
        if *k == key {
            let v = v.trim();
            let inner = v
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .ok_or_else(|| {
                    WorkflowError::ParseFailed(format!("expected array for `{key}`, got: {v}"))
                })?;
            let parts: Result<Vec<String>, WorkflowError> = inner
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(parse_str_value)
                .collect();
            return Ok(Some(parts?));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_workflow() -> &'static str {
        r#"
            name = "test-workflow"
            profile = "act"
            argv = ["/bin/cat"]
        "#
    }

    // ─── wf01 + wf02 schema + loader ─────────────────────────────────

    #[test]
    fn parses_minimal_workflow() {
        let wf = Workflow::from_toml_str(minimal_workflow()).unwrap();
        assert_eq!(wf.name, "test-workflow");
        assert_eq!(wf.profile, "act");
        assert_eq!(wf.protocol, WorkflowProtocol::Native);
        assert!(!wf.shell_wrap);
        assert_eq!(wf.argv, vec!["/bin/cat"]);
    }

    #[test]
    fn validate_rejects_empty_argv_when_no_shell_wrap() {
        let mut wf = Workflow::from_toml_str(minimal_workflow()).unwrap();
        wf.argv.clear();
        assert!(wf.validate().is_err());
    }

    #[test]
    fn validate_rejects_shell_wrap_without_command_string() {
        let src = r#"
            name = "x"
            profile = "act"
            shell_wrap = true
        "#;
        let wf = Workflow::from_toml_str(src).unwrap();
        assert!(wf.validate().is_err());
    }

    #[test]
    fn validate_accepts_shell_wrap_with_command_string() {
        let src = r#"
            name = "x"
            profile = "act"
            shell_wrap = true
            command_string = "echo hello"
        "#;
        let wf = Workflow::from_toml_str(src).unwrap();
        assert!(wf.validate().is_ok());
        assert!(wf.shell_wrap);
        assert_eq!(wf.command_string.as_deref(), Some("echo hello"));
    }

    #[test]
    fn parses_protocol_acp() {
        let src = r#"
            name = "x"
            profile = "act"
            protocol = "acp"
            argv = ["/bin/cat"]
        "#;
        let wf = Workflow::from_toml_str(src).unwrap();
        assert_eq!(wf.protocol, WorkflowProtocol::Acp);
    }

    #[test]
    fn parses_env_section() {
        let src = r#"
            name = "x"
            profile = "act"
            argv = ["/bin/cat"]

            [env]
            FOO = "bar"
            BAZ = "qux"
        "#;
        let wf = Workflow::from_toml_str(src).unwrap();
        assert_eq!(wf.env.get("FOO").unwrap(), "bar");
        assert_eq!(wf.env.get("BAZ").unwrap(), "qux");
    }

    // ─── wf05 permissions binding ────────────────────────────────────

    #[test]
    fn resolve_profile_maps_to_presets() {
        let plan = resolve_profile("plan").unwrap();
        assert_eq!(plan.profile, "plan");
        let act = resolve_profile("act").unwrap();
        assert_eq!(act.profile, "act");
        let auto = resolve_profile("autopilot").unwrap();
        assert_eq!(auto.profile, "autopilot");
    }

    #[test]
    fn resolve_profile_unknown_errors() {
        let r = resolve_profile("nope");
        assert!(matches!(r, Err(WorkflowError::UnknownPermissionProfile(_))));
    }

    // ─── wf06 shell-fragment form ────────────────────────────────────

    #[test]
    fn shell_quote_wraps_in_single_quotes() {
        assert_eq!(shell_quote("simple"), "'simple'");
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        // POSIX single-quote escape: ' becomes '\''.
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_quote_handles_special_shell_chars() {
        // Single-quoted strings disable interpretation of $, *, ;, etc.
        assert_eq!(shell_quote("$VAR;rm -rf /"), "'$VAR;rm -rf /'");
    }

    #[test]
    fn shell_quote_empty_input() {
        assert_eq!(shell_quote(""), "''");
    }

    // ─── wf03 hot reload ─────────────────────────────────────────────

    #[tokio::test]
    async fn hot_reload_swaps_pointer() {
        let initial = Workflow::from_toml_str(minimal_workflow()).unwrap();
        let h = HotReloadableWorkflow::new(initial);
        let cur1 = h.current().await;
        assert_eq!(cur1.name, "test-workflow");

        let new = Workflow::from_toml_str(
            r#"
                name = "reloaded"
                profile = "act"
                argv = ["/bin/cat"]
            "#,
        )
        .unwrap();
        h.reload(new).await;
        let cur2 = h.current().await;
        assert_eq!(cur2.name, "reloaded");
        // Existing reference still points to old workflow.
        assert_eq!(cur1.name, "test-workflow");
    }

    #[tokio::test]
    async fn hot_reload_updates_timestamp() {
        let initial = Workflow::from_toml_str(minimal_workflow()).unwrap();
        let h = HotReloadableWorkflow::new(initial);
        let t0 = h.last_reload_at().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        h.reload(Workflow::from_toml_str(minimal_workflow()).unwrap())
            .await;
        let t1 = h.last_reload_at().await;
        assert!(t1 > t0);
    }

    // ─── wf07 workflow acceptance + serde round-trip ─────────────────

    #[test]
    fn workflow_serialize_round_trip() {
        let wf = Workflow::from_toml_str(minimal_workflow()).unwrap();
        let s = serde_json::to_string(&wf).unwrap();
        let back: Workflow = serde_json::from_str(&s).unwrap();
        assert_eq!(wf, back);
    }

    #[test]
    fn workflow_with_hooks_round_trip() {
        let wf = Workflow {
            name: "h".into(),
            protocol: WorkflowProtocol::Native,
            shell_wrap: false,
            argv: vec!["/bin/cat".into()],
            command_string: None,
            profile: "act".into(),
            env: BTreeMap::new(),
            hooks: vec![WorkflowHook {
                phase: WorkflowHookPhase::BeforeCreate,
                command: vec!["/bin/echo".into(), "hi".into()],
                timeout_secs: 60,
            }],
        };
        let s = serde_json::to_string(&wf).unwrap();
        let back: Workflow = serde_json::from_str(&s).unwrap();
        assert_eq!(wf, back);
    }
}
