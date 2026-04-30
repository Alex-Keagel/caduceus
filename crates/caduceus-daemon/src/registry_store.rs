//! Workspace registry store (ws06).
//!
//! Per the implementation DAG, this module wraps `JsonRowStore<WorkspaceRegistryRow>`
//! with the daemon-level concerns that the bare row store does not handle:
//!
//! - **Single-writer-per-root advisory lock** (`.caduceusd.lock`) per
//!   spec #3 I-8.  Acquired on store open; released on drop.  A second
//!   daemon attempting to come up against the same `workspace_root`
//!   MUST refuse — we surface this via `RegistryError::AnotherDaemonHoldingLock`.
//!
//! - **Boot recovery hook** — `list_status_creating()` / `list_status_cleanup_failed()`
//!   so `or00-boot-reconcile-sweep` can reconcile partial states from
//!   prior daemon runs.
//!
//! - **Atomic state transitions** — `transition_to_ready()` /
//!   `transition_to_cleaning_up()` enforce the §3.5 / §3.6 state-machine
//!   pre-conditions (only `Creating → Ready`, only `Ready → CleaningUp`,
//!   etc.) and persist atomically via the underlying JsonRowStore.
//!
//! `repo_bindings` (spec #3 §3.1A) is a sibling table whose schema is
//! out-of-scope for this todo; it lands with `xs03-spec-7-author` /
//! orchestrator-level work.

use crate::error::WorkspaceError;
use crate::registry::{WorkspaceRegistryRow, WorkspaceStatus};
use crate::storage::{JsonRowStore, StorageError};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors specific to the registry store wrapper (above and beyond the
/// generic `StorageError` which is wrapped via `From`).
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),

    /// A second `caduceusd` is already running against this `workspace_root`.
    /// Spec #3 I-8 — single-writer-per-root.
    #[error(
        "another caduceusd is already running against {workspace_root}; refusing to start (spec #3 I-8)"
    )]
    AnotherDaemonHoldingLock { workspace_root: PathBuf },

    #[error("invalid state transition for workspace {workspace_id}: {from} -> {to}")]
    InvalidTransition {
        workspace_id: String,
        from: WorkspaceStatus,
        to: WorkspaceStatus,
    },

    #[error("workspace not found: {0}")]
    NotFound(String),

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// The registry store.  Holds the file lock for the daemon's lifetime;
/// `Drop` releases the lock and removes the lock file.
pub struct RegistryStore {
    workspace_root: PathBuf,
    rows: JsonRowStore<WorkspaceRegistryRow>,
    /// File handle holding the `.caduceusd.lock` advisory lock.  Kept
    /// alive for the lifetime of the store.
    #[cfg(unix)]
    _lock: std::fs::File,
}

impl std::fmt::Debug for RegistryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryStore")
            .field("workspace_root", &self.workspace_root)
            .field("rows_len", &self.rows.len())
            .finish()
    }
}

impl RegistryStore {
    /// Open or create the registry store rooted at `workspace_root`.
    /// Acquires the advisory `.caduceusd.lock` file lock; fails with
    /// `AnotherDaemonHoldingLock` if a sibling daemon already holds it.
    pub fn open(workspace_root: impl Into<PathBuf>) -> Result<Self, RegistryError> {
        let workspace_root = workspace_root.into();
        std::fs::create_dir_all(&workspace_root).map_err(|source| RegistryError::Io {
            path: workspace_root.clone(),
            source,
        })?;

        #[cfg(unix)]
        let lock = acquire_advisory_lock(&workspace_root)?;

        let registry_path = workspace_root.join(".caduceusd.registry.ndjson");
        let rows = JsonRowStore::<WorkspaceRegistryRow>::open(&registry_path)?;

        Ok(Self {
            workspace_root,
            rows,
            #[cfg(unix)]
            _lock: lock,
        })
    }

    /// Insert a placeholder row at §3.5 step 1.4.  The row MUST be in
    /// `Creating` status; any other status returns `InvalidTransition`.
    /// Iter-28 #3-2: caller MUST have pre-computed `safe_run_id`,
    /// `target`, and `workspace_id` (per I-6) before this call.
    pub fn insert_placeholder(&self, row: WorkspaceRegistryRow) -> Result<(), RegistryError> {
        if row.status != WorkspaceStatus::Creating {
            return Err(RegistryError::InvalidTransition {
                workspace_id: row.workspace_id,
                from: WorkspaceStatus::Creating,
                to: row.status,
            });
        }
        Ok(self.rows.put(row)?)
    }

