//! Tests for issues #259–#261.
//!
//! Relocated from `lib.rs` (ST-B1 Wave 3) to keep the main module
//! surface manageable. The `#[cfg(test)] mod feature_tests_259_261;`
//! declaration in `lib.rs` brings these tests in unchanged.

use super::*;

// ── P3.2 — request_logprobs builder/accessor ─────────────────────────

#[test]
fn p3_2_request_logprobs_default_off() {
    use caduceus_providers::mock::MockLlmAdapter;
    let provider = std::sync::Arc::new(MockLlmAdapter::new(vec![]));
    let h = AgentHarness::new(provider, ToolRegistry::new(), 4096, "sys");
    assert!(!h.request_logprobs());
}

#[test]
fn p3_2_request_logprobs_builder_toggles() {
    use caduceus_providers::mock::MockLlmAdapter;
    let provider = std::sync::Arc::new(MockLlmAdapter::new(vec![]));
    let h = AgentHarness::new(provider, ToolRegistry::new(), 4096, "sys")
        .with_request_logprobs(true);
    assert!(h.request_logprobs());
    let h = h.with_request_logprobs(false);
    assert!(!h.request_logprobs());
}

// ── #259 AgentScaffolder ──────────────────────────────────────────────────

#[test]
fn agent_available_tool_sets_returns_three() {
    let sets = AgentScaffolder::available_tool_sets();
    assert_eq!(sets.len(), 3);
    let names: Vec<&str> = sets.iter().map(|(n, _)| *n).collect();
    assert!(names.contains(&"read-only"));
    assert!(names.contains(&"standard"));
    assert!(names.contains(&"full"));
}

#[test]
fn agent_tool_set_contents() {
    let sets = AgentScaffolder::available_tool_sets();
    let standard = sets.iter().find(|(n, _)| *n == "standard").unwrap();
    assert!(standard.1.contains(&"shell"));
    assert!(standard.1.contains(&"edit"));
    let full = sets.iter().find(|(n, _)| *n == "full").unwrap();
    assert!(full.1.contains(&"browser"));
    assert!(full.1.contains(&"mcp"));
}

#[test]
fn agent_suggest_triggers_review() {
    let triggers = AgentScaffolder::suggest_triggers("code review tool");
    assert!(!triggers.is_empty());
    assert!(triggers.iter().any(|t| t.contains("review")));
}

#[test]
fn agent_suggest_triggers_fallback() {
    let triggers = AgentScaffolder::suggest_triggers("xyzzy obscure thing");
    assert!(!triggers.is_empty());
}

#[test]
fn agent_generate_contains_required_sections() {
    let config = AgentScaffoldConfig {
        name: "test-agent".to_string(),
        description: "A test agent".to_string(),
        tools: vec!["read".into(), "search".into()],
        model: None,
        trigger_phrases: vec!["test this".to_string()],
        persona: "You are a senior tester.".to_string(),
        instructions: vec!["Step one.".to_string(), "Step two.".to_string()],
    };
    let out = AgentScaffolder::generate(&config);
    assert!(out.contains("---"));
    assert!(out.contains("name: test-agent"));
    assert!(out.contains("tools: ['read', 'search']"));
    assert!(out.contains("# Test Agent"));
    assert!(out.contains("You are a senior tester."));
    assert!(out.contains("## When Invoked"));
    assert!(out.contains("1. Step one."));
    assert!(out.contains("2. Step two."));
    assert!(out.contains("## Quality Checklist"));
}

#[test]
fn agent_generate_with_model() {
    let config = AgentScaffoldConfig {
        name: "my-agent".to_string(),
        description: "desc".to_string(),
        tools: vec!["shell".into()],
        model: Some("claude-opus-4".to_string()),
        trigger_phrases: vec![],
        persona: "You are an expert.".to_string(),
        instructions: vec![],
    };
    let out = AgentScaffolder::generate(&config);
    assert!(out.contains("model: claude-opus-4"));
}

#[test]
fn agent_generate_no_model_omits_model_line() {
    let config = AgentScaffoldConfig {
        name: "my-agent".to_string(),
        description: "desc".to_string(),
        tools: vec![],
        model: None,
        trigger_phrases: vec![],
        persona: "Expert.".to_string(),
        instructions: vec![],
    };
    let out = AgentScaffolder::generate(&config);
    assert!(!out.contains("model:"));
}

#[test]
fn agent_quick_generate_valid_output() {
    let out = AgentScaffolder::quick_generate("my-agent", "reviews pull requests");
    assert!(out.contains("name: my-agent"));
    assert!(out.contains("# My Agent"));
    assert!(out.contains("reviews pull requests"));
    assert!(out.contains("## When Invoked"));
}

#[test]
fn agent_title_case_kebab() {
    assert_eq!(to_title_case("my-agent-name"), "My Agent Name");
    assert_eq!(to_title_case("single"), "Single");
    assert_eq!(to_title_case("snake_case"), "Snake Case");
}

// ── #260 SkillScaffolder ──────────────────────────────────────────────────

#[test]
fn skill_generate_contains_required_sections() {
    let config = SkillScaffoldConfig {
        name: "my-skill".to_string(),
        description: "Does something useful".to_string(),
        trigger_phrases: vec!["do the thing".to_string(), "help me".to_string()],
        steps: vec!["First step.".to_string(), "Second step.".to_string()],
        examples: vec![("input text".to_string(), "output text".to_string())],
        tools_needed: vec!["bash".to_string()],
    };
    let out = SkillScaffolder::generate(&config);
    assert!(out.contains("name: my-skill"));
    assert!(out.contains("# My Skill"));
    assert!(out.contains("## When to Use"));
    assert!(out.contains("- do the thing"));
    assert!(out.contains("## Steps"));
    assert!(out.contains("1. First step."));
    assert!(out.contains("2. Second step."));
    assert!(out.contains("## Tools Required"));
    assert!(out.contains("- bash"));
    assert!(out.contains("## Examples"));
    assert!(out.contains("**Input:** input text"));
    assert!(out.contains("**Output:** output text"));
}

#[test]
fn skill_generate_no_tools_no_tools_section() {
    let config = SkillScaffoldConfig {
        name: "minimal-skill".to_string(),
        description: "Minimal".to_string(),
        trigger_phrases: vec!["trigger".to_string()],
        steps: vec!["Do it.".to_string()],
        examples: vec![],
        tools_needed: vec![],
    };
    let out = SkillScaffolder::generate(&config);
    assert!(!out.contains("## Tools Required"));
    assert!(!out.contains("## Examples"));
}

#[test]
fn skill_generate_description_has_triggers_inline() {
    let config = SkillScaffoldConfig {
        name: "s".to_string(),
        description: "My skill".to_string(),
        trigger_phrases: vec!["phrase a".to_string(), "phrase b".to_string()],
        steps: vec![],
        examples: vec![],
        tools_needed: vec![],
    };
    let out = SkillScaffolder::generate(&config);
    assert!(out.contains("phrase a"));
    assert!(out.contains("phrase b"));
}

#[test]
fn skill_quick_generate_valid() {
    let out = SkillScaffolder::quick_generate("pdf-reader", "reads PDF files");
    assert!(out.contains("name: pdf-reader"));
    assert!(out.contains("# Pdf Reader"));
    assert!(out.contains("## Steps"));
}

#[test]
fn skill_from_conversation_extracts_steps() {
    let msgs = vec![
        "First, read the file.".to_string(),
        "Then, parse the content.".to_string(),
        "Finally, return the result.".to_string(),
    ];
    let out = SkillScaffolder::from_conversation(&msgs);
    assert!(out.contains("## Steps"));
    assert!(out.contains("First, read the file."));
    assert!(out.contains("Then, parse the content."));
    assert!(out.contains("Finally, return the result."));
}

#[test]
fn skill_from_conversation_empty_fallback() {
    let out = SkillScaffolder::from_conversation(&[]);
    assert!(out.contains("## Steps"));
    assert!(out.contains("Review the conversation context."));
}

// ── #261 InstructionsScaffolder ───────────────────────────────────────────

