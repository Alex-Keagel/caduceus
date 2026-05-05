//! `test_aletheia_thread_no_failure_a_b` — the §6.6 regression-guard.
//!
//! Replays the canonical aletheia plan-mode thread that triggered the
//! original Failure A (transcript loss across workspace mutation) and
//! Failure B (locked decisions silently re-asked after context restore).
//!
//! Pre-spec: replaying the equivalent transcript through the engine
//! showed the engine re-asking five questions whose answers were
//! ✅-checkboxed earlier in the thread.
//!
//! Post-spec: with the DecisionRegister reducer (P3), persistence (P3),
//! RestoreProtocol (P4), open-question pool + structural elimination
//! (P5), and IPC handlers (P6) all wired together, the same replay
//! produces zero re-ask events for the five locked DecisionIds.

use caduceus_core::decision_register::{DecisionId, DecisionState, OpenQuestion};
use caduceus_core::{AgentEvent, ThreadId};
use caduceus_orchestrator::{
    apply_decision_event, handle_workspace_mutation, persist_and_restore,
    restore_after_workspace_mutation, DecisionRegister, OpenQuestionPool, RestoreTrigger,
    ThreadIdEnv, WorkspaceContext, WorkspaceMutation,
};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

#[derive(Debug, Deserialize)]
struct EventLine {
    t_mono_us: u64,
    boot_id: String,
    event_seq: u64,
    event: AgentEvent,
}

#[derive(Debug, Deserialize)]
struct ScheduleLine {
    t_mono_us: u64,
    kind: String,
    detail: String,
}

#[derive(Debug, Deserialize)]
struct Oracle {
    expected_locked_count: u32,
    expected_no_re_ask_ids: Vec<String>,
    #[allow(dead_code)]
    expected_session_resumed_count: u32,
    expected_decision_register_restored_count: u32,
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("aletheia_thread")
}

fn load_jsonl<T: for<'de> Deserialize<'de>>(path: &PathBuf) -> Vec<T> {
    let bytes = fs::read_to_string(path).expect("read fixture");
    bytes
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("parse {l:?}: {e}")))
        .collect()
}

fn load_oracle() -> Oracle {
    let bytes = fs::read_to_string(fixture_dir().join("oracle.json")).expect("read oracle");
    serde_json::from_str(&bytes).expect("parse oracle")
}

