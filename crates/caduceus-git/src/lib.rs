pub mod checkpoints;

pub use checkpoints::{Checkpoint, CheckpointManager};

use caduceus_core::{CaduceusError, Result};
use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusEntry {
    pub path: String,
    pub status: FileStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileStatus {
    New,
    Modified,
    Deleted,
    Renamed { from: String },
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSummary {
    pub path: String,
    pub insertions: usize,
    pub deletions: usize,
    pub patch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitResult {
    pub oid: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchFreshness {
    pub branch: String,
    pub upstream: String,
    pub ahead: usize,
    pub behind: usize,
}

impl BranchFreshness {
    pub fn is_stale(&self) -> bool {
        self.behind > 0
    }
}

pub struct GitRepo {
    inner: git2::Repository,
}

impl GitRepo {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let inner = git2::Repository::open(path)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git open: {e}")))?;
        Ok(Self { inner })
    }

    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let inner = git2::Repository::discover(path)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git discover: {e}")))?;
        Ok(Self { inner })
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.inner.workdir().map(|p| p.to_path_buf())
    }

    /// Returns the current branch name, or a short SHA for detached HEAD.
    pub fn current_branch(&self) -> Result<String> {
        let head = self
            .inner
            .head()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git head: {e}")))?;

        if head.is_branch() {
            Ok(head.shorthand().unwrap_or("HEAD").to_string())
        } else {
            let oid = head
                .target()
                .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("no HEAD target")))?;
            let sha = oid.to_string();
            Ok(format!("HEAD ({})", &sha[..7.min(sha.len())]))
        }
    }

    pub fn check_freshness(&self) -> Result<Option<BranchFreshness>> {
        let head = self
            .inner
            .head()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git head: {e}")))?;
        if !head.is_branch() {
            return Ok(None);
        }

        let branch_name = head
            .shorthand()
            .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("missing branch shorthand")))?;
        let local_branch = self
            .inner
            .find_branch(branch_name, git2::BranchType::Local)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("find branch: {e}")))?;
        let upstream = match local_branch.upstream() {
            Ok(branch) => branch,
            Err(err) if err.code() == git2::ErrorCode::NotFound => return Ok(None),
            Err(err) => {
                return Err(CaduceusError::Other(anyhow::anyhow!(
                    "find upstream branch: {err}"
                )))
            }
        };

        let local_oid = local_branch
            .get()
            .target()
            .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("missing local branch target")))?;
        let upstream_oid = upstream
            .get()
            .target()
            .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("missing upstream target")))?;
        let (ahead, behind) = self
            .inner
            .graph_ahead_behind(local_oid, upstream_oid)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("graph ahead/behind: {e}")))?;

        Ok(Some(BranchFreshness {
            branch: branch_name.to_string(),
            upstream: upstream
                .name()
                .ok()
                .flatten()
                .unwrap_or_default()
                .to_string(),
            ahead,
            behind,
        }))
    }

    /// Returns the last `n` commits from HEAD.
    pub fn log(&self, n: usize) -> Result<Vec<CommitInfo>> {
        let mut revwalk = self
            .inner
            .revwalk()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
        revwalk
            .push_head()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;

        let mut commits = Vec::new();
        for oid in revwalk.take(n) {
            let oid = oid.map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
            let commit = self
                .inner
                .find_commit(oid)
                .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;

            let author_name = commit.author().name().unwrap_or("Unknown").to_string();
            let secs = commit.time().seconds();
            let date = chrono::Utc
                .timestamp_opt(secs, 0)
                .single()
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();

            commits.push(CommitInfo {
                sha: oid.to_string(),
                message: commit.message().unwrap_or("").trim().to_string(),
                author: author_name,
                date,
            });
        }
        Ok(commits)
    }

    pub fn status(&self) -> Result<Vec<StatusEntry>> {
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);

        let statuses = self
            .inner
            .statuses(Some(&mut opts))
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;

        let mut entries = Vec::new();
        for entry in statuses.iter() {
            let path = entry.path().unwrap_or("").to_string();
            let s = entry.status();
            let status = if s.is_index_new() || s.is_wt_new() {
                FileStatus::New
            } else if s.is_index_modified() || s.is_wt_modified() {
                FileStatus::Modified
            } else if s.is_index_deleted() || s.is_wt_deleted() {
                FileStatus::Deleted
            } else if s.is_index_renamed() || s.is_wt_renamed() {
                FileStatus::Renamed {
                    from: String::new(),
                }
            } else if s.is_conflicted() {
                FileStatus::Conflicted
            } else {
                FileStatus::Untracked
            };
            entries.push(StatusEntry { path, status });
        }
        Ok(entries)
    }

    /// Returns unified diff text for staged (`staged=true`) or unstaged changes.
    pub fn diff(&self, staged: bool) -> Result<String> {
        let diff = if staged {
            let head = self.inner.head().ok().and_then(|h| h.peel_to_tree().ok());
            self.inner
                .diff_tree_to_index(head.as_ref(), None, None)
                .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?
        } else {
            self.inner
                .diff_index_to_workdir(None, None)
                .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?
        };

        let mut output = Vec::<u8>::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            output.extend_from_slice(line.content());
            true
        })
        .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;

        Ok(String::from_utf8_lossy(&output).to_string())
    }

    pub fn diff_unstaged(&self) -> Result<Vec<DiffSummary>> {
        let diff = self
            .inner
            .diff_index_to_workdir(None, None)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
        self.diff_to_summaries(diff)
    }

    pub fn diff_staged(&self) -> Result<Vec<DiffSummary>> {
        let head = self.inner.head().ok().and_then(|h| h.peel_to_tree().ok());
        let diff = self
            .inner
            .diff_tree_to_index(head.as_ref(), None, None)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
        self.diff_to_summaries(diff)
    }

    fn diff_to_summaries(&self, diff: git2::Diff<'_>) -> Result<Vec<DiffSummary>> {
        // Use interior mutability so the FnMut closure can accumulate per-file data.
        let file_data: RefCell<HashMap<String, (usize, usize, String)>> =
            RefCell::new(HashMap::new());

        diff.print(git2::DiffFormat::Patch, |delta, _hunk, line| {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .and_then(|p| p.to_str())
                .unwrap_or("")
                .to_string();

            let mut map = file_data.borrow_mut();
            let entry = map.entry(path).or_insert((0usize, 0usize, String::new()));
            let content = std::str::from_utf8(line.content()).unwrap_or("");

            match line.origin() {
                '+' => {
                    entry.0 += 1;
                    entry.2.push('+');
                    entry.2.push_str(content);
                }
                '-' => {
                    entry.1 += 1;
                    entry.2.push('-');
                    entry.2.push_str(content);
                }
                _ => {
                    entry.2.push(line.origin());
                    entry.2.push_str(content);
                }
            }
            true
        })
        .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;

        Ok(file_data
            .into_inner()
            .into_iter()
            .map(|(path, (ins, del, patch))| DiffSummary {
                path,
                insertions: ins,
                deletions: del,
                patch,
            })
            .collect())
    }

    pub fn stage_paths(&self, paths: &[String]) -> Result<()> {
        let mut index = self
            .inner
            .index()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
        for path in paths {
            index
                .add_path(Path::new(path))
                .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
        }
        index
            .write()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
        Ok(())
    }

    pub fn commit(&self, message: &str) -> Result<CommitResult> {
        let sig = self
            .inner
            .signature()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
        let mut index = self
            .inner
            .index()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
        let tree_oid = index
            .write_tree()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
        let tree = self
            .inner
            .find_tree(tree_oid)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;

        let parent = self.inner.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();

        let oid = self
            .inner
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;

        Ok(CommitResult {
            oid: oid.to_string(),
            message: message.to_string(),
        })
    }
}