#[test]
fn instructions_generate_contains_all_sections() {
    let config = InstructionsConfig {
        project_name: "my-project".to_string(),
        project_type: "Rust".to_string(),
        languages: vec!["Rust".to_string()],
        build_command: "cargo build".to_string(),
        test_command: "cargo test".to_string(),
        lint_command: "cargo clippy".to_string(),
        architecture_notes: vec!["Single crate.".to_string()],
        coding_standards: vec!["Use rustfmt.".to_string()],
        important_files: vec!["src/lib.rs — Library root".to_string()],
        custom_rules: vec!["No unsafe code.".to_string()],
    };
    let out = InstructionsScaffolder::generate(&config);
    assert!(out.contains("# Project Instructions"));
    assert!(out.contains("- Name: my-project"));
    assert!(out.contains("- Type: Rust"));
    assert!(out.contains("- Languages: Rust"));
    assert!(out.contains("- Build: `cargo build`"));
    assert!(out.contains("- Test: `cargo test`"));
    assert!(out.contains("- Lint: `cargo clippy`"));
    assert!(out.contains("## Architecture"));
    assert!(out.contains("- Single crate."));
    assert!(out.contains("## Coding Standards"));
    assert!(out.contains("- Use rustfmt."));
    assert!(out.contains("## Important Files"));
    assert!(out.contains("`src/lib.rs — Library root`"));
    assert!(out.contains("## Rules"));
    assert!(out.contains("- No unsafe code."));
}

#[test]
fn instructions_generate_defaults_when_empty() {
    let config = InstructionsConfig {
        project_name: "p".to_string(),
        project_type: "".to_string(),
        languages: vec![],
        build_command: "build".to_string(),
        test_command: "test".to_string(),
        lint_command: "lint".to_string(),
        architecture_notes: vec![],
        coding_standards: vec![],
        important_files: vec![],
        custom_rules: vec![],
    };
    let out = InstructionsScaffolder::generate(&config);
    assert!(out.contains("No architecture notes provided."));
    assert!(out.contains("Follow language idioms"));
    assert!(out.contains("No important files specified."));
    assert!(out.contains("Always run tests before committing."));
}

#[test]
fn instructions_auto_detect_rust() {
    let cfg =
        InstructionsScaffolder::auto_detect("/home/user/myapp", &["Rust".to_string()], 42);
    assert_eq!(cfg.project_name, "myapp");
    assert_eq!(cfg.project_type, "Rust");
    assert!(cfg.build_command.contains("cargo"));
    assert!(cfg.test_command.contains("cargo test"));
    assert!(cfg.architecture_notes.iter().any(|n| n.contains("42")));
}

#[test]
fn instructions_auto_detect_python() {
    let cfg = InstructionsScaffolder::auto_detect("/proj", &["Python".to_string()], 10);
    assert_eq!(cfg.project_type, "Python");
    assert!(cfg.test_command.contains("pytest"));
}

#[test]
fn instructions_auto_detect_typescript() {
    let cfg = InstructionsScaffolder::auto_detect("/proj", &["TypeScript".to_string()], 5);
    assert_eq!(cfg.project_type, "TypeScript");
    assert!(cfg.build_command.contains("npm"));
}

#[test]
fn instructions_auto_detect_rust_and_ts() {
    let cfg = InstructionsScaffolder::auto_detect(
        "/proj",
        &["Rust".to_string(), "TypeScript".to_string()],
        100,
    );
    assert_eq!(cfg.project_type, "Rust + TypeScript");
    assert!(cfg.build_command.contains("cargo"));
}

#[test]
fn instructions_template_rust() {
    let out = InstructionsScaffolder::template_for("rust");
    assert!(out.contains("cargo build"));
    assert!(out.contains("cargo clippy"));
    assert!(out.contains("rustfmt"));
}

#[test]
fn instructions_template_python() {
    let out = InstructionsScaffolder::template_for("python");
    assert!(out.contains("pytest"));
    assert!(out.contains("ruff"));
    assert!(out.contains("mypy"));
}

#[test]
fn instructions_template_typescript() {
    let out = InstructionsScaffolder::template_for("typescript");
    assert!(out.contains("vitest"));
    assert!(out.contains("eslint"));
    assert!(out.contains("prettier"));
}

#[test]
fn instructions_template_react() {
    let out = InstructionsScaffolder::template_for("react");
    assert!(out.contains("React"));
    assert!(out.contains("Testing Library"));
}

#[test]
fn instructions_template_fullstack() {
    let out = InstructionsScaffolder::template_for("fullstack");
    assert!(out.contains("cargo"));
    assert!(out.contains("npm"));
    assert!(out.contains("openapi"));
}

#[test]
fn instructions_template_unknown_fallback() {
    let out = InstructionsScaffolder::template_for("cobol");
    assert!(out.contains("cobol"));
    assert!(out.contains("make build"));
}

// ── P9.1: AgentHarness compaction-telemetry wiring ──────────────────────

#[test]
fn mark_compaction_re_ask_returns_false_without_telemetry_attached() {
    let h = make_test_harness();
    // No telemetry attached → no-op, must return false (not panic).
    assert!(!h.mark_compaction_re_ask(0, true));
}

#[test]
fn mark_compaction_re_ask_returns_false_when_no_matching_event() {
    use crate::compaction_telemetry::CompactionTelemetry;
    use std::sync::{Arc, Mutex};

    let telem = Arc::new(Mutex::new(CompactionTelemetry::default()));
    let h = make_test_harness().with_compaction_telemetry(Arc::clone(&telem));
    assert!(!h.mark_compaction_re_ask(99, true));
}

#[test]
fn mark_compaction_re_ask_updates_matching_event() {
    use crate::compaction_telemetry::{CompactionEvent, CompactionTelemetry};
    use std::sync::{Arc, Mutex};

    let telem = Arc::new(Mutex::new(CompactionTelemetry::default()));
    telem.lock().unwrap().record(CompactionEvent {
        strategy: "truncate-oldest".into(),
        tokens_before: 100,
        tokens_after: 50,
        messages_before: 10,
        messages_after: 5,
        turn_index: 3,
        at_secs: 0,
        downstream_re_ask: None,
    });
    let h = make_test_harness().with_compaction_telemetry(Arc::clone(&telem));
    assert!(h.mark_compaction_re_ask(3, true));
    // Event was actually mutated.
    let jsonl = telem.lock().unwrap().to_jsonl();
    assert!(jsonl.contains("\"downstream_re_ask\":true"));
}

#[test]
fn compaction_telemetry_accessor_returns_attached_collector() {
    use crate::compaction_telemetry::CompactionTelemetry;
    use std::sync::{Arc, Mutex};

    let telem = Arc::new(Mutex::new(CompactionTelemetry::default()));
    let h = make_test_harness().with_compaction_telemetry(Arc::clone(&telem));
    assert!(h.compaction_telemetry().is_some());
    // And by Arc identity.
    assert!(Arc::ptr_eq(h.compaction_telemetry().unwrap(), &telem));
}

fn make_test_harness() -> AgentHarness {
    let provider = caduceus_providers::mock::MockLlmAdapter::new(vec![]);
    AgentHarness::new(Arc::new(provider), ToolRegistry::new(), 8000, "test")
}

fn make_test_state(model: &str) -> caduceus_core::SessionState {
    caduceus_core::SessionState::new(
        std::path::PathBuf::from("/tmp/p9_3"),
        caduceus_core::ProviderId::new("test"),
        caduceus_core::ModelId::new(model),
    )
}

// ── P9.4: CheckpointStore wiring + revert IPC ──────────────────────

#[test]
fn p9_4_with_checkpoint_store_attaches_and_accessor_returns_it() {
    use crate::checkpoint::CheckpointStore;
    use std::sync::{Arc, Mutex};

    let store = Arc::new(Mutex::new(CheckpointStore::default()));
    let h = make_test_harness().with_checkpoint_store(Arc::clone(&store));
    assert!(h.checkpoint_store().is_some());
    assert!(Arc::ptr_eq(h.checkpoint_store().unwrap(), &store));
}

