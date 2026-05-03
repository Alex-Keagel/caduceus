//! Snapshot fingerprint + replay index + pubsub broadcast +
//! subscribe outcome algorithm + clause-(d′) detection +
//! replay-cancelled recovery + local-only transport gate +
//! snapshot/subscribe RPC entrypoints
//! (sn05 + sn06 + sn07 + sn08 + sn09 + sn10 + sn11 + sn12 + sn13).
//!
//! This module bundles the snapshot-side state machine and IPC
//! surface.  Spec #4 §3 + §4.6 + iter-28 backlog items #4-1, #4-3,
//! #4-4, #4-5 are absorbed here.
//!
//! ## Subscribe outcome algorithm (iter-28 #4-4 single normative source)
//!
//! Evaluate `subscribe(...)` in this order:
//!
//! 1. Input validity: `since_fingerprint = Some(_)` with
//!    `since_stream_seq = None` routes to clause (c) recovery.
//! 2. Clause priority: `(a) > (P) > (b) > (c) > (d) > (d')`.
//! 3. The first matching clause determines the single `SubscribeAck`.
//! 4. If none match, emit **vacuous `UpToDate`**.
//! 5. Clause (d) is the only non-vacuous `UpToDate`.
//! 6. All other clause-driven outcomes emit `Resync`.
//!
//! ## Clause-(d′) detection (iter-28 #4-3 boot-edge fix)
//!
//! Define `fp_at(s_c)` as: if `s_c == current_stream_seq` then
//! `current_fingerprint`; else replay-index entry at `s_c`.  If
//! `s_c != current_stream_seq` and absent from replay index, clauses
//! (a) or (P) fire instead.  Clause (d′) fires exactly when
//! `fp_at(s_c) != last_known_fingerprint`.

use crate::error::{DaemonError, SnapshotUnavailableReason};
use crate::ipc::PeerCreds;
use crate::snapshot_shapes::SnapshotStruct;
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, RwLock};

// ─────────────── sn05 SnapshotFingerprint ──────────────────────

/// Spec #4 I-7: fingerprint derived from canonical projection.
/// Stable across processes for the same logical state.
pub fn fingerprint(snap: &SnapshotStruct) -> String {
    // Canonical encoding: serialize with sorted keys and stable order.
    // serde_json with default settings is field-order stable per derive.
    let canonical = serde_json::to_vec(snap).unwrap_or_default();
    let mut h = Hasher::new();
    h.update(b"caduceus.snapshot.fp.v1");
    h.update(b"\x1f");
    h.update(&canonical);
    let digest = h.finalize();
    let bytes = &digest.as_bytes()[..16];
    let mut s = String::with_capacity(3 + 32);
    s.push_str("fp_");
    for b in bytes {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").unwrap();
    }
    s
}

// ─────────────── sn10 ReplayIndex ──────────────────────────────

/// Bounded ring of (stream_seq, fingerprint) entries.  Spec #4 §3.4 +
/// iter-28 #4-3.  Enables clause-(d′) detection at non-current stream
/// positions.  Boot starts empty (Z-29).
#[derive(Debug)]
pub struct ReplayIndex {
    capacity: usize,
    inner: VecDeque<(u64, String)>,
}

impl ReplayIndex {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 1, "replay_index capacity MUST be >= 1");
        Self {
            capacity,
            inner: VecDeque::with_capacity(capacity),
        }
    }

    pub fn record(&mut self, stream_seq: u64, fp: String) {
        if self.inner.len() >= self.capacity {
            self.inner.pop_front();
        }
        self.inner.push_back((stream_seq, fp));
    }

    pub fn fp_at(&self, stream_seq: u64) -> Option<&str> {
        self.inner
            .iter()
            .find(|(s, _)| *s == stream_seq)
            .map(|(_, fp)| fp.as_str())
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

// ─────────────── sn08 + sn09 SubscribeOutcome ──────────────────

/// Subscribe request from a client.  Spec #4 §3.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeRequest {
    pub since_fingerprint: Option<String>,
    pub since_stream_seq: Option<u64>,
    pub since_boot_id: Option<String>,
}

