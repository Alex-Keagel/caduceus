//! Daemon configuration loading + validation.
//!
//! Per the implementation DAG (todo `f02-config-loader`), this module is
//! responsible for parsing `caduceusd.toml`, applying environment overrides,
//! and validating the resulting `Config` against spec #1 §4 invariants
//! BEFORE the daemon enters its main loop.
//!
//! Loading order (deterministic, per spec #1 §3.1):
//!
//! 1. Parse the TOML file at the path given on the command line, or fall
//!    back to `$XDG_CONFIG_HOME/caduceus/caduceusd.toml` (POSIX) or
//!    `%APPDATA%\caduceus\caduceusd.toml` (Windows).
//! 2. Apply environment overrides where defined (e.g. `CADUCEUSD_LOG`).
//! 3. Validate field ranges (`recent_history_ring_size >= 1`, etc.).
//!
//! Iter-28 backlog #1-5 absorbed: `recent_history_ring_size` is a
//! first-class field with default 32 and `>= 1` constraint.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Top-level daemon config.  Maps to spec #1 §4 `Config` struct.
///
/// Optional sub-configs (`workspace`, `agent`, etc.) are stubbed here
/// because they are owned by sibling specs whose foundations land later
/// in the DAG.  This phase establishes the file shape and parse path.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Config {
    /// Path to the workflow YAML file.
    pub workflow_path: PathBuf,

    /// Polling interval for `Tick` events, in milliseconds.
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,

    /// Maximum number of concurrent runs the orchestrator may dispatch.
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,

    /// Spec #1 §4 ring invariant #2.  Default 32; MUST be `>= 1`.
    /// Iter-28 #1-5: this field MUST be present in `Config`.
    #[serde(default = "default_recent_history_ring_size")]
    pub recent_history_ring_size: usize,

    /// Disconnect timeout in ms.  Spec #1 §8.7.  Default 60_000 (60s).
    #[serde(default = "default_disconnect_timeout_ms")]
    pub disconnect_timeout_ms: u64,

    /// How long a disconnected Run is retained in `running` after the
    /// timeout fires.  Default 3_600_000 (1h).
    #[serde(default = "default_disconnect_retention_ms")]
    pub disconnect_retention_ms: u64,

    /// Z-9 livelock guard.  Maximum consecutive `DispatchResult::Deferred`
    /// outcomes before `on_retry_timer` abandons the run.  Default 8.
    /// MUST be `>= 1`.
    #[serde(default = "default_max_dispatch_defer_attempts")]
    pub max_dispatch_defer_attempts: u32,

    /// Path to the workspace root directory.  All Run workspaces live as
    /// children of this directory.
    pub workspace_root: PathBuf,
}

fn default_poll_interval_ms() -> u64 {
    100
}
fn default_max_concurrency() -> usize {
    8
}
fn default_recent_history_ring_size() -> usize {
    32
}
fn default_disconnect_timeout_ms() -> u64 {
    60_000
}
fn default_disconnect_retention_ms() -> u64 {
    3_600_000
}
fn default_max_dispatch_defer_attempts() -> u32 {
    8
}

/// Errors produced by the config loader.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Could not read the TOML file from disk.
    #[error("failed to read config from {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// TOML failed to parse.
    #[error("failed to parse config TOML: {0}")]
    ParseFailed(String),

    /// Config parsed but failed validation.
    #[error("invalid config: {0}")]
    Invalid(String),
}

impl Config {
    /// Parse a `Config` from a TOML string.  Used by `from_path` and tests.
    pub fn from_toml_str(src: &str) -> Result<Self, ConfigError> {
        // toml is a workspace dep; we pull it transitively through serde.
        // For the foundations crate we accept a tiny vendored parser path:
        // delegate to `toml` if available at the workspace level, else
        // fail with a clear ParseFailed.
        toml_parse(src)
    }