#[tokio::test]
async fn p9_4_revert_checkpoint_returns_snapshots_and_emits_event() {
    use crate::checkpoint::CheckpointStore;
    use caduceus_core::AgentEvent;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    let store = Arc::new(Mutex::new(CheckpointStore::default()));
    let id = {
        let mut g = store.lock().unwrap();
        let id = g.begin_batch(1, "edit_file", 1000);
        g.record_edit(
            id,
            std::path::PathBuf::from("/tmp/p9_4.txt"),
            Some("orig".into()),
        )
        .unwrap();
        g.commit(id).unwrap();
        id
    };

    let (tx, mut rx) = mpsc::channel(16);
    let emitter = AgentEventEmitter::new(tx);
    let h = make_test_harness()
        .with_checkpoint_store(Arc::clone(&store))
        .with_emitter(emitter);

    let snaps = h.revert_checkpoint(id).await.expect("revert ok");
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].before.as_deref(), Some("orig"));

    // Event must be emitted with ok=true and matching id.
    let mut found = false;
    while let Ok(ev) = rx.try_recv() {
        if let AgentEvent::CheckpointReverted {
            id: rid, ok, files, ..
        } = ev
        {
            if rid == id.raw() && ok && files == 1 {
                found = true;
                break;
            }
        }
    }
    assert!(found, "CheckpointReverted(ok=true) not emitted");
}

#[tokio::test]
async fn p9_4_revert_checkpoint_no_store_emits_failure_event() {
    use caduceus_core::AgentEvent;
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel(16);
    let emitter = AgentEventEmitter::new(tx);
    let h = make_test_harness().with_emitter(emitter);

    let res = h
        .revert_checkpoint(crate::checkpoint::CheckpointId(42))
        .await;
    assert!(res.is_err());

    let mut found = false;
    while let Ok(ev) = rx.try_recv() {
        if let AgentEvent::CheckpointReverted { id, ok, reason, .. } = ev {
            if id == 42 && !ok && reason.contains("no checkpoint store") {
                found = true;
                break;
            }
        }
    }
    assert!(
        found,
        "CheckpointReverted(ok=false) not emitted for missing store"
    );
}

#[tokio::test]
async fn p9_4_revert_checkpoint_unknown_id_returns_err_and_emits_event() {
    use crate::checkpoint::{CheckpointError, CheckpointId, CheckpointStore};
    use caduceus_core::AgentEvent;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    let store = Arc::new(Mutex::new(CheckpointStore::default()));
    let (tx, mut rx) = mpsc::channel(16);
    let emitter = AgentEventEmitter::new(tx);
    let h = make_test_harness()
        .with_checkpoint_store(store)
        .with_emitter(emitter);

    let err = h.revert_checkpoint(CheckpointId(999)).await.unwrap_err();
    assert!(matches!(err, CheckpointError::Unknown(_)));

    let mut found = false;
    while let Ok(ev) = rx.try_recv() {
        if let AgentEvent::CheckpointReverted { id, ok, .. } = ev {
            if id == 999 && !ok {
                found = true;
                break;
            }
        }
    }
    assert!(found);
}

#[tokio::test]
async fn p9_4_revert_checkpoint_idempotent_revert_rejected() {
    use crate::checkpoint::CheckpointStore;
    use std::sync::{Arc, Mutex};

    let store = Arc::new(Mutex::new(CheckpointStore::default()));
    let id = {
        let mut g = store.lock().unwrap();
        let id = g.begin_batch(1, "edit_file", 1000);
        g.commit(id).unwrap();
        id
    };
    let h = make_test_harness().with_checkpoint_store(Arc::clone(&store));

    // First revert succeeds.
    h.revert_checkpoint(id).await.unwrap();
    // Second revert is rejected (closed-fail).
    assert!(h.revert_checkpoint(id).await.is_err());
}

// ── P9.6: TranscriptStore folding wiring ──────────────────────────

#[test]
fn p9_6_with_transcript_store_attaches_and_accessor_returns_it() {
    use crate::context_fold::TranscriptStore;
    use std::sync::{Arc, Mutex};

    let store = Arc::new(Mutex::new(TranscriptStore::default()));
    let h = make_test_harness().with_transcript_store(Arc::clone(&store));
    assert!(h.transcript_store().is_some());
    assert!(Arc::ptr_eq(h.transcript_store().unwrap(), &store));
}

#[test]
fn p9_6_fold_tool_result_passthrough_when_no_store() {
    let h = make_test_harness();
    let big = "x".repeat(50_000);
    let out = h.fold_tool_result("shell", big.clone());
    assert_eq!(out, big, "no store ⇒ verbatim passthrough");
}

#[test]
fn p9_6_fold_tool_result_passthrough_when_under_threshold() {
    use crate::context_fold::TranscriptStore;
    use std::sync::{Arc, Mutex};

    let store = Arc::new(Mutex::new(TranscriptStore::default()));
    let h = make_test_harness().with_transcript_store(Arc::clone(&store));
    let small = "small output".to_string();
    let out = h.fold_tool_result("shell", small.clone());
    assert_eq!(out, small);
    // Store should be empty.
    assert!(store
        .lock()
        .unwrap()
        .expand(crate::context_fold::TranscriptId(1))
        .is_err());
}

#[test]
fn p9_6_fold_tool_result_replaces_with_json_when_over_threshold() {
    use crate::context_fold::{TranscriptStore, DEFAULT_FOLD_THRESHOLD_CHARS};
    use std::sync::{Arc, Mutex};

    let store = Arc::new(Mutex::new(TranscriptStore::default()));
    let h = make_test_harness().with_transcript_store(Arc::clone(&store));
    let big = "X".repeat(DEFAULT_FOLD_THRESHOLD_CHARS + 100);
    let out = h.fold_tool_result("subagent_security", big.clone());

    assert_ne!(out, big, "above threshold should be folded");
    // The folded output is JSON containing the subagent name.
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("folded output is JSON");
    assert_eq!(parsed["subagent"], "subagent_security");
    assert!(parsed["id"].is_number() || parsed["id"].is_object());
    assert_eq!(parsed["original_chars"], big.len() as u64);
}

#[test]
fn p9_6_expand_transcript_returns_original_after_fold() {
    use crate::context_fold::{TranscriptStore, DEFAULT_FOLD_THRESHOLD_CHARS};
    use std::sync::{Arc, Mutex};

    let store = Arc::new(Mutex::new(TranscriptStore::default()));
    let h = make_test_harness().with_transcript_store(Arc::clone(&store));
    let big = "Y".repeat(DEFAULT_FOLD_THRESHOLD_CHARS + 50);

    let folded = h.fold_tool_result("shell", big.clone());
    let parsed: serde_json::Value = serde_json::from_str(&folded).unwrap();
    let raw_id = parsed["id"]["0"]
        .as_u64()
        .or_else(|| parsed["id"].as_u64())
        .expect("id resolvable");

    let expanded = h
        .expand_transcript(crate::context_fold::TranscriptId(raw_id))
        .expect("expand ok");
    assert_eq!(expanded, big);
}

// ── P9.5: MemoryBlocks mirror wiring ──────────────────────────────

#[test]
fn p9_5_with_memory_blocks_attaches_and_accessor_returns_it() {
    use crate::memory_blocks::MemoryBlocks;
    use std::sync::{Arc, Mutex};

    let mb = Arc::new(Mutex::new(MemoryBlocks::default()));
    let h = make_test_harness().with_memory_blocks(Arc::clone(&mb));
    assert!(h.memory_blocks().is_some());
    assert!(Arc::ptr_eq(h.memory_blocks().unwrap(), &mb));
}

#[test]
fn p9_5_sync_memory_blocks_returns_none_when_no_blocks_attached() {
    let h = make_test_harness();
    let report = h.sync_memory_blocks("persona", "ctx", &[]);
    assert!(report.is_none());
}

