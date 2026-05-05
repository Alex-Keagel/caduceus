//! Open-question pool + workspace-mutation handler isolation.
//!
//! P5 of `spec-decision-register`. Two tightly-coupled responsibilities:
//!
//! 1. **Open-question pool** (spec §3.4.2): per-thread `BTreeMap<DecisionId,
//!    OpenQuestion>` persisted at `~/.caduceus/threads/<tid>/open_questions.json`.
//!    The pool drives the structural restore prong (Z8-D33a, the primary
//!    mechanism for closing Failure B).
//! 2. **Workspace-mutation handler** (spec §3.6 / Z8-D30..D32): a single
//!    named function that mutates ONLY `WorkspaceContext`, never thread or
//!    session identity. The static-check test asserts this module's
//!    enforcement function has no `use` edges to session-creation /
//!    transcript-rebind / ThreadId-mint paths.

use anyhow::{Context, Result};
use caduceus_core::decision_register::OpenQuestion;
use caduceus_core::{AgentEvent, DecisionId, ThreadId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use uuid::Uuid;

use crate::decision_register::DecisionRegister;
use crate::thread_id::ThreadIdEnv;

/// Filename for the persisted pool.
const POOL_FILE: &str = "open_questions.json";

/// On-disk schema version for the pool. Bumps follow
/// `spec-cross-cutting-wiring.md` §3.10.
pub const POOL_SCHEMA_VERSION: u16 = 1;

/// Per-thread open-question pool. Persisted as JSON via the same
/// fsync-+-atomic-rename discipline as the decision register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenQuestionPool {
    pub schema_version: u16,
    pub thread_id: ThreadId,
    pub entries: BTreeMap<DecisionId, OpenQuestion>,
    /// Per-id counter of agent turns elapsed since the question was
    /// presented. Used to surface `OpenQuestionUnanswered` after N=2
    /// turns (Z8-D48).
    pub turns_since_presented: BTreeMap<DecisionId, u32>,
}

impl OpenQuestionPool {
    pub fn new(thread_id: ThreadId) -> Self {
        Self {
            schema_version: POOL_SCHEMA_VERSION,
            thread_id,
            entries: BTreeMap::new(),
            turns_since_presented: BTreeMap::new(),
        }
    }

    /// Insert a question; idempotent on `id` (last-write-wins).
    pub fn present(&mut self, q: OpenQuestion) {
        let id = q.id.clone();
        self.entries.insert(id.clone(), q);
        self.turns_since_presented.insert(id, 0);
    }

    /// Eliminate (the question got an answer). Returns true if the entry
    /// existed.
    pub fn eliminate(&mut self, id: &DecisionId) -> bool {
        let removed = self.entries.remove(id).is_some();
        self.turns_since_presented.remove(id);
        removed
    }

    /// Iterate the currently-pending DecisionIds in lex order
    /// (`BTreeMap` already orders them; explicit method for callers).
    pub fn ids(&self) -> impl Iterator<Item = &DecisionId> {
        self.entries.keys()
    }

    /// Increment per-question turn counters. Returns the ids whose counter
    /// crossed the [`UNANSWERED_TURNS_THRESHOLD`] this call — the caller
    /// emits one `OpenQuestionUnanswered` event per id and resets the
    /// counter (or removes the entry, depending on policy; see Z8-D48).
    pub fn tick_turn(&mut self) -> Vec<DecisionId> {
        let mut newly_unanswered = Vec::new();
        for (id, counter) in self.turns_since_presented.iter_mut() {
            *counter += 1;
            if *counter == UNANSWERED_TURNS_THRESHOLD {
                newly_unanswered.push(id.clone());
            }
        }
        newly_unanswered
    }

    /// Reset the per-id counter without removing the entry — used when the
    /// caller emits `OpenQuestionUnanswered` and wants to suppress further
    /// emissions for the same id until the next threshold crossing.
    pub fn ack_unanswered(&mut self, id: &DecisionId) {
        self.turns_since_presented.insert(id.clone(), 0);
    }
}

