//! Per-tool-batch checkpoint registry (gap G13 / P3.3).
//!
//! Records a snapshot of file contents BEFORE each agent tool batch
//! mutates them, so the user can review a tool batch's changes and
//! one-click revert. The store does NOT touch the filesystem itself —
//! it only records `(path, before_contents)` tuples; the caller (the
//! Zed-side bridge or CLI) is responsible for writing the snapshot
//! back when the user invokes revert. This keeps the store
//! filesystem-agnostic and trivially testable.
//!
//! Lifecycle:
//!   1. `begin_batch(turn, tool_summary)` → returns a `CheckpointId`
//!   2. `record_edit(id, path, before)` for each file the tool is about
//!      to modify (called by the tool wrapper just before the write)
//!   3. `commit(id)` once the tool batch finishes (success or failure;
//!      we still want the rollback option). Returns the committed
//!      checkpoint metadata.
//!   4. UI calls `list()` to render the timeline; `revert(id)` returns
//!      the snapshots so the host can apply them.
//!
//! Storage is a bounded ring (default cap 64) so a long agent session
//! doesn't grow without bound. Eviction is FIFO by checkpoint id.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use thiserror::Error;

/// Strongly-typed checkpoint id. Monotonically increasing; never reused
/// even after eviction so stale UI references fail closed via
/// `CheckpointError::Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CheckpointId(pub u64);

impl CheckpointId {
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// A single file's pre-edit content. `before = None` means the file
/// did not exist before the batch — reverting therefore means deleting
/// it (the caller decides how).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub before: Option<String>,
}

/// State of a single batch in the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchState {
    /// `begin_batch` has been called but `commit` has not yet — the
    /// batch is still recording edits.
    Open,
    /// Committed. The set of edits is frozen.
    Committed,
    /// User reverted this batch. Snapshots remain so a later "redo"
    /// could replay them, but the UI dims the entry.
    Reverted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBatchCheckpoint {
    pub id: CheckpointId,
    /// Which agent turn produced the batch (1-indexed). Lets the UI
    /// group multiple checkpoints under one assistant message.
    pub turn_index: u32,
    /// Human-readable summary of what tool ran (e.g. "edit_file +
    /// run_terminal"). Just for UI rendering — not parsed.
    pub tool_summary: String,
    /// Snapshots, in record-order. May be empty if a tool batch
    /// claimed it would edit but didn't actually touch any file (we
    /// still keep the empty checkpoint as an audit trail).
    pub edits: Vec<FileSnapshot>,
    pub state: BatchState,
    /// Wall-clock seconds since epoch for UI sorting / display. Stored
    /// as u64 so the type stays serde-friendly without chrono.
    pub created_at_secs: u64,
}

#[derive(Debug, Error, Clone, Serialize, Deserialize)]
pub enum CheckpointError {
    #[error("checkpoint {0:?} not found (evicted or never existed)")]
    Unknown(CheckpointId),
    #[error("checkpoint {0:?} is still open; call commit() first")]
    StillOpen(CheckpointId),
    #[error("checkpoint {0:?} is already committed; cannot record more edits")]
    AlreadyCommitted(CheckpointId),
    #[error("checkpoint {0:?} is already reverted")]
    AlreadyReverted(CheckpointId),
}

/// Bounded FIFO ring of tool-batch checkpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointStore {
    checkpoints: VecDeque<ToolBatchCheckpoint>,
    cap: usize,
    next_id: u64,
}

impl Default for CheckpointStore {
    fn default() -> Self {
        Self::with_capacity(64)
    }
}