#[test]
fn p9_5_sync_memory_blocks_mirrors_persona_project_and_history() {
    use crate::memory_blocks::MemoryBlocks;
    use std::sync::{Arc, Mutex};

    let mb = Arc::new(Mutex::new(MemoryBlocks::default()));
    let h = make_test_harness().with_memory_blocks(Arc::clone(&mb));

    let msgs = vec![
        Arc::new(caduceus_providers::Message::user("hello")),
        Arc::new(caduceus_providers::Message::assistant("hi there")),
    ];
    let report = h
        .sync_memory_blocks("you are caduceus", "open: src/lib.rs", &msgs)
        .expect("blocks attached");

    let g = mb.lock().unwrap();
    assert_eq!(g.persona, "you are caduceus");
    assert_eq!(g.project_context, "open: src/lib.rs");
    assert_eq!(g.working_history.len(), 2);
    assert_eq!(g.working_history[0].role, "user");
    assert_eq!(g.working_history[0].text, "hello");
    assert_eq!(g.working_history[1].role, "assistant");
    // Compaction is idempotent on this small input.
    assert_eq!(report.working_evicted, 0);
}

#[test]
fn p9_5_sync_memory_blocks_assigns_pair_id_for_tool_calls_and_results() {
    use crate::memory_blocks::MemoryBlocks;
    use std::sync::{Arc, Mutex};

    let mb = Arc::new(Mutex::new(MemoryBlocks::default()));
    let h = make_test_harness().with_memory_blocks(Arc::clone(&mb));

    let mut assistant = caduceus_providers::Message::assistant("calling tool");
    assistant.tool_calls.push(caduceus_core::ToolUse {
        id: "call_abc".into(),
        name: "edit_file".into(),
        input: serde_json::json!({}),
    });
    let tool_msg = caduceus_providers::Message {
        role: "tool".into(),
        content: "OK".into(),
        content_blocks: None,
        tool_calls: vec![],
        tool_result: Some(
            caduceus_core::ToolResult::success("OK").with_tool_use_id("call_abc"),
        ),
        cache_breakpoint: false,
    };

    h.sync_memory_blocks("p", "c", &[Arc::new(assistant), Arc::new(tool_msg)])
        .expect("blocks attached");

    let g = mb.lock().unwrap();
    assert_eq!(g.working_history.len(), 2);
    assert_eq!(g.working_history[0].pair_id.as_deref(), Some("call_abc"));
    assert_eq!(g.working_history[1].pair_id.as_deref(), Some("call_abc"));
}

#[test]
fn p9_5_sync_memory_blocks_compacts_when_over_budget() {
    use crate::memory_blocks::{BlockLimits, MemoryBlocks};
    use std::sync::{Arc, Mutex};

    let mb = Arc::new(Mutex::new(MemoryBlocks::new(BlockLimits {
        persona_chars: 2_000,
        project_context_tokens: 8_000,
        working_history_tokens: 4, // tiny budget — forces eviction
        archival_summary_tokens: 16_000,
    })));
    let h = make_test_harness().with_memory_blocks(Arc::clone(&mb));

    // Each message is ~6 chars => ~2 tokens. 3 messages => ~6 tokens > 4.
    let msgs = vec![
        Arc::new(caduceus_providers::Message::user("aaaaaa")),
        Arc::new(caduceus_providers::Message::user("bbbbbb")),
        Arc::new(caduceus_providers::Message::user("cccccc")),
    ];
    let report = h
        .sync_memory_blocks("p", "c", &msgs)
        .expect("blocks attached");
    assert!(report.working_evicted >= 1, "expected eviction to fire");

    let g = mb.lock().unwrap();
    assert!(g.working_tokens() <= g.limits.working_history_tokens);
}

// ── P9.3: per-model TokenBudget wiring ─────────────────────────────

#[tokio::test]
async fn p9_3_apply_model_budget_mutates_session_to_per_model_spec() {
    use caduceus_core::TokenBudget;

    let h = make_test_harness();
    let mut state = make_test_state("claude-opus-4.6");
    assert_eq!(
        state.token_budget.context_limit,
        TokenBudget::DEFAULT_CONTEXT_LIMIT
    );

    let changed = h
        .apply_model_budget_for_turn(&mut state, "claude-opus-4.6")
        .await;
    let (ctx, reserved) = TokenBudget::model_spec("claude-opus-4.6");
    assert!(changed);
    assert_eq!(state.token_budget.context_limit, ctx);
    assert_eq!(state.token_budget.reserved_output, reserved);
}

#[tokio::test]
async fn p9_3_apply_model_budget_preserves_used_counters() {
    let h = make_test_harness();
    let mut state = make_test_state("gpt-4o");
    state.token_budget.used_input = 1234;
    state.token_budget.used_output = 567;

    let _ = h.apply_model_budget_for_turn(&mut state, "gpt-4o").await;
    assert_eq!(state.token_budget.used_input, 1234);
    assert_eq!(state.token_budget.used_output, 567);
}

#[tokio::test]
async fn p9_3_apply_model_budget_no_op_when_already_correct() {
    use caduceus_core::TokenBudget;

    let h = make_test_harness();
    let mut state = make_test_state("claude-opus-4.6");
    let (ctx, reserved) = TokenBudget::model_spec("claude-opus-4.6");
    state.token_budget.context_limit = ctx;
    state.token_budget.reserved_output = reserved;

    let changed = h
        .apply_model_budget_for_turn(&mut state, "claude-opus-4.6")
        .await;
    assert!(!changed);
}

#[tokio::test]
async fn p9_3_apply_model_budget_emits_budget_updated_event() {
    use caduceus_core::AgentEvent;
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel(16);
    let emitter = AgentEventEmitter::new(tx);
    let h = make_test_harness().with_emitter(emitter);

    let mut state = make_test_state("claude-opus-4.6");
    let _ = h
        .apply_model_budget_for_turn(&mut state, "claude-opus-4.6")
        .await;

    let mut found = false;
    while let Ok(ev) = rx.try_recv() {
        if matches!(
            ev,
            AgentEvent::BudgetUpdated { ref model_id, .. } if model_id == "claude-opus-4.6"
        ) {
            found = true;
            break;
        }
    }
    assert!(found, "BudgetUpdated event not emitted");
}

#[tokio::test]
async fn p9_3_apply_model_budget_unknown_model_uses_defaults() {
    use caduceus_core::TokenBudget;

    let h = make_test_harness();
    let mut state = make_test_state("totally-fake-model-xyz");
    state.token_budget.context_limit = 999;
    state.token_budget.reserved_output = 99;

    let changed = h
        .apply_model_budget_for_turn(&mut state, "totally-fake-model-xyz")
        .await;
    assert!(changed);
    assert_eq!(
        state.token_budget.context_limit,
        TokenBudget::DEFAULT_CONTEXT_LIMIT
    );
    assert_eq!(
        state.token_budget.reserved_output,
        TokenBudget::DEFAULT_RESERVED_OUTPUT
    );
}

// ── P11.2 — per-tool timeouts ───────────────────────────────────────────
//
// A tool that sleeps for `delay`. Returning success after `delay` so we
// can prove the override actually shortens the wall-clock budget vs. the
// global default (which is far longer than any test is willing to wait).
struct SlowTool {
    name: &'static str,
    delay: std::time::Duration,
}