/// Spec §3.9.4 / Z8-D48: open question with no `DecisionLocked` after
/// N turns surfaces `OpenQuestionUnanswered`. Default N=2.
pub const UNANSWERED_TURNS_THRESHOLD: u32 = 2;

/// Path to the persisted pool for `tid`.
pub fn pool_path(env: &ThreadIdEnv, tid: &ThreadId) -> PathBuf {
    env.thread_dir(tid).join(POOL_FILE)
}

/// Load the pool from disk, returning `Ok(None)` when absent.
pub fn load_pool(env: &ThreadIdEnv, tid: &ThreadId) -> Result<Option<OpenQuestionPool>> {
    let target = pool_path(env, tid);
    if !target.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&target).with_context(|| format!("read {}", target.display()))?;
    let pool: OpenQuestionPool =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", target.display()))?;
    if pool.thread_id != *tid {
        anyhow::bail!(
            "pool at {} stores thread_id={} but lookup was for {}",
            target.display(),
            pool.thread_id,
            tid,
        );
    }
    if pool.schema_version > POOL_SCHEMA_VERSION {
        anyhow::bail!(
            "pool at {} schema_version={} > supported {}",
            target.display(),
            pool.schema_version,
            POOL_SCHEMA_VERSION
        );
    }
    Ok(Some(pool))
}

/// Persist via temp-+-fsync-+-rename-+-fsync-parent.
pub fn persist_pool(env: &ThreadIdEnv, pool: &OpenQuestionPool) -> Result<()> {
    let dir = env.thread_dir(&pool.thread_id);
    fs::create_dir_all(&dir).with_context(|| format!("mkdir -p {}", dir.display()))?;
    let target = pool_path(env, &pool.thread_id);
    let bytes = serde_json::to_vec_pretty(pool).context("serialize OpenQuestionPool")?;
    let tmp = dir.join(format!("{}.tmp.{:08x}", POOL_FILE, rand_suffix()));
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("create tmp {}", tmp.display()))?;
        f.write_all(&bytes)
            .with_context(|| format!("write tmp {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync tmp {}", tmp.display()))?;
    }
    fs::rename(&tmp, &target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
    if let Ok(parent) = fs::File::open(&dir) {
        let _ = parent.sync_all();
    }
    Ok(())
}

fn rand_suffix() -> u32 {
    Uuid::new_v4().as_u128() as u32
}

/// Apply an [`AgentEvent`] to the pool. Mirrors the decision-register
/// reducer's sparse-pass-through discipline (Z8-D13): non-pool events are
/// no-ops that return `false`.
///
/// Returns `true` iff the pool was mutated (caller should persist).
pub fn apply_pool_event(pool: &mut OpenQuestionPool, event: &AgentEvent) -> bool {
    match event {
        AgentEvent::OpenQuestionPresented {
            id,
            prompt,
            kind,
            options,
        } => {
            pool.present(OpenQuestion {
                id: id.clone(),
                prompt: prompt.clone(),
                kind: *kind,
                options: options.clone(),
                presented_at: now_iso8601(),
                presented_by_execution_id: caduceus_core::ExecutionId(0),
            });
            true
        }
        AgentEvent::OpenQuestionEliminated { id, .. } => pool.eliminate(id),
        AgentEvent::DecisionLocked { id, .. } => {
            // A lock implicitly eliminates the matching open question.
            pool.eliminate(id)
        }
        _ => false,
    }
}

fn now_iso8601() -> String {
    use chrono::{DateTime, Utc};
    let now: DateTime<Utc> = Utc::now();
    now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

// ── Workspace-mutation handler isolation (spec §3.6) ──────────────────────────

/// Discriminant carried by the `WorkspaceMutationEvent`s the handler
/// receives. Production wire types are owned by `caduceus-zed`; this is
/// the daemon-side mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum WorkspaceMutation {
    RootAdded(String),
    RootRemoved(String),
    RootRenamed { from: String, to: String },
}

/// Workspace-context block. Mutated ONLY by [`apply_workspace_mutation`].
/// Persisted under `~/.caduceus/threads/<tid>/workspace_context.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkspaceContext {
    pub roots: Vec<String>,
}

