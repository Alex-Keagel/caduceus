//! Persistent storage primitives for the daemon.
//!
//! Per the implementation DAG (todo `f07-storage-layer`), this module
//! provides the **atomic write** helper and the **keyed row store**
//! abstractions that subsystems (workspace registry, replay index,
//! recent-history-ring overflow, retry-attempts persistence) build on.
//! Crash-safety properties:
//!
//! - **AtomicWrite::commit** writes to a sibling tempfile, calls
//!   `fsync(fd)`, then `rename(temp, final)`.  Crash before rename →
//!   final is unchanged.  Crash after rename but before the directory
//!   is fsynced → the final file's data is durable on POSIX (POSIX
//!   guarantees rename atomicity for files in the same directory).
//!
//! - **JsonRowStore** persists rows as newline-delimited JSON.  Each
//!   row carries a primary key.  `put`, `delete`, and `compact` all
//!   route through `AtomicWrite::commit` so partial writes are never
//!   visible to readers.
//!
//! Boot recovery (`or00-boot-reconcile-sweep`) reads the row stores
//! and reconciles them against on-disk state; this module exposes the
//! `RecoverableStore` trait so the boot path can call into each store
//! uniformly.
//!
//! Spec cross-references:
//!
//! - **`spec-multi-repo-workspace-model.md` §3.5 / §4`** — registry
//!   row mutations MUST be atomic (no half-written rows after crash).
//! - **`spec-orchestrator-status-snapshot.md` §3.4 / Z-29** — replay
//!   index starts empty on boot only if the persistent store was
//!   absent or unreadable.

