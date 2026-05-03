//! Snapshot data shapes — projection, RunRow, RunDetail, token_aggregate
//! (sn01 + sn02 + sn03 + sn04 + sn14).
//!
//! Per the implementation DAG, this module ships the wire shapes the
//! snapshot RPC (sn06) and subscribe RPC (sn07) serve.  The shapes are
//! v1 — non-local transport mutation is FORBIDDEN (iter-28 #4-1, see
//! sn13).
//!
//! Spec cross-references:
//!
//! - **`spec-orchestrator-status-snapshot.md` §4.1** — `RunRow`,
//!   `RunStatus`.  Iter-28 #4-2: `RunStatus::Finished` is required for
//!   `recent_history_ring` rows; `RunDetail.exit_reason` is `Some`
//!   iff `Finished`.
//! - **`spec-orchestrator-status-snapshot.md` §4.5** — `RunDetail`
//!   shape with event_log_tail, token_history, prompt_hash_trail,
//!   hook_log, workspace.  Iter-28 #4-2 absorbed.
//! - **`spec-orchestrator-status-snapshot.md` §4.7** — `TokensAggregate`.
//!   Iter-28 #4-6: T-3 asserts on mandatory `input_tokens`/`output_tokens`,
//!   NOT on the MAY-only `absolute_total` field.

use crate::mailbox::{RunId, SessionId};
use crate::registry::RepoCoordinate;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// Top-level snapshot projection.  Spec #4 §4 — single immutable
/// snapshot served by the snapshot RPC.
///
/// Iter-28 #4-1 absorbed: this struct is **v1 local-only**.  Any
/// transport that cannot establish local peer identity MUST reject
/// before serialization rather than mutate field presence/type/meaning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotStruct {
    /// Runs currently producing turns.
    pub running: Vec<RunRow>,
    /// Runs awaiting retry (in `state.retry_attempts`).
    pub retrying: Vec<RunRow>,
    /// Runs marked disconnected; engine has dropped the connection
    /// but the runner is still alive.
    pub disconnected: Vec<RunRow>,
    /// Recently completed runs (bounded by `recent_history_ring_size`).
    /// Status here is always `Finished` (iter-28 #4-2).
    pub recent_history: Vec<RunRow>,
    /// Per-snapshot token aggregate (sn14).
    pub tokens_aggregate: TokensAggregate,
    /// Spec #4 I-7: derived from canonical projection.
    pub fingerprint: String,
    /// Monotonically increasing per-snapshot sequence number; consumed
    /// by the subscribe outcome algorithm (sn08).
    pub stream_seq: u64,
    /// Boot id; reset on daemon restart.  Used for clause-(c) routing
    /// in the subscribe outcome algorithm (iter-28 #4-3 + #4-5).
    pub boot_id: String,
    /// Snapshot generation time.
    pub generated_at: SystemTime,
    /// Server version string for compatibility checks.
    pub server_version: String,
}

/// A row in the snapshot for a single Run.  Spec #4 §4.1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRow {
    pub run_id: RunId,
    pub status: RunStatus,
    /// `repo_coordinate` is inline on every row (caduceus-new I-5;
    /// always present so subscribers don't need a side-table lookup).
    pub repo_coordinate: RepoCoordinate,
    pub attempt: u32,
    pub session_id: Option<SessionId>,
    pub tokens: RunRowTokens,
    pub last_event: Option<LastEvent>,
}

/// Per-row token totals.  Spec #4 §4.1 + iter-28 #4-6.  Mandatory fields
/// (`input_tokens`, `output_tokens`) MUST be present; MAY-only fields
/// (`absolute_total`) are intentionally omitted from this v1 shape.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunRowTokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
}

/// A summary of the most recent event observed for a Run.  Diagnostic;
/// the full log_tail is in `RunDetail`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastEvent {
    pub kind: String,
    pub at: SystemTime,
}

/// Run lifecycle status as visible in the snapshot.  Spec #4 §4.1 +
/// iter-28 #4-2: `Finished` is required so terminal `RunDetail` rows
/// can be served from `recent_history_ring`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Retrying,
    Disconnected,
    /// Terminal.  Served only via `RunDetail` (sn04) or
    /// `recent_history_ring` (top-level snapshot field).
    Finished,
}

