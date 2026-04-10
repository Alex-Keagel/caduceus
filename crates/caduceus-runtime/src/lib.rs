pub mod sandbox;
use caduceus_core::{CaduceusError, Result};
use caduceus_permissions::{Capability, PermissionEnforcer};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const MAX_READ_SIZE: u64 = 1 * 1024 * 1024; // 1 MB
const MAX_WRITE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
const MAX_OUTPUT_SIZE: usize = 1 * 1024 * 1024; // 1 MB for command output
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

// Env vars to strip from child processes for safety
const SANITIZED_ENV_VARS: &[&str] = &[
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "NPM_TOKEN",
];

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
        let root: PathBuf = workspace_root.into();
        Self {
            workspace_root: root.canonicalize().unwrap_or(root),
        }
    }

    /// Truncate a string to at most `max_bytes` bytes on a valid UTF-8 boundary.
    fn truncate_output(s: &str, max_bytes: usize) -> String {
        if s.len() <= max_bytes {
            return s.to_string();
        }
        let mut end = max_bytes;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut truncated = s[..end].to_string();
        truncated.push_str("\n... [output truncated]");
        truncated
    }

    pub async fn execute(&self, request: ExecRequest) -> Result<ExecResult> {
        let cwd = request
            .cwd
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.workspace_root.clone());

        // Enforce workspace boundary for cwd
        if cwd.is_absolute() {
            let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
            if !canonical.starts_with(&self.workspace_root) {
                return Err(CaduceusError::PermissionDenied {
                    capability: "fs".into(),
                    tool: "cwd escapes workspace".into(),
                });
            }
        }

        let timeout = Duration::from_secs(request.timeout_secs.unwrap_or(30));

        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&request.command)
            .current_dir(&cwd)
            .kill_on_drop(true);

        // Sanitize environment: add user-specified env, but strip sensitive vars
        for (k, v) in &request.env {
            cmd.env(k, v);
        }
        for var in SANITIZED_ENV_VARS {
            cmd.env_remove(var);
        }

        let result = tokio::time::timeout(timeout, cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let stdout_raw = String::from_utf8_lossy(&output.stdout);
                let stderr_raw = String::from_utf8_lossy(&output.stderr);
                Ok(ExecResult {
                    stdout: Self::truncate_output(&stdout_raw, MAX_OUTPUT_SIZE),
                    stderr: Self::truncate_output(&stderr_raw, MAX_OUTPUT_SIZE),
                    exit_code: output.status.code().unwrap_or(-1),
                    timed_out: false,
                })
            }
            Ok(Err(e)) => Err(CaduceusError::Io(e)),
            Err(_elapsed) => {
                // Timeout: the process is killed by kill_on_drop
                Ok(ExecResult {
                    stdout: String::new(),
                    stderr: format!("Command timed out after {}s", timeout.as_secs()),
                    exit_code: -1,
                    timed_out: true,
                })
            }
        }
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
        Self {
            workspace_root: canonical_root,
        }
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let p = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace_root.join(path)
        };

        if p.exists() {
            let canonical = p.canonicalize().map_err(|e| CaduceusError::Io(e))?;
            if !canonical.starts_with(&self.workspace_root) {
                return Err(CaduceusError::PermissionDenied {
                    capability: "fs".into(),
                    tool: "Path escapes workspace".into(),
                });
            }
            Ok(canonical)
        } else {
            let parent = p.parent().unwrap_or(&p);
            if parent.exists() {
                let canonical_parent = parent.canonicalize().map_err(|e| CaduceusError::Io(e))?;
                if !canonical_parent.starts_with(&self.workspace_root) {
                    return Err(CaduceusError::PermissionDenied {
                        capability: "fs".into(),
                        tool: "Path escapes workspace".into(),
                    });
                }
            }
            Ok(p)
        }
    }

    pub async fn read(&self, path: &str) -> Result<String> {
        let resolved = self.resolve_path(path)?;
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| CaduceusError::Io(e))?;
        if meta.len() > MAX_READ_SIZE {
            return Err(CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!(
                    "File too large to read: {} bytes (max {})",
                    meta.len(),
                    MAX_READ_SIZE
                ),
            });
        }
        tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| CaduceusError::Io(e))
    }

    pub async fn write(&self, path: &str, content: &str) -> Result<()> {
        if content.len() as u64 > MAX_WRITE_SIZE {
            return Err(CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!(
                    "Content too large to write: {} bytes (max {})",
                    content.len(),
                    MAX_WRITE_SIZE
                ),
            });
        }
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
            return Err(CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!("String not found in {path}"),
            });
        }
        if count > 1 {
            return Err(CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!("Ambiguous edit: {count} occurrences in {path}"),
            });
        }
        let updated = content.replacen(old, new, 1);
        self.write(path, &updated).await?;
        Ok(1)
    }

    /// Check whether a path exists within the workspace.
    pub async fn exists(&self, path: &str) -> Result<bool> {
        let resolved = self.resolve_path(path)?;
        Ok(tokio::fs::try_exists(&resolved).await.unwrap_or(false))
    }

    /// List directory entries (non-recursive, names only).
    pub async fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        let resolved = self.resolve_path(path)?;
        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&resolved)
            .await
            .map_err(|e| CaduceusError::Io(e))?;
        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|e| CaduceusError::Io(e))?
        {
            if let Some(name) = entry.file_name().to_str() {
                let file_type = entry.file_type().await.map_err(|e| CaduceusError::Io(e))?;
                let suffix = if file_type.is_dir() { "/" } else { "" };
                entries.push(format!("{name}{suffix}"));
            }
        }
        entries.sort();
        Ok(entries)
    }

    /// Simple glob search relative to workspace root.
    pub async fn glob_search(&self, pattern: &str) -> Result<Vec<String>> {
        let full_pattern = self.workspace_root.join(pattern);
        let pattern_str = full_pattern.to_string_lossy().to_string();
        let root = self.workspace_root.clone();

        tokio::task::spawn_blocking(move || {
            let mut results = Vec::new();
            let entries = glob::glob(&pattern_str).map_err(|e| CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!("Invalid glob pattern: {e}"),
            })?;
            for entry in entries.flatten() {
                if let Ok(rel) = entry.strip_prefix(&root) {
                    results.push(rel.to_string_lossy().to_string());
                }
            }
            Ok(results)
        })
        .await
        .map_err(|e| CaduceusError::Tool {
            tool: "runtime".into(),
            message: format!("Glob task failed: {e}"),
        })?
    }

    /// Line-by-line grep search within the workspace. Returns matching file:line pairs.
    pub async fn grep_search(
        &self,
        pattern: &str,
        file_glob: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<String>> {
        let root = self.workspace_root.clone();
        let pattern = pattern.to_string();
        let file_glob = file_glob.map(|s| s.to_string());
        let max = max_results;

        tokio::task::spawn_blocking(move || {
            let re = regex::RegexBuilder::new(&pattern)
                .case_insensitive(false)
                .build()
                .map_err(|e| CaduceusError::Tool {
                    tool: "runtime".into(),
                    message: format!("Invalid regex: {e}"),
                })?;

            let glob_pattern = file_glob.unwrap_or_else(|| "**/*".to_string());
            let full_glob = root.join(&glob_pattern);
            let entries =
                glob::glob(&full_glob.to_string_lossy()).map_err(|e| CaduceusError::Tool {
                    tool: "runtime".into(),
                    message: format!("Invalid glob: {e}"),
                })?;

            let mut results = Vec::new();
            'outer: for entry in entries.flatten() {
                if !entry.is_file() {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&entry) {
                    for (line_no, line) in content.lines().enumerate() {
                        if re.is_match(line) {
                            let rel = entry
                                .strip_prefix(&root)
                                .unwrap_or(&entry)
                                .to_string_lossy();
                            results.push(format!("{}:{}: {}", rel, line_no + 1, line));
                            if results.len() >= max {
                                break 'outer;
                            }
                        }
                    }
                }
            }
            Ok(results)
        })
        .await
        .map_err(|e| CaduceusError::Tool {
            tool: "runtime".into(),
            message: format!("Grep task failed: {e}"),
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_creation() {
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

    #[tokio::test]
    async fn file_ops_exists() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        assert!(!ops.exists("nope.txt").await.unwrap());
        ops.write("yep.txt", "data").await.unwrap();
        assert!(ops.exists("yep.txt").await.unwrap());
    }

    #[tokio::test]
    async fn file_ops_list_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        ops.write("a.txt", "aaa").await.unwrap();
        ops.write("b.txt", "bbb").await.unwrap();
        let entries = ops.list_dir(".").await.unwrap();
        assert!(entries.contains(&"a.txt".to_string()));
        assert!(entries.contains(&"b.txt".to_string()));
    }

    #[tokio::test]
    async fn file_ops_edit() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        ops.write("code.rs", "fn main() { hello() }").await.unwrap();
        let changed = ops.edit("code.rs", "hello()", "world()").await.unwrap();
        assert_eq!(changed, 1);
        let content = ops.read("code.rs").await.unwrap();
        assert_eq!(content, "fn main() { world() }");
    }

    #[tokio::test]
    async fn file_ops_write_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        let huge = "x".repeat(11 * 1024 * 1024); // 11MB > 10MB limit
        let result = ops.write("big.txt", &huge).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn bash_sandbox_echo() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = BashSandbox::new(dir.path());
        let result = sandbox
            .execute(ExecRequest {
                command: "echo hello".into(),
                args: vec![],
                cwd: None,
                env: std::collections::HashMap::new(),
                timeout_secs: Some(5),
            })
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn bash_sandbox_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = BashSandbox::new(dir.path());
        let result = sandbox
            .execute(ExecRequest {
                command: "sleep 60".into(),
                args: vec![],
                cwd: None,
                env: std::collections::HashMap::new(),
                timeout_secs: Some(1),
            })
            .await
            .unwrap();
        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);
    }

    #[test]
    fn truncate_output_works() {
        let long = "a".repeat(2_000_000);
        let truncated = BashSandbox::truncate_output(&long, MAX_OUTPUT_SIZE);
        assert!(truncated.len() < long.len());
        assert!(truncated.ends_with("... [output truncated]"));
    }

    #[tokio::test]
    async fn file_ops_glob_search() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        ops.write("src/main.rs", "fn main(){}").await.unwrap();
        ops.write("src/lib.rs", "pub mod foo;").await.unwrap();
        ops.write("readme.md", "# Hello").await.unwrap();
        let results = ops.glob_search("src/*.rs").await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|r| r.contains("main.rs")));
        assert!(results.iter().any(|r| r.contains("lib.rs")));
    }

    #[tokio::test]
    async fn file_ops_grep_search() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        ops.write("a.txt", "hello world\nfoo bar\nhello again")
            .await
            .unwrap();
        ops.write("b.txt", "no match here").await.unwrap();
        let results = ops.grep_search("hello", Some("*.txt"), 100).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].contains("a.txt:1:"));
        assert!(results[1].contains("a.txt:3:"));
    }
}