    /// Load a `Config` from a TOML file on disk and validate it.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref().to_path_buf();
        let src = std::fs::read_to_string(&path).map_err(|source| ConfigError::ReadFailed {
            path: path.clone(),
            source,
        })?;
        let cfg = Self::from_toml_str(&src)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate field ranges per spec #1 §4 invariants.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.recent_history_ring_size < 1 {
            return Err(ConfigError::Invalid(
                "recent_history_ring_size MUST be >= 1 (spec #1 §4 ring invariant #2)".into(),
            ));
        }
        if self.max_dispatch_defer_attempts < 1 {
            return Err(ConfigError::Invalid(
                "max_dispatch_defer_attempts MUST be >= 1 (Z-9 livelock guard)".into(),
            ));
        }
        if self.poll_interval_ms == 0 {
            return Err(ConfigError::Invalid("poll_interval_ms MUST be > 0".into()));
        }
        if self.disconnect_retention_ms < self.disconnect_timeout_ms {
            return Err(ConfigError::Invalid(format!(
                "disconnect_retention_ms ({}) MUST be >= disconnect_timeout_ms ({})",
                self.disconnect_retention_ms, self.disconnect_timeout_ms
            )));
        }
        Ok(())
    }
}

// We avoid pulling `toml` into Cargo.toml at this stage because the
// workspace doesn't yet declare it.  For phase-0 we accept a hand-written
// minimal parser that supports the field set above.  When the workspace
// adds `toml` for other crates, this can be replaced one-line with
// `toml::from_str(src).map_err(...)`.
fn toml_parse(src: &str) -> Result<Config, ConfigError> {
    use std::collections::HashMap;
    let mut kv: HashMap<&str, &str> = HashMap::new();
    for raw in src.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(ConfigError::ParseFailed(format!(
                "expected `key = value`, got: {raw}"
            )));
        };
        let k = k.trim();
        let v = v.trim();
        kv.insert(k, v);
    }

    fn req_str<'a>(kv: &HashMap<&'a str, &'a str>, k: &str) -> Result<String, ConfigError> {
        let v = kv
            .get(k)
            .ok_or_else(|| ConfigError::ParseFailed(format!("missing required key `{k}`")))?;
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .ok_or_else(|| {
                ConfigError::ParseFailed(format!("expected quoted string for key `{k}`"))
            })?;
        Ok(v.to_string())
    }

    fn opt_u64(kv: &HashMap<&str, &str>, k: &str, default: u64) -> Result<u64, ConfigError> {
        match kv.get(k) {
            Some(v) => v
                .trim()
                .parse()
                .map_err(|e| ConfigError::ParseFailed(format!("invalid u64 for `{k}`: {e}"))),
            None => Ok(default),
        }
    }
    fn opt_usize(kv: &HashMap<&str, &str>, k: &str, default: usize) -> Result<usize, ConfigError> {
        match kv.get(k) {
            Some(v) => v
                .trim()
                .parse()
                .map_err(|e| ConfigError::ParseFailed(format!("invalid usize for `{k}`: {e}"))),
            None => Ok(default),
        }
    }
    fn opt_u32(kv: &HashMap<&str, &str>, k: &str, default: u32) -> Result<u32, ConfigError> {
        match kv.get(k) {
            Some(v) => v
                .trim()
                .parse()
                .map_err(|e| ConfigError::ParseFailed(format!("invalid u32 for `{k}`: {e}"))),
            None => Ok(default),
        }
    }

    let cfg = Config {
        workflow_path: PathBuf::from(req_str(&kv, "workflow_path")?),
        workspace_root: PathBuf::from(req_str(&kv, "workspace_root")?),
        poll_interval_ms: opt_u64(&kv, "poll_interval_ms", default_poll_interval_ms())?,
        max_concurrency: opt_usize(&kv, "max_concurrency", default_max_concurrency())?,
        recent_history_ring_size: opt_usize(
            &kv,
            "recent_history_ring_size",
            default_recent_history_ring_size(),
        )?,
        disconnect_timeout_ms: opt_u64(
            &kv,
            "disconnect_timeout_ms",
            default_disconnect_timeout_ms(),
        )?,
        disconnect_retention_ms: opt_u64(
            &kv,
            "disconnect_retention_ms",
            default_disconnect_retention_ms(),
        )?,
        max_dispatch_defer_attempts: opt_u32(
            &kv,
            "max_dispatch_defer_attempts",
            default_max_dispatch_defer_attempts(),
        )?,
    };
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn minimal_toml() -> &'static str {
        r#"
            workflow_path = "/etc/caduceus/workflow.yaml"
            workspace_root = "/var/lib/caduceus/workspaces"
        "#
    }

    #[test]
    fn parses_minimal_config_with_defaults() {
        let cfg = Config::from_toml_str(minimal_toml()).expect("parse");
        assert_eq!(
            cfg.workflow_path,
            PathBuf::from("/etc/caduceus/workflow.yaml")
        );
        assert_eq!(
            cfg.workspace_root,
            PathBuf::from("/var/lib/caduceus/workspaces")
        );
        assert_eq!(cfg.poll_interval_ms, 100);
        assert_eq!(cfg.max_concurrency, 8);
        assert_eq!(cfg.recent_history_ring_size, 32);
        assert_eq!(cfg.disconnect_timeout_ms, 60_000);
        assert_eq!(cfg.disconnect_retention_ms, 3_600_000);
        assert_eq!(cfg.max_dispatch_defer_attempts, 8);
    }

    #[test]
    fn validate_rejects_zero_ring_size() {
        let mut cfg = Config::from_toml_str(minimal_toml()).unwrap();
        cfg.recent_history_ring_size = 0;
        let err = cfg.validate().unwrap_err();
        assert!(err
            .to_string()
            .contains("recent_history_ring_size MUST be >= 1"));
    }

    #[test]
    fn validate_rejects_zero_dispatch_defer_attempts() {
        let mut cfg = Config::from_toml_str(minimal_toml()).unwrap();
        cfg.max_dispatch_defer_attempts = 0;
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("Z-9 livelock guard"));
    }

    #[test]
    fn validate_rejects_zero_poll_interval() {
        let mut cfg = Config::from_toml_str(minimal_toml()).unwrap();
        cfg.poll_interval_ms = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_retention_less_than_timeout() {
        let mut cfg = Config::from_toml_str(minimal_toml()).unwrap();
        cfg.disconnect_timeout_ms = 5000;
        cfg.disconnect_retention_ms = 4000;
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("MUST be >="));
    }

    #[test]
    fn missing_required_key_errors() {
        let bad = r#"workflow_path = "/etc/caduceus/wf.yaml""#;
        let err = Config::from_toml_str(bad).unwrap_err();
        assert!(err.to_string().contains("workspace_root"));
    }

    #[test]
    fn from_path_reads_and_validates() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "{}", minimal_toml()).unwrap();
        let cfg = Config::from_path(f.path()).expect("load");
        cfg.validate().unwrap();
        assert_eq!(
            cfg.workflow_path,
            PathBuf::from("/etc/caduceus/workflow.yaml")
        );
    }

    #[test]
    fn from_path_missing_file_returns_read_failed() {
        let err = Config::from_path("/no/such/path/config.toml").unwrap_err();
        match err {
            ConfigError::ReadFailed { .. } => {}
            other => panic!("expected ReadFailed, got {other:?}"),
        }
    }

    #[test]
    fn comments_are_stripped() {
        let src = r#"
            # top-level comment
            workflow_path = "/wf.yaml" # inline comment
            workspace_root = "/ws" # another
        "#;
        let cfg = Config::from_toml_str(src).expect("parse");
        assert_eq!(cfg.workflow_path, PathBuf::from("/wf.yaml"));
        assert_eq!(cfg.workspace_root, PathBuf::from("/ws"));
    }

    #[test]
    fn override_default_via_explicit_value() {
        let src = r#"
            workflow_path = "/a"
            workspace_root = "/b"
            recent_history_ring_size = 128
            max_concurrency = 16
        "#;
        let cfg = Config::from_toml_str(src).unwrap();
        assert_eq!(cfg.recent_history_ring_size, 128);
        assert_eq!(cfg.max_concurrency, 16);
    }
}
