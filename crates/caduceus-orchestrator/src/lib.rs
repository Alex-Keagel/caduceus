pub mod automations;
pub mod background;
pub mod branching_planner;
pub mod branching_planner_llm;
pub mod broadcast_bus;
pub mod bugbot;
pub mod checkpoint;
pub mod compaction;
pub mod compaction_scorer;
pub mod compaction_telemetry;
pub mod context;
pub mod context_fold;
pub mod critic;
pub mod critique_fanout;
pub mod headless;
pub mod hygiene;
pub mod instructions;
pub mod kanban;
pub mod learned_selector;
pub mod memories;
pub mod memory_blocks;
pub mod mentions;
pub mod modes;
pub mod notifications;
mod pairing;
pub mod reflexion;
pub mod rollout_prm;
pub mod scoped_context;
pub mod self_consistency;
pub mod snapshot;
pub mod wiki_slash;
pub mod worker_pool;
pub mod workers;

// ST-B1 Wave 0a — extracted modules.
mod scaffolders;
#[cfg(test)]
pub(crate) use scaffolders::to_title_case;
pub use scaffolders::{
    AgentScaffoldConfig, AgentScaffolder, InstructionsConfig, InstructionsScaffolder,
    SkillScaffoldConfig, SkillScaffolder,
};

// ST-B1 Wave 0b — extracted modules.
mod prd_parser;
mod progress_inference;
mod task_hierarchy;
mod task_recommender;
mod time_tracking;
pub use prd_parser::{PrdParser, PrdTask};
pub use progress_inference::{InferredProgress, ProgressInferrer};
pub use task_hierarchy::{HierarchicalTask, TaskTree};
pub use task_recommender::{TaskRecommendation, TaskRecommender};
pub use time_tracking::{TimeEntry, TimeTracker};

// ST-B1 Wave 0c — extracted modules.
mod config_loader;
pub mod decision_register;
mod effort_levels;
mod execution_tree;
mod query_config;
pub mod restore_protocol;
pub mod thread_id;
pub use config_loader::ConfigLoader;
pub use decision_register::{
    apply_event as apply_decision_event, load as load_decision_register,
    persist as persist_decision_register, register_path as decision_register_path, ApplyOutcome,
    DecisionRegister, ReducerError as DecisionReducerError,
};
pub use effort_levels::EffortLevel;
pub use execution_tree::{ExecutionTreeViz, VizTreeNode};
pub use query_config::QueryConfig;
pub use restore_protocol::{
    compute_eliminations, persist_and_restore, render_reconciliation_message, run_restore,
    RestoreOutcome, RestoreTrigger, RECONCILIATION_BUDGET_BYTES,
};
pub use thread_id::{
    migrate_pre_spec_layout, resolve_thread_id_for_session, ResolveOutcome, ThreadIdEnv,
    DEFAULT_BASE_DIR,
};

// ST-B1 Wave 1 — extracted modules.
mod context_assembler;
mod session_manager;
pub use context_assembler::MessageAssembler;
pub use session_manager::SessionManager;

// ST-B1 Wave 2 — extracted modules.
mod agent_event_emitter;
pub use agent_event_emitter::{
    AgentEventEmitter, DEFAULT_BROADCAST_CAP, DEFAULT_EMITTER_RETENTION,
};

// ST-B1 finalization: AgentHarness + associated types
mod agent_harness;
pub use agent_harness::{
    execute_tool_calls, extract_memories, preflight_envelope_of, AgentHarness, PreflightOutcome,
    ProfilePreflightOutcome, SubmitGrantError, SubmitSwitchError, SwitchOutcome, TestGateConfig,
    TestGateOutcome,
};
#[cfg(test)]
pub(crate) use agent_harness::{extract_host, tail_chars};

pub use branching_planner::PlannerConfig;
pub use context::{AssembledContext, ContextSource};
pub use critique_fanout::IntrospectionSink;
pub use headless::{
    CompactOutputFilter, ReplAction, ReplMode, ReplState, SummaryCompressor, TypoSuggester,
};
pub use modes::{AgentPersona, PersonaRegistry};
pub use scoped_context::{
    BuiltinScopedContextInjector, ContextInjector, PassthroughContextInjector, ScopeRequest,
    ScopedContext,
};

#[cfg(test)]
use caduceus_core::{AgentEvent, CancellationToken, ModelId, SessionPhase, StopReason};
use caduceus_core::{CaduceusError, Result};
#[cfg(test)]
use caduceus_permissions::envelope::PermissionEnvelope;
#[cfg(test)]
use caduceus_providers::LlmAdapter;
#[cfg(test)]
use caduceus_tools::ToolRegistry;
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use tokio::sync::mpsc;

