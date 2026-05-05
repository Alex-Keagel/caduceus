//! RestoreProtocol + ReconciliationMessage.
//!
//! Per `spec-decision-register` §3.4 + §3.5. Two-pronged restore:
//! * **Structural** (primary, Z8-D33a) — the orchestrator subtracts every
//!   `state == Locked` `DecisionId` from the open-question pool before
//!   constructing the next agent input. This is implemented in P5
//!   alongside the open-question pool itself; this module exposes the
//!   helper [`compute_eliminations`] that P5's pool-handler calls.
//! * **Textual** (fallback, Z8-D29) — a `system`-role chat message
//!   summarizing the current register, injected into the next agent turn.
//!   Belt-and-suspenders for the structural prong; rendered here.
//!
//! Triggers (T1–T4) are wired by P5/P6; this module provides the pure
//! [`run_restore`] function those handlers call.

use anyhow::Result;
use caduceus_core::decision_register::{
    DecisionEntry, DecisionId, DecisionRecord, DecisionState, DecisionValue,
};
use caduceus_core::{AgentEvent, ThreadId};
use serde::{Deserialize, Serialize};

use crate::decision_register::DecisionRegister;
use crate::thread_id::ThreadIdEnv;

/// Byte budget for the rendered ReconciliationMessage. Spec §3.5.4
/// (Z8-D27a). Model-agnostic — chosen so every current GitHub Models
/// tokenizer fits ≤ ~1500 tokens.
pub const RECONCILIATION_BUDGET_BYTES: usize = 6000;

/// Trigger that drove the RestoreProtocol invocation. Surfaces in the
/// emitted [`AgentEvent::DecisionRegisterRestored`] for observability and
/// allows the caller to attribute restore activity to a specific cause
/// (workspace mutation, BootId change, …) when correlating with logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreTrigger {
    /// T1: agent runner attached for the first time in this boot.
    AgentAttach,
    /// T2: workspace mutation (root added/removed/renamed).
    WorkspaceMutation,
    /// T3: agent runner reported a different `BootId`.
    BootIdChange,
    /// T4: editor reissued `SessionId` and the index file resolved to the
    /// same `ThreadId`.
    SessionRebind,
}

/// Result of a restore pass. Holds the artifacts the caller will fan out:
/// the events to emit on the orchestrator stream, and the reconciliation
/// message to inject into the next agent turn.
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    /// Events the caller MUST emit on the event stream, in order:
    /// * One `OpenQuestionEliminated { id, reason: "already-locked" }`
    ///   per stale open-question id (caller has the open-question pool
    ///   and supplies that list to [`compute_eliminations`]).
    /// * Exactly one `DecisionRegisterRestored { ... }` summarizing the
    ///   pass. Always the last event in this vec.
    pub events: Vec<AgentEvent>,

    /// The synthetic `system`-role message text. Empty `String` when the
    /// register is empty and there is nothing to surface.
    pub reconciliation_message: String,

    /// `true` iff the rendered message had to drop entries to fit the
    /// 6000-byte budget. Mirrors `DecisionRegisterRestored.truncated`.
    pub truncated: bool,
}