/// Reason a Run reached `Finished` status.  Spec #4 §4.5 +
/// iter-28 #4-2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    Completed,
    Aborted,
    Error(String),
    /// stop_cascade reaped the runner (iter-28 #2-2 honest outcome).
    Reaped {
        stage: String,
        exit_code: Option<i32>,
    },
}

/// Detail view of a single Run.  Returned by the snapshot RPC's
/// `RunDetail` endpoint (planned for sn06; v1 shape + invariants).
///
/// Iter-28 #4-2 absorbed: `exit_reason` is `Some` iff
/// `row.status == Finished`; this is documented as a runtime invariant
/// + verified by sn15 acceptance tests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunDetail {
    pub row: RunRow,
    /// `Some` iff `row.status == Finished` (iter-28 #4-2).
    pub exit_reason: Option<ExitReason>,
    /// Last N events; default 100.  Schema owned by spec #5.
    pub event_log_tail: Vec<EventRecord>,
    /// (turn, totals).  Per-turn token history.
    pub token_history: Vec<(u32, TokensAggregate)>,
    /// (turn, hash).  256-bit BLAKE3.  Spec #2 §4 prompt_hash invariant.
    pub prompt_hash_trail: Vec<(u32, [u8; 32])>,
    /// Hook execution log; schema owned by spec #3 §3.5.
    pub hook_log: Vec<HookExecutionRecord>,
    /// Workspace metadata.
    pub workspace: WorkspaceMeta,
}

/// Per-event record in the event_log_tail.  V1 minimal; spec #5 owns
/// the rich payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventRecord {
    pub seq: u64,
    pub kind: String,
    pub at: SystemTime,
}

/// Hook execution record.  Spec #3 §3.5.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookExecutionRecord {
    pub phase: String,
    pub exit_code: Option<i32>,
    pub at: SystemTime,
}

/// Workspace metadata embedded in `RunDetail`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceMeta {
    pub workspace_id: String,
    pub path: String,
    pub repo_coordinate: RepoCoordinate,
}

/// Snapshot-level token aggregate.  Spec #4 §4.7 + iter-28 #4-6.
/// Sums + per-Run breakdown.  Acceptance test T-3 asserts agreement
/// with per-Run totals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TokensAggregate {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl TokensAggregate {
    /// Aggregate token totals across a slice of `RunRow`s.  Used by
    /// sn01 projection and sn14 acceptance.
    pub fn from_rows(rows: &[RunRow]) -> Self {
        let mut agg = TokensAggregate::default();
        for row in rows {
            agg.input_tokens = agg.input_tokens.saturating_add(row.tokens.input_tokens);
            agg.output_tokens = agg.output_tokens.saturating_add(row.tokens.output_tokens);
            if let Some(c) = row.tokens.cache_read_tokens {
                agg.cache_read_tokens = agg.cache_read_tokens.saturating_add(c);
            }
            if let Some(c) = row.tokens.cache_write_tokens {
                agg.cache_write_tokens = agg.cache_write_tokens.saturating_add(c);
            }
        }
        agg
    }
}

