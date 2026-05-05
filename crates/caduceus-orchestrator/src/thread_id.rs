//! ThreadId infrastructure — durable storage key + session→thread index.
//!
//! Per `spec-decision-register` §3.0 (Z8-D40..D43), durable per-conversation
//! state (decision register, transcript, plan state, mode, permission
//! envelope) is keyed by `ThreadId`, NOT `SessionId`. This module owns:
//!
//! * The session→thread index file at
//!   `~/.caduceus/sessions/<session_id>/thread_id` — a single-line UTF-8
//!   record mapping a session to its durable thread. Atomic-rename written.
//! * Resolution: `resolve_thread_id_for_session` reads the index; if absent
//!   (truly new session), mints a new `ThreadId` and writes the index.
//! * Migration from the pre-spec layout (`~/.caduceus/sessions/<session_id>/`)
//!   to the new layout (`~/.caduceus/threads/<thread_id>/`). Idempotent.
//!
//! The reducer / persistence layers (P3) consume `thread_dir(thread_id)` as
//! the canonical durable directory.

use anyhow::{Context, Result};
use caduceus_core::{decision_register::SessionThreadIndex, SessionId, ThreadId};
use std::fs;
use std::path::{Path, PathBuf};

/// Default base directory for caduceus state. Matches the
/// `m-e2e-architecture.md` convention. Tests override via [`ThreadIdEnv`].
pub const DEFAULT_BASE_DIR: &str = ".caduceus";

/// Test-friendly environment for resolving the base dir and producing
/// per-session / per-thread paths.
#[derive(Debug, Clone)]
pub struct ThreadIdEnv {
    base: PathBuf,
}

impl ThreadIdEnv {
    /// Production constructor: base = `<home>/.caduceus`.
    pub fn from_home() -> Result<Self> {
        let home = dirs::home_dir().context("could not resolve $HOME")?;
        Ok(Self {
            base: home.join(DEFAULT_BASE_DIR),
        })
    }

    /// Tests / explicit overrides.
    pub fn with_base(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// `~/.caduceus/`
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// `~/.caduceus/sessions/<session_id>/`
    pub fn session_dir(&self, sid: &SessionId) -> PathBuf {
        self.base.join("sessions").join(sid.0.to_string())
    }

    /// `~/.caduceus/sessions/<session_id>/thread_id`
    pub fn session_thread_index_path(&self, sid: &SessionId) -> PathBuf {
        self.session_dir(sid).join("thread_id")
    }

    /// `~/.caduceus/threads/<thread_id>/`
    pub fn thread_dir(&self, tid: &ThreadId) -> PathBuf {
        self.base.join("threads").join(tid.0.to_string())
    }
}

/// Resolution outcome — exposes whether a fresh `ThreadId` was minted so
/// callers can emit `AgentEvent::ThreadIdMigrated` or similar audit events
/// only on first-time resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// The session already had an index file; the existing `ThreadId` was
    /// returned.
    Existing(ThreadId),
    /// No index existed; we minted a new `ThreadId` and wrote the index.
    Minted(ThreadId),
}

impl ResolveOutcome {
    pub fn thread_id(&self) -> &ThreadId {
        match self {
            Self::Existing(t) | Self::Minted(t) => t,
        }
    }

    pub fn is_minted(&self) -> bool {
        matches!(self, Self::Minted(_))
    }
}

/// Resolve a `SessionId` to its durable `ThreadId`.
///
/// * If the session→thread index file exists and parses, returns
///   [`ResolveOutcome::Existing`]. (Z8-D40 — survives `SessionId` rebinds.)
/// * If absent (truly new session), mints a new `ThreadId`, writes the
///   index file via atomic rename, and returns [`ResolveOutcome::Minted`].
pub fn resolve_thread_id_for_session(env: &ThreadIdEnv, sid: &SessionId) -> Result<ResolveOutcome> {
    let index_path = env.session_thread_index_path(sid);
    if index_path.exists() {
        let bytes = fs::read(&index_path)
            .with_context(|| format!("read index file {}", index_path.display()))?;
        let parsed: SessionThreadIndex = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "parse index file {} (corrupt session→thread index)",
                index_path.display()
            )
        })?;
        // Sanity: stored session_id should match the lookup; if not, treat as
        // corruption — the index is per-session-directory so a mismatch means
        // someone copied the dir, which is unsupported.
        if parsed.session_id != *sid {
            anyhow::bail!(
                "session→thread index at {} stores session_id={} but lookup was for {}; \
                 refusing to use stale index (delete the file to mint a new ThreadId)",
                index_path.display(),
                parsed.session_id,
                sid,
            );
        }
        return Ok(ResolveOutcome::Existing(parsed.thread_id));
    }

    let thread_id = ThreadId::new();
    write_session_thread_index(env, sid, &thread_id)?;
    Ok(ResolveOutcome::Minted(thread_id))
}

