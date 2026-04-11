use caduceus_core::{CaduceusError, Result};
use chrono::{DateTime, Utc};
use git2::{
    build::CheckoutBuilder, DiffFormat, DiffOptions, IndexAddOption, Oid, Repository, Signature,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const CHECKPOINT_REF_PREFIX: &str = "refs/caduceus/checkpoints";
const CHECKPOINT_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub id: String,
    pub session_id: String,
    pub message: String,
    pub stash_id: Option<Oid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredCheckpoint {
    schema_version: u8,
    id: String,
    session_id: String,
    message: String,
    created_at: DateTime<Utc>,
}

pub struct CheckpointManager {
    repo: Repository,
    checkpoints: Vec<Checkpoint>,
}

impl CheckpointManager {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let repo = Repository::open(path)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git open: {e}")))?;
        Self::new(repo)
    }

    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let repo = Repository::discover(path)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git discover: {e}")))?;
        Self::new(repo)
    }

    pub fn new(repo: Repository) -> Result<Self> {
        let checkpoints = Self::load_checkpoints(&repo)?;
        Ok(Self { repo, checkpoints })
    }

    pub fn create(&mut self, session_id: &str, message: &str) -> Result<Checkpoint> {
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let tree_oid = Self::snapshot_tree(&self.repo)?;
        let signature = Self::signature(&self.repo)?;
        let metadata = StoredCheckpoint {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            id: id.clone(),
            session_id: session_id.to_string(),
            message: message.to_string(),
            created_at,
        };
        let ref_name = Self::ref_name(&id);
        let oid = {
            let parent_commit = self
                .repo
                .head()
                .ok()
                .and_then(|head| head.peel_to_commit().ok());
            let parents: Vec<&git2::Commit<'_>> = parent_commit.iter().collect();
            let tree = self
                .repo
                .find_tree(tree_oid)
                .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git find tree: {e}")))?;
            self.repo
                .commit(
                    Some(&ref_name),
                    &signature,
                    &signature,
                    &Self::encode_commit_message(&metadata)?,
                    &tree,
                    &parents,
                )
                .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint commit: {e}")))?
        };

        let checkpoint = Checkpoint {
            id,
            session_id: session_id.to_string(),
            message: message.to_string(),
            stash_id: Some(oid),
            created_at,
        };
        self.checkpoints.push(checkpoint.clone());
        self.sort_checkpoints();
        Ok(checkpoint)
    }

    pub fn restore(&self, checkpoint_id: &str) -> Result<()> {
        let commit = self.find_checkpoint_commit(checkpoint_id)?;
        let tree = commit
            .tree()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint tree: {e}")))?;
        let mut checkout = CheckoutBuilder::new();
        checkout.force();
        checkout.remove_untracked(true);
        checkout.remove_ignored(true);
        self.repo
            .checkout_tree(tree.as_object(), Some(&mut checkout))
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint checkout: {e}")))?;
        let mut index = self
            .repo
            .index()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git index: {e}")))?;
        index
            .read_tree(&tree)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git index read tree: {e}")))?;
        index
            .write()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git index write: {e}")))?;
        Ok(())
    }

    pub fn list(&self, session_id: &str) -> Vec<&Checkpoint> {
        self.checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.session_id == session_id)
            .collect()
    }

    pub fn diff(&self, checkpoint_id: &str) -> Result<String> {
        let commit = self.find_checkpoint_commit(checkpoint_id)?;
        let tree = commit
            .tree()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint tree: {e}")))?;
        let mut options = DiffOptions::new();
        options.include_untracked(true).recurse_untracked_dirs(true);
        let diff = self
            .repo
            .diff_tree_to_workdir_with_index(Some(&tree), Some(&mut options))
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint diff: {e}")))?;
        let mut output = Vec::<u8>::new();
        diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
            output.extend_from_slice(line.content());
            true
        })
        .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint diff print: {e}")))?;
        Ok(String::from_utf8_lossy(&output).to_string())
    }

    pub fn prune(&mut self, keep: usize) -> Result<usize> {
        self.sort_checkpoints();
        if self.checkpoints.len() <= keep {
            return Ok(0);
        }
        let remove_count = self.checkpoints.len() - keep;
        let removed: Vec<Checkpoint> = self.checkpoints.drain(..remove_count).collect();
        for checkpoint in &removed {
            let mut reference = self
                .repo
                .find_reference(&Self::ref_name(&checkpoint.id))
                .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint ref: {e}")))?;
            reference
                .delete()
                .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint delete: {e}")))?;
        }
        Ok(remove_count)
    }

    fn load_checkpoints(repo: &Repository) -> Result<Vec<Checkpoint>> {
        let mut checkpoints = Vec::new();
        let mut refs = repo
            .references_glob(&format!("{CHECKPOINT_REF_PREFIX}/*"))
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint refs: {e}")))?;
        for reference in refs.by_ref() {
            let reference = reference
                .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint ref: {e}")))?;
            let Some(target) = reference.target() else {
                continue;
            };
            let commit = repo
                .find_commit(target)
                .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint commit: {e}")))?;
            if let Some(checkpoint) = Self::decode_checkpoint(&commit) {
                checkpoints.push(checkpoint);
            }
        }
        checkpoints.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(checkpoints)
    }

    fn snapshot_tree(repo: &Repository) -> Result<Oid> {
        let _index_snapshot = IndexSnapshot::capture(repo)?;
        let deleted_paths = Self::deleted_paths(repo)?;
        let mut index = repo
            .index()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git index: {e}")))?;
        index
            .read(true)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git index read: {e}")))?;
        index
            .add_all(["*"], IndexAddOption::DEFAULT, None)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git index add_all: {e}")))?;
        for path in deleted_paths {
            match index.remove_path(Path::new(&path)) {
                Ok(()) => {}
                Err(err) if err.code() == git2::ErrorCode::NotFound => {}
                Err(err) => {
                    return Err(CaduceusError::Other(anyhow::anyhow!(
                        "git index remove_path: {err}"
                    )))
                }
            }
        }
        index
            .write()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git index write: {e}")))?;
        index
            .write_tree()
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git write tree: {e}")))
    }

    fn deleted_paths(repo: &Repository) -> Result<Vec<String>> {
        let mut options = git2::StatusOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(false);
        let statuses = repo
            .statuses(Some(&mut options))
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git statuses: {e}")))?;
        let mut deleted = Vec::new();
        for entry in statuses.iter() {
            let status = entry.status();
            if (status.is_wt_deleted() || status.is_index_deleted()) && entry.path().is_some() {
                deleted.push(entry.path().unwrap_or_default().to_string());
            }
        }
        deleted.sort();
        deleted.dedup();
        Ok(deleted)
    }

    fn signature(repo: &Repository) -> Result<Signature<'static>> {
        repo.signature()
            .or_else(|_| Signature::now("Caduceus", "caduceus@local"))
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git signature: {e}")))
    }

    fn encode_commit_message(metadata: &StoredCheckpoint) -> Result<String> {
        Ok(format!(
            "caduceus-checkpoint\n\n{}",
            serde_json::to_string(metadata)?
        ))
    }

    fn decode_checkpoint(commit: &git2::Commit<'_>) -> Option<Checkpoint> {
        let message = commit.message()?;
        let payload = message
            .split_once("\n\n")
            .map(|(_, json)| json)
            .unwrap_or(message);
        let metadata: StoredCheckpoint = serde_json::from_str(payload).ok()?;
        if metadata.schema_version != CHECKPOINT_SCHEMA_VERSION {
            return None;
        }
        Some(Checkpoint {
            id: metadata.id,
            session_id: metadata.session_id,
            message: metadata.message,
            stash_id: Some(commit.id()),
            created_at: metadata.created_at,
        })
    }

    fn find_checkpoint_commit(&self, checkpoint_id: &str) -> Result<git2::Commit<'_>> {
        let oid = self
            .repo
            .find_reference(&Self::ref_name(checkpoint_id))
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint ref: {e}")))?
            .target()
            .ok_or_else(|| {
                CaduceusError::Other(anyhow::anyhow!("checkpoint ref missing target"))
            })?;
        self.repo
            .find_commit(oid)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("git checkpoint commit: {e}")))
    }

    fn ref_name(checkpoint_id: &str) -> String {
        format!("{CHECKPOINT_REF_PREFIX}/{checkpoint_id}")
    }

    fn sort_checkpoints(&mut self) {
        self.checkpoints
            .sort_by(|left, right| left.created_at.cmp(&right.created_at));
    }
}