    /// Look up a row by workspace_id.
    pub fn get(&self, workspace_id: &str) -> Option<WorkspaceRegistryRow> {
        self.rows.get(workspace_id)
    }

    /// Transition `Creating → Ready`.  Returns `InvalidTransition` if
    /// the current status is anything else.
    pub fn transition_to_ready(&self, workspace_id: &str) -> Result<(), RegistryError> {
        self.transition(
            workspace_id,
            WorkspaceStatus::Creating,
            WorkspaceStatus::Ready,
        )
    }

    /// Transition `Ready → CleaningUp`.
    pub fn transition_to_cleaning_up(&self, workspace_id: &str) -> Result<(), RegistryError> {
        self.transition(
            workspace_id,
            WorkspaceStatus::Ready,
            WorkspaceStatus::CleaningUp,
        )
    }

    /// Transition `CleaningUp → CleanupFailed` (rollback abort point).
    pub fn transition_to_cleanup_failed(&self, workspace_id: &str) -> Result<(), RegistryError> {
        self.transition(
            workspace_id,
            WorkspaceStatus::CleaningUp,
            WorkspaceStatus::CleanupFailed,
        )
    }

    fn transition(
        &self,
        workspace_id: &str,
        expected: WorkspaceStatus,
        target: WorkspaceStatus,
    ) -> Result<(), RegistryError> {
        let mut row = self
            .rows
            .get(workspace_id)
            .ok_or_else(|| RegistryError::NotFound(workspace_id.to_string()))?;
        if row.status != expected {
            return Err(RegistryError::InvalidTransition {
                workspace_id: workspace_id.to_string(),
                from: row.status,
                to: target,
            });
        }
        row.status = target;
        Ok(self.rows.put(row)?)
    }

    /// Remove a row from the registry.  Used by §3.6 step 9 after
    /// successful cleanup or by `OrphanReclaim` at terminal state.
    /// Returns the removed row (or `None` if absent — caller treats
    /// missing rows as no-op per §3.6 short-circuit semantics).
    pub fn delete(
        &self,
        workspace_id: &str,
    ) -> Result<Option<WorkspaceRegistryRow>, RegistryError> {
        Ok(self.rows.delete(workspace_id)?)
    }

    /// All rows.  Useful for the snapshot RPC and boot reconcile.
    pub fn list(&self) -> Vec<WorkspaceRegistryRow> {
        self.rows.list()
    }

    /// Boot-recovery helper: rows in `Creating` status (potentially
    /// orphaned by a crashed prior daemon).
    pub fn list_status_creating(&self) -> Vec<WorkspaceRegistryRow> {
        self.rows
            .list()
            .into_iter()
            .filter(|r| r.status == WorkspaceStatus::Creating)
            .collect()
    }

    /// Boot-recovery helper: rows in `CleanupFailed` for `OrphanReclaim`
    /// to retry.
    pub fn list_status_cleanup_failed(&self) -> Vec<WorkspaceRegistryRow> {
        self.rows
            .list()
            .into_iter()
            .filter(|r| r.status == WorkspaceStatus::CleanupFailed)
            .collect()
    }