/// Write the index file via atomic rename (Z8-D43).
fn write_session_thread_index(env: &ThreadIdEnv, sid: &SessionId, tid: &ThreadId) -> Result<()> {
    let dir = env.session_dir(sid);
    fs::create_dir_all(&dir).with_context(|| format!("mkdir -p {}", dir.display()))?;

    let index = SessionThreadIndex {
        session_id: sid.clone(),
        thread_id: tid.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&index).context("serialize SessionThreadIndex")?;

    let tmp = dir.join(format!("thread_id.tmp.{:08x}", rand_u32_for_tmp_suffix()));
    fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;

    let target = env.session_thread_index_path(sid);
    fs::rename(&tmp, &target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}

/// Cheap pseudo-random suffix for the temp filename — enough to avoid
/// collisions across rapid concurrent writes within a session dir; does
/// not need cryptographic randomness.
fn rand_u32_for_tmp_suffix() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    nanos ^ std::process::id()
}

/// Migration helper for the pre-spec on-disk layout. For each existing
/// `<base>/sessions/<sid>/` directory:
///
/// 1. Read or mint the `ThreadId` for that session.
/// 2. If `<base>/threads/<tid>/` does not exist, **move** the session
///    directory's durable subkeys (everything except the thread_id index
///    itself) to `<base>/threads/<tid>/`. The session directory is left
///    in place with only the index file.
/// 3. Idempotent: a session whose thread directory already exists is a
///    no-op.
///
/// Returns the list of `(SessionId, ThreadId)` pairs whose state was newly
/// migrated this call (callers should emit `AgentEvent::ThreadIdMigrated`
/// once per pair). Failures abort the migration with the path that failed
/// — operator intervention is expected per spec §3.0.2.
pub fn migrate_pre_spec_layout(env: &ThreadIdEnv) -> Result<Vec<(SessionId, ThreadId)>> {
    let sessions_root = env.base.join("sessions");
    if !sessions_root.exists() {
        return Ok(Vec::new());
    }
    let mut migrated = Vec::new();

    let entries = fs::read_dir(&sessions_root)
        .with_context(|| format!("readdir {}", sessions_root.display()))?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("readdir entry under {}", sessions_root.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let sid = match uuid::Uuid::parse_str(&dir_name) {
            Ok(u) => SessionId(u),
            Err(_) => continue,
        };

        let outcome = resolve_thread_id_for_session(env, &sid)?;
        let tid = outcome.thread_id().clone();
        let thread_dir = env.thread_dir(&tid);

        if thread_dir.exists() {
            // Already migrated; idempotent no-op.
            continue;
        }

        let session_dir = env.session_dir(&sid);
        let payload_count = move_durable_subkeys(&session_dir, &thread_dir)?;

        if payload_count > 0 {
            migrated.push((sid, tid));
        }
    }
    Ok(migrated)
}