/// Invariant check (runtime debug-asserted by `RunDetail::new`):
/// `exit_reason.is_some() == (row.status == Finished)`.  Iter-28 #4-2.
impl RunDetail {
    pub fn new(row: RunRow, exit_reason: Option<ExitReason>) -> Self {
        debug_assert_eq!(
            exit_reason.is_some(),
            row.status == RunStatus::Finished,
            "iter-28 #4-2: exit_reason is Some iff status == Finished"
        );
        Self {
            row,
            exit_reason,
            event_log_tail: Vec::new(),
            token_history: Vec::new(),
            prompt_hash_trail: Vec::new(),
            hook_log: Vec::new(),
            workspace: WorkspaceMeta {
                workspace_id: String::new(),
                path: String::new(),
                repo_coordinate: RepoCoordinate {
                    slug: String::new(),
                    remote_url: None,
                    default_branch: None,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::sanitize_repo_slug;

    fn coord() -> RepoCoordinate {
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        RepoCoordinate::new(
            slug,
            Some("https://github.com/o/r".into()),
            Some("main".into()),
        )
    }

    fn row(id: &str, status: RunStatus, input: u64, output: u64) -> RunRow {
        RunRow {
            run_id: RunId(id.into()),
            status,
            repo_coordinate: coord(),
            attempt: 1,
            session_id: None,
            tokens: RunRowTokens {
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            last_event: None,
        }
    }

    #[test]
    fn run_row_serialize_round_trip() {
        let r = row("r1", RunStatus::Running, 100, 50);
        let s = serde_json::to_string(&r).unwrap();
        let back: RunRow = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn run_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&RunStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&RunStatus::Disconnected).unwrap(),
            "\"disconnected\""
        );
        assert_eq!(
            serde_json::to_string(&RunStatus::Finished).unwrap(),
            "\"finished\""
        );
    }

    #[test]
    fn iter28_4_2_finished_is_required_variant() {
        // Compile-time check: RunStatus::Finished MUST exist.
        let r = row("r1", RunStatus::Finished, 0, 0);
        assert_eq!(r.status, RunStatus::Finished);
    }

    #[test]
    fn iter28_4_2_run_detail_invariant_holds_for_finished() {
        let r = row("r1", RunStatus::Finished, 0, 0);
        // exit_reason MUST be Some when Finished.
        let _detail = RunDetail::new(r, Some(ExitReason::Completed));
    }

    #[test]
    fn iter28_4_2_run_detail_invariant_holds_for_non_finished() {
        let r = row("r1", RunStatus::Running, 0, 0);
        // exit_reason MUST be None when not Finished.
        let _detail = RunDetail::new(r, None);
    }

    #[test]
    #[should_panic(expected = "iter-28 #4-2")]
    fn iter28_4_2_panics_when_finished_without_exit_reason() {
        let r = row("r1", RunStatus::Finished, 0, 0);
        let _detail = RunDetail::new(r, None);
    }

    #[test]
    #[should_panic(expected = "iter-28 #4-2")]
    fn iter28_4_2_panics_when_non_finished_with_exit_reason() {
        let r = row("r1", RunStatus::Running, 0, 0);
        let _detail = RunDetail::new(r, Some(ExitReason::Completed));
    }

    #[test]
    fn tokens_aggregate_sums_across_rows() {
        let rows = vec![
            row("r1", RunStatus::Running, 100, 50),
            row("r2", RunStatus::Running, 200, 100),
            row("r3", RunStatus::Finished, 50, 25),
        ];
        let agg = TokensAggregate::from_rows(&rows);
        assert_eq!(agg.input_tokens, 350);
        assert_eq!(agg.output_tokens, 175);
    }

    #[test]
    fn snapshot_struct_serialize_round_trip() {
        let snap = SnapshotStruct {
            running: vec![row("r1", RunStatus::Running, 1, 1)],
            retrying: vec![],
            disconnected: vec![],
            recent_history: vec![row("r0", RunStatus::Finished, 5, 3)],
            tokens_aggregate: TokensAggregate {
                input_tokens: 6,
                output_tokens: 4,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            fingerprint: "fp_abc".into(),
            stream_seq: 42,
            boot_id: "boot_xyz".into(),
            generated_at: SystemTime::UNIX_EPOCH,
            server_version: "0.1.0".into(),
        };
        let s = serde_json::to_string(&snap).unwrap();
        let back: SnapshotStruct = serde_json::from_str(&s).unwrap();
        assert_eq!(snap, back);
    }

    #[test]
    fn run_row_repo_coordinate_inline_present() {
        // I-5 caduceus-new: repo_coordinate inline on every row.
        let r = row("r1", RunStatus::Running, 0, 0);
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("repo_coordinate"));
        assert!(s.contains("github_com_o_r"));
    }

    #[test]
    fn run_row_tokens_omits_cache_when_none() {
        let r = row("r1", RunStatus::Running, 10, 5);
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("cache_read_tokens"));
        assert!(!s.contains("cache_write_tokens"));
    }
}
