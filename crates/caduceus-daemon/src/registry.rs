//! Workspace registry row types (ws05).
//!
//! Per the implementation DAG (todo `ws05-registry-row-types`), this
//! module defines the persistent row shape and `RepoCoordinate` /
//! `Status` types used by the workspace registry.  The actual store
//! (`ws06-registry-store`) consumes these types and persists them via
//! `JsonRowStore<WorkspaceRegistryRow>`.
//!
//! Spec cross-references:
//!
//! - **`spec-multi-repo-workspace-model.md` §4** — registry row shape +
//!   I-4 sticky slug + I-6 derivable workspace_id.
//! - **`spec-multi-repo-workspace-model.md` §3.5** — `Creating` status
//!   set on placeholder row (iter-28 #3-2 absorbed).
//! - **`spec-multi-repo-workspace-model.md` §3.6** — `CleaningUp` /
//!   `CleanupFailed` transitions on cleanup path.

use crate::storage::Row;
use crate::workspace::{RepoSlug, SafeRunId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

/// Stable identity for a logical repo across cosmetic remote-URL changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoCoordinate {
    /// Sticky slug (I-4).  Once recorded, never mutated.
    pub slug: String,
    /// Most recently observed canonical remote URL.  MAY be updated.
    pub remote_url: Option<String>,
    /// Advisory metadata; MAY be updated.
    pub default_branch: Option<String>,
}

impl RepoCoordinate {
    /// Construct a coordinate.  `slug` must come from `sanitize_repo_slug`.
    pub fn new(slug: RepoSlug, remote_url: Option<String>, default_branch: Option<String>) -> Self {
        Self {
            slug: slug.as_str().to_string(),
            remote_url,
            default_branch,
        }
    }
}

/// Workspace lifecycle status.  Spec #3 §3.5/§3.6 + iter-28 #3-2 (placeholder
/// row inserted with `Creating` BEFORE the leaf is created).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkspaceStatus {
    /// Placeholder row inserted at §3.5 step 1.4 before any filesystem
    /// state exists.  Reachable concurrent readers MUST treat this as
    /// "in-progress" and MUST NOT try to use the workspace path.
    Creating,
    /// `create_workspace` succeeded; leaf exists; runner is permitted to
    /// run inside.
    Ready,
    /// `cleanup_workspace` is in progress.  The leaf MAY be partially
    /// torn down; readers MUST NOT use it.
    CleaningUp,
    /// `cleanup_workspace` aborted before completion.  The row is
    /// retained for reconcile to retry (`OrphanReclaim`, §5B.2).
    CleanupFailed,
}

impl std::fmt::Display for WorkspaceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            WorkspaceStatus::Creating => "Creating",
            WorkspaceStatus::Ready => "Ready",
            WorkspaceStatus::CleaningUp => "CleaningUp",
            WorkspaceStatus::CleanupFailed => "CleanupFailed",
        };
        f.write_str(s)
    }
}

/// Persistent registry row.  Persisted via `JsonRowStore<WorkspaceRegistryRow>`.
///
/// Spec #3 §4.  `workspace_id` is derived (I-6); two daemons against
/// the same `workspace_root` and the same `(slug, safe_run_id)` produce
/// identical rows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceRegistryRow {
    /// Primary key.  Format `wsp_<32 hex chars>` per `workspace_id()`.
    pub workspace_id: String,
    /// Lifecycle status.
    pub status: WorkspaceStatus,
    /// Canonicalized path on disk.  Set after §3.5 step 3 succeeds; for
    /// `Creating` rows this is the **target** path (output of step 2),
    /// because canonicalization happens at step 3.
    pub path: PathBuf,
    /// The repo this workspace serves.
    pub repo_coordinate: RepoCoordinate,
    /// The (sanitized) run id.  Sibling key with workspace_id; both are
    /// needed for `OrphanReclaim` lookup.
    pub safe_run_id: String,
    /// Wall-clock timestamp at row insertion.  Diagnostic only; spec #3
    /// I-6 derivation is path-stable across restarts so creation time
    /// is not part of the identity.
    pub created_at: SystemTime,
}

impl WorkspaceRegistryRow {
    pub fn new(
        workspace_id: String,
        status: WorkspaceStatus,
        path: PathBuf,
        repo_coordinate: RepoCoordinate,
        safe_run_id: SafeRunId,
        created_at: SystemTime,
    ) -> Self {
        Self {
            workspace_id,
            status,
            path,
            repo_coordinate,
            safe_run_id: safe_run_id.as_str().to_string(),
            created_at,
        }
    }
}

impl Row for WorkspaceRegistryRow {
    fn key(&self) -> &str {
        &self.workspace_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::JsonRowStore;
    use crate::workspace::{sanitize_repo_slug, sanitize_run_id};

    fn fixture_row(workspace_id: &str) -> WorkspaceRegistryRow {
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        let rid = sanitize_run_id("01H8XYZ").unwrap();
        WorkspaceRegistryRow::new(
            workspace_id.to_string(),
            WorkspaceStatus::Creating,
            PathBuf::from("/var/lib/caduceus/github_com_o_r/01H8XYZ/"),
            RepoCoordinate::new(slug, Some("https://github.com/o/r".into()), None),
            rid,
            SystemTime::UNIX_EPOCH,
        )
    }

    #[test]
    fn row_serializes_round_trip() {
        let row = fixture_row("wsp_deadbeef");
        let s = serde_json::to_string(&row).unwrap();
        let back: WorkspaceRegistryRow = serde_json::from_str(&s).unwrap();
        assert_eq!(row, back);
    }

    #[test]
    fn row_key_is_workspace_id() {
        let row = fixture_row("wsp_abc");
        assert_eq!(row.key(), "wsp_abc");
    }

    #[test]
    fn row_persists_and_reloads_via_json_row_store() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("registry.ndjson");
        {
            let store: JsonRowStore<WorkspaceRegistryRow> = JsonRowStore::open(&p).unwrap();
            store.put(fixture_row("wsp_aaa")).unwrap();
            store.put(fixture_row("wsp_bbb")).unwrap();
        }
        let store: JsonRowStore<WorkspaceRegistryRow> = JsonRowStore::open(&p).unwrap();
        let aaa = store.get("wsp_aaa").unwrap();
        let bbb = store.get("wsp_bbb").unwrap();
        assert_eq!(aaa.status, WorkspaceStatus::Creating);
        assert_eq!(bbb.workspace_id, "wsp_bbb");
    }

    #[test]
    fn workspace_status_display_strings_match_spec() {
        assert_eq!(WorkspaceStatus::Creating.to_string(), "Creating");
        assert_eq!(WorkspaceStatus::Ready.to_string(), "Ready");
        assert_eq!(WorkspaceStatus::CleaningUp.to_string(), "CleaningUp");
        assert_eq!(WorkspaceStatus::CleanupFailed.to_string(), "CleanupFailed");
    }

    #[test]
    fn repo_coordinate_construct_with_slug() {
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        let rc = RepoCoordinate::new(slug, None, None);
        assert_eq!(rc.slug, "github_com_o_r");
        assert_eq!(rc.remote_url, None);
        assert_eq!(rc.default_branch, None);
    }
}