// ── Feature #136: Stale-base Preflight / Git Freshness ────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitFreshness {
    pub current_branch: String,
    pub tracking_branch: Option<String>,
    pub commits_behind: usize,
    pub commits_ahead: usize,
    pub is_diverged: bool,
    pub last_fetch: Option<u64>,
    pub is_stale: bool,
}

pub struct StaleBaseChecker;

impl StaleBaseChecker {
    pub fn check_freshness(repo_path: &Path) -> Result<GitFreshness> {
        let repo = git2::Repository::discover(repo_path)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git discover: {e}")))?;

        let head = repo
            .head()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("head: {e}")))?;

        let current_branch = if head.is_branch() {
            head.shorthand().unwrap_or("HEAD").to_string()
        } else {
            let oid = head
                .target()
                .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("no HEAD target")))?;
            let sha = oid.to_string();
            format!("HEAD ({})", &sha[..7.min(sha.len())])
        };

        let git_dir = repo.path();
        let fetch_head = git_dir.join("FETCH_HEAD");
        let last_fetch = fetch_head.metadata().ok().and_then(|m| {
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
        });

        if !head.is_branch() {
            return Ok(GitFreshness {
                current_branch,
                tracking_branch: None,
                commits_behind: 0,
                commits_ahead: 0,
                is_diverged: false,
                last_fetch,
                is_stale: false,
            });
        }

        let branch_name = head.shorthand().unwrap_or("HEAD");
        let local_branch = repo
            .find_branch(branch_name, git2::BranchType::Local)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("find branch: {e}")))?;

        let upstream = match local_branch.upstream() {
            Ok(b) => b,
            Err(_) => {
                return Ok(GitFreshness {
                    current_branch,
                    tracking_branch: None,
                    commits_behind: 0,
                    commits_ahead: 0,
                    is_diverged: false,
                    last_fetch,
                    is_stale: false,
                });
            }
        };

        let tracking_branch = upstream.name().ok().flatten().map(|s| s.to_string());
        let local_oid = local_branch
            .get()
            .target()
            .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("no local target")))?;
        let upstream_oid = upstream
            .get()
            .target()
            .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("no upstream target")))?;
        let (ahead, behind) = repo
            .graph_ahead_behind(local_oid, upstream_oid)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("graph: {e}")))?;

        let is_diverged = ahead > 0 && behind > 0;
        let is_stale = behind > 0;

        Ok(GitFreshness {
            current_branch,
            tracking_branch,
            commits_behind: behind,
            commits_ahead: ahead,
            is_diverged,
            last_fetch,
            is_stale,
        })
    }

    pub fn is_stale(repo_path: &Path, max_behind: usize) -> Result<bool> {
        let freshness = Self::check_freshness(repo_path)?;
        Ok(freshness.commits_behind > max_behind)
    }

    pub fn check_diverged(repo_path: &Path) -> Result<bool> {
        let freshness = Self::check_freshness(repo_path)?;
        Ok(freshness.is_diverged)
    }
}

