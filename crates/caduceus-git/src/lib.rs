use caduceus_core::{CaduceusError, Result};
use serde::{Deserialize, Serialize};
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
                FileStatus::Renamed { from: String::new() }
            } else if s.is_conflicted() {
                FileStatus::Conflicted
            } else {
                FileStatus::Untracked
            };
            entries.push(StatusEntry { path, status });
        }
        Ok(entries)
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
        let stats = diff
            .stats()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("{e}")))?;
        let _ = stats; // stats are per-file; iterate deltas for per-file info

        let mut summaries = Vec::new();
        for delta in diff.deltas() {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .and_then(|p| p.to_str())
                .unwrap_or("")
                .to_string();
            summaries.push(DiffSummary {
                path,
                insertions: 0,
                deletions: 0,
                patch: String::new(),
            });
        }
        Ok(summaries)
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

        let parent = self
            .inner
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok());
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

    #[test]
    fn it_works() {
        // Just test that types exist
        let _status = FileStatus::New;
        let _status = FileStatus::Modified;
    }
}