    /// Workspace root the store is rooted at.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

#[cfg(unix)]
fn acquire_advisory_lock(workspace_root: &Path) -> Result<std::fs::File, RegistryError> {
    let lock_path = workspace_root.join(".caduceusd.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(&lock_path)
        .map_err(|source| RegistryError::Io {
            path: lock_path.clone(),
            source,
        })?;
    let fd = f.as_raw_fd();
    // LOCK_EX | LOCK_NB — exclusive, non-blocking.  Returns -1 / EWOULDBLOCK
    // if another daemon already holds it.
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        // EWOULDBLOCK / EAGAIN means a sibling daemon holds the lock.
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) || err.raw_os_error() == Some(libc::EAGAIN)
        {
            return Err(RegistryError::AnotherDaemonHoldingLock {
                workspace_root: workspace_root.to_path_buf(),
            });
        }
        return Err(RegistryError::Io {
            path: lock_path,
            source: err,
        });
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::RepoCoordinate;
    use crate::workspace::{sanitize_repo_slug, sanitize_run_id, workspace_id, WorkspaceIdKey};
    use std::time::SystemTime;

    fn fixture_row(root: &Path, run_id: &str) -> WorkspaceRegistryRow {
        let key = WorkspaceIdKey::derive(root);
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        let rid = sanitize_run_id(run_id).unwrap();
        let wid = workspace_id(&key, &slug, &rid);
        WorkspaceRegistryRow::new(
            wid,
            WorkspaceStatus::Creating,
            root.join("github_com_o_r").join(rid.as_str()),
            RepoCoordinate::new(slug, Some("https://github.com/o/r".into()), None),
            rid,
            SystemTime::UNIX_EPOCH,
        )
    }

    #[test]
    fn open_creates_root_and_lock_file() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().join("ws-root");
        let _store = RegistryStore::open(&root).unwrap();
        assert!(root.exists());
        assert!(root.join(".caduceusd.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn second_open_against_same_root_fails_with_advisory_lock_error() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let _first = RegistryStore::open(&root).unwrap();
        let second = RegistryStore::open(&root);
        match second {
            Err(RegistryError::AnotherDaemonHoldingLock { .. }) => {}
            other => panic!("expected AnotherDaemonHoldingLock, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn lock_released_on_drop_allows_subsequent_open() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        {
            let _first = RegistryStore::open(&root).unwrap();
            // _first dropped here, lock released.
        }
        let _second = RegistryStore::open(&root).unwrap();
    }

    #[test]
    fn insert_placeholder_rejects_non_creating_status() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let store = RegistryStore::open(&root).unwrap();
        let mut row = fixture_row(&root, "01H8XYZ");
        row.status = WorkspaceStatus::Ready;
        match store.insert_placeholder(row) {
            Err(RegistryError::InvalidTransition { .. }) => {}
            other => panic!("expected InvalidTransition, got {other:?}"),
        }
    }

    #[test]
    fn transition_creating_to_ready_only() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let store = RegistryStore::open(&root).unwrap();
        let row = fixture_row(&root, "01H8XYZ");
        let wid = row.workspace_id.clone();
        store.insert_placeholder(row).unwrap();
        // Creating -> Ready: ok.
        store.transition_to_ready(&wid).unwrap();
        // Ready -> Ready: rejected (not Creating).
        match store.transition_to_ready(&wid) {
            Err(RegistryError::InvalidTransition { .. }) => {}
            other => panic!("expected InvalidTransition, got {other:?}"),
        }
    }

    #[test]
    fn full_lifecycle_creating_ready_cleaning_up_delete() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let store = RegistryStore::open(&root).unwrap();
        let row = fixture_row(&root, "01H8XYZ");
        let wid = row.workspace_id.clone();
        store.insert_placeholder(row).unwrap();
        store.transition_to_ready(&wid).unwrap();
        store.transition_to_cleaning_up(&wid).unwrap();
        let removed = store.delete(&wid).unwrap();
        assert!(removed.is_some());
        assert!(store.get(&wid).is_none());
    }

    #[test]
    fn transition_to_cleanup_failed_from_cleaning_up() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let store = RegistryStore::open(&root).unwrap();
        let row = fixture_row(&root, "01H8XYZ");
        let wid = row.workspace_id.clone();
        store.insert_placeholder(row).unwrap();
        store.transition_to_ready(&wid).unwrap();
        store.transition_to_cleaning_up(&wid).unwrap();
        store.transition_to_cleanup_failed(&wid).unwrap();
        let row = store.get(&wid).unwrap();
        assert_eq!(row.status, WorkspaceStatus::CleanupFailed);
    }

    #[test]
    fn boot_recovery_helpers_partition_by_status() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let store = RegistryStore::open(&root).unwrap();
        let r1 = fixture_row(&root, "01H8XYZ");
        let r2 = fixture_row(&root, "02ABCDE");
        let wid1 = r1.workspace_id.clone();
        let wid2 = r2.workspace_id.clone();
        store.insert_placeholder(r1).unwrap();
        store.insert_placeholder(r2).unwrap();
        store.transition_to_ready(&wid2).unwrap();
        store.transition_to_cleaning_up(&wid2).unwrap();
        store.transition_to_cleanup_failed(&wid2).unwrap();

        let creating = store.list_status_creating();
        let failed = store.list_status_cleanup_failed();
        assert_eq!(creating.len(), 1);
        assert_eq!(creating[0].workspace_id, wid1);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].workspace_id, wid2);
    }

    #[test]
    fn delete_missing_workspace_returns_none() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let store = RegistryStore::open(&root).unwrap();
        let removed = store.delete("wsp_missing").unwrap();
        assert!(removed.is_none());
    }

    #[test]
    fn rows_persist_across_reopen() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let row = fixture_row(&root, "01H8XYZ");
        let wid = row.workspace_id.clone();
        {
            let store = RegistryStore::open(&root).unwrap();
            store.insert_placeholder(row).unwrap();
        }
        let store2 = RegistryStore::open(&root).unwrap();
        let r = store2.get(&wid).unwrap();
        assert_eq!(r.status, WorkspaceStatus::Creating);
    }
}