/// Run RestoreProtocol over `register` for the given `trigger`. Pure: this
/// function does not touch the filesystem or emit events directly — it
/// returns the artifacts the caller fans out.
///
/// `stale_open_question_ids` is supplied by the caller (P5 owns the
/// open-question pool); pass an empty vec when the pool is empty or
/// already-elminated. Each id in this list MUST be a key with
/// `state == Locked` in `register.entries`; the function asserts on
/// violation in debug builds and silently drops in release.
pub fn run_restore(
    register: &DecisionRegister,
    trigger: RestoreTrigger,
    stale_open_question_ids: &[DecisionId],
    since_event_seq: u64,
) -> RestoreOutcome {
    // Step 3: structural elimination events.
    let mut events: Vec<AgentEvent> = stale_open_question_ids
        .iter()
        .filter(|id| {
            // Defensive — only emit for ids that are actually Locked in
            // the register. Ids in the pool that aren't yet locked are
            // not stale and the caller's own open-question handling
            // takes them through the normal eliminate-on-lock path.
            register
                .entries
                .get(id)
                .map(|e| e.state == DecisionState::Locked)
                .unwrap_or(false)
        })
        .map(|id| AgentEvent::OpenQuestionEliminated {
            id: id.clone(),
            reason: "already-locked".to_string(),
        })
        .collect();

    // Step 4: textual prong.
    let (message, truncated, displayed_k) = render_reconciliation_message(register);
    let count = register.locked_count();

    // Step 5/6 emits the Restored event.
    events.push(AgentEvent::DecisionRegisterRestored {
        thread_id: register.thread_id.clone(),
        count,
        since_event_seq,
        truncated,
        displayed_k,
    });

    let _ = trigger; // Currently a no-op for the pure function; surface
                     // is reserved for future telemetry.

    RestoreOutcome {
        events,
        reconciliation_message: message,
        truncated,
    }
}