// ── P1: Loop Detection ─────────────────────────────────────────────────────────
// F2: unified implementation lives in caduceus-core. The engine re-exports
// it here (via top-level re-export) and uses it throughout.
pub use caduceus_core::{LoopCheckResult, LoopDetector};

// ── Slash commands ─────────────────────────────────────────────────────────────

// ── Conversation history ───────────────────────────────────────────────────────

/// Manages an ordered list of provider-level messages for the conversation.
///
/// Messages are stored as `Arc<Message>` (ST-C2 Phase 2) so that cloning the
/// history — which happens on every turn to feed the provider — is O(messages)
/// pointer copies instead of deep content clones. External callers still see
/// `&[Arc<Message>]`; treat each element as if it were a `&Message` (Arc
/// auto-derefs on field access).
#[derive(Debug, Clone, Default)]
pub struct ConversationHistory {
    messages: Vec<Arc<caduceus_providers::Message>>,
}

impl ConversationHistory {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// Append a fresh message; wraps it in `Arc` internally.
    pub fn append(&mut self, message: caduceus_providers::Message) {
        self.messages.push(Arc::new(message));
    }

    /// Append a message that is already `Arc`-wrapped (sharing scenarios).
    pub fn append_arc(&mut self, message: Arc<caduceus_providers::Message>) {
        self.messages.push(message);
    }