// ── Feature #93: Worktree Isolation ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub head_sha: String,
    pub is_locked: bool,
}

pub struct WorktreeManager;

impl WorktreeManager {
    pub fn create_worktree(repo_path: &Path, branch: &str, worktree_path: &Path) -> Result<()> {
        let output = std::process::Command::new("git")
            .args(["worktree", "add", "-b", branch])
            .arg(worktree_path)
            .arg("HEAD")
            .current_dir(repo_path)
            .output()
            .map_err(CaduceusError::Io)?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(CaduceusError::Other(anyhow::anyhow!(
                "git worktree add: {err}"
            )));
        }
        Ok(())
    }

    pub fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> Result<()> {
        let output = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(worktree_path)
            .current_dir(repo_path)
            .output()
            .map_err(CaduceusError::Io)?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(CaduceusError::Other(anyhow::anyhow!(
                "git worktree remove: {err}"
            )));
        }
        Ok(())
    }

    pub fn list_worktrees(repo_path: &Path) -> Result<Vec<WorktreeInfo>> {
        let output = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(repo_path)
            .output()
            .map_err(CaduceusError::Io)?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(CaduceusError::Other(anyhow::anyhow!(
                "git worktree list: {err}"
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut worktrees = Vec::new();
        let mut current_path: Option<PathBuf> = None;
        let mut current_branch = String::new();
        let mut current_sha = String::new();
        let mut current_locked = false;

        for line in stdout.lines() {
            if line.is_empty() {
                if let Some(path) = current_path.take() {
                    worktrees.push(WorktreeInfo {
                        path,
                        branch: std::mem::take(&mut current_branch),
                        head_sha: std::mem::take(&mut current_sha),
                        is_locked: current_locked,
                    });
                    current_locked = false;
                }
            } else if let Some(path) = line.strip_prefix("worktree ") {
                current_path = Some(PathBuf::from(path));
            } else if let Some(sha) = line.strip_prefix("HEAD ") {
                current_sha = sha.to_string();
            } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                current_branch = branch.to_string();
            } else if line.starts_with("detached") {
                current_branch = "HEAD (detached)".to_string();
            } else if line.starts_with("locked") {
                current_locked = true;
            }
        }

        // Handle last entry when output has no trailing blank line
        if let Some(path) = current_path {
            worktrees.push(WorktreeInfo {
                path,
                branch: current_branch,
                head_sha: current_sha,
                is_locked: current_locked,
            });
        }

        Ok(worktrees)
    }
}

// ── Feature #163: Auto-commit / Auto-PR per task ──────────────────────────────

pub struct AutoCommitter {
    pub enabled: bool,
    pub commit_message_template: String,
    pub auto_pr: bool,
}

impl Default for AutoCommitter {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoCommitter {
    pub fn new() -> Self {
        Self {
            enabled: true,
            commit_message_template: "auto: {task}".to_string(),
            auto_pr: false,
        }
    }

    /// Stage all changes and create a commit. Returns the commit SHA.
    pub fn commit_changes(repo_path: &Path, message: &str) -> Result<String> {
        let repo = git2::Repository::discover(repo_path)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git discover: {e}")))?;

        let sig = repo
            .signature()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("signature: {e}")))?;

        let mut index = repo
            .index()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("index: {e}")))?;

        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("add all: {e}")))?;
        index
            .write()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("write index: {e}")))?;

        let tree_oid = index
            .write_tree()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("write tree: {e}")))?;
        let tree = repo
            .find_tree(tree_oid)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("find tree: {e}")))?;

        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();

        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("commit: {e}")))?;

        Ok(oid.to_string())
    }

    /// Create a new branch for the task from HEAD. Returns the branch name.
    pub fn create_task_branch(repo_path: &Path, task_name: &str) -> Result<String> {
        let repo = git2::Repository::discover(repo_path)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git discover: {e}")))?;

        let sanitized: String = task_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .to_lowercase();
        let sanitized = sanitized.trim_matches('-');
        let branch_name = format!("task/{sanitized}");

        let head_commit = repo
            .head()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("head: {e}")))?
            .peel_to_commit()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("peel: {e}")))?;

        repo.branch(&branch_name, &head_commit, false)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("create branch: {e}")))?;

        Ok(branch_name)
    }

    /// Auto-generate a commit message from a list of changed file paths.
    pub fn generate_commit_message(changes: &[String]) -> String {
        if changes.is_empty() {
            return "auto: no changes".to_string();
        }
        if changes.len() == 1 {
            return format!("auto: update {}", changes[0]);
        }
        let preview: Vec<&str> = changes.iter().take(3).map(String::as_str).collect();
        let suffix = if changes.len() > 3 {
            format!(" and {} more", changes.len() - 3)
        } else {
            String::new()
        };
        format!("auto: update {}{}", preview.join(", "), suffix)
    }
}