/// Subscribe outcome.  Spec #4 §3.4 + iter-28 #4-4 single source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscribeAck {
    /// Client is up to date as of `stream_seq`.  Replay nothing; just
    /// stream new deltas as they arrive.  Vacuous variant: no replay,
    /// no resync.  Clause (d) only.
    UpToDate { stream_seq: u64 },
    /// Client must re-subscribe with a full snapshot.  All non-vacuous
    /// outcomes route here.
    Resync,
}

/// Spec #4 §3.4 — single normative subscribe outcome source.
/// Iter-28 #4-4 absorbed.
pub fn subscribe_outcome(
    req: &SubscribeRequest,
    current_stream_seq: u64,
    current_fingerprint: &str,
    current_boot_id: &str,
    replay_index: &ReplayIndex,
) -> SubscribeAck {
    // 1. Input validity: fingerprint without stream_seq routes to (c).
    if req.since_fingerprint.is_some() && req.since_stream_seq.is_none() {
        return SubscribeAck::Resync; // clause (c)
    }

    // Clause (a): boot mismatch.  Iter-28 #4-5: if since_boot_id is
    // present and differs from current_boot_id, route to (c) via the
    // returned Resync (subscriber re-subscribes with cleared
    // since_fingerprint + since_stream_seq).
    if let Some(since_boot) = &req.since_boot_id {
        if since_boot != current_boot_id {
            return SubscribeAck::Resync; // clause (a)
        }
    }

    // Clause (P): no since_* at all -> first subscribe -> Resync with
    // full snapshot.
    if req.since_fingerprint.is_none() && req.since_stream_seq.is_none() {
        return SubscribeAck::Resync; // clause (P)
    }

    // Clauses (b), (c), (d), (d').
    let s_c = match req.since_stream_seq {
        Some(s) => s,
        None => return SubscribeAck::Resync, // clause (c)
    };
    let last_known = req.since_fingerprint.as_deref();

    // Compute fp_at(s_c).
    let fp_at_sc = if s_c == current_stream_seq {
        Some(current_fingerprint)
    } else {
        replay_index.fp_at(s_c)
    };
    let fp_at_sc = match fp_at_sc {
        Some(fp) => fp,
        None => {
            // Iter-28 #4-3: if s_c != current and absent from index,
            // clause (a) / (P) fires (Resync).
            return SubscribeAck::Resync;
        }
    };

    // Clause (d) — vacuous UpToDate: at current_stream_seq AND fp matches.
    if s_c == current_stream_seq && Some(fp_at_sc) == last_known {
        return SubscribeAck::UpToDate { stream_seq: s_c };
    }

    // Clause (d') — fp_at(s_c) != last_known.  Iter-28 #4-3.
    if Some(fp_at_sc) != last_known {
        return SubscribeAck::Resync; // clause (d')
    }

    // Default: clause (b) replay window from s_c.  V1: just resync;
    // delta replay lands with sn12 PubSub broadcast wiring.
    SubscribeAck::Resync
}

// ─────────────── sn11 ReplayCancelled recovery ─────────────────

/// Subscriber-side helper: spec #4 + iter-28 #4-5.  When a
/// `ReplayCancelled` arrives, MUST re-subscribe with cleared
/// fingerprint + stream_seq + retain `since_boot_id` to route through
/// clause (c) deterministically.
pub fn build_replay_cancelled_resub(last_boot_id: Option<String>) -> SubscribeRequest {
    SubscribeRequest {
        since_fingerprint: None,
        since_stream_seq: None,
        since_boot_id: last_boot_id,
    }
}

// ─────────────── sn12 PubSub delta broadcast ───────────────────