    pub fn messages(&self) -> &[Arc<caduceus_providers::Message>] {
        &self.messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Drop the oldest non-system messages until we are at or below `max_messages`.
    ///
    /// Pair-aware: an assistant message carrying `tool_calls = [t1..tN]` and
    /// the immediately following `tool` messages whose `tool_use_id` matches
    /// one of those calls form a single atomic unit and are dropped together.
    /// This prevents orphaned tool_use / tool_result pairs that providers
    /// (especially Anthropic) reject with HTTP 400.
    pub fn truncate_oldest(&mut self, max_messages: usize) {
        let _ = self.truncate_oldest_with_report(max_messages);
    }

    /// Same as [`Self::truncate_oldest`] but returns a per-unit report of
    /// what was dropped (kind, message count, approximate token cost). Used
    /// by callers that want to surface eviction telemetry through
    /// [`AgentEvent::ContextGroupsEvicted`] (G31).
    pub fn truncate_oldest_with_report(
        &mut self,
        max_messages: usize,
    ) -> Vec<caduceus_core::EvictedGroupRef> {
        if self.messages.len() <= max_messages {
            return Vec::new();
        }
        let units = crate::pairing::pair_aware_units(&self.messages);
        let mut to_drop: Vec<(usize, usize)> = Vec::new();
        let mut remaining = self.messages.len();
        for (start, end) in units {
            if remaining <= max_messages {
                break;
            }
            if end - start == 1 && self.messages[start].role == "system" {
                continue;
            }
            to_drop.push((start, end));
            remaining -= end - start;
        }
        // Snapshot evicted refs *before* draining so token estimates reflect
        // the actual messages that were removed.
        let evicted: Vec<caduceus_core::EvictedGroupRef> = to_drop
            .iter()
            .map(|&(start, end)| {
                let slice = &self.messages[start..end];
                let kind = if end - start > 1 {
                    "tool_call"
                } else {
                    match slice[0].role.as_str() {
                        "user" => "user",
                        "assistant" => "assistant_text",
                        "tool" => "tool_call",
                        "system" => "system",
                        _ => "other",
                    }
                };
                let tokens: u32 = slice
                    .iter()
                    .map(|m| crate::MessageAssembler::message_tokens(m.as_ref()))
                    .sum();
                caduceus_core::EvictedGroupRef {
                    kind: kind.to_string(),
                    message_count: (end - start) as u32,
                    token_count: tokens,
                    reason: "oldest-non-system".to_string(),
                }
            })
            .collect();
        for (start, end) in to_drop.into_iter().rev() {
            self.messages.drain(start..end);
        }
        evicted
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    pub fn serialize(&self) -> Result<String> {
        // Build a view of &Message refs so serde can serialize without
        // requiring the serde `rc` feature for Arc<T>.
        let view: Vec<&caduceus_providers::Message> =
            self.messages.iter().map(|a| a.as_ref()).collect();
        serde_json::to_string(&view).map_err(|e| CaduceusError::Config(e.to_string()))
    }

    pub fn deserialize(json: &str) -> Result<Self> {
        // Deserialize into owned Messages then wrap in Arc — avoids needing
        // serde's `rc` feature flag.
        let raw: Vec<caduceus_providers::Message> =
            serde_json::from_str(json).map_err(|e| CaduceusError::Config(e.to_string()))?;
        Ok(Self {
            messages: raw.into_iter().map(Arc::new).collect(),
        })
    }
}

/// ST-C2 Phase 3 — retrospective benchmark validating the Phase 2 win.
///
/// Phase 2 changed `ConversationHistory::messages` from `Vec<Message>` to
/// `Vec<Arc<Message>>`. This means `ConversationHistory::clone()` — which
/// the harness does once per turn when snapshotting state — now bumps
/// refcounts instead of deep-cloning each `Message` (and every `String`
/// inside it).
///
/// Run with:
/// ```text
/// cargo test -p caduceus-orchestrator --release \
///     phase3_clone_benchmark -- --ignored --nocapture
/// ```
#[cfg(test)]
mod phase3_bench {
    use super::*;
    use caduceus_providers::Message;
    use std::time::Instant;

    fn sample_message(i: usize) -> Message {
        // 2 KB-ish body per message — realistic assistant turn size.
        let body = "lorem ipsum ".repeat(170);
        Message {
            role: if i.is_multiple_of(2) {
                "user"
            } else {
                "assistant"
            }
            .into(),
            content: format!("[turn {i}] {body}"),
            content_blocks: None,
            tool_calls: vec![],
            tool_result: None,
            cache_breakpoint: false,
        }
    }

    #[test]
    #[ignore = "perf benchmark; run explicitly with --ignored"]
    fn phase3_clone_benchmark() {
        const N: usize = 100;
        const ITERS: usize = 10_000;

        // Build the "after" representation (today's ConversationHistory).
        let mut arc_history = ConversationHistory::new();
        for i in 0..N {
            arc_history.append(sample_message(i));
        }

        // Build the "before" representation — a plain Vec<Message> of the
        // same content. Clone semantics match pre-Phase-2 behavior.
        let pre_phase2: Vec<Message> = (0..N).map(sample_message).collect();

        // Warm up caches.
        let _warm_a = arc_history.clone();
        let _warm_b = pre_phase2.clone();

        let before = Instant::now();
        for _ in 0..ITERS {
            let clone = pre_phase2.clone();
            std::hint::black_box(clone);
        }
        let deep_clone_elapsed = before.elapsed();

        let before = Instant::now();
        for _ in 0..ITERS {
            let clone = arc_history.clone();
            std::hint::black_box(clone);
        }
        let arc_clone_elapsed = before.elapsed();

        let ratio =
            deep_clone_elapsed.as_nanos() as f64 / arc_clone_elapsed.as_nanos().max(1) as f64;

        println!(
            "Phase 3 clone benchmark (N={N} messages, {ITERS} iters):\n  \
             pre-Phase-2 deep clone:   {:>12?}\n  \
             post-Phase-2 Arc clone:   {:>12?}\n  \
             speedup:                  {ratio:.1}×",
            deep_clone_elapsed, arc_clone_elapsed,
        );

        // Sanity floor: the Arc refcount clone must be materially cheaper
        // than the deep clone. A 2× floor is conservative; real runs show
        // 20–100×. Guards against a regression that re-introduces deep
        // cloning (e.g. changing `messages: Vec<Arc<Message>>` back to
        // `Vec<Message>` without noticing).
        assert!(
            ratio >= 2.0,
            "Phase 3 regression: Arc clone should be ≥2× faster than deep \
             clone, got {ratio:.2}× (deep={:?}, arc={:?})",
            deep_clone_elapsed,
            arc_clone_elapsed,
        );
    }
}

// ── Crate-wide harness tests ──────────────────────────────────────────────────
// (Body relocated to harness_tests.rs — ST-B1 Wave 3.)

#[cfg(test)]
mod harness_tests;

// ── Tests for #236–#237, #239–#240, #245–#246 ────────────────────────────────

// ── Tests for #236–#237, #239–#240, #245–#246 ────────────────────────────────
// (Body relocated to feature_tests_236_246.rs — ST-B1 Wave 3.)

#[cfg(test)]
mod feature_tests_236_246;

// ── Tests for #259–#261 ───────────────────────────────────────────────────────

// ── Tests for #259–#261 ───────────────────────────────────────────────────────
// (Body relocated to feature_tests_259_261.rs — ST-B1 Wave 3.)

#[cfg(test)]
mod feature_tests_259_261;
