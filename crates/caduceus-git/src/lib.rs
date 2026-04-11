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
}