impl CheckpointStore {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            checkpoints: VecDeque::with_capacity(cap.max(1)),
            cap: cap.max(1),
            next_id: 1,
        }
    }

    /// Capacity (max retained checkpoints, including reverted).
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Open a new batch. Returns the id the caller will use for
    /// `record_edit` / `commit` / `revert`.
    pub fn begin_batch(
        &mut self,
        turn_index: u32,
        tool_summary: impl Into<String>,
        now_secs: u64,
    ) -> CheckpointId {
        let id = CheckpointId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.checkpoints.push_back(ToolBatchCheckpoint {
            id,
            turn_index,
            tool_summary: tool_summary.into(),
            edits: Vec::new(),
            state: BatchState::Open,
            created_at_secs: now_secs,
        });
        // Enforce ring cap; oldest goes first regardless of state so a
        // hot loop of failed reverts can't pin the ring.
        while self.checkpoints.len() > self.cap {
            self.checkpoints.pop_front();
        }
        id
    }

    /// Append a file snapshot to an OPEN batch.
    pub fn record_edit(
        &mut self,
        id: CheckpointId,
        path: PathBuf,
        before: Option<String>,
    ) -> Result<(), CheckpointError> {
        let cp = self.find_mut(id)?;
        match cp.state {
            BatchState::Open => {
                cp.edits.push(FileSnapshot { path, before });
                Ok(())
            }
            BatchState::Committed => Err(CheckpointError::AlreadyCommitted(id)),
            BatchState::Reverted => Err(CheckpointError::AlreadyReverted(id)),
        }
    }

    /// Commit an open batch. After commit no further edits accepted.
    pub fn commit(&mut self, id: CheckpointId) -> Result<&ToolBatchCheckpoint, CheckpointError> {
        let cp = self.find_mut(id)?;
        match cp.state {
            BatchState::Open => {
                cp.state = BatchState::Committed;
                Ok(&*cp)
            }
            BatchState::Committed => Err(CheckpointError::AlreadyCommitted(id)),
            BatchState::Reverted => Err(CheckpointError::AlreadyReverted(id)),
        }
    }

    /// Mark a committed batch as reverted and return its snapshots so
    /// the host can write them back. Idempotent revert is REJECTED so
    /// double-clicking the UI button can't silently no-op.
    pub fn revert(&mut self, id: CheckpointId) -> Result<Vec<FileSnapshot>, CheckpointError> {
        let cp = self.find_mut(id)?;
        match cp.state {
            BatchState::Open => Err(CheckpointError::StillOpen(id)),
            BatchState::Reverted => Err(CheckpointError::AlreadyReverted(id)),
            BatchState::Committed => {
                cp.state = BatchState::Reverted;
                Ok(cp.edits.clone())
            }
        }
    }

    /// Newest-first listing for UI.
    pub fn list(&self) -> Vec<&ToolBatchCheckpoint> {
        self.checkpoints.iter().rev().collect()
    }

    pub fn get(&self, id: CheckpointId) -> Option<&ToolBatchCheckpoint> {
        self.checkpoints.iter().find(|c| c.id == id)
    }

    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    fn find_mut(&mut self, id: CheckpointId) -> Result<&mut ToolBatchCheckpoint, CheckpointError> {
        self.checkpoints
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or(CheckpointError::Unknown(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn full_lifecycle_records_and_reverts() {
        let mut store = CheckpointStore::default();
        let id = store.begin_batch(1, "edit_file", 1000);
        store
            .record_edit(id, p("/a.rs"), Some("before-a".into()))
            .unwrap();
        store.record_edit(id, p("/b.rs"), None).unwrap();
        let cp = store.commit(id).unwrap();
        assert_eq!(cp.state, BatchState::Committed);
        assert_eq!(cp.edits.len(), 2);

        let snaps = store.revert(id).unwrap();
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].path, p("/a.rs"));
        assert_eq!(snaps[0].before.as_deref(), Some("before-a"));
        assert_eq!(snaps[1].before, None);
        assert_eq!(store.get(id).unwrap().state, BatchState::Reverted);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let mut store = CheckpointStore::with_capacity(3);
        let ids: Vec<_> = (0..5)
            .map(|i| store.begin_batch(i, "tool", i as u64))
            .collect();
        assert_eq!(store.len(), 3);
        // First two should be evicted.
        assert!(store.get(ids[0]).is_none());
        assert!(store.get(ids[1]).is_none());
        assert!(store.get(ids[4]).is_some());
    }

    #[test]
    fn revert_unknown_fails_closed() {
        let mut store = CheckpointStore::default();
        let bogus = CheckpointId(9999);
        assert!(matches!(
            store.revert(bogus),
            Err(CheckpointError::Unknown(_))
        ));
    }

    #[test]
    fn revert_open_batch_rejected() {
        let mut store = CheckpointStore::default();
        let id = store.begin_batch(1, "t", 0);
        assert!(matches!(
            store.revert(id),
            Err(CheckpointError::StillOpen(_))
        ));
    }

    #[test]
    fn double_revert_rejected_not_silent() {
        let mut store = CheckpointStore::default();
        let id = store.begin_batch(1, "t", 0);
        store.commit(id).unwrap();
        store.revert(id).unwrap();
        assert!(matches!(
            store.revert(id),
            Err(CheckpointError::AlreadyReverted(_))
        ));
    }

    #[test]
    fn cannot_record_after_commit() {
        let mut store = CheckpointStore::default();
        let id = store.begin_batch(1, "t", 0);
        store.commit(id).unwrap();
        assert!(matches!(
            store.record_edit(id, p("/x"), None),
            Err(CheckpointError::AlreadyCommitted(_))
        ));
    }

    #[test]
    fn list_is_newest_first() {
        let mut store = CheckpointStore::default();
        let a = store.begin_batch(1, "a", 1);
        let b = store.begin_batch(2, "b", 2);
        let c = store.begin_batch(3, "c", 3);
        let listed: Vec<_> = store.list().iter().map(|cp| cp.id).collect();
        assert_eq!(listed, vec![c, b, a]);
    }

    #[test]
    fn empty_batch_is_still_audit_trail() {
        let mut store = CheckpointStore::default();
        let id = store.begin_batch(7, "noop_tool", 42);
        let cp = store.commit(id).unwrap();
        assert!(cp.edits.is_empty());
        assert_eq!(cp.tool_summary, "noop_tool");
        assert_eq!(cp.turn_index, 7);
    }

    #[test]
    fn ids_never_reused_even_after_eviction() {
        let mut store = CheckpointStore::with_capacity(2);
        let id1 = store.begin_batch(1, "t", 0);
        let _id2 = store.begin_batch(2, "t", 0);
        let _id3 = store.begin_batch(3, "t", 0); // evicts id1
        assert!(store.get(id1).is_none());
        let id4 = store.begin_batch(4, "t", 0);
        assert_ne!(id4, id1);
        assert!(id4.raw() > id1.raw());
    }

    #[test]
    fn serde_roundtrip_preserves_state() {
        let mut store = CheckpointStore::default();
        let id = store.begin_batch(1, "edit_file", 100);
        store
            .record_edit(id, p("/foo.rs"), Some("x".into()))
            .unwrap();
        store.commit(id).unwrap();
        let json = serde_json::to_string(&store).unwrap();
        let back: CheckpointStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 1);
        let cp = back.get(id).unwrap();
        assert_eq!(cp.state, BatchState::Committed);
        assert_eq!(cp.edits[0].before.as_deref(), Some("x"));
    }
}