/// Move every entry inside `from` to `to`, EXCEPT the `thread_id` index
/// file (which must remain in the session dir to keep resolution working).
/// Returns how many entries were moved.
fn move_durable_subkeys(from: &Path, to: &Path) -> Result<usize> {
    fs::create_dir_all(to).with_context(|| format!("mkdir -p {}", to.display()))?;
    let mut moved = 0;
    for entry in fs::read_dir(from).with_context(|| format!("readdir {}", from.display()))? {
        let entry = entry.with_context(|| format!("readdir entry under {}", from.display()))?;
        let src = entry.path();
        let name = match src.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if name == "thread_id" {
            // Keep the index in place.
            continue;
        }
        let dst = to.join(&name);
        fs::rename(&src, &dst)
            .with_context(|| format!("rename {} -> {}", src.display(), dst.display()))?;
        moved += 1;
    }
    Ok(moved)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn env_in(td: &TempDir) -> ThreadIdEnv {
        ThreadIdEnv::with_base(td.path().to_path_buf())
    }

    #[test]
    fn resolves_minted_when_index_absent_then_existing_on_second_call() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let sid = SessionId::new();

        let first = resolve_thread_id_for_session(&env, &sid).unwrap();
        assert!(first.is_minted());
        let tid = first.thread_id().clone();
        assert!(env.session_thread_index_path(&sid).exists());

        let second = resolve_thread_id_for_session(&env, &sid).unwrap();
        assert_eq!(second, ResolveOutcome::Existing(tid.clone()));
    }

    #[test]
    fn z8_d40_session_rebind_resolves_same_thread_id() {
        // Simulate the rebind: same on-disk session dir + index, two
        // logical "resolve" calls that succeed regardless of whether the
        // editor has reissued anything in memory.
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let sid = SessionId::new();
        let outcome_a = resolve_thread_id_for_session(&env, &sid).unwrap();
        let tid = outcome_a.thread_id().clone();

        // "Re-bind" in the editor: drop and re-construct the env (no in-
        // memory cache survives).
        let env2 = env_in(&td);
        let outcome_b = resolve_thread_id_for_session(&env2, &sid).unwrap();
        assert_eq!(outcome_b, ResolveOutcome::Existing(tid));
    }

    #[test]
    fn z8_d43_index_uses_atomic_rename_pattern() {
        // We can't observe the rename atom directly, but we can assert
        // that no `thread_id.tmp.*` file is left behind after a successful
        // mint, AND the index file exists with the expected content.
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let sid = SessionId::new();

        let _ = resolve_thread_id_for_session(&env, &sid).unwrap();
        let session_dir = env.session_dir(&sid);
        let entries: Vec<String> = fs::read_dir(&session_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        assert!(
            !entries.iter().any(|n| n.starts_with("thread_id.tmp.")),
            "unexpected stale temp file: {entries:?}"
        );
        assert!(entries.contains(&"thread_id".to_string()));
    }

    #[test]
    fn corrupt_index_with_mismatched_session_id_is_rejected() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let sid_a = SessionId::new();
        let sid_b = SessionId::new();

        // Write an index for sid_a's directory but containing sid_b inside.
        let dir = env.session_dir(&sid_a);
        fs::create_dir_all(&dir).unwrap();
        let bogus = SessionThreadIndex {
            session_id: sid_b,
            thread_id: ThreadId::new(),
        };
        fs::write(
            env.session_thread_index_path(&sid_a),
            serde_json::to_vec_pretty(&bogus).unwrap(),
        )
        .unwrap();

        let err = resolve_thread_id_for_session(&env, &sid_a).unwrap_err();
        assert!(err.to_string().contains("stale index"), "got: {err}");
    }

    #[test]
    fn z8_d42_thread_dir_keyed_by_thread_id() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let tid = ThreadId::new();
        let dir = env.thread_dir(&tid);
        let display = dir.display().to_string();
        assert!(
            display.contains("/threads/"),
            "expected /threads/ in {display}"
        );
        assert!(
            display.contains(&tid.to_string()),
            "expected thread id in {display}"
        );
    }

    #[test]
    fn migration_no_op_when_sessions_root_missing() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let migrated = migrate_pre_spec_layout(&env).unwrap();
        assert!(migrated.is_empty());
    }

    #[test]
    fn migration_moves_subkeys_then_is_idempotent() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);

        // Pre-spec layout: sessions/<sid>/{transcript.jsonl, mode}, no thread_id index.
        let sid = SessionId::new();
        let session_dir = env.session_dir(&sid);
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(session_dir.join("transcript.jsonl"), b"{}\n").unwrap();
        fs::write(session_dir.join("mode"), b"plan").unwrap();

        let migrated = migrate_pre_spec_layout(&env).unwrap();
        assert_eq!(migrated.len(), 1);
        let (got_sid, tid) = &migrated[0];
        assert_eq!(*got_sid, sid);

        let thread_dir = env.thread_dir(tid);
        assert!(thread_dir.join("transcript.jsonl").exists());
        assert!(thread_dir.join("mode").exists());
        // Index remains in session dir.
        assert!(env.session_thread_index_path(&sid).exists());
        // Subkeys removed from session dir.
        assert!(!session_dir.join("transcript.jsonl").exists());
        assert!(!session_dir.join("mode").exists());

        // Re-running is a no-op.
        let again = migrate_pre_spec_layout(&env).unwrap();
        assert!(
            again.is_empty(),
            "second run should be no-op, got {again:?}"
        );
    }

    #[test]
    fn z8_d41_workspace_mutation_does_not_mint_thread_id() {
        // This invariant lives in the workspace-mutation handler (P5), but
        // we can pin its substrate here: re-resolving an existing session
        // never mints a new ThreadId.
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let sid = SessionId::new();
        let first = resolve_thread_id_for_session(&env, &sid).unwrap();
        for _ in 0..5 {
            let again = resolve_thread_id_for_session(&env, &sid).unwrap();
            assert_eq!(again, ResolveOutcome::Existing(first.thread_id().clone()));
        }
    }

    #[test]
    fn migration_skips_dirs_with_existing_thread_dir_already_migrated() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);

        let sid = SessionId::new();
        let session_dir = env.session_dir(&sid);
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(session_dir.join("transcript.jsonl"), b"a").unwrap();

        // First migration creates threads/<tid>/.
        let first = migrate_pre_spec_layout(&env).unwrap();
        assert_eq!(first.len(), 1);
        let tid = first[0].1.clone();

        // Now seed a NEW file in the session dir to simulate post-migration
        // legacy writes (shouldn't happen but be defensive).
        fs::write(session_dir.join("late_addition"), b"b").unwrap();

        // Second migration sees thread_dir already exists → idempotent no-op
        // (does NOT touch the late_addition file).
        let second = migrate_pre_spec_layout(&env).unwrap();
        assert!(second.is_empty());
        assert!(session_dir.join("late_addition").exists());
        assert!(!env.thread_dir(&tid).join("late_addition").exists());
    }
}