#[async_trait::async_trait]
impl caduceus_tools::Tool for SlowTool {
    fn spec(&self) -> caduceus_core::ToolSpec {
        caduceus_core::ToolSpec {
            name: self.name.into(),
            description: "sleeps".into(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
            required_capability: None,
        }
    }
    async fn call(
        &self,
        _input: serde_json::Value,
    ) -> caduceus_core::Result<caduceus_core::ToolResult> {
        tokio::time::sleep(self.delay).await;
        Ok(caduceus_core::ToolResult::success("done"))
    }
}

fn p11_2_chat_text(text: &str) -> caduceus_providers::ChatResponse {
    caduceus_providers::ChatResponse {
        content: text.to_string(),
        input_tokens: 5,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        stop_reason: caduceus_providers::StopReason::EndTurn,
        tool_calls: vec![],
        logprobs: None,
        thinking: String::new(),
    }
}

fn p11_2_chat_tool(tool_name: &str, id: &str) -> caduceus_providers::ChatResponse {
    caduceus_providers::ChatResponse {
        content: format!("calling {tool_name}"),
        input_tokens: 5,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        stop_reason: caduceus_providers::StopReason::ToolUse,
        tool_calls: vec![caduceus_core::ToolUse {
            id: id.into(),
            name: tool_name.into(),
            input: serde_json::json!({}),
        }],
        logprobs: None,
        thinking: String::new(),
    }
}

fn p11_2_session() -> caduceus_core::SessionState {
    caduceus_core::SessionState::new(
        std::path::PathBuf::from("/tmp/p11_2"),
        caduceus_core::ProviderId::new("test"),
        caduceus_core::ModelId::new("test-model"),
    )
}

async fn p11_2_drain_timed_out(
    emitter: AgentEventEmitter,
    mut rx: tokio::sync::mpsc::Receiver<caduceus_core::AgentEvent>,
) -> Vec<(String, u64, u64)> {
    drop(emitter);
    // Give any in-flight emit() a moment.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let caduceus_core::AgentEvent::ToolTimedOut {
            tool,
            timeout_secs,
            elapsed_ms,
        } = ev
        {
            out.push((tool, timeout_secs, elapsed_ms));
        }
    }
    out
}

#[tokio::test]
async fn p11_2_with_tool_timeout_for_overrides_global() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_a", "tc1"),
        p11_2_chat_text("after timeout"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_a",
        delay: std::time::Duration::from_millis(200),
    }));
    let harness = AgentHarness::new(adapter, registry, 4096, "system")
        .with_tool_timeout_for("slow_a", std::time::Duration::from_millis(50));

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let result = harness.run(&mut state, &mut history, "go").await.unwrap();
    assert_eq!(result, "after timeout");
    let timed_out = history
        .messages()
        .iter()
        .filter_map(|m| m.tool_result.as_ref())
        .any(|tr| tr.is_error && tr.content.contains("timed out"));
    assert!(
        timed_out,
        "expected a timeout-marked tool_result in history"
    );
}

#[tokio::test]
async fn p11_2_tool_timeout_falls_back_to_global_when_no_override() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_b", "tc1"),
        p11_2_chat_text("after fallback"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_b",
        delay: std::time::Duration::from_millis(200),
    }));
    let harness = AgentHarness::new(adapter, registry, 4096, "system")
        .with_tool_timeout(std::time::Duration::from_millis(50));

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let result = harness.run(&mut state, &mut history, "go").await.unwrap();
    assert_eq!(result, "after fallback");
}

#[tokio::test]
async fn p11_2_tool_timed_out_event_emitted_on_timeout() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_c", "tc1"),
        p11_2_chat_text("done"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_c",
        delay: std::time::Duration::from_millis(200),
    }));
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(64);
    let emitter = AgentEventEmitter::new(event_tx);
    let harness = AgentHarness::new(adapter, registry, 4096, "system")
        .with_tool_timeout_for("slow_c", std::time::Duration::from_millis(50))
        .with_emitter(emitter.clone());

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let _ = harness.run(&mut state, &mut history, "go").await.unwrap();

    let events = p11_2_drain_timed_out(emitter, event_rx).await;
    assert_eq!(events.len(), 1, "exactly one ToolTimedOut event expected");
}

#[tokio::test]
async fn p11_2_tool_timed_out_event_carries_correct_tool_name_and_budget() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_d", "tc1"),
        p11_2_chat_text("done"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_d",
        delay: std::time::Duration::from_millis(2000),
    }));
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(64);
    let emitter = AgentEventEmitter::new(event_tx);
    let harness = AgentHarness::new(adapter, registry, 4096, "system")
        .with_tool_timeout_for("slow_d", std::time::Duration::from_secs(1))
        .with_emitter(emitter.clone());

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let _ = harness.run(&mut state, &mut history, "go").await.unwrap();

    let events = p11_2_drain_timed_out(emitter, event_rx).await;
    assert_eq!(events.len(), 1);
    let (tool, budget_secs, _elapsed) = &events[0];
    assert_eq!(tool, "slow_d");
    assert_eq!(
        *budget_secs, 1,
        "budget should reflect the per-tool override"
    );
}

#[tokio::test]
async fn p11_2_per_tool_override_does_not_affect_other_tools() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_fast_e", "tc1"),
        p11_2_chat_tool("slow_e", "tc2"),
        p11_2_chat_text("both handled"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_fast_e",
        delay: std::time::Duration::from_millis(50),
    }));
    registry.register(Arc::new(SlowTool {
        name: "slow_e",
        delay: std::time::Duration::from_millis(200),
    }));
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(64);
    let emitter = AgentEventEmitter::new(event_tx);
    let harness = AgentHarness::new(adapter, registry, 4096, "system")
        .with_tool_timeout_for("slow_e", std::time::Duration::from_millis(20))
        .with_emitter(emitter.clone());

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let _ = harness.run(&mut state, &mut history, "go").await.unwrap();

    let events = p11_2_drain_timed_out(emitter, event_rx).await;
    assert!(
        events.iter().all(|(name, _, _)| name == "slow_e"),
        "only the overridden tool should time out; got: {events:?}"
    );
    assert!(events.iter().any(|(name, _, _)| name == "slow_e"));
}

// ── P11.5 — cancel mid-tool ─────────────────────────────────────────────

async fn p11_5_drain_cancelled(
    emitter: AgentEventEmitter,
    mut rx: tokio::sync::mpsc::Receiver<caduceus_core::AgentEvent>,
) -> Vec<(String, u64)> {
    drop(emitter);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let caduceus_core::AgentEvent::ToolCancelled { tool, elapsed_ms } = ev {
            out.push((tool, elapsed_ms));
        }
    }
    out
}

#[tokio::test]
async fn p11_5_cancellation_after_tool_starts_aborts_it() {
    // Tool sleeps 5s; we cancel ~50ms in. With a polling token, the
    // tool's spawned future is dropped and we see a ToolCancelled
    // outcome instead of waiting the full 5s.
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_p11_5_a", "tc1"),
        p11_2_chat_text("after"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_p11_5_a",
        delay: std::time::Duration::from_secs(5),
    }));
    let token = caduceus_core::CancellationToken::new();
    let harness = AgentHarness::new(adapter, registry, 4096, "system")
        .with_cancellation_token(token.clone());

    // Cancel shortly after the run starts.
    let token_to_fire = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        token_to_fire.cancel();
    });

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let started = std::time::Instant::now();
    let _ = harness.run(&mut state, &mut history, "go").await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "cancellation must abort the in-flight tool, took {elapsed:?}"
    );
}

#[tokio::test]
async fn p11_5_tool_cancelled_event_emitted_with_correct_name() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_p11_5_b", "tc1"),
        p11_2_chat_text("after"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_p11_5_b",
        delay: std::time::Duration::from_secs(5),
    }));
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(64);
    let emitter = AgentEventEmitter::new(event_tx);
    let token = caduceus_core::CancellationToken::new();
    let harness = AgentHarness::new(adapter, registry, 4096, "system")
        .with_emitter(emitter.clone())
        .with_cancellation_token(token.clone());

    let token_to_fire = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        token_to_fire.cancel();
    });

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let _ = harness.run(&mut state, &mut history, "go").await;

    let events = p11_5_drain_cancelled(emitter, event_rx).await;
    assert_eq!(events.len(), 1, "exactly one ToolCancelled expected");
    assert_eq!(events[0].0, "slow_p11_5_b");
}