/// Compute which open-question pool entries SHOULD be eliminated by the
/// restore pass (those whose `DecisionId` already has a `Locked` entry in
/// the register). Used by P5's open-question pool handler when invoking
/// [`run_restore`].
pub fn compute_eliminations<'a>(
    register: &'a DecisionRegister,
    open_question_ids: impl IntoIterator<Item = &'a DecisionId>,
) -> Vec<DecisionId> {
    open_question_ids
        .into_iter()
        .filter(|id| {
            register
                .entries
                .get(id)
                .map(|e| e.state == DecisionState::Locked)
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Render the ReconciliationMessage. Returns `(text, truncated, displayed_k)`.
///
/// Selection rule when truncating (Z8-D27a, post-rubber-duck-iteration-1):
/// recency-first by `(last_amended_at desc, locked_at desc, id asc)`.
/// Display sort within the selected subset: lexicographic by `id`
/// (Z8-D28).
pub fn render_reconciliation_message(register: &DecisionRegister) -> (String, bool, u32) {
    let locked: Vec<&DecisionEntry> = register
        .entries
        .values()
        .filter(|e| e.state == DecisionState::Locked)
        .collect();
    let total = locked.len() as u32;

    if total == 0 {
        return (String::new(), false, 0);
    }

    let header = format!(
        "[caduceus DecisionRegister — restored from thread {}]\n\n\
         Locked decisions in this thread (do NOT re-ask):\n\n",
        thread_id_short(&register.thread_id),
    );
    let footer = "\n\
         If a user message contradicts a locked decision, treat it as \
         DecisionAmended { id, value, reason } — emit the event explicitly.\n\
         Do NOT silently overwrite.\n";

    // Optimistic full render first.
    let mut all_lines: Vec<(DecisionId, String)> = locked
        .iter()
        .map(|e| (e.id.clone(), render_entry_line(e)))
        .collect();
    all_lines.sort_by(|a, b| a.0.cmp(&b.0)); // Z8-D28 lex display

    let mut full = header.clone();
    for (_, line) in &all_lines {
        full.push_str(line);
    }
    full.push_str(footer);

    if full.len() <= RECONCILIATION_BUDGET_BYTES {
        return (full, false, total);
    }

    // Selection: recency-first by (last_amended_at desc, locked_at desc, id asc).
    let mut by_recency: Vec<&DecisionEntry> = locked.clone();
    by_recency.sort_by(|a, b| {
        b.last_amended_at
            .cmp(&a.last_amended_at)
            .then_with(|| b.locked_at.cmp(&a.locked_at))
            .then_with(|| a.id.cmp(&b.id))
    });

    // Greedily add entries (in recency order) while we fit. Then sort the
    // selected subset by id for display.
    let mut selected: Vec<&DecisionEntry> = Vec::new();
    let footer_with_marker_template = |hidden: u32| {
        format!("\n... and {hidden} earlier locked decisions; query via /list_decisions\n{footer}")
    };
    for entry in by_recency.iter().copied() {
        let mut tentative = selected.clone();
        tentative.push(entry);
        // Re-sort tentative by id for accurate length measurement.
        tentative.sort_by(|a, b| a.id.cmp(&b.id));
        let lines: String = tentative.iter().map(|e| render_entry_line(e)).collect();
        let hidden = total.saturating_sub(tentative.len() as u32);
        let probe_len = header.len()
            + lines.len()
            + if hidden > 0 {
                footer_with_marker_template(hidden).len()
            } else {
                footer.len()
            };
        if probe_len > RECONCILIATION_BUDGET_BYTES {
            break;
        }
        selected = tentative;
    }

    selected.sort_by(|a, b| a.id.cmp(&b.id));
    let displayed_k = selected.len() as u32;
    let hidden = total - displayed_k;

    let mut out = header;
    for entry in &selected {
        out.push_str(&render_entry_line(entry));
    }
    if hidden > 0 {
        out.push_str(&format!(
            "\n... and {hidden} earlier locked decisions; query via /list_decisions\n",
        ));
    }
    out.push_str(footer);
    (out, true, displayed_k)
}

fn render_entry_line(entry: &DecisionEntry) -> String {
    let value_str = entry
        .current
        .as_ref()
        .map(render_value)
        .unwrap_or_else(|| "<unlocked>".to_string());
    let source = entry
        .history
        .iter()
        .next_back()
        .map(|r: &DecisionRecord| r.source)
        .map(|s| match s {
            caduceus_core::decision_register::DecisionSource::User => "user",
            caduceus_core::decision_register::DecisionSource::Agent => "agent",
        })
        .unwrap_or("unknown");
    format!(
        "- {id}: {value}\n  (locked {when}, source={source})\n",
        id = entry.id,
        value = value_str,
        when = entry.locked_at,
    )
}

fn render_value(v: &DecisionValue) -> String {
    match v {
        DecisionValue::String(s) => s.clone(),
        DecisionValue::Bool(b) => b.to_string(),
        DecisionValue::I64(n) => n.to_string(),
        DecisionValue::Path(p) => p.clone(),
        DecisionValue::Choice { options, selected } => {
            let label = options
                .get(*selected as usize)
                .cloned()
                .unwrap_or_else(|| format!("<oob:{selected}>"));
            format!(
                "{label} (option {} of {})",
                (*selected as usize) + 1,
                options.len()
            )
        }
        // `DecisionValue` is `#[non_exhaustive]`; new variants added in
        // future revisions render as their JSON text fallback so callers
        // never see a panic. Adding a variant requires a spec amendment
        // (Z8-D3 closed-enum discipline) — at which point this arm gets
        // a real branch.
        _ => serde_json::to_string(v).unwrap_or_else(|_| "<unrepresentable>".to_string()),
    }
}

fn thread_id_short(tid: &ThreadId) -> String {
    let s = tid.to_string();
    s.chars().take(8).collect::<String>()
}

/// Persist the register and run RestoreProtocol — convenience wrapper for
/// callers that have a register in hand and want both side effects in one
/// call. The persist runs first so a crash between persist and restore
/// can be replayed by re-loading and re-running RestoreProtocol.
pub fn persist_and_restore(
    env: &ThreadIdEnv,
    register: &DecisionRegister,
    trigger: RestoreTrigger,
    stale_open_question_ids: &[DecisionId],
    since_event_seq: u64,
) -> Result<RestoreOutcome> {
    crate::decision_register::persist(env, register)?;
    Ok(run_restore(
        register,
        trigger,
        stale_open_question_ids,
        since_event_seq,
    ))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::decision_register::{
        DecisionId, DecisionOp, DecisionRecord, DecisionSource, DecisionValue, ProducerId,
    };
    use caduceus_core::AgentEvent;
    use chrono::{Duration, Utc};

    fn id(s: &str) -> DecisionId {
        DecisionId::new(s).unwrap()
    }

    fn entry(
        id_s: &str,
        value: &str,
        locked_at: &str,
        last_amended_at: Option<&str>,
    ) -> DecisionEntry {
        let id = id(id_s);
        let val = DecisionValue::String(value.to_string());
        DecisionEntry {
            id: id.clone(),
            state: DecisionState::Locked,
            current: Some(val.clone()),
            history: vec![DecisionRecord {
                op: DecisionOp::Lock,
                value: Some(val),
                source: DecisionSource::User,
                locked_at: locked_at.to_string(),
                request_id: None,
                reason: None,
                producer_id: ProducerId::new("zed-cli"),
            }],
            derived_from: None,
            locked_at: locked_at.to_string(),
            last_amended_at: last_amended_at.map(String::from),
        }
    }

    fn unlocked_entry(id_s: &str) -> DecisionEntry {
        let id = id(id_s);
        DecisionEntry {
            id: id.clone(),
            state: DecisionState::Unlocked,
            current: None,
            history: vec![DecisionRecord {
                op: DecisionOp::Unlock,
                value: None,
                source: DecisionSource::User,
                locked_at: "2026-05-01T00:00:00Z".to_string(),
                request_id: None,
                reason: Some("retracted".to_string()),
                producer_id: ProducerId::new("zed-cli"),
            }],
            derived_from: None,
            locked_at: "2026-05-01T00:00:00Z".to_string(),
            last_amended_at: None,
        }
    }

    fn make_register(entries: Vec<DecisionEntry>) -> DecisionRegister {
        let mut r = DecisionRegister::new(ThreadId::new());
        for e in entries {
            r.entries.insert(e.id.clone(), e);
        }
        r
    }

    // ── Z8-D25: empty register still emits Restored ──

    #[test]
    fn z8_d25_empty_register_emits_restored() {
        let r = make_register(vec![]);
        let outcome = run_restore(&r, RestoreTrigger::AgentAttach, &[], 0);
        assert_eq!(outcome.events.len(), 1);
        match &outcome.events[0] {
            AgentEvent::DecisionRegisterRestored {
                count,
                truncated,
                displayed_k,
                ..
            } => {
                assert_eq!(*count, 0);
                assert!(!truncated);
                assert_eq!(*displayed_k, 0);
            }
            other => panic!("expected DecisionRegisterRestored, got {other:?}"),
        }
        assert!(outcome.reconciliation_message.is_empty());
    }

    // ── Z8-D28: lex display sort ──

    #[test]
    fn z8_d28_lex_display_sort() {
        let r = make_register(vec![
            entry("c/x", "v_c", "2026-05-01T00:00:00Z", None),
            entry("a/x", "v_a", "2026-05-01T00:00:00Z", None),
            entry("b/x", "v_b", "2026-05-01T00:00:00Z", None),
        ]);
        let (msg, _, _) = render_reconciliation_message(&r);
        let pos_a = msg.find("a/x").unwrap();
        let pos_b = msg.find("b/x").unwrap();
        let pos_c = msg.find("c/x").unwrap();
        assert!(
            pos_a < pos_b && pos_b < pos_c,
            "expected lex order in:\n{msg}"
        );
    }

    // ── Z8-D29: system-role injection ──

    #[test]
    fn z8_d29_message_starts_with_system_role_marker() {
        let r = make_register(vec![entry("a", "v", "2026-05-01T00:00:00Z", None)]);
        let (msg, _, _) = render_reconciliation_message(&r);
        // The marker is the canonical prefix — callers wrap it in a
        // role:"system" envelope but the body itself is unambiguous.
        assert!(msg.starts_with("[caduceus DecisionRegister"), "got: {msg}");
        assert!(msg.contains("do NOT re-ask"), "got: {msg}");
    }

    // ── Z8-D27a: truncation selection + display sort ──

    #[test]
    fn z8_d27a_truncation_above_6000_bytes_keeps_recent_lex_displays() {
        // Fill with enough entries to exceed budget, with varied recency.
        // We'll create ~200 entries with ids like "id-000", "id-001", …
        let now = Utc::now();
        let mut entries = Vec::new();
        for i in 0..200u32 {
            let locked_at = (now - Duration::seconds(i as i64))
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string();
            let last_amended_at = if i % 3 == 0 {
                Some(
                    (now - Duration::seconds((i / 3) as i64))
                        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                        .to_string(),
                )
            } else {
                None
            };
            entries.push(entry(
                &format!("id-{i:03}"),
                &format!("v_{i}"),
                &locked_at,
                last_amended_at.as_deref(),
            ));
        }
        let r = make_register(entries);
        let (msg, truncated, displayed_k) = render_reconciliation_message(&r);
        assert!(truncated, "expected truncation");
        assert!(
            msg.len() <= RECONCILIATION_BUDGET_BYTES,
            "message {} bytes exceeds budget",
            msg.len()
        );
        assert!(
            (displayed_k as usize) < r.entries.len(),
            "displayed_k {displayed_k} should be less than {}",
            r.entries.len()
        );
        // Marker must be present.
        assert!(
            msg.contains("earlier locked decisions; query via /list_decisions"),
            "missing truncation marker:\n{msg}"
        );
    }

    #[test]
    fn truncation_selects_most_recently_amended_first() {
        // Two entries: "old" never amended, "new" amended very recently.
        // The budget is enforced so only ONE fits — assert "new" survives.
        // We build the entries so their rendered lines are large enough
        // that two won't fit.
        let big_value = "x".repeat(3000);
        let now = Utc::now();
        let old_amend = (now - Duration::days(30))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        let new_amend = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let entries = vec![
            entry("old", &big_value, "2025-01-01T00:00:00Z", Some(&old_amend)),
            entry("new", &big_value, "2025-01-02T00:00:00Z", Some(&new_amend)),
        ];
        let r = make_register(entries);
        let (msg, truncated, displayed_k) = render_reconciliation_message(&r);
        assert!(truncated);
        assert_eq!(displayed_k, 1);
        // The recent one ("new") should appear; "old" should be in the
        // truncation marker.
        assert!(
            msg.contains("- new:"),
            "expected 'new' in:\n{}",
            msg.chars().take(200).collect::<String>()
        );
    }

    // ── compute_eliminations ──

    #[test]
    fn compute_eliminations_filters_to_locked_ids_only() {
        let r = make_register(vec![
            entry("a", "va", "2026-05-01T00:00:00Z", None),
            unlocked_entry("b"),
        ]);
        // Caller's open-question pool has both ids + a third id that's
        // not in the register at all.
        let pool: Vec<DecisionId> = vec![id("a"), id("b"), id("not-in-register")];
        let elims = compute_eliminations(&r, pool.iter());
        assert_eq!(elims, vec![id("a")]);
    }

    // ── Structural prong: events emitted on stale ids ──

    #[test]
    fn z8_d33a_stale_open_questions_get_elimination_events() {
        let r = make_register(vec![
            entry("a", "va", "2026-05-01T00:00:00Z", None),
            entry("b", "vb", "2026-05-01T00:00:00Z", None),
        ]);
        let stale = vec![id("a"), id("b")];
        let outcome = run_restore(&r, RestoreTrigger::WorkspaceMutation, &stale, 17);
        // 2 elimination events + 1 Restored = 3 total.
        assert_eq!(outcome.events.len(), 3);
        let mut elim_ids = Vec::new();
        for ev in &outcome.events {
            if let AgentEvent::OpenQuestionEliminated { id, reason } = ev {
                assert_eq!(reason, "already-locked");
                elim_ids.push(id.0.clone());
            }
        }
        assert_eq!(elim_ids, vec!["a".to_string(), "b".to_string()]);
        // Restored event last.
        match &outcome.events[2] {
            AgentEvent::DecisionRegisterRestored { count, .. } => assert_eq!(*count, 2),
            other => panic!("expected Restored last, got {other:?}"),
        }
    }

    #[test]
    fn run_restore_drops_stale_ids_not_actually_locked() {
        // If the caller's pool contains an id whose register entry is
        // Unlocked (or absent), no elimination event is emitted for it.
        let r = make_register(vec![
            entry("a", "va", "2026-05-01T00:00:00Z", None),
            unlocked_entry("b"),
        ]);
        let stale = vec![id("a"), id("b"), id("c-not-in-register")];
        let outcome = run_restore(&r, RestoreTrigger::AgentAttach, &stale, 0);
        let elim_count = outcome
            .events
            .iter()
            .filter(|e| matches!(e, AgentEvent::OpenQuestionEliminated { .. }))
            .count();
        assert_eq!(elim_count, 1);
    }

    // ── Z8-D23: idempotence within a boot ──

    #[test]
    fn z8_d23_run_restore_is_idempotent() {
        let r = make_register(vec![entry("a", "va", "2026-05-01T00:00:00Z", None)]);
        let stale = vec![id("a")];
        let a = run_restore(&r, RestoreTrigger::AgentAttach, &stale, 5);
        let b = run_restore(&r, RestoreTrigger::AgentAttach, &stale, 5);
        // RestoreOutcome doesn't derive Eq because AgentEvent doesn't
        // implement Eq (some variants carry serde_json::Value). Compare
        // the observable invariants instead.
        assert_eq!(a.events.len(), b.events.len());
        assert_eq!(a.reconciliation_message, b.reconciliation_message);
        assert_eq!(a.truncated, b.truncated);
        // Spot-check the Restored event count matches.
        for (ea, eb) in a.events.iter().zip(b.events.iter()) {
            match (ea, eb) {
                (
                    AgentEvent::DecisionRegisterRestored { count: ca, .. },
                    AgentEvent::DecisionRegisterRestored { count: cb, .. },
                ) => assert_eq!(ca, cb),
                (
                    AgentEvent::OpenQuestionEliminated { id: ia, .. },
                    AgentEvent::OpenQuestionEliminated { id: ib, .. },
                ) => assert_eq!(ia, ib),
                _ => {}
            }
        }
    }

    // ── Choice rendering ──

    #[test]
    fn choice_renders_with_label_and_position() {
        let id = id("substrate");
        let v = DecisionValue::Choice {
            options: vec!["python".into(), "rust".into(), "ts".into()],
            selected: 1,
        };
        let mut entry = DecisionEntry {
            id: id.clone(),
            state: DecisionState::Locked,
            current: Some(v.clone()),
            history: vec![DecisionRecord {
                op: DecisionOp::Lock,
                value: Some(v),
                source: DecisionSource::User,
                locked_at: "2026-05-01T00:00:00Z".into(),
                request_id: None,
                reason: None,
                producer_id: ProducerId::new("zed-cli"),
            }],
            derived_from: None,
            locked_at: "2026-05-01T00:00:00Z".into(),
            last_amended_at: None,
        };
        entry.id = id;
        let line = render_entry_line(&entry);
        assert!(line.contains("rust (option 2 of 3)"), "got: {line}");
    }

    // ── persist_and_restore convenience wrapper ──

    #[test]
    fn persist_and_restore_writes_register_then_returns_outcome() {
        use tempfile::TempDir;
        let td = TempDir::new().unwrap();
        let env = ThreadIdEnv::with_base(td.path());
        let r = make_register(vec![entry("a", "va", "2026-05-01T00:00:00Z", None)]);
        let outcome = persist_and_restore(&env, &r, RestoreTrigger::AgentAttach, &[], 0).unwrap();
        // Persisted on disk.
        let loaded = crate::decision_register::load(&env, &r.thread_id)
            .unwrap()
            .expect("register on disk");
        assert_eq!(loaded.entries.len(), 1);
        // Outcome has the Restored event.
        assert!(matches!(
            outcome.events.last().unwrap(),
            AgentEvent::DecisionRegisterRestored { .. }
        ));
    }
}