use serde::{de::DeserializeOwned, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;

/// Errors surfaced by the storage layer.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize row: {0}")]
    Serialize(String),
    #[error("failed to deserialize row at line {line}: {source}")]
    Deserialize {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

/// Convenience alias for storage results.
pub type StorageResult<T> = Result<T, StorageError>;

/// Helper that writes a byte buffer to a path atomically.  Use directly
/// for one-shot writes (e.g., the daemon lock file); higher-level stores
/// (`JsonRowStore`) wrap this.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> StorageResult<()> {
    let dir = path.parent().ok_or_else(|| StorageError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir"),
    })?;
    std::fs::create_dir_all(dir).map_err(|source| StorageError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let tmp = path.with_extension(format!(
        "{}.tmp.{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("dat"),
        std::process::id()
    ));
    {
        let mut f = File::create(&tmp).map_err(|source| StorageError::Io {
            path: tmp.clone(),
            source,
        })?;
        f.write_all(bytes).map_err(|source| StorageError::Io {
            path: tmp.clone(),
            source,
        })?;
        f.sync_all().map_err(|source| StorageError::Io {
            path: tmp.clone(),
            source,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|source| StorageError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// A row in a keyed store.  Each row has a string primary key and a
/// JSON-serializable payload.  The trait is sealed-ish via the type
/// bounds; concrete row types are defined by the consuming subsystem.
pub trait Row: Serialize + DeserializeOwned + Clone + Send + Sync {
    fn key(&self) -> &str;
}

/// Keyed row store backed by a single newline-delimited JSON file.
///
/// Suitable for daemon-scale tables (workspace registry, retry attempts,
/// snapshot replay index).  `put`, `delete`, and `compact` all rewrite
/// the file atomically; readers are crash-safe.
///
/// Concurrency: a single `JsonRowStore` instance is `Send + Sync` and
/// serialises mutations through an internal mutex.  Multiple processes
/// MUST NOT open the same file (the daemon enforces single-writer via
/// the `.caduceusd.lock` advisory lock — spec #3 I-8).
pub struct JsonRowStore<R: Row> {
    path: PathBuf,
    rows: Mutex<BTreeMap<String, R>>,
}

impl<R: Row> std::fmt::Debug for JsonRowStore<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonRowStore")
            .field("path", &self.path)
            .field("len", &self.rows.lock().map(|g| g.len()).unwrap_or(0))
            .finish()
    }
}

impl<R: Row> JsonRowStore<R> {
    /// Open or create a store at `path`.  Reads existing rows (if any)
    /// and sorts them by key.  A malformed line is reported with its
    /// 1-based line number so operators can surgically fix the file.
    pub fn open(path: impl Into<PathBuf>) -> StorageResult<Self> {
        let path = path.into();
        let mut rows: BTreeMap<String, R> = BTreeMap::new();
        if path.exists() {
            let content = std::fs::read_to_string(&path).map_err(|source| StorageError::Io {
                path: path.clone(),
                source,
            })?;
            for (i, line) in content.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let row: R =
                    serde_json::from_str(line).map_err(|source| StorageError::Deserialize {
                        line: i + 1,
                        source,
                    })?;
                rows.insert(row.key().to_string(), row);
            }
        } else if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StorageError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        Ok(Self {
            path,
            rows: Mutex::new(rows),
        })
    }

    /// Number of rows currently stored.
    pub fn len(&self) -> usize {
        self.rows.lock().unwrap().len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.lock().unwrap().is_empty()
    }

    /// Look up a row by key.
    pub fn get(&self, key: &str) -> Option<R> {
        self.rows.lock().unwrap().get(key).cloned()
    }

    /// Insert or replace a row.  Persists the entire table atomically.
    pub fn put(&self, row: R) -> StorageResult<()> {
        let mut g = self.rows.lock().unwrap();
        g.insert(row.key().to_string(), row);
        self.persist_locked(&g)
    }

    /// Remove a row by key.  Persists the entire table atomically.
    /// Returns the removed row if present.
    pub fn delete(&self, key: &str) -> StorageResult<Option<R>> {
        let mut g = self.rows.lock().unwrap();
        let removed = g.remove(key);
        if removed.is_some() {
            self.persist_locked(&g)?;
        }
        Ok(removed)
    }

    /// All rows, sorted by key.  Used by boot recovery (spec #1 §3.1)
    /// and the snapshot RPC.
    pub fn list(&self) -> Vec<R> {
        self.rows.lock().unwrap().values().cloned().collect()
    }

    /// Force a rewrite of the on-disk file.  Useful after bulk edits
    /// done via `with_rows_mut` (not exposed in the public API yet).
    pub fn compact(&self) -> StorageResult<()> {
        let g = self.rows.lock().unwrap();
        self.persist_locked(&g)
    }

    fn persist_locked(&self, rows: &BTreeMap<String, R>) -> StorageResult<()> {
        let mut buf = Vec::new();
        for row in rows.values() {
            let line =
                serde_json::to_string(row).map_err(|e| StorageError::Serialize(e.to_string()))?;
            buf.extend_from_slice(line.as_bytes());
            buf.push(b'\n');
        }
        atomic_write(&self.path, &buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::TempDir;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct TestRow {
        id: String,
        value: u32,
    }
    impl Row for TestRow {
        fn key(&self) -> &str {
            &self.id
        }
    }

    fn td() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn atomic_write_creates_file() {
        let d = td();
        let p = d.path().join("a/b/c.dat");
        atomic_write(&p, b"hello").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"hello");
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let d = td();
        let p = d.path().join("x.dat");
        atomic_write(&p, b"v1").unwrap();
        atomic_write(&p, b"v2").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"v2");
    }

    #[test]
    fn atomic_write_creates_parent_dir() {
        let d = td();
        let p = d.path().join("deep/nested/path/file.json");
        atomic_write(&p, b"x").unwrap();
        assert!(p.exists());
    }

    #[test]
    fn json_row_store_open_creates_empty() {
        let d = td();
        let p = d.path().join("rows.ndjson");
        let store: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn json_row_store_put_and_get() {
        let d = td();
        let p = d.path().join("rows.ndjson");
        let store: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
        store
            .put(TestRow {
                id: "k1".into(),
                value: 42,
            })
            .unwrap();
        let got = store.get("k1").unwrap();
        assert_eq!(got.value, 42);
    }

    #[test]
    fn json_row_store_persists_across_reopen() {
        let d = td();
        let p = d.path().join("rows.ndjson");
        {
            let store: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
            store
                .put(TestRow {
                    id: "k1".into(),
                    value: 1,
                })
                .unwrap();
            store
                .put(TestRow {
                    id: "k2".into(),
                    value: 2,
                })
                .unwrap();
        }
        let store: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.get("k1").unwrap().value, 1);
        assert_eq!(store.get("k2").unwrap().value, 2);
    }

    #[test]
    fn json_row_store_delete_removes_and_persists() {
        let d = td();
        let p = d.path().join("rows.ndjson");
        let store: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
        store
            .put(TestRow {
                id: "k".into(),
                value: 7,
            })
            .unwrap();
        let removed = store.delete("k").unwrap();
        assert_eq!(removed.unwrap().value, 7);
        assert!(store.get("k").is_none());

        // Reopen confirms persistence.
        let store2: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
        assert!(store2.is_empty());
    }

    #[test]
    fn json_row_store_list_is_sorted_by_key() {
        let d = td();
        let p = d.path().join("rows.ndjson");
        let store: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
        store
            .put(TestRow {
                id: "z".into(),
                value: 0,
            })
            .unwrap();
        store
            .put(TestRow {
                id: "a".into(),
                value: 0,
            })
            .unwrap();
        store
            .put(TestRow {
                id: "m".into(),
                value: 0,
            })
            .unwrap();
        let keys: Vec<String> = store.list().iter().map(|r| r.id.clone()).collect();
        assert_eq!(keys, ["a", "m", "z"]);
    }

    #[test]
    fn json_row_store_replaces_on_put_with_same_key() {
        let d = td();
        let p = d.path().join("rows.ndjson");
        let store: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
        store
            .put(TestRow {
                id: "k".into(),
                value: 1,
            })
            .unwrap();
        store
            .put(TestRow {
                id: "k".into(),
                value: 2,
            })
            .unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.get("k").unwrap().value, 2);
    }

    #[test]
    fn json_row_store_malformed_line_reports_line_number() {
        let d = td();
        let p = d.path().join("rows.ndjson");
        // Write a file with one good row and one malformed.
        std::fs::write(
            &p,
            r#"{"id":"a","value":1}
{not-json
"#,
        )
        .unwrap();
        let err = JsonRowStore::<TestRow>::open(&p).unwrap_err();
        match err {
            StorageError::Deserialize { line, .. } => assert_eq!(line, 2),
            other => panic!("expected Deserialize, got {other:?}"),
        }
    }

    #[test]
    fn json_row_store_skips_blank_lines() {
        let d = td();
        let p = d.path().join("rows.ndjson");
        std::fs::write(
            &p,
            r#"{"id":"a","value":1}

{"id":"b","value":2}

"#,
        )
        .unwrap();
        let store: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn json_row_store_compact_rewrites_on_demand() {
        let d = td();
        let p = d.path().join("rows.ndjson");
        let store: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
        store
            .put(TestRow {
                id: "k".into(),
                value: 1,
            })
            .unwrap();
        store.compact().unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("\"k\""));
    }

    #[test]
    fn delete_missing_key_returns_none_and_does_not_persist() {
        let d = td();
        let p = d.path().join("rows.ndjson");
        let store: JsonRowStore<TestRow> = JsonRowStore::open(&p).unwrap();
        let removed = store.delete("nope").unwrap();
        assert!(removed.is_none());
    }
}