#[tokio::test]
async fn p11_5_tool_result_marked_error_on_cancel() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_p11_5_c", "tc1"),
        p11_2_chat_text("after"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_p11_5_c",
        delay: std::time::Duration::from_secs(5),
    }));
    let token = caduceus_core::CancellationToken::new();
    let harness = AgentHarness::new(adapter, registry, 4096, "system")
        .with_cancellation_token(token.clone());

    let token_to_fire = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        token_to_fire.cancel();
    });

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let _ = harness.run(&mut state, &mut history, "go").await;

    let cancelled = history
        .messages()
        .iter()
        .filter_map(|m| m.tool_result.as_ref())
        .any(|tr| tr.is_error && tr.content.contains("cancelled"));
    assert!(
        cancelled,
        "history must contain a tool_result marked cancelled"
    );
}

#[tokio::test]
async fn p11_5_no_cancellation_token_means_no_polling_or_event() {
    // Without a token, the spawn closure takes the simpler path —
    // no polling task — and ToolCancelled MUST NOT be emitted.
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("fast_p11_5_d", "tc1"),
        p11_2_chat_text("after"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "fast_p11_5_d",
        delay: std::time::Duration::from_millis(20),
    }));
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(64);
    let emitter = AgentEventEmitter::new(event_tx);
    let harness =
        AgentHarness::new(adapter, registry, 4096, "system").with_emitter(emitter.clone());

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let _ = harness.run(&mut state, &mut history, "go").await.unwrap();

    let events = p11_5_drain_cancelled(emitter, event_rx).await;
    assert!(events.is_empty(), "no token → no ToolCancelled events");
}

#[tokio::test]
async fn p11_5_pre_cancelled_token_skips_tool_invocation_entirely() {
    // Cancel BEFORE run starts. The spawn closure must short-circuit
    // to Cancelled without invoking the slow tool — proves the
    // pre-check path inside the closure works.
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_p11_5_e", "tc1"),
        p11_2_chat_text("after"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_p11_5_e",
        delay: std::time::Duration::from_secs(5),
    }));
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(64);
    let emitter = AgentEventEmitter::new(event_tx);
    let token = caduceus_core::CancellationToken::new();
    token.cancel(); // pre-cancel
    let harness = AgentHarness::new(adapter, registry, 4096, "system")
        .with_emitter(emitter.clone())
        .with_cancellation_token(token);

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let started = std::time::Instant::now();
    let _ = harness.run(&mut state, &mut history, "go").await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "pre-cancelled token must short-circuit, took {elapsed:?}"
    );
    // The pre-loop cancel check may trip first (returning early
    // without scheduling tools). That's acceptable — the contract
    // is "do not run a slow tool when we already know cancel".
    // We don't assert ToolCancelled here for that reason.
    let _ = p11_5_drain_cancelled(emitter, event_rx).await;
}

// ── P12.2 — speculative cache wiring ───────────────────────────────

#[tokio::test]
async fn p12_2_cache_hit_short_circuits_tool_execution() {
    // SlowTool would take 500ms, but a pre-seeded cache entry
    // should let the call return effectively instantly.
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_cache", "tc1"),
        p11_2_chat_text("after cache hit"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_cache",
        delay: std::time::Duration::from_millis(500),
    }));
    let cache = caduceus_tools::SpeculativeCache::new(std::time::Duration::from_secs(5));
    let key = caduceus_tools::SpecKey::new("slow_cache", &serde_json::json!({}));
    cache.reserve(&key);
    cache.complete(&key, Ok(caduceus_core::ToolResult::success("from-cache")));
    let harness = AgentHarness::new(adapter, registry, 4096, "system")
        .with_speculative_cache(cache.clone());

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let started = std::time::Instant::now();
    let result = harness.run(&mut state, &mut history, "go").await.unwrap();
    let elapsed = started.elapsed();
    assert_eq!(result, "after cache hit");
    // Cache hit must beat the 500ms tool delay decisively.
    assert!(
        elapsed < std::time::Duration::from_millis(300),
        "cache hit should short-circuit, took {elapsed:?}"
    );
    // The injected tool_result content should be visible in history.
    let saw_cached = history
        .messages()
        .iter()
        .filter_map(|m| m.tool_result.as_ref())
        .any(|tr| tr.content.contains("from-cache"));
    assert!(saw_cached, "expected cached tool_result in history");
}

#[tokio::test]
async fn p12_2_cache_miss_falls_through_to_real_tool() {
    // Empty cache → tool runs normally.
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![
        p11_2_chat_tool("slow_miss", "tc1"),
        p11_2_chat_text("after miss"),
    ]));
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(SlowTool {
        name: "slow_miss",
        delay: std::time::Duration::from_millis(20),
    }));
    let cache = caduceus_tools::SpeculativeCache::new(std::time::Duration::from_secs(5));
    let harness =
        AgentHarness::new(adapter, registry, 4096, "system").with_speculative_cache(cache);

    let mut state = p11_2_session();
    let mut history = ConversationHistory::new();
    let result = harness.run(&mut state, &mut history, "go").await.unwrap();
    assert_eq!(result, "after miss");
    let saw_done = history
        .messages()
        .iter()
        .filter_map(|m| m.tool_result.as_ref())
        .any(|tr| tr.content.contains("done"));
    assert!(saw_done, "real tool's 'done' content should be in history");
}

#[tokio::test]
async fn p12_2_cache_take_consumes_so_second_call_falls_through() {
    let cache = caduceus_tools::SpeculativeCache::new(std::time::Duration::from_secs(5));
    let key = caduceus_tools::SpecKey::new("slow_once", &serde_json::json!({}));
    cache.reserve(&key);
    cache.complete(&key, Ok(caduceus_core::ToolResult::success("from-cache")));
    // First take consumes; second take is a miss.
    assert!(cache.take(&key).is_some());
    assert!(cache.take(&key).is_none());
}

// ── P12.4 — reflexion wiring ───────────────────────────────────────

#[test]
fn p12_4_with_reflexion_attaches_and_accessor_returns_it() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![]));
    let registry = caduceus_tools::ToolRegistry::new();
    let mem = Arc::new(std::sync::Mutex::new(
        crate::reflexion::ReflexionMemory::new(8),
    ));
    let h = AgentHarness::new(adapter, registry, 4096, "system").with_reflexion(mem.clone());
    assert!(h.reflexion().is_some());
}

#[test]
fn p12_4_reflexion_prelude_returns_empty_when_no_memory() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![]));
    let registry = caduceus_tools::ToolRegistry::new();
    let h = AgentHarness::new(adapter, registry, 4096, "system");
    assert_eq!(h.reflexion_prelude("any-task", 5), "");
}

#[test]
fn p12_4_reflexion_prelude_renders_recorded_lessons() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![]));
    let registry = caduceus_tools::ToolRegistry::new();
    let mem = Arc::new(std::sync::Mutex::new(
        crate::reflexion::ReflexionMemory::new(8),
    ));
    let h = AgentHarness::new(adapter, registry, 4096, "system").with_reflexion(mem.clone());
    let r = crate::reflexion::HeuristicReflector;
    let outcome = crate::reflexion::AttemptOutcome::Failure {
        error: "timeout calling solve()".into(),
        attempted_action: Some("solve(x)".into()),
    };
    let stored = h.record_attempt_outcome(&r, "task-A", &outcome);
    assert!(stored.is_some());
    let prelude = h.reflexion_prelude("task-A", 5);
    assert!(prelude.starts_with("Lessons from previous attempts:"));
    assert!(prelude.contains("solve(x)"));
    assert!(prelude.contains("timeout"));
    // Filter: a different task tag yields empty.
    assert_eq!(h.reflexion_prelude("task-B", 5), "");
}

#[test]
fn p12_4_record_attempt_outcome_no_op_when_unattached() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![]));
    let registry = caduceus_tools::ToolRegistry::new();
    let h = AgentHarness::new(adapter, registry, 4096, "system");
    let r = crate::reflexion::HeuristicReflector;
    let outcome = crate::reflexion::AttemptOutcome::Failure {
        error: "x".into(),
        attempted_action: None,
    };
    assert!(h.record_attempt_outcome(&r, "t", &outcome).is_none());
}

// ── P13.2 — mid‑turn Reflexion injection on tool failure ──────────