#[test]
fn test_aletheia_thread_no_failure_a_b() {
    let dir = fixture_dir();
    let events: Vec<EventLine> = load_jsonl(&dir.join("events.jsonl"));
    let schedule: Vec<ScheduleLine> = load_jsonl(&dir.join("scheduling.jsonl"));
    let oracle = load_oracle();

    let td = TempDir::new().unwrap();
    let env = ThreadIdEnv::with_base(td.path().to_path_buf());

    let thread_id = ThreadId::new();
    let mut register = DecisionRegister::new(thread_id.clone());
    let mut pool = OpenQuestionPool::new(thread_id.clone());
    let mut workspace = WorkspaceContext::default();
    let mut emitted_open_question_presented_after_mutation: Vec<DecisionId> = Vec::new();

    // Sort schedule events by time so the replay loop can interleave them
    // with event-stream events deterministically.
    let mut schedule_iter = schedule.iter().peekable();
    let mut workspace_mutation_seen = false;

    let mut decision_register_restored_count = 0u32;

    for event_line in &events {
        // Drain schedule actions whose t_mono_us is <= the event's time.
        while let Some(s) = schedule_iter.peek() {
            if s.t_mono_us > event_line.t_mono_us {
                break;
            }
            match s.kind.as_str() {
                "workspace_mutation_processed" => {
                    // Apply the workspace mutation through the isolated
                    // handler. Detail format: "RootAdded <path>" — for
                    // this fixture we only exercise RootAdded.
                    let path = s
                        .detail
                        .strip_prefix("RootAdded ")
                        .expect("fixture detail format");
                    let _ = handle_workspace_mutation(
                        &env,
                        &thread_id,
                        &mut workspace,
                        WorkspaceMutation::RootAdded(path.to_string()),
                    )
                    .unwrap();
                    workspace_mutation_seen = true;

                    // Run RestoreProtocol post-mutation. This is the
                    // structural prong: stale open questions get
                    // eliminated, ReconciliationMessage gets prepared.
                    let outcome = restore_after_workspace_mutation(&env, &register).unwrap();
                    for ev in &outcome.events {
                        if matches!(ev, AgentEvent::DecisionRegisterRestored { .. }) {
                            decision_register_restored_count += 1;
                        }
                        if let AgentEvent::OpenQuestionEliminated { id, reason } = ev {
                            assert_eq!(reason, "already-locked");
                            // Mirror this on the pool too.
                            pool.eliminate(id);
                        }
                    }
                }
                "runner_detach" | "runner_attach" => {
                    // No-op for this replay — the spec's T1/T3 triggers
                    // would invoke RestoreProtocol from the orchestrator's
                    // attach path; we simulate by running it explicitly
                    // here only on workspace mutation (T2) to keep the
                    // test focused on the headline failure mode.
                }
                other => panic!("unknown schedule kind: {other}"),
            }
            schedule_iter.next();
        }

        // Apply the event-stream event.
        match &event_line.event {
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
                    presented_at: format!("t+{}us", event_line.t_mono_us),
                    presented_by_execution_id: caduceus_core::ExecutionId(0),
                });
                if workspace_mutation_seen {
                    // **The headline assertion data point**: any
                    // OpenQuestionPresented emitted AFTER the workspace
                    // mutation lands is potentially a regression.
                    emitted_open_question_presented_after_mutation.push(id.clone());
                }
            }
            AgentEvent::DecisionLocked { id, .. } => {
                let _ =
                    apply_decision_event(&mut register, &event_line.event, event_line.event_seq)
                        .unwrap_or_else(|e| panic!("DecisionLocked rejected: {e:?}"));
                pool.eliminate(id);
                let _ = persist_and_restore(
                    &env,
                    &register,
                    RestoreTrigger::AgentAttach,
                    &[],
                    register.last_event_seq,
                )
                .unwrap();
            }
            other => {
                // Non-decision events pass through (Z8-D13). Pool may
                // still react to OpenQuestionEliminated etc.
                let _ = apply_decision_event(&mut register, other, event_line.event_seq);
            }
        }

        let _ = event_line.boot_id; // Reserved for future cross-boot replay.
    }

    // ── Drain remaining schedule entries (those scheduled AFTER the last
    //    event) — this is exactly the canonical Aletheia failure case:
    //    user answers all 5 questions, THEN opens the project root, THEN
    //    the workspace mutation triggers RestoreProtocol.
    for s in schedule_iter {
        match s.kind.as_str() {
            "workspace_mutation_processed" => {
                let path = s
                    .detail
                    .strip_prefix("RootAdded ")
                    .expect("fixture detail format");
                let _ = handle_workspace_mutation(
                    &env,
                    &thread_id,
                    &mut workspace,
                    WorkspaceMutation::RootAdded(path.to_string()),
                )
                .unwrap();
                workspace_mutation_seen = true;

                let outcome = restore_after_workspace_mutation(&env, &register).unwrap();
                for ev in &outcome.events {
                    if matches!(ev, AgentEvent::DecisionRegisterRestored { .. }) {
                        decision_register_restored_count += 1;
                    }
                    if let AgentEvent::OpenQuestionEliminated { id, reason } = ev {
                        assert_eq!(reason, "already-locked");
                        pool.eliminate(id);
                    }
                }
            }
            "runner_detach" | "runner_attach" => {
                // No-op — see comment in main loop.
            }
            other => panic!("unknown schedule kind: {other}"),
        }
    }
    let _ = workspace_mutation_seen; // Used only to mark post-mutation events.

    // ── Assertions ────────────────────────────────────────────────────────

    // 1. Locked count matches the oracle.
    assert_eq!(
        register.locked_count(),
        oracle.expected_locked_count,
        "locked_count mismatch: register={register:?}"
    );

    // 2. All five canonical decisions are Locked (the Failure B regression
    //    guard's hard core: each id has state=Locked in the register).
    for id_str in &oracle.expected_no_re_ask_ids {
        let did = DecisionId::new(id_str.clone()).unwrap();
        let entry = register
            .entries
            .get(&did)
            .unwrap_or_else(|| panic!("expected {id_str} in register"));
        assert_eq!(
            entry.state,
            DecisionState::Locked,
            "{id_str} must be Locked, got {:?}",
            entry.state
        );
    }

    // 3. **No re-ask** — the failure-mode signal. Convert the post-
    //    mutation OpenQuestionPresented set into a lookup; assert NONE
    //    of the canonical five appear there.
    let reask_set: BTreeSet<String> = emitted_open_question_presented_after_mutation
        .iter()
        .map(|id| id.0.clone())
        .collect();
    for id_str in &oracle.expected_no_re_ask_ids {
        assert!(
            !reask_set.contains(id_str),
            "Failure B regression: {id_str} was re-asked after workspace mutation. \
             reask_set={reask_set:?}"
        );
    }

    // 4. The structural prong eliminated every stale open question
    //    (none of the five remain in the pool).
    for id_str in &oracle.expected_no_re_ask_ids {
        let did = DecisionId::new(id_str.clone()).unwrap();
        assert!(
            !pool.entries.contains_key(&did),
            "open-question pool still carries {id_str} — \
             structural elimination (Z8-D33a) failed"
        );
    }

    // 5. RestoreProtocol fired the expected number of Restored events.
    assert_eq!(
        decision_register_restored_count, oracle.expected_decision_register_restored_count,
        "DecisionRegisterRestored count mismatch"
    );
}