// ── #217: Scaffold Quality Benchmark ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ScaffoldMeasurement {
    pub model: String,
    pub task_complexity: String,
    pub tokens_used: usize,
    pub tools_called: usize,
    pub errors_recovered: usize,
    pub success: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Default)]
pub struct ScaffoldBenchmark {
    measurements: Vec<ScaffoldMeasurement>,
}

impl ScaffoldBenchmark {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, measurement: ScaffoldMeasurement) {
        self.measurements.push(measurement);
    }

    /// efficiency = success_rate × (1000 / avg_tokens), normalized so that
    /// using fewer tokens at the same success rate is more efficient.
    pub fn efficiency_score(&self) -> f64 {
        if self.measurements.is_empty() {
            return 0.0;
        }
        let successes = self.measurements.iter().filter(|m| m.success).count();
        let success_rate = successes as f64 / self.measurements.len() as f64;
        let avg_tokens: f64 = self
            .measurements
            .iter()
            .map(|m| m.tokens_used as f64)
            .sum::<f64>()
            / self.measurements.len() as f64;
        let avg_tokens_normalized = avg_tokens.max(1.0) / 1000.0;
        success_rate * (1.0 / avg_tokens_normalized)
    }

    pub fn by_model<'a>(&'a self, model: &str) -> Vec<&'a ScaffoldMeasurement> {
        self.measurements
            .iter()
            .filter(|m| m.model == model)
            .collect()
    }

    pub fn by_complexity<'a>(&'a self, complexity: &str) -> Vec<&'a ScaffoldMeasurement> {
        self.measurements
            .iter()
            .filter(|m| m.task_complexity == complexity)
            .collect()
    }

    /// Returns `(model, efficiency_score)` pairs sorted descending by score.
    pub fn compare_models(&self) -> Vec<(String, f64)> {
        let mut model_names: Vec<String> = self
            .measurements
            .iter()
            .map(|m| m.model.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        model_names.sort_unstable();

        let mut scores: Vec<(String, f64)> = model_names
            .into_iter()
            .map(|model| {
                let subset: Vec<&ScaffoldMeasurement> = self.by_model(&model);
                let successes = subset.iter().filter(|m| m.success).count();
                let success_rate = successes as f64 / subset.len() as f64;
                let avg_tokens: f64 =
                    subset.iter().map(|m| m.tokens_used as f64).sum::<f64>() / subset.len() as f64;
                let avg_tokens_normalized = avg_tokens.max(1.0) / 1000.0;
                let score = success_rate * (1.0 / avg_tokens_normalized);
                (model, score)
            })
            .collect();

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn make_temp_repo() -> (tempfile::TempDir, GitRepo) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        // Configure git user for commits
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();

        // Create an initial commit so HEAD exists
        let sig = repo.signature().unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            let path = dir.path().join("README.md");
            std::fs::write(&path, "# Test Repo\n").unwrap();
            index.add_path(std::path::Path::new("README.md")).unwrap();
            index.write().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
            .unwrap();

        let git_repo = GitRepo::open(dir.path()).expect("open temp repo");
        (dir, git_repo)
    }

    fn commit_file(
        repo: &git2::Repository,
        repo_root: &Path,
        path: &str,
        content: &str,
        message: &str,
    ) {
        fs::write(repo_root.join(path), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap();
    }

    fn setup_remote_tracking_repo() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
        GitRepo,
        git2::Repository,
        String,
    ) {
        let remote_dir = tempfile::tempdir().unwrap();
        let _remote_repo = git2::Repository::init_bare(remote_dir.path()).unwrap();

        let seed_dir = tempfile::tempdir().unwrap();
        let seed_repo = git2::Repository::init(seed_dir.path()).unwrap();
        let mut seed_config = seed_repo.config().unwrap();
        seed_config.set_str("user.name", "Test User").unwrap();
        seed_config
            .set_str("user.email", "test@example.com")
            .unwrap();
        commit_file(
            &seed_repo,
            seed_dir.path(),
            "README.md",
            "# Seed Repo\n",
            "Initial commit",
        );
        let branch = seed_repo.head().unwrap().shorthand().unwrap().to_string();
        if seed_repo.find_remote("origin").is_err() {
            seed_repo
                .remote("origin", remote_dir.path().to_str().unwrap())
                .unwrap();
        }
        let mut push_remote = seed_repo.find_remote("origin").unwrap();
        push_remote
            .push(&[format!("refs/heads/{0}:refs/heads/{0}", branch)], None)
            .unwrap();

        let local_dir = tempfile::tempdir().unwrap();
        let local_repo = git2::build::RepoBuilder::new()
            .clone(remote_dir.path().to_str().unwrap(), local_dir.path())
            .unwrap();
        let mut local_config = local_repo.config().unwrap();
        local_config.set_str("user.name", "Test User").unwrap();
        local_config
            .set_str("user.email", "test@example.com")
            .unwrap();

        let updater_dir = tempfile::tempdir().unwrap();
        let updater_repo = git2::build::RepoBuilder::new()
            .clone(remote_dir.path().to_str().unwrap(), updater_dir.path())
            .unwrap();
        let mut updater_config = updater_repo.config().unwrap();
        updater_config.set_str("user.name", "Test User").unwrap();
        updater_config
            .set_str("user.email", "test@example.com")
            .unwrap();

        let git_repo = GitRepo::open(local_dir.path()).expect("open local clone");
        (
            remote_dir,
            local_dir,
            updater_dir,
            git_repo,
            updater_repo,
            branch,
        )
    }

    #[test]
    fn file_status_variants_exist() {
        let _new = FileStatus::New;
        let _modified = FileStatus::Modified;
        let _deleted = FileStatus::Deleted;
        let _renamed = FileStatus::Renamed {
            from: "old.rs".into(),
        };
        let _untracked = FileStatus::Untracked;
        let _conflicted = FileStatus::Conflicted;
    }

    #[test]
    fn open_temp_repo() {
        let (_dir, _repo) = make_temp_repo();
    }

    #[test]
    fn discover_from_subdirectory() {
        let (dir, _repo) = make_temp_repo();
        let sub = dir.path().join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        let discovered = GitRepo::discover(sub.to_str().unwrap());
        assert!(discovered.is_ok(), "should discover repo from subdirectory");
    }

    #[test]
    fn current_branch_returns_string() {
        let (_dir, repo) = make_temp_repo();
        let branch = repo.current_branch().expect("get branch");
        assert!(!branch.is_empty());
    }

    #[test]
    fn log_returns_commits() {
        let (_dir, repo) = make_temp_repo();
        let commits = repo.log(5).expect("get log");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].sha.len(), 40);
        assert_eq!(commits[0].author, "Test User");
        assert!(commits[0].message.contains("Initial commit"));
    }

    #[test]
    fn status_returns_entries() {
        let (dir, repo) = make_temp_repo();
        // Create an untracked file
        std::fs::write(dir.path().join("new.txt"), "hello").unwrap();
        let entries = repo.status().expect("get status");
        assert!(!entries.is_empty());
    }

    #[test]
    fn diff_unstaged_succeeds() {
        let (dir, repo) = make_temp_repo();
        // Modify tracked file
        std::fs::write(dir.path().join("README.md"), "# Changed\n").unwrap();
        let summaries = repo.diff_unstaged().expect("diff unstaged");
        assert!(!summaries.is_empty());
        assert_eq!(summaries[0].path, "README.md");
    }

    #[test]
    fn diff_text_staged_is_string() {
        let (_dir, repo) = make_temp_repo();
        let text = repo.diff(true).expect("diff staged text");
        // Nothing staged, should be empty
        assert!(text.is_empty());
    }

    #[test]
    fn commit_result_serializes() {
        let cr = CommitResult {
            oid: "abc123".into(),
            message: "test commit".into(),
        };
        let json = serde_json::to_string(&cr).expect("serialize");
        assert!(json.contains("abc123"));
    }

    #[test]
    fn check_freshness_returns_none_without_upstream() {
        let (_dir, repo) = make_temp_repo();
        assert_eq!(repo.check_freshness().unwrap(), None);
    }

    #[test]
    fn check_freshness_detects_stale_branch() {
        let (_remote_dir, local_dir, updater_dir, repo, updater_repo, branch) =
            setup_remote_tracking_repo();

        commit_file(
            &updater_repo,
            updater_dir.path(),
            "CHANGELOG.md",
            "new line\n",
            "Remote update",
        );
        let mut updater_remote = updater_repo.find_remote("origin").unwrap();
        updater_remote
            .push(&[format!("refs/heads/{0}:refs/heads/{0}", branch)], None)
            .unwrap();

        let local_repo = git2::Repository::open(local_dir.path()).unwrap();
        let mut local_remote = local_repo.find_remote("origin").unwrap();
        local_remote.fetch(&[&branch], None, None).unwrap();

        let freshness = repo.check_freshness().unwrap().unwrap();
        assert!(freshness.is_stale());
        assert_eq!(freshness.behind, 1);
        assert_eq!(freshness.ahead, 0);
    }

    // ── StaleBaseChecker tests ────────────────────────────────────────────

    #[test]
    fn stale_base_checker_fresh_repo_no_upstream() {
        let (dir, _) = make_temp_repo();
        let freshness = StaleBaseChecker::check_freshness(dir.path()).unwrap();
        assert!(!freshness.is_stale);
        assert_eq!(freshness.commits_behind, 0);
        assert_eq!(freshness.commits_ahead, 0);
        assert!(!freshness.is_diverged);
        assert!(freshness.tracking_branch.is_none());
    }

    #[test]
    fn stale_base_checker_is_stale_false_for_no_upstream() {
        let (dir, _) = make_temp_repo();
        assert!(!StaleBaseChecker::is_stale(dir.path(), 0).unwrap());
    }

    #[test]
    fn stale_base_checker_not_diverged_no_upstream() {
        let (dir, _) = make_temp_repo();
        assert!(!StaleBaseChecker::check_diverged(dir.path()).unwrap());
    }

    #[test]
    fn stale_base_checker_detects_stale_with_upstream() {
        let (_remote_dir, local_dir, updater_dir, _repo, updater_repo, branch) =
            setup_remote_tracking_repo();

        commit_file(
            &updater_repo,
            updater_dir.path(),
            "NEWS.md",
            "new line\n",
            "Remote update for stale check",
        );
        let mut remote = updater_repo.find_remote("origin").unwrap();
        remote
            .push(&[format!("refs/heads/{0}:refs/heads/{0}", branch)], None)
            .unwrap();

        let local_repo = git2::Repository::open(local_dir.path()).unwrap();
        let mut local_remote = local_repo.find_remote("origin").unwrap();
        local_remote.fetch(&[&branch], None, None).unwrap();

        let freshness = StaleBaseChecker::check_freshness(local_dir.path()).unwrap();
        assert!(freshness.is_stale);
        assert_eq!(freshness.commits_behind, 1);
        assert_eq!(freshness.commits_ahead, 0);
        assert!(!freshness.is_diverged);
        assert!(freshness.tracking_branch.is_some());
        assert!(freshness.last_fetch.is_some());

        assert!(StaleBaseChecker::is_stale(local_dir.path(), 0).unwrap());
        assert!(!StaleBaseChecker::is_stale(local_dir.path(), 1).unwrap());
        assert!(!StaleBaseChecker::check_diverged(local_dir.path()).unwrap());
    }

    // ── WorktreeManager tests ─────────────────────────────────────────────

    #[test]
    fn worktree_create_and_list() {
        let (dir, _) = make_temp_repo();
        let wt_parent = tempfile::tempdir().unwrap();
        let wt_path = wt_parent.path().join("wt-feature");

        WorktreeManager::create_worktree(dir.path(), "wt-feature-branch", &wt_path).unwrap();

        let worktrees = WorktreeManager::list_worktrees(dir.path()).unwrap();
        assert!(worktrees.len() >= 2, "should have main + new worktree");
        assert!(
            worktrees.iter().any(|w| w.branch == "wt-feature-branch"),
            "new worktree branch not found in list"
        );
        let wt = worktrees
            .iter()
            .find(|w| w.branch == "wt-feature-branch")
            .unwrap();
        assert!(!wt.head_sha.is_empty());
        assert!(!wt.is_locked);
    }

    #[test]
    fn worktree_remove() {
        let (dir, _) = make_temp_repo();
        let wt_parent = tempfile::tempdir().unwrap();
        let wt_path = wt_parent.path().join("wt-removable");

        WorktreeManager::create_worktree(dir.path(), "wt-removable-branch", &wt_path).unwrap();

        let before = WorktreeManager::list_worktrees(dir.path()).unwrap();
        assert!(before.iter().any(|w| w.branch == "wt-removable-branch"));

        WorktreeManager::remove_worktree(dir.path(), &wt_path).unwrap();

        let after = WorktreeManager::list_worktrees(dir.path()).unwrap();
        assert!(!after.iter().any(|w| w.branch == "wt-removable-branch"));
    }

    #[test]
    fn worktree_list_main_only() {
        let (dir, _) = make_temp_repo();
        let worktrees = WorktreeManager::list_worktrees(dir.path()).unwrap();
        assert_eq!(worktrees.len(), 1, "only main worktree expected");
        assert!(!worktrees[0].head_sha.is_empty());
    }

    // ── AutoCommitter tests ───────────────────────────────────────────────

    #[test]
    fn auto_committer_generate_message_empty() {
        assert_eq!(
            AutoCommitter::generate_commit_message(&[]),
            "auto: no changes"
        );
    }

    #[test]
    fn auto_committer_generate_message_single() {
        let msg = AutoCommitter::generate_commit_message(&["src/main.rs".to_string()]);
        assert_eq!(msg, "auto: update src/main.rs");
    }

    #[test]
    fn auto_committer_generate_message_two() {
        let msg = AutoCommitter::generate_commit_message(&["a.rs".to_string(), "b.rs".to_string()]);
        assert!(msg.contains("a.rs"));
        assert!(msg.contains("b.rs"));
    }

    #[test]
    fn auto_committer_generate_message_overflow() {
        let changes: Vec<String> = (1..=5).map(|i| format!("file{i}.rs")).collect();
        let msg = AutoCommitter::generate_commit_message(&changes);
        assert!(msg.contains("file1.rs"));
        assert!(msg.contains("and 2 more"));
    }

    #[test]
    fn auto_committer_new_defaults() {
        let ac = AutoCommitter::new();
        assert!(ac.enabled);
        assert!(!ac.auto_pr);
        assert!(ac.commit_message_template.contains("{task}"));
    }

    #[test]
    fn auto_committer_commit_changes() {
        let (dir, _) = make_temp_repo();
        std::fs::write(dir.path().join("auto_file.txt"), "auto content").unwrap();
        let sha = AutoCommitter::commit_changes(dir.path(), "auto test commit").unwrap();
        assert_eq!(sha.len(), 40);

        let repo = git2::Repository::open(dir.path()).unwrap();
        let commit = repo
            .find_commit(git2::Oid::from_str(&sha).unwrap())
            .unwrap();
        assert_eq!(commit.message().unwrap().trim(), "auto test commit");
    }

    #[test]
    fn auto_committer_create_task_branch() {
        let (dir, _) = make_temp_repo();
        let branch = AutoCommitter::create_task_branch(dir.path(), "My Feature Task!").unwrap();
        assert_eq!(branch, "task/my-feature-task");

        let repo = git2::Repository::open(dir.path()).unwrap();
        assert!(repo.find_branch(&branch, git2::BranchType::Local).is_ok());
    }

    #[test]
    fn auto_committer_create_task_branch_simple() {
        let (dir, _) = make_temp_repo();
        let branch = AutoCommitter::create_task_branch(dir.path(), "cleanup").unwrap();
        assert_eq!(branch, "task/cleanup");
    }

    // ── #217: ScaffoldBenchmark tests ─────────────────────────────────────────

    fn make_measurement(
        model: &str,
        complexity: &str,
        tokens: usize,
        success: bool,
    ) -> ScaffoldMeasurement {
        ScaffoldMeasurement {
            model: model.to_string(),
            task_complexity: complexity.to_string(),
            tokens_used: tokens,
            tools_called: 3,
            errors_recovered: 0,
            success,
            duration_ms: 500,
        }
    }

    #[test]
    fn scaffold_benchmark_empty_efficiency() {
        let bench = ScaffoldBenchmark::new();
        assert_eq!(bench.efficiency_score(), 0.0);
    }

    #[test]
    fn scaffold_benchmark_record_and_efficiency() {
        let mut bench = ScaffoldBenchmark::new();
        bench.record(make_measurement("gpt-4", "simple", 1000, true));
        bench.record(make_measurement("gpt-4", "simple", 1000, true));
        // success_rate = 1.0, avg_tokens = 1000, normalized = 1.0 → efficiency = 1.0
        let score = bench.efficiency_score();
        assert!((score - 1.0).abs() < 1e-10);
    }

    #[test]
    fn scaffold_benchmark_partial_success_lowers_efficiency() {
        let mut bench = ScaffoldBenchmark::new();
        bench.record(make_measurement("m", "simple", 1000, true));
        bench.record(make_measurement("m", "simple", 1000, false));
        // success_rate = 0.5, avg_tokens_norm = 1.0 → 0.5
        let score = bench.efficiency_score();
        assert!((score - 0.5).abs() < 1e-10);
    }

    #[test]
    fn scaffold_benchmark_fewer_tokens_more_efficient() {
        let mut bench_cheap = ScaffoldBenchmark::new();
        bench_cheap.record(make_measurement("cheap", "simple", 500, true));

        let mut bench_expensive = ScaffoldBenchmark::new();
        bench_expensive.record(make_measurement("expensive", "simple", 2000, true));

        assert!(bench_cheap.efficiency_score() > bench_expensive.efficiency_score());
    }

    #[test]
    fn scaffold_benchmark_by_model() {
        let mut bench = ScaffoldBenchmark::new();
        bench.record(make_measurement("gpt-4", "simple", 1000, true));
        bench.record(make_measurement("claude", "complex", 2000, false));
        bench.record(make_measurement("gpt-4", "medium", 1500, true));

        let gpt4 = bench.by_model("gpt-4");
        assert_eq!(gpt4.len(), 2);
        assert!(gpt4.iter().all(|m| m.model == "gpt-4"));

        let claude = bench.by_model("claude");
        assert_eq!(claude.len(), 1);
    }

    #[test]
    fn scaffold_benchmark_by_complexity() {
        let mut bench = ScaffoldBenchmark::new();
        bench.record(make_measurement("m", "simple", 1000, true));
        bench.record(make_measurement("m", "complex", 2000, true));
        bench.record(make_measurement("m", "simple", 800, false));

        let simple = bench.by_complexity("simple");
        assert_eq!(simple.len(), 2);
        assert!(simple.iter().all(|m| m.task_complexity == "simple"));
    }

    #[test]
    fn scaffold_benchmark_compare_models_sorted_descending() {
        let mut bench = ScaffoldBenchmark::new();
        // cheap-model: 100% success, 500 tokens → efficiency = 2.0
        bench.record(make_measurement("cheap-model", "simple", 500, true));
        // heavy-model: 100% success, 2000 tokens → efficiency = 0.5
        bench.record(make_measurement("heavy-model", "simple", 2000, true));

        let ranking = bench.compare_models();
        assert_eq!(ranking.len(), 2);
        assert_eq!(ranking[0].0, "cheap-model");
        assert!(ranking[0].1 > ranking[1].1);
    }

    #[test]
    fn scaffold_benchmark_compare_models_empty() {
        let bench = ScaffoldBenchmark::new();
        assert!(bench.compare_models().is_empty());
    }
}