/// **The single named handler that mutates `WorkspaceContext`.** Per spec
/// §3.6 / Z8-D30, this function MUST NOT touch `ThreadId`, `SessionId`,
/// the transcript, or the decision register.
///
/// The static-check test (`workspace_handler_does_not_touch_thread_id_or_session`)
/// asserts this module's source has no string occurrences of
/// session-creation / transcript-rebind / `ThreadId::new` / etc. paths.
/// That's a coarse but enforceable check that this function (or any
/// helper added next to it) doesn't sneak in a re-key path.
pub fn apply_workspace_mutation(ctx: &mut WorkspaceContext, mutation: &WorkspaceMutation) {
    match mutation {
        WorkspaceMutation::RootAdded(p) => {
            if !ctx.roots.contains(p) {
                ctx.roots.push(p.clone());
                ctx.roots.sort();
            }
        }
        WorkspaceMutation::RootRemoved(p) => {
            ctx.roots.retain(|r| r != p);
        }
        WorkspaceMutation::RootRenamed { from, to } => {
            for root in ctx.roots.iter_mut() {
                if root == from {
                    *root = to.clone();
                }
            }
            ctx.roots.sort();
            ctx.roots.dedup();
        }
    }
}

const WORKSPACE_CTX_FILE: &str = "workspace_context.json";

pub fn workspace_context_path(env: &ThreadIdEnv, tid: &ThreadId) -> PathBuf {
    env.thread_dir(tid).join(WORKSPACE_CTX_FILE)
}