struct IndexSnapshot {
    index_path: PathBuf,
    original_bytes: Option<Vec<u8>>,
}

impl IndexSnapshot {
    fn capture(repo: &Repository) -> Result<Self> {
        let index_path = repo.path().join("index");
        let original_bytes = fs::read(&index_path).ok();
        Ok(Self {
            index_path,
            original_bytes,
        })
    }
}

impl Drop for IndexSnapshot {
    fn drop(&mut self) {
        match &self.original_bytes {
            Some(bytes) => {
                let _ = fs::write(&self.index_path, bytes);
            }
            None => {
                if self.index_path.exists() {
                    let _ = fs::remove_file(&self.index_path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_temp_repo() -> (tempfile::TempDir, CheckpointManager) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();

        let sig = repo.signature().unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            fs::write(dir.path().join("README.md"), "first\n").unwrap();
            index.add_path(Path::new("README.md")).unwrap();
            index.write().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
            .unwrap();

        let manager = CheckpointManager::open(dir.path()).unwrap();
        (dir, manager)
    }

    #[test]
    fn create_checkpoint_records_metadata() {
        let (_dir, mut manager) = make_temp_repo();
        let checkpoint = manager.create("session-1", "before tool").unwrap();
        assert_eq!(checkpoint.session_id, "session-1");
        assert_eq!(checkpoint.message, "before tool");
        assert!(checkpoint.stash_id.is_some());
        assert_eq!(manager.list("session-1").len(), 1);
    }

    #[test]
    fn list_filters_by_session() {
        let (_dir, mut manager) = make_temp_repo();
        manager.create("session-1", "first").unwrap();
        manager.create("session-2", "second").unwrap();
        assert_eq!(manager.list("session-1").len(), 1);
        assert_eq!(manager.list("session-2").len(), 1);
    }

    #[test]
    fn restore_checkpoint_reverts_workspace_and_untracked_files() {
        let (dir, mut manager) = make_temp_repo();
        let checkpoint = manager.create("session-1", "baseline").unwrap();
        fs::write(dir.path().join("README.md"), "second\n").unwrap();
        fs::write(dir.path().join("scratch.txt"), "temp\n").unwrap();

        manager.restore(&checkpoint.id).unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "first\n"
        );
        assert!(!dir.path().join("scratch.txt").exists());
    }

    #[test]
    fn diff_reports_changes_since_checkpoint() {
        let (dir, mut manager) = make_temp_repo();
        let checkpoint = manager.create("session-1", "baseline").unwrap();
        fs::write(dir.path().join("README.md"), "changed\n").unwrap();
        let diff = manager.diff(&checkpoint.id).unwrap();
        assert!(diff.contains("changed"));
    }

    #[test]
    fn prune_keeps_latest_checkpoints() {
        let (_dir, mut manager) = make_temp_repo();
        let first = manager.create("session-1", "first").unwrap();
        let _second = manager.create("session-1", "second").unwrap();
        let _third = manager.create("session-1", "third").unwrap();

        let removed = manager.prune(1).unwrap();

        assert_eq!(removed, 2);
        assert_eq!(manager.list("session-1").len(), 1);
        assert!(manager
            .repo
            .find_reference(&CheckpointManager::ref_name(&first.id))
            .is_err());
    }
}