#[tokio::test]
async fn p13_2_failed_tool_inlines_reflexion_lesson() {
    use caduceus_core::{StopReason, ToolUse};
    use caduceus_providers::mock::MockLlmAdapter;
    use caduceus_providers::ChatResponse;
    use caduceus_tools::ReadFileTool;

    fn final_resp(text: &str) -> ChatResponse {
        ChatResponse {
            content: text.into(),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
            thinking: String::new(),
        }
    }
    fn session() -> caduceus_core::SessionState {
        caduceus_core::SessionState::new(
            ".",
            caduceus_core::ProviderId::new("mock"),
            caduceus_core::ModelId::new("mock-model"),
        )
    }

    let tool_call = ChatResponse {
        content: "".into(),
        input_tokens: 1,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        stop_reason: StopReason::ToolUse,
        tool_calls: vec![ToolUse {
            id: "tc_p13_2".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "definitely_missing_p13_2.txt"}),
        }],
        logprobs: None,
        thinking: String::new(),
    };

    let adapter = Arc::new(MockLlmAdapter::new(vec![tool_call, final_resp("done")]));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool::new(dir.path())));

    let mem = Arc::new(std::sync::Mutex::new(
        crate::reflexion::ReflexionMemory::new(8),
    ));
    let harness =
        AgentHarness::new(adapter, registry, 200_000, "test").with_reflexion(mem.clone());

    let mut state = session();
    let mut history = ConversationHistory::new();
    let _ = harness.run(&mut state, &mut history, "read missing").await;

    let tool_msg = history
        .messages()
        .iter()
        .find(|m| m.role == "tool")
        .expect("tool result message must exist");
    let tr = tool_msg.tool_result.as_ref().unwrap();
    assert!(tr.is_error, "underlying tool must have errored");
    assert!(
        tr.content.contains("[Reflexion lesson:"),
        "lesson must be inlined into the failing tool_result so the \
         next provider call sees it within the same turn; got: {}",
        tr.content
    );

    let recent = mem.lock().unwrap().recent_for("read_file", 5);
    assert_eq!(recent.len(), 1, "exactly one lesson recorded");
}

#[tokio::test]
async fn p13_2_no_reflexion_when_no_memory_attached() {
    use caduceus_core::{StopReason, ToolUse};
    use caduceus_providers::mock::MockLlmAdapter;
    use caduceus_providers::ChatResponse;
    use caduceus_tools::ReadFileTool;

    fn final_resp(text: &str) -> ChatResponse {
        ChatResponse {
            content: text.into(),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
            thinking: String::new(),
        }
    }
    fn session() -> caduceus_core::SessionState {
        caduceus_core::SessionState::new(
            ".",
            caduceus_core::ProviderId::new("mock"),
            caduceus_core::ModelId::new("mock-model"),
        )
    }

    let tool_call = ChatResponse {
        content: "".into(),
        input_tokens: 1,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        stop_reason: StopReason::ToolUse,
        tool_calls: vec![ToolUse {
            id: "tc_p13_2_b".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "missing_p13_2_b.txt"}),
        }],
        logprobs: None,
        thinking: String::new(),
    };
    let adapter = Arc::new(MockLlmAdapter::new(vec![tool_call, final_resp("done")]));
    let dir = tempfile::tempdir().unwrap();
    let mut registry = caduceus_tools::ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool::new(dir.path())));
    let harness = AgentHarness::new(adapter, registry, 200_000, "test");
    let mut state = session();
    let mut history = ConversationHistory::new();
    let _ = harness.run(&mut state, &mut history, "read missing").await;
    let tool_msg = history
        .messages()
        .iter()
        .find(|m| m.role == "tool")
        .expect("tool result message must exist");
    assert!(
        !tool_msg
            .tool_result
            .as_ref()
            .unwrap()
            .content
            .contains("[Reflexion lesson:"),
        "no lesson must be appended when no ReflexionMemory is attached"
    );
}

// ── P12.3 — ToT branching planner wiring ───────────────────────────

struct TotPathExpander;
impl crate::branching_planner::BranchExpander<String> for TotPathExpander {
    fn expand(
        &self,
        node: &crate::branching_planner::ThoughtNode<String>,
        k: usize,
    ) -> Vec<(String, bool)> {
        (1..=k)
            .map(|i| {
                let next = format!("{}+{i}", node.thought);
                let terminal = node.depth + 1 >= 2;
                (next, terminal)
            })
            .collect()
    }
}
struct TotSuffixScorer;
impl crate::branching_planner::BranchScorer<String> for TotSuffixScorer {
    fn score(&self, node: &crate::branching_planner::ThoughtNode<String>) -> f32 {
        node.thought
            .rsplit('+')
            .next()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.0)
    }
}

#[test]
fn p12_3_with_tot_config_attaches_and_accessor_returns_it() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![]));
    let registry = caduceus_tools::ToolRegistry::new();
    let cfg = crate::branching_planner::PlannerConfig {
        branching_factor: 4,
        beam_width: 3,
        max_depth: 7,
    };
    let h = AgentHarness::new(adapter, registry, 4096, "system").with_tot_config(cfg);
    let stored = h.tot_config().expect("config attached");
    assert_eq!(stored.branching_factor, 4);
    assert_eq!(stored.beam_width, 3);
    assert_eq!(stored.max_depth, 7);
}

#[test]
fn p12_3_plan_with_tot_uses_attached_config() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![]));
    let registry = caduceus_tools::ToolRegistry::new();
    let cfg = crate::branching_planner::PlannerConfig {
        branching_factor: 3,
        beam_width: 2,
        max_depth: 5,
    };
    let h = AgentHarness::new(adapter, registry, 4096, "system").with_tot_config(cfg);
    let result = h.plan_with_tot("root".to_string(), TotPathExpander, TotSuffixScorer);
    let best = result.best.expect("must find a best");
    assert!(best.terminal, "should reach terminal at depth 2");
    // SuffixScorer + branching=3 → highest-scoring child each
    // round is "+3"; chain of length 2 yields "root+3+3".
    assert!(best.thought.ends_with("+3"));
}

#[test]
fn p12_3_plan_with_tot_uses_default_when_no_config_attached() {
    let adapter = Arc::new(caduceus_providers::mock::MockLlmAdapter::new(vec![]));
    let registry = caduceus_tools::ToolRegistry::new();
    let h = AgentHarness::new(adapter, registry, 4096, "system");
    assert!(h.tot_config().is_none());
    let result = h.plan_with_tot("r".to_string(), TotPathExpander, TotSuffixScorer);
    assert!(
        result.best.is_some(),
        "default config must still produce a plan"
    );
}

// ── P13.6 — per‑turn critic loop ─────────────────────────────────

#[tokio::test]
async fn p13_6_critic_reject_triggers_revision_turn() {
    use caduceus_core::StopReason;
    use caduceus_providers::mock::MockLlmAdapter;
    use caduceus_providers::ChatResponse;

    fn final_resp(text: &str) -> ChatResponse {
        ChatResponse {
            content: text.into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
            thinking: String::new(),
        }
    }
    fn session() -> caduceus_core::SessionState {
        caduceus_core::SessionState::new(
            ".",
            caduceus_core::ProviderId::new("mock"),
            caduceus_core::ModelId::new("mock-model"),
        )
    }

    // Two assistant responses queued: bad answer first, good answer
    // after the critic feedback is appended.
    let adapter = Arc::new(MockLlmAdapter::new(vec![
        final_resp("first attempt — too short"),
        final_resp("second attempt — fully fleshed out final answer."),
    ]));
    let registry = caduceus_tools::ToolRegistry::new();

    // Scripted critic: reject the first candidate, accept the second.
    let critic = Arc::new(crate::critic::ScriptedCritic::new(vec![
        crate::critic::Verdict::Reject {
            feedback: "be more thorough".into(),
        },
        crate::critic::Verdict::Accept,
    ]));

    let harness = AgentHarness::new(adapter, registry, 200_000, "test")
        .with_critic(critic.clone() as Arc<dyn crate::critic::Critic>)
        .with_critic_max_iters(2);

    let mut state = session();
    let mut history = ConversationHistory::new();
    let out = harness
        .run(&mut state, &mut history, "give me an answer")
        .await
        .unwrap();
    assert!(
        out.contains("second attempt"),
        "harness must return the revised answer, got: {out}"
    );
    // Critic feedback must be in history as a synthetic user message.
    let has_feedback = history
        .messages()
        .iter()
        .any(|m| m.role == "user" && m.content.contains("[Critic feedback]"));
    assert!(
        has_feedback,
        "synthetic '[Critic feedback]' user message must be appended after reject"
    );
}