pub fn load_workspace_context(env: &ThreadIdEnv, tid: &ThreadId) -> Result<WorkspaceContext> {
    let target = workspace_context_path(env, tid);
    if !target.exists() {
        return Ok(WorkspaceContext::default());
    }
    let bytes = fs::read(&target).with_context(|| format!("read {}", target.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", target.display()))
}

pub fn persist_workspace_context(
    env: &ThreadIdEnv,
    tid: &ThreadId,
    ctx: &WorkspaceContext,
) -> Result<()> {
    let dir = env.thread_dir(tid);
    fs::create_dir_all(&dir).with_context(|| format!("mkdir -p {}", dir.display()))?;
    let target = workspace_context_path(env, tid);
    let bytes = serde_json::to_vec_pretty(ctx).context("serialize WorkspaceContext")?;
    let tmp = dir.join(format!("{}.tmp.{:08x}", WORKSPACE_CTX_FILE, rand_suffix()));
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("create tmp {}", tmp.display()))?;
        f.write_all(&bytes)
            .with_context(|| format!("write tmp {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync tmp {}", tmp.display()))?;
    }
    fs::rename(&tmp, &target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
    if let Ok(parent) = fs::File::open(&dir) {
        let _ = parent.sync_all();
    }
    Ok(())
}

/// Convenience: full workspace-mutation pipeline. Mutates `ctx`, persists
/// the new state, and returns the events the caller fans out (currently
/// only the one mutation event itself, kept as a Vec for forward-compat).
pub fn handle_workspace_mutation(
    env: &ThreadIdEnv,
    tid: &ThreadId,
    ctx: &mut WorkspaceContext,
    mutation: WorkspaceMutation,
) -> Result<Vec<WorkspaceMutation>> {
    apply_workspace_mutation(ctx, &mutation);
    persist_workspace_context(env, tid, ctx)?;
    Ok(vec![mutation])
}

/// Run a Restore pass after a workspace mutation has been processed.
/// Spec §3.4.1 T2 trigger. Caller composes:
///
///   1. handle_workspace_mutation  (mutate ctx, persist)
///   2. restore_after_workspace_mutation  (this function: reads pool +
///      register, runs run_restore, returns events for the caller to
///      emit).
pub fn restore_after_workspace_mutation(
    env: &ThreadIdEnv,
    register: &DecisionRegister,
) -> Result<crate::restore_protocol::RestoreOutcome> {
    let pool_opt = load_pool(env, &register.thread_id)?;
    let pool_ids: Vec<DecisionId> = pool_opt
        .as_ref()
        .map(|p| p.entries.keys().cloned().collect())
        .unwrap_or_default();
    let stale = crate::restore_protocol::compute_eliminations(register, pool_ids.iter());
    Ok(crate::restore_protocol::run_restore(
        register,
        crate::restore_protocol::RestoreTrigger::WorkspaceMutation,
        &stale,
        register.last_event_seq,
    ))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::decision_register::{
        DecisionEntry, DecisionOp, DecisionRecord, DecisionSource, DecisionState, DecisionValue,
        DecisionValueKind, ProducerId,
    };
    use caduceus_core::AgentEvent;
    use tempfile::TempDir;

    fn id(s: &str) -> DecisionId {
        DecisionId::new(s).unwrap()
    }

    fn env_in(td: &TempDir) -> ThreadIdEnv {
        ThreadIdEnv::with_base(td.path())
    }

    // ── Pool tests ──

    #[test]
    fn present_then_eliminate_round_trip() {
        let mut p = OpenQuestionPool::new(ThreadId::new());
        p.present(OpenQuestion {
            id: id("naming"),
            prompt: "Name?".into(),
            kind: DecisionValueKind::String,
            options: None,
            presented_at: now_iso8601(),
            presented_by_execution_id: caduceus_core::ExecutionId(0),
        });
        assert_eq!(p.entries.len(), 1);
        assert!(p.eliminate(&id("naming")));
        assert!(p.entries.is_empty());
        assert!(p.turns_since_presented.is_empty());
    }

    #[test]
    fn pool_persists_round_trip() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let mut p = OpenQuestionPool::new(ThreadId::new());
        p.present(OpenQuestion {
            id: id("a"),
            prompt: "p1".into(),
            kind: DecisionValueKind::Bool,
            options: None,
            presented_at: now_iso8601(),
            presented_by_execution_id: caduceus_core::ExecutionId(0),
        });
        persist_pool(&env, &p).unwrap();
        let loaded = load_pool(&env, &p.thread_id).unwrap().expect("must load");
        assert_eq!(loaded, p);
    }

    #[test]
    fn pool_persist_no_stale_temp() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let p = OpenQuestionPool::new(ThreadId::new());
        persist_pool(&env, &p).unwrap();
        let dir = env.thread_dir(&p.thread_id);
        let entries: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        assert!(entries.contains(&POOL_FILE.to_string()));
        assert!(
            !entries
                .iter()
                .any(|n| n.starts_with(&format!("{}.tmp.", POOL_FILE))),
            "stale temp leaked: {entries:?}"
        );
    }

    #[test]
    fn z8_d24_lock_event_eliminates_open_question() {
        let mut p = OpenQuestionPool::new(ThreadId::new());
        p.present(OpenQuestion {
            id: id("a"),
            prompt: "?".into(),
            kind: DecisionValueKind::String,
            options: None,
            presented_at: now_iso8601(),
            presented_by_execution_id: caduceus_core::ExecutionId(0),
        });
        let ev = AgentEvent::DecisionLocked {
            id: id("a"),
            value: DecisionValue::String("x".into()),
            source: DecisionSource::User,
            derived_from: None,
            reason: None,
            request_id: None,
            producer_id: ProducerId::new("pid"),
        };
        let mutated = apply_pool_event(&mut p, &ev);
        assert!(mutated);
        assert!(p.entries.is_empty());
    }

    #[test]
    fn z8_d48_unanswered_threshold_after_two_turns() {
        let mut p = OpenQuestionPool::new(ThreadId::new());
        p.present(OpenQuestion {
            id: id("a"),
            prompt: "?".into(),
            kind: DecisionValueKind::String,
            options: None,
            presented_at: now_iso8601(),
            presented_by_execution_id: caduceus_core::ExecutionId(0),
        });
        let crossed_t1 = p.tick_turn();
        assert!(crossed_t1.is_empty());
        let crossed_t2 = p.tick_turn();
        assert_eq!(crossed_t2, vec![id("a")]);
        // ack_unanswered prevents re-emit on next tick until the next
        // threshold crossing.
        p.ack_unanswered(&id("a"));
        let crossed_t3 = p.tick_turn();
        assert!(crossed_t3.is_empty());
    }

    #[test]
    fn elimination_event_drops_pool_entry() {
        let mut p = OpenQuestionPool::new(ThreadId::new());
        p.present(OpenQuestion {
            id: id("a"),
            prompt: "?".into(),
            kind: DecisionValueKind::String,
            options: None,
            presented_at: now_iso8601(),
            presented_by_execution_id: caduceus_core::ExecutionId(0),
        });
        let ev = AgentEvent::OpenQuestionEliminated {
            id: id("a"),
            reason: "user-skipped".into(),
        };
        assert!(apply_pool_event(&mut p, &ev));
        assert!(p.entries.is_empty());
    }

    // ── Workspace-mutation handler tests (§6.4) ──

    #[test]
    fn z8_d30_workspace_handler_only_mutates_workspace_context() {
        let mut ctx = WorkspaceContext::default();
        apply_workspace_mutation(
            &mut ctx,
            &WorkspaceMutation::RootAdded("/Users/alex/aletheia".into()),
        );
        assert_eq!(ctx.roots, vec!["/Users/alex/aletheia"]);
        // Re-add is idempotent.
        apply_workspace_mutation(
            &mut ctx,
            &WorkspaceMutation::RootAdded("/Users/alex/aletheia".into()),
        );
        assert_eq!(ctx.roots.len(), 1);
        // Add second + sorted.
        apply_workspace_mutation(
            &mut ctx,
            &WorkspaceMutation::RootAdded("/Users/alex/caduceus".into()),
        );
        assert_eq!(
            ctx.roots,
            vec!["/Users/alex/aletheia", "/Users/alex/caduceus"]
        );
        // Remove.
        apply_workspace_mutation(
            &mut ctx,
            &WorkspaceMutation::RootRemoved("/Users/alex/aletheia".into()),
        );
        assert_eq!(ctx.roots, vec!["/Users/alex/caduceus"]);
        // Rename.
        apply_workspace_mutation(
            &mut ctx,
            &WorkspaceMutation::RootRenamed {
                from: "/Users/alex/caduceus".into(),
                to: "/Users/alex/caduceus2".into(),
            },
        );
        assert_eq!(ctx.roots, vec!["/Users/alex/caduceus2"]);
    }

    #[test]
    fn z8_d31_workspace_handler_persistence_round_trip() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let tid = ThreadId::new();
        let mut ctx = WorkspaceContext::default();
        let _ = handle_workspace_mutation(
            &env,
            &tid,
            &mut ctx,
            WorkspaceMutation::RootAdded("/x/y".into()),
        )
        .unwrap();
        let loaded = load_workspace_context(&env, &tid).unwrap();
        assert_eq!(loaded.roots, vec!["/x/y"]);
    }

    /// **Static check** — the enforcement mechanism for Z8-D30. We read
    /// THIS module's source and assert that `apply_workspace_mutation`
    /// (and by extension the only path through which workspace mutations
    /// touch state) does not reference any of the symbols that would
    /// indicate session-creation, transcript-rebind, or `ThreadId`-mint
    /// activity. This is a coarse text grep, but it catches the entire
    /// class of regressions the rubber-duck reviewer flagged: someone
    /// adding "for convenience" a `ThreadId::new()` call into the
    /// workspace-mutation path is rejected at test time.
    #[test]
    fn z8_d30_static_check_handler_isolation() {
        let src = include_str!("./openq_workspace.rs");

        // Locate the body of `apply_workspace_mutation`. The function is
        // intentionally short (just the match + helpers); we scan only its
        // body to keep the check resilient to comments / docstrings
        // elsewhere in the file.
        let needle = "pub fn apply_workspace_mutation(";
        let start = src
            .find(needle)
            .expect("apply_workspace_mutation must exist in this module");
        // Find the function's closing brace by scanning forward until
        // depth zero.
        let mut depth = 0i32;
        let mut end = start;
        for (i, ch) in src[start..].char_indices() {
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    end = start + i + 1;
                    break;
                }
            }
        }
        assert!(end > start, "couldn't bracket apply_workspace_mutation");
        let body = &src[start..end];

        let forbidden = [
            "ThreadId::new",
            "SessionId::new",
            "RequestId::new",
            "session_thread_index",
            "resolve_thread_id_for_session",
            "migrate_pre_spec_layout",
            "transcript",
            "DecisionRegister::",
            "register_path",
            "load_decision_register",
            "persist_decision_register",
            "apply_decision_event",
        ];
        for f in forbidden {
            assert!(
                !body.contains(f),
                "Z8-D30 violation: apply_workspace_mutation body references {f:?}; \
                 workspace-mutation handler MUST mutate only WorkspaceContext. \
                 If you genuinely need to touch that subsystem, route it through \
                 a separate handler outside this function."
            );
        }
    }

    // ── Restore-after-mutation integration ──

    fn locked_register(thread_id: ThreadId, ids: &[&str]) -> DecisionRegister {
        let mut r = DecisionRegister::new(thread_id);
        for s in ids {
            let did = id(s);
            let val = DecisionValue::String(format!("v_{s}"));
            let rec = DecisionRecord {
                op: DecisionOp::Lock,
                value: Some(val.clone()),
                source: DecisionSource::User,
                locked_at: "2026-05-04T10:00:00Z".into(),
                request_id: None,
                reason: None,
                producer_id: ProducerId::new("zed-cli"),
            };
            let entry = DecisionEntry {
                id: did.clone(),
                state: DecisionState::Locked,
                current: Some(val),
                history: vec![rec],
                derived_from: None,
                locked_at: "2026-05-04T10:00:00Z".into(),
                last_amended_at: None,
            };
            r.entries.insert(did, entry);
        }
        r
    }

    #[test]
    fn restore_after_workspace_mutation_eliminates_stale_open_questions() {
        let td = TempDir::new().unwrap();
        let env = env_in(&td);
        let tid = ThreadId::new();

        // Register has 3 locked decisions.
        let register = locked_register(tid.clone(), &["a", "b", "c"]);

        // Pool was carrying entries for `a` (already locked → stale) and
        // `d` (unrelated to the register).
        let mut pool = OpenQuestionPool::new(tid.clone());
        for q in ["a", "d"] {
            pool.present(OpenQuestion {
                id: id(q),
                prompt: q.into(),
                kind: DecisionValueKind::String,
                options: None,
                presented_at: now_iso8601(),
                presented_by_execution_id: caduceus_core::ExecutionId(0),
            });
        }
        persist_pool(&env, &pool).unwrap();

        // Workspace mutation lands.
        let mut ctx = WorkspaceContext::default();
        let _ = handle_workspace_mutation(
            &env,
            &tid,
            &mut ctx,
            WorkspaceMutation::RootAdded("/Users/alex/aletheia".into()),
        )
        .unwrap();

        // Run restore.
        let outcome = restore_after_workspace_mutation(&env, &register).unwrap();

        let elim_ids: Vec<String> = outcome
            .events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::OpenQuestionEliminated { id, .. } => Some(id.0.clone()),
                _ => None,
            })
            .collect();
        // `a` is stale (already locked) → eliminated. `d` is unrelated
        // (not locked) → not eliminated.
        assert_eq!(elim_ids, vec!["a".to_string()]);

        // Restored event present.
        assert!(outcome
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::DecisionRegisterRestored { .. })));
    }

    #[test]
    fn z8_d32_thread_id_stable_across_workspace_mutations() {
        // The mutation handler does not even take a ThreadId — it cannot
        // change one. This test pins the API: the function signatures we
        // commit to do not let a workspace mutation re-key the thread.
        let mut ctx = WorkspaceContext::default();
        apply_workspace_mutation(&mut ctx, &WorkspaceMutation::RootAdded("/x".into()));
        // Compile-time check: `apply_workspace_mutation` accepts only
        // `(&mut WorkspaceContext, &WorkspaceMutation)`. No ThreadId or
        // SessionId arg. (This assertion is implicit in the signature;
        // keeping the test as documentation.)
        assert_eq!(ctx.roots, vec!["/x"]);
    }
}