/// Delta event broadcast over a tokio broadcast channel.  Spec #4 §3
/// observability_pubsub equivalent.  Subscribers consume via
/// `broadcast::Receiver`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SnapshotDelta {
    /// New snapshot generation; carries the new fingerprint and stream_seq.
    NewSnapshot {
        fingerprint: String,
        stream_seq: u64,
    },
    /// Run row token totals updated.
    TokensUpdated {
        run_id: crate::mailbox::RunId,
        new_input: u64,
        new_output: u64,
    },
    /// Run row status changed.
    StatusChanged {
        run_id: crate::mailbox::RunId,
        new_status: crate::snapshot_shapes::RunStatus,
    },
    /// Replay was cancelled (server-side deadline exhausted etc.).
    ReplayCancelled { stream_seq: u64 },
}

/// Snapshot pubsub channel.  Cheap to clone; subscribers via
/// `subscribe()`.
#[derive(Debug, Clone)]
pub struct SnapshotPubSub {
    sender: broadcast::Sender<SnapshotDelta>,
}

impl SnapshotPubSub {
    pub fn new(capacity: usize) -> Self {
        let (sender, _rx) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SnapshotDelta> {
        self.sender.subscribe()
    }

    /// Broadcast a delta.  Returns the number of receivers that received
    /// it (lagging receivers are dropped per broadcast semantics).
    pub fn broadcast(&self, delta: SnapshotDelta) -> usize {
        self.sender.send(delta).unwrap_or(0)
    }

    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

// ─────────────── sn01 SnapshotProjection (presenter) ───────────

/// Snapshot projection state.  The dispatch loop (or10) owns one
/// instance and updates it on relevant transitions; the snapshot RPC
/// reads from it under an RwLock.
#[derive(Debug)]
pub struct SnapshotProjection {
    /// The current canonical projection.
    pub current: RwLock<SnapshotStruct>,
    /// Bounded replay index for clause-(d')/('b') lookups.
    pub replay_index: RwLock<ReplayIndex>,
    pub stream_seq: AtomicU64,
    pub boot_id: String,
    pub pubsub: SnapshotPubSub,
}

impl SnapshotProjection {
    pub fn new(initial: SnapshotStruct, replay_capacity: usize, boot_id: String) -> Self {
        Self {
            current: RwLock::new(initial),
            replay_index: RwLock::new(ReplayIndex::new(replay_capacity)),
            stream_seq: AtomicU64::new(0),
            boot_id,
            pubsub: SnapshotPubSub::new(64),
        }
    }

    /// Replace the current snapshot, advance stream_seq, record the
    /// previous (fingerprint, seq) in the replay index, and broadcast
    /// a NewSnapshot delta.
    pub async fn publish(&self, mut next: SnapshotStruct) -> u64 {
        let new_seq = self.stream_seq.fetch_add(1, Ordering::AcqRel) + 1;
        next.stream_seq = new_seq;
        next.fingerprint = fingerprint(&next);
        next.boot_id = self.boot_id.clone();
        let fp = next.fingerprint.clone();
        {
            let mut idx = self.replay_index.write().await;
            idx.record(new_seq, fp.clone());
        }
        {
            let mut cur = self.current.write().await;
            *cur = next;
        }
        self.pubsub.broadcast(SnapshotDelta::NewSnapshot {
            fingerprint: fp,
            stream_seq: new_seq,
        });
        new_seq
    }
}

// ─────────────── sn13 Local-only transport gate ────────────────

/// Iter-28 #4-1: v1 surface is local-only.  Non-local transports MUST
/// reject before serialization (this function) rather than mutate
/// wire shape.  Returns Err if the transport is not locally trusted.
pub fn check_local_only_gate(creds: Option<&PeerCreds>) -> Result<(), DaemonError> {
    match creds {
        Some(_) => Ok(()),
        None => Err(DaemonError::SnapshotUnavailable(
            SnapshotUnavailableReason::TransportNotLocallyTrusted,
        )),
    }
}

// ─────────────── sn06 + sn07 RPC entrypoints ───────────────────

/// Snapshot RPC entrypoint.  Spec #4 §3.  Returns the current
/// snapshot or DaemonError::SnapshotUnavailable per sn13 gate / drain.
pub async fn snapshot_rpc(
    projection: &SnapshotProjection,
    creds: Option<&PeerCreds>,
    is_ready: bool,
) -> Result<SnapshotStruct, DaemonError> {
    check_local_only_gate(creds)?;
    if !is_ready {
        return Err(DaemonError::SnapshotUnavailable(
            SnapshotUnavailableReason::NotReady,
        ));
    }
    let g = projection.current.read().await;
    Ok(g.clone())
}

/// Subscribe RPC entrypoint.  Returns (initial_ack, delta_receiver).
/// Caller drives the receiver; on Lagged/Err, caller MUST re-subscribe.
pub async fn subscribe_rpc(
    projection: &SnapshotProjection,
    creds: Option<&PeerCreds>,
    is_ready: bool,
    req: SubscribeRequest,
) -> Result<(SubscribeAck, broadcast::Receiver<SnapshotDelta>), DaemonError> {
    check_local_only_gate(creds)?;
    if !is_ready {
        return Err(DaemonError::SnapshotUnavailable(
            SnapshotUnavailableReason::NotReady,
        ));
    }
    let cur = projection.current.read().await;
    let idx = projection.replay_index.read().await;
    let ack = subscribe_outcome(
        &req,
        cur.stream_seq,
        &cur.fingerprint,
        &projection.boot_id,
        &idx,
    );
    Ok((ack, projection.pubsub.subscribe()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot_shapes::{ExitReason, RunRow, RunRowTokens, RunStatus, TokensAggregate};

    fn empty_snap() -> SnapshotStruct {
        SnapshotStruct {
            running: vec![],
            retrying: vec![],
            disconnected: vec![],
            recent_history: vec![],
            tokens_aggregate: TokensAggregate::default(),
            fingerprint: String::new(),
            stream_seq: 0,
            boot_id: String::new(),
            generated_at: std::time::SystemTime::UNIX_EPOCH,
            server_version: "0.1.0".into(),
        }
    }

    // ─── sn05 fingerprint ────────────────────────────────────────────

    #[test]
    fn fingerprint_format_is_fp_plus_32_hex() {
        let fp = fingerprint(&empty_snap());
        assert!(fp.starts_with("fp_"));
        assert_eq!(fp.len(), 3 + 32);
        assert!(fp[3..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let s = empty_snap();
        assert_eq!(fingerprint(&s), fingerprint(&s));
    }

    #[test]
    fn fingerprint_differs_when_state_differs() {
        let s1 = empty_snap();
        let mut s2 = empty_snap();
        s2.stream_seq = 99;
        assert_ne!(fingerprint(&s1), fingerprint(&s2));
    }

    // ─── sn10 ReplayIndex ────────────────────────────────────────────

    #[test]
    fn replay_index_record_and_lookup() {
        let mut idx = ReplayIndex::new(8);
        idx.record(1, "fp_a".into());
        idx.record(2, "fp_b".into());
        assert_eq!(idx.fp_at(1), Some("fp_a"));
        assert_eq!(idx.fp_at(2), Some("fp_b"));
        assert_eq!(idx.fp_at(99), None);
    }

    #[test]
    fn replay_index_evicts_at_capacity() {
        let mut idx = ReplayIndex::new(2);
        idx.record(1, "fp_1".into());
        idx.record(2, "fp_2".into());
        idx.record(3, "fp_3".into());
        assert_eq!(idx.fp_at(1), None);
        assert_eq!(idx.fp_at(2), Some("fp_2"));
        assert_eq!(idx.fp_at(3), Some("fp_3"));
    }

    // ─── sn08 SubscribeOutcome (iter-28 #4-4) ────────────────────────

    #[test]
    fn subscribe_no_since_returns_resync_clause_p() {
        let req = SubscribeRequest {
            since_fingerprint: None,
            since_stream_seq: None,
            since_boot_id: None,
        };
        let idx = ReplayIndex::new(8);
        assert_eq!(
            subscribe_outcome(&req, 5, "fp_x", "boot_a", &idx),
            SubscribeAck::Resync
        );
    }

    #[test]
    fn subscribe_boot_mismatch_returns_resync_clause_a() {
        let req = SubscribeRequest {
            since_fingerprint: Some("fp_x".into()),
            since_stream_seq: Some(5),
            since_boot_id: Some("boot_old".into()),
        };
        let idx = ReplayIndex::new(8);
        assert_eq!(
            subscribe_outcome(&req, 5, "fp_x", "boot_new", &idx),
            SubscribeAck::Resync
        );
    }

    #[test]
    fn subscribe_at_current_with_matching_fp_returns_uptodate_clause_d() {
        let req = SubscribeRequest {
            since_fingerprint: Some("fp_match".into()),
            since_stream_seq: Some(5),
            since_boot_id: Some("boot_a".into()),
        };
        let idx = ReplayIndex::new(8);
        assert_eq!(
            subscribe_outcome(&req, 5, "fp_match", "boot_a", &idx),
            SubscribeAck::UpToDate { stream_seq: 5 }
        );
    }

    #[test]
    fn subscribe_at_current_with_mismatched_fp_returns_resync_clause_d_prime() {
        // Iter-28 #4-3: clause (d') fires when fp_at(s_c) != last_known.
        let req = SubscribeRequest {
            since_fingerprint: Some("fp_old".into()),
            since_stream_seq: Some(5),
            since_boot_id: Some("boot_a".into()),
        };
        let idx = ReplayIndex::new(8);
        assert_eq!(
            subscribe_outcome(&req, 5, "fp_new", "boot_a", &idx),
            SubscribeAck::Resync
        );
    }

    #[test]
    fn subscribe_fp_without_stream_seq_routes_clause_c() {
        let req = SubscribeRequest {
            since_fingerprint: Some("fp_x".into()),
            since_stream_seq: None,
            since_boot_id: Some("boot_a".into()),
        };
        let idx = ReplayIndex::new(8);
        assert_eq!(
            subscribe_outcome(&req, 5, "fp_x", "boot_a", &idx),
            SubscribeAck::Resync
        );
    }

    #[test]
    fn subscribe_seq_absent_from_replay_index_returns_resync() {
        // Iter-28 #4-3: s_c != current AND absent from index -> Resync.
        let req = SubscribeRequest {
            since_fingerprint: Some("fp_x".into()),
            since_stream_seq: Some(2), // not in index
            since_boot_id: Some("boot_a".into()),
        };
        let idx = ReplayIndex::new(8);
        assert_eq!(
            subscribe_outcome(&req, 5, "fp_curr", "boot_a", &idx),
            SubscribeAck::Resync
        );
    }

    // ─── sn11 ReplayCancelled recovery ───────────────────────────────

    #[test]
    fn replay_cancelled_resub_clears_fp_and_seq_keeps_boot() {
        let req = build_replay_cancelled_resub(Some("boot_a".into()));
        assert!(req.since_fingerprint.is_none());
        assert!(req.since_stream_seq.is_none());
        assert_eq!(req.since_boot_id, Some("boot_a".into()));
    }

    // ─── sn13 Local-only gate ────────────────────────────────────────

    #[test]
    fn local_only_gate_rejects_no_creds() {
        let r = check_local_only_gate(None);
        match r {
            Err(DaemonError::SnapshotUnavailable(
                SnapshotUnavailableReason::TransportNotLocallyTrusted,
            )) => {}
            other => panic!("expected TransportNotLocallyTrusted, got {other:?}"),
        }
    }

    #[test]
    fn local_only_gate_accepts_present_creds() {
        let creds = PeerCreds {
            pid: 42,
            uid: 1000,
            gid: 1000,
        };
        assert!(check_local_only_gate(Some(&creds)).is_ok());
    }

    // ─── sn12 PubSub broadcast ───────────────────────────────────────

    #[tokio::test]
    async fn pubsub_broadcasts_to_subscribers() {
        let pubsub = SnapshotPubSub::new(8);
        let mut rx = pubsub.subscribe();
        pubsub.broadcast(SnapshotDelta::NewSnapshot {
            fingerprint: "fp_x".into(),
            stream_seq: 5,
        });
        let received = rx.recv().await.unwrap();
        match received {
            SnapshotDelta::NewSnapshot {
                fingerprint,
                stream_seq,
            } => {
                assert_eq!(fingerprint, "fp_x");
                assert_eq!(stream_seq, 5);
            }
            other => panic!("expected NewSnapshot, got {other:?}"),
        }
    }

    // ─── sn01 SnapshotProjection.publish() integration ───────────────

    #[tokio::test]
    async fn projection_publish_advances_seq_and_records_index() {
        let proj = SnapshotProjection::new(empty_snap(), 16, "boot_test".into());
        let mut rx = proj.pubsub.subscribe();
        let new_seq = proj.publish(empty_snap()).await;
        assert_eq!(new_seq, 1);
        // PubSub delta delivered.
        let _ = rx.recv().await.unwrap();
        // Replay index recorded.
        let idx = proj.replay_index.read().await;
        assert!(idx.fp_at(1).is_some());
    }

    // ─── sn06 + sn07 RPC entrypoints ─────────────────────────────────

    #[tokio::test]
    async fn snapshot_rpc_rejects_no_creds() {
        let proj = SnapshotProjection::new(empty_snap(), 16, "boot".into());
        let r = snapshot_rpc(&proj, None, true).await;
        assert!(matches!(
            r,
            Err(DaemonError::SnapshotUnavailable(
                SnapshotUnavailableReason::TransportNotLocallyTrusted
            ))
        ));
    }

    #[tokio::test]
    async fn snapshot_rpc_rejects_when_not_ready() {
        let proj = SnapshotProjection::new(empty_snap(), 16, "boot".into());
        let creds = PeerCreds {
            pid: 1,
            uid: 0,
            gid: 0,
        };
        let r = snapshot_rpc(&proj, Some(&creds), false).await;
        assert!(matches!(
            r,
            Err(DaemonError::SnapshotUnavailable(
                SnapshotUnavailableReason::NotReady
            ))
        ));
    }

    #[tokio::test]
    async fn snapshot_rpc_returns_current_when_ready_and_local() {
        let proj = SnapshotProjection::new(empty_snap(), 16, "boot".into());
        let creds = PeerCreds {
            pid: 1,
            uid: 0,
            gid: 0,
        };
        let snap = snapshot_rpc(&proj, Some(&creds), true).await.unwrap();
        assert_eq!(snap.stream_seq, 0);
    }

    #[tokio::test]
    async fn subscribe_rpc_returns_ack_and_receiver() {
        let proj = SnapshotProjection::new(empty_snap(), 16, "boot".into());
        let creds = PeerCreds {
            pid: 1,
            uid: 0,
            gid: 0,
        };
        let (ack, _rx) = subscribe_rpc(
            &proj,
            Some(&creds),
            true,
            SubscribeRequest {
                since_fingerprint: None,
                since_stream_seq: None,
                since_boot_id: None,
            },
        )
        .await
        .unwrap();
        // Clause (P) — first subscribe -> Resync.
        assert_eq!(ack, SubscribeAck::Resync);
    }

    // ─── ExitReason serialize round-trip ─────────────────────────────

    #[test]
    fn exit_reason_serialize_round_trip() {
        let reasons = vec![
            ExitReason::Completed,
            ExitReason::Aborted,
            ExitReason::Error("oom".into()),
            ExitReason::Reaped {
                stage: "sigkill".into(),
                exit_code: None,
            },
        ];
        for r in reasons {
            let s = serde_json::to_string(&r).unwrap();
            let back: ExitReason = serde_json::from_str(&s).unwrap();
            assert_eq!(r, back);
        }
    }

    // Avoid dead-code warning on RunRow construction in this module.
    #[test]
    fn runrow_helper_compiles() {
        let r = RunRow {
            run_id: crate::mailbox::RunId("r".into()),
            status: RunStatus::Running,
            repo_coordinate: crate::registry::RepoCoordinate {
                slug: "s".into(),
                remote_url: None,
                default_branch: None,
            },
            attempt: 1,
            session_id: None,
            tokens: RunRowTokens::default(),
            last_event: None,
        };
        assert_eq!(r.attempt, 1);
    }
}
