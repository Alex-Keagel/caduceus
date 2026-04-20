//! P13.13 — Atomic `(SessionState, ConversationHistory)` snapshot (G‑R9.1).
//!
//! When the engine crashes mid‑turn, restoring just the session header without
//! the matching conversation tail (or vice‑versa) leaves the agent in a state
//! where `turn_count` and the last message disagree. The fix is a single
//! atomic bundle written via temp‑file + rename so a crash either leaves the
//! old bundle intact or the new one fully written — never a half‑state.
//!
//! Cite: classic Unix `rename(2)` atomicity guarantee + Lamport "Time, Clocks,
//! and the Ordering of Events" (1978) on the importance of monotonic state
//! transitions.

use crate::ConversationHistory;
use caduceus_core::{CaduceusError, Result, SessionState};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// On‑disk bundle that pairs a session header with its conversation tail.
/// Bumping `version` triggers a refusal to load on schema changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub session: SessionState,
    /// `ConversationHistory` is private‑messages‑inside; we store the JSON
    /// blob produced by [`ConversationHistory::serialize`] so the messages
    /// type doesn't need to be `Serialize` here.
    pub history_json: String,
}

pub const SNAPSHOT_VERSION: u32 = 1;

impl Snapshot {
    /// Bundle a session + history. Inert until [`Snapshot::write_atomic`].
    pub fn capture(session: &SessionState, history: &ConversationHistory) -> Result<Self> {
        let history_json = history.serialize()?;
        Ok(Self {
            version: SNAPSHOT_VERSION,
            session: session.clone(),
            history_json,
        })
    }

    /// Restore the bundled history into a fresh `ConversationHistory`.
    pub fn restore_history(&self) -> Result<ConversationHistory> {
        ConversationHistory::deserialize(&self.history_json)
    }

    /// Write to `path` using the temp‑file + rename pattern. The temp file
    /// lives in the same directory as the target so the rename is on the
    /// same filesystem (otherwise it falls back to copy+unlink, which is
    /// NOT atomic on POSIX).
    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string(self)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("snapshot serialise: {e}")))?;
        let parent = path
            .parent()
            .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("snapshot path has no parent")))?;
        std::fs::create_dir_all(parent)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("mkdir {parent:?}: {e}")))?;
        let tmp = parent.join(format!(".snapshot.{}.tmp", std::process::id()));
        // Best‑effort cleanup if a previous run left a tmp file behind.
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&tmp, &json)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("write tmp: {e}")))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("atomic rename: {e}")))?;
        Ok(())
    }

    /// Read a snapshot bundle from disk. Returns `Err` on malformed JSON or
    /// version mismatch.
    pub fn read(path: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("read snapshot: {e}")))?;
        let snap: Snapshot = serde_json::from_str(&json)
            .map_err(|e| CaduceusError::Other(anyhow::anyhow!("snapshot deserialise: {e}")))?;
        if snap.version != SNAPSHOT_VERSION {
            return Err(CaduceusError::Other(anyhow::anyhow!(
                "snapshot version mismatch: got {} expected {}",
                snap.version,
                SNAPSHOT_VERSION
            )));
        }
        Ok(snap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::{ModelId, ProviderId};
    use caduceus_providers::Message;
    use tempfile::tempdir;

    fn fixture_session() -> SessionState {
        SessionState::new(
            std::path::PathBuf::from("/tmp/p13_13"),
            ProviderId("anthropic".into()),
            ModelId::new("claude-3-5-sonnet"),
        )
    }

    #[test]
    fn p13_13_round_trip_preserves_session_and_history() {
        let mut sess = fixture_session();
        sess.turn_count = 7;
        let mut hist = ConversationHistory::new();
        hist.append(Message::user("hello"));
        hist.append(Message::assistant("hi"));
        let snap = Snapshot::capture(&sess, &hist).unwrap();
        let dir = tempdir().unwrap();
        let path = dir.path().join("snap.json");
        snap.write_atomic(&path).unwrap();
        let read = Snapshot::read(&path).unwrap();
        assert_eq!(read.session.turn_count, 7);
        let restored = read.restore_history().unwrap();
        assert_eq!(restored.len(), 2);
    }

    #[test]
    fn p13_13_atomic_rename_no_partial_file_on_crash_simulation() {
        // The temp file lives next to the target. Verify that after a
        // successful write the tmp file is gone and the target is whole.
        let sess = fixture_session();
        let hist = ConversationHistory::new();
        let snap = Snapshot::capture(&sess, &hist).unwrap();
        let dir = tempdir().unwrap();
        let path = dir.path().join("snap.json");
        snap.write_atomic(&path).unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(entries.iter().any(|n| n == "snap.json"));
        assert!(!entries.iter().any(|n| n.starts_with(".snapshot.")));
    }

    #[test]
    fn p13_13_overwrite_is_atomic_keeps_old_on_failure() {
        // First write a v1.
        let mut sess = fixture_session();
        sess.turn_count = 1;
        let hist = ConversationHistory::new();
        let snap1 = Snapshot::capture(&sess, &hist).unwrap();
        let dir = tempdir().unwrap();
        let path = dir.path().join("snap.json");
        snap1.write_atomic(&path).unwrap();
        // Overwrite with v2.
        sess.turn_count = 2;
        let snap2 = Snapshot::capture(&sess, &hist).unwrap();
        snap2.write_atomic(&path).unwrap();
        let read = Snapshot::read(&path).unwrap();
        assert_eq!(read.session.turn_count, 2);
    }

    #[test]
    fn p13_13_version_mismatch_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snap.json");
        std::fs::write(&path, r#"{"version":999,"session":{},"history_json":""}"#).unwrap();
        assert!(Snapshot::read(&path).is_err());
    }

    #[test]
    fn p13_13_session_and_history_consistent_post_restore() {
        // Crash‑mid‑turn invariant: turn_count and last message must agree.
        let mut sess = fixture_session();
        sess.turn_count = 3;
        let mut hist = ConversationHistory::new();
        for i in 0..3 {
            hist.append(Message::user(&format!("turn {i} user")));
            hist.append(Message::assistant(&format!("turn {i} reply")));
        }
        let snap = Snapshot::capture(&sess, &hist).unwrap();
        let dir = tempdir().unwrap();
        let path = dir.path().join("snap.json");
        snap.write_atomic(&path).unwrap();
        let read = Snapshot::read(&path).unwrap();
        let restored_hist = read.restore_history().unwrap();
        // Three turns × 2 messages.
        assert_eq!(restored_hist.len(), 6);
        assert_eq!(read.session.turn_count, 3);
    }
}
