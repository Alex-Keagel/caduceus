use caduceus_core::{CaduceusError, Result};
use caduceus_permissions::{Capability, PermissionEnforcer};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

// ── Process execution ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: std::collections::HashMap<String, String>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

pub struct BashSandbox {
    workspace_root: PathBuf,
}

impl BashSandbox {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self { workspace_root: workspace_root.into() }
    }

    pub async fn execute(&self, request: ExecRequest) -> Result<ExecResult> {
        let cwd = request
            .cwd
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.workspace_root.clone());

        // Enforce workspace boundary for cwd (both absolute and relative)
        let resolved_cwd = if cwd.is_absolute() {
            cwd.clone()
        } else {
            self.workspace_root.join(&cwd)
        };
        let canonical_cwd = resolved_cwd.canonicalize().unwrap_or_else(|_| resolved_cwd.clone());
        if !canonical_cwd.starts_with(&self.workspace_root) {
            return Err(CaduceusError::PermissionDenied {
                capability: "fs".into(),
                tool: "cwd escapes workspace".into(),
            });
        }

        let timeout = Duration::from_secs(request.timeout_secs.unwrap_or(30));

        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&request.command)
            .current_dir(&cwd)
            .envs(&request.env)
            .kill_on_drop(true);

        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| CaduceusError::Tool { tool: "bash".into(), message: "Command timed out".into() })?
            .map_err(|e| CaduceusError::Io(e))?;

        Ok(ExecResult {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
            timed_out: false,
        })
    }
}

// ── File operations ────────────────────────────────────────────────────────────

pub struct FileOps {
    workspace_root: PathBuf,
}

impl FileOps {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let root: PathBuf = workspace_root.into();
        let canonical_root = root.canonicalize().unwrap_or(root);
        Self { workspace_root: canonical_root }
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let p = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace_root.join(path)
        };

        // Normalize to eliminate ../ components without requiring existence
        let mut normalized = PathBuf::new();
        for component in p.components() {
            match component {
                std::path::Component::ParentDir => { normalized.pop(); }
                std::path::Component::CurDir => {}
                other => normalized.push(other),
            }
        }

        // Boundary check on normalized path
        if !normalized.starts_with(&self.workspace_root) {
            return Err(CaduceusError::PermissionDenied {
                capability: "fs".into(),
                tool: "Path escapes workspace".into(),
            });
        }

        // If path exists, also verify after symlink resolution
        if normalized.exists() {
            let canonical = normalized.canonicalize().map_err(CaduceusError::Io)?;
            if !canonical.starts_with(&self.workspace_root) {
                return Err(CaduceusError::PermissionDenied {
                    capability: "fs".into(),
                    tool: "Symlink escapes workspace".into(),
                });
            }
            return Ok(canonical);
        }

        Ok(normalized)
    }

    pub async fn read(&self, path: &str) -> Result<String> {
        let resolved = self.resolve_path(path)?;
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| CaduceusError::Io(e))?;
        if meta.len() > MAX_FILE_SIZE {
            return Err(CaduceusError::Tool { tool: "runtime".into(), message: format!(
                "File too large: {} bytes (max {})",
                meta.len(),
                MAX_FILE_SIZE
            )} );
        }
        tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| CaduceusError::Io(e))
    }

    pub async fn write(&self, path: &str, content: &str) -> Result<()> {
        let resolved = self.resolve_path(path)?;
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CaduceusError::Io(e))?;
        }
        tokio::fs::write(&resolved, content)
            .await
            .map_err(|e| CaduceusError::Io(e))
    }

    pub async fn edit(&self, path: &str, old: &str, new: &str) -> Result<usize> {
        let content = self.read(path).await?;
        let count = content.matches(old).count();
        if count == 0 {
            return Err(CaduceusError::Tool { tool: "runtime".into(), message: format!(
                "String not found in {path}"
            )} );
        }
        if count > 1 {
            return Err(CaduceusError::Tool { tool: "runtime".into(), message: format!(
                "Ambiguous edit: {count} occurrences in {path}"
            )} );
        }
        let updated = content.replacen(old, new, 1);
        self.write(path, &updated).await?;
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let _sandbox = BashSandbox::new("/workspace");
    }

    #[tokio::test]
    async fn file_ops_write_read() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        ops.write("test.txt", "hello").await.unwrap();
        let content = ops.read("test.txt").await.unwrap();
        assert_eq!(content, "hello");
    }
}