#[tokio::test]
async fn p13_6_no_critic_no_extra_turn() {
    use caduceus_core::StopReason;
    use caduceus_providers::mock::MockLlmAdapter;
    use caduceus_providers::ChatResponse;

    fn final_resp(text: &str) -> ChatResponse {
        ChatResponse {
            content: text.into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
            thinking: String::new(),
        }
    }
    fn session() -> caduceus_core::SessionState {
        caduceus_core::SessionState::new(
            ".",
            caduceus_core::ProviderId::new("mock"),
            caduceus_core::ModelId::new("mock-model"),
        )
    }

    // Only one response queued — if the harness called the LLM
    // twice without a critic, we'd panic on adapter underflow.
    let adapter = Arc::new(MockLlmAdapter::new(vec![final_resp("ok")]));
    let registry = caduceus_tools::ToolRegistry::new();
    let harness = AgentHarness::new(adapter, registry, 200_000, "test");

    let mut state = session();
    let mut history = ConversationHistory::new();
    let out = harness.run(&mut state, &mut history, "hi").await.unwrap();
    assert_eq!(out, "ok");
    assert!(
        !history
            .messages()
            .iter()
            .any(|m| m.content.contains("[Critic feedback]")),
        "no critic attached → no synthetic feedback in history"
    );
}

#[tokio::test]
async fn p13_6_max_iters_bounds_revision_loops() {
    use caduceus_core::StopReason;
    use caduceus_providers::mock::MockLlmAdapter;
    use caduceus_providers::ChatResponse;

    fn final_resp(text: &str) -> ChatResponse {
        ChatResponse {
            content: text.into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
            thinking: String::new(),
        }
    }
    fn session() -> caduceus_core::SessionState {
        caduceus_core::SessionState::new(
            ".",
            caduceus_core::ProviderId::new("mock"),
            caduceus_core::ModelId::new("mock-model"),
        )
    }

    // Critic always rejects — harness must STOP after critic_max_iters
    // revisions, meaning total LLM calls = 1 + critic_max_iters.
    let adapter = Arc::new(MockLlmAdapter::new(vec![
        final_resp("v1"),
        final_resp("v2"),
        // No v3 — if the harness loops a third time we panic.
    ]));
    let registry = caduceus_tools::ToolRegistry::new();
    let critic = Arc::new(crate::critic::ScriptedCritic::new(vec![
        crate::critic::Verdict::Reject {
            feedback: "no".into(),
        },
        crate::critic::Verdict::Reject {
            feedback: "still no".into(),
        },
    ]));
    let harness = AgentHarness::new(adapter, registry, 200_000, "test")
        .with_critic(critic.clone() as Arc<dyn crate::critic::Critic>)
        .with_critic_max_iters(1);

    let mut state = session();
    let mut history = ConversationHistory::new();
    let out = harness.run(&mut state, &mut history, "x").await.unwrap();
    // Bound is 1 → first reject triggers revision, second response
    // is taken as-is (critic_iters=1 == max, skip critic entirely).
    assert_eq!(out, "v2");
}

// ── P5: behavior_rules preamble + envelope-aware system prompt ────────────

fn mk_plain_harness() -> AgentHarness {
    use caduceus_providers::mock::MockLlmAdapter;
    let provider = Arc::new(MockLlmAdapter::new(vec![]));
    let tools = ToolRegistry::new();
    AgentHarness::new(provider, tools, 8192, "base instructions")
}

#[test]
fn p5_behavior_rules_always_present() {
    let h = mk_plain_harness();
    let prompt = h.effective_system_prompt();
    assert!(prompt.contains("<behavior_rules>"));
    assert!(prompt.contains("</behavior_rules>"));
    // The key anti-mode-theater rule must be present verbatim.
    assert!(
        prompt.contains("Do NOT retry the same denied call"),
        "behavior_rules must forbid retry-on-denial loops"
    );
    assert!(
        prompt.contains("scope_expansion") && prompt.contains("ONCE"),
        "behavior_rules must bound scope-expansion to a single ask"
    );
    assert!(
        prompt.contains("Never invent tools"),
        "behavior_rules must forbid tool hallucination"
    );
    assert!(
        prompt.contains("untrusted DATA"),
        "behavior_rules must treat fetched content as untrusted"
    );
}

#[test]
fn p5_mode_block_renders_when_mode_set() {
    let h = mk_plain_harness().with_mode(modes::AgentMode::Plan);
    let prompt = h.effective_system_prompt();
    assert!(prompt.contains("<agent_mode mode=\"plan\">"));
    assert!(prompt.contains("PLAN mode"));
}

#[test]
fn p5_act_lens_appears_in_mode_attr_when_non_normal() {
    let h = mk_plain_harness()
        .with_mode(modes::AgentMode::Act)
        .with_mode_lens(modes::ActLens::Debug);
    let prompt = h.effective_system_prompt();
    assert!(prompt.contains("mode=\"act\""));
    assert!(prompt.contains("lens=\"debug\""));
    assert!(prompt.contains("Debug lens"));
}

#[test]
fn p5_act_normal_lens_omits_lens_attr() {
    let h = mk_plain_harness().with_mode(modes::AgentMode::Act);
    let prompt = h.effective_system_prompt();
    assert!(prompt.contains("mode=\"act\""));
    assert!(
        !prompt.contains("lens=\"normal\""),
        "normal lens should not clutter the mode tag"
    );
}

#[test]
fn p5_mode_selection_sets_mode_and_lens() {
    let sel = modes::ModeSelection::from_mode_str("review").unwrap();
    let h = mk_plain_harness().with_mode_selection(sel);
    let prompt = h.effective_system_prompt();
    assert!(prompt.contains("mode=\"act\""));
    assert!(prompt.contains("lens=\"review\""));
    assert!(prompt.contains("Review lens"));
}

#[test]
fn p5_envelope_summary_rendered_when_set() {
    let env = PermissionEnvelope::plan_preset();
    let h = mk_plain_harness().with_permission_envelope(env);
    let prompt = h.effective_system_prompt();
    assert!(prompt.contains("<permission_envelope>"));
    assert!(prompt.contains("approval_cadence: per-major-step"));
    assert!(prompt.contains("skill_budget: 6"));
    // Plan preset has exec disabled.
    assert!(prompt.contains("exec: disabled"));
}

#[test]
fn p5_envelope_summary_absent_when_unset() {
    let h = mk_plain_harness();
    let prompt = h.effective_system_prompt();
    assert!(!prompt.contains("<permission_envelope>"));
}

#[test]
fn p5_base_system_prompt_preserved_after_preamble() {
    let h = mk_plain_harness();
    let prompt = h.effective_system_prompt();
    // Base prompt ("base instructions") must appear *after* the preamble.
    let rules_idx = prompt.find("</behavior_rules>").expect("preamble present");
    let base_idx = prompt
        .find("base instructions")
        .expect("base prompt present");
    assert!(
        base_idx > rules_idx,
        "behavior_rules must come before the caller-supplied system prompt"
    );
}

#[test]
fn p5_autopilot_mode_still_re_asks_on_scope_expansion() {
    let h = mk_plain_harness().with_mode(modes::AgentMode::Autopilot);
    let prompt = h.effective_system_prompt();
    // Autopilot permits no per-step approval, but scope expansion must
    // still re-prompt. The mode prompt says so explicitly.
    assert!(prompt.contains("AUTOPILOT"));
    assert!(
        prompt.contains("scope expansion always re-prompts"),
        "autopilot prompt must state that scope expansion re-prompts"
    );
}
