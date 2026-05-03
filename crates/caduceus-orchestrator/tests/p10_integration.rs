//! P10 — 14 integration tests for the Caduceus behavior fix.
//!
//! Each test covers one of the failure modes from the original transcript
//! (items 1–7) or one of the new envelope/mode semantics (items 8–14).
//! Where possible we assert at the black-box boundary (public API of the
//! orchestrator + permissions crates). Items that would require a real LLM
//! are covered at the prompt-text level: we assert the guarantees the
//! system prompt commits to, so an LLM following the prompt cannot regress
//! silently.

use caduceus_core::{Critique, CritiqueSeverity};
use caduceus_orchestrator::critique_fanout::{
    plan_critique_personas, spawn_critique_fanout, CritiqueRunner,
};
use caduceus_orchestrator::instructions::InstructionLoader;
use caduceus_orchestrator::modes::{ActLens, AgentMode, ModeSelection, PersonaRegistry};
use caduceus_orchestrator::AgentHarness;
use caduceus_permissions::envelope::{
    ApprovalCadence, Decision, EnvelopeScope, ExecPolicy, FanoutPolicy, NetworkPolicy,
    PathAllowlist, PermissionEnvelope,
};
use caduceus_providers::mock::MockLlmAdapter;
use caduceus_tools::ToolRegistry;
use std::sync::Arc;

// ── helpers ──────────────────────────────────────────────────────────────────

fn is_allow(d: &Decision) -> bool {
    matches!(d, Decision::Allow)
}
fn is_deny(d: &Decision) -> bool {
    matches!(d, Decision::Deny(_))
}

fn behavior_rules_and_mode_prompt(mode: AgentMode, lens: ActLens) -> String {
    let sel = ModeSelection::new(mode, lens);
    let provider = Arc::new(MockLlmAdapter::new(vec![]));
    let tools = ToolRegistry::new();
    let harness = AgentHarness::new(provider, tools, 8192, "test").with_mode_selection(sel);
    harness.effective_system_prompt()
}

// ── 1. Research mode prompt greenlights web fetch (no mode-switch needed) ────

#[test]
fn p10_01_research_prompt_allows_fetch_and_search() {
    let prompt = behavior_rules_and_mode_prompt(AgentMode::Plan, ActLens::Normal);
    // Must explicitly mention that reads + web are available in Research.
    let lc_full = prompt.to_lowercase();
    // Plan now subsumes Research; prompt must reference research/web/fetch.
    assert!(
        (lc_full.contains("research") || lc_full.contains("plan"))
            && (prompt.contains("fetch") || prompt.contains("web")),
        "Combined Plan/Research prompt missing web-fetch guarantee:\n{prompt}"
    );
    // Must NOT tell the model to switch modes in order to read. The
    // preamble's "Never request a mode change to perform a read" is a
    // prohibition, not an instruction — exclude it by anchoring on the
    // affirmative form only.
    let lc = prompt.to_lowercase();
    assert!(
        !lc.contains("switch to act mode to fetch")
            && !lc.contains("switch modes to read")
            && !lc.contains("please request a mode change to read"),
        "Research prompt still instructs mode-change for reads:\n{prompt}"
    );
}

// ── 2. Plan mode prompt greenlights web fetch too ────────────────────────────

#[test]
fn p10_02_plan_prompt_allows_fetch() {
    let prompt = behavior_rules_and_mode_prompt(AgentMode::Plan, ActLens::Normal);
    // Plan explicitly MAY fetch URLs and search the web.
    assert!(
        prompt.contains("PLAN") && (prompt.contains("fetch") || prompt.contains("web")),
        "Plan prompt must preserve read capabilities including web:\n{prompt}"
    );
}

// ── 3. behavior_rules preamble pins the verbatim-error and fallback rules ────

#[test]
fn p10_03_behavior_rules_preamble_present() {
    let prompt = behavior_rules_and_mode_prompt(AgentMode::Plan, ActLens::Normal);
    // Must contain the structured behavior_rules block.
    assert!(
        prompt.contains("<behavior_rules>"),
        "missing behavior_rules preamble"
    );
    assert!(
        prompt.contains("</behavior_rules>"),
        "behavior_rules block not closed"
    );
    // Must carry the five rules — we assert via unique anchor words that
    // cannot appear by accident.
    for anchor in ["unverified", "fails", "read", "untrusted", "verify"] {
        assert!(
            prompt.to_lowercase().contains(anchor),
            "behavior_rules missing anchor '{anchor}':\n{prompt}"
        );
    }
    // Sweep #2 (post nanoreason-thread review) — five new rules must be
    // present so future edits don't silently drop them.
    for anchor in [
        "Match response length",   // PB1 brevity
        "/dev/null",               // PB2 fake-path ban
        "ONE clarifying question", // PB3 stop padding
        "Before citing",           // PB4 cite-then-fetch
        "empty `<thinking>",       // PB5 no empty thinking
    ] {
        assert!(
            prompt.contains(anchor),
            "sweep-#2 behavior rule missing: '{anchor}':\n{prompt}"
        );
    }
    // Phase 3 (DAG orchestration) — PB6 self-pause + diversity-by-default
    // + plan-first-for-non-trivial. Three anchors must be present.
    for anchor in [
        "Self-pause check",     // PB6a self-pause
        "Diversity by default", // PB6b diversity-by-default
        "Plan-first",           // PB6c plan-first for non-trivial
    ] {
        assert!(
            prompt.contains(anchor),
            "phase-3 PB6 rule missing: '{anchor}':\n{prompt}"
        );
    }
    // Sweep #3 (post nanoTeacher-thread dead-end) — PB7 forbids treating an
    // empty target directory as a blocker when the user asked to create
    // files there. Two anchors guarantee the wording survives edits.
    for anchor in ["Empty target is a green light", "is empty\" and stop"] {
        assert!(
            prompt.contains(anchor),
            "PB7 empty-target rule missing: '{anchor}':\n{prompt}"
        );
    }
}

// ── 3b. Phase-3 autonomy thresholds block is rendered with env-overridable ──
//        defaults (CADUCEUS_AUTONOMY_BUDGET, CADUCEUS_PARALLEL_SPAWN_LIMIT).
//        Tests run in parallel and share process env, so we keep a single
//        serial test that checks both default and override paths.

#[test]
fn p10_03b_autonomy_thresholds_block_default_and_override() {
    // SAFETY: the test mutates process-global env. Run --test-threads=1 if
    // failing in parallel — but we keep both checks in one test so we do
    // not race ourselves.
    unsafe {
        std::env::remove_var("CADUCEUS_AUTONOMY_BUDGET");
        std::env::remove_var("CADUCEUS_PARALLEL_SPAWN_LIMIT");
    }
    let prompt = behavior_rules_and_mode_prompt(AgentMode::Plan, ActLens::Normal);
    assert!(
        prompt.contains("<autonomy_thresholds>"),
        "missing autonomy_thresholds block:\n{prompt}"
    );
    assert!(
        prompt.contains("after 4 consecutive assistant turns"),
        "default autonomy budget (4) not rendered:\n{prompt}"
    );
    assert!(
        prompt.contains("more than 2 sub-agents"),
        "default parallel-spawn limit (2) not rendered:\n{prompt}"
    );

    unsafe {
        std::env::set_var("CADUCEUS_AUTONOMY_BUDGET", "7");
        std::env::set_var("CADUCEUS_PARALLEL_SPAWN_LIMIT", "5");
    }
    let prompt = behavior_rules_and_mode_prompt(AgentMode::Plan, ActLens::Normal);
    unsafe {
        std::env::remove_var("CADUCEUS_AUTONOMY_BUDGET");
        std::env::remove_var("CADUCEUS_PARALLEL_SPAWN_LIMIT");
    }
    assert!(
        prompt.contains("after 7 consecutive assistant turns"),
        "env override of autonomy budget not honored:\n{prompt}"
    );
    assert!(
        prompt.contains("more than 5 sub-agents"),
        "env override of parallel-spawn limit not honored:\n{prompt}"
    );
}

// ── 4 & 5. No-hallucination and fallback instructions live in the preamble ───

#[test]
fn p10_04_preamble_forbids_hallucination_and_requires_fallback() {
    let prompt = behavior_rules_and_mode_prompt(AgentMode::Plan, ActLens::Normal);
    // The preamble must tell the LLM to try alternatives when a tool fails,
    // not to declare blocked immediately.
    assert!(
        prompt.to_lowercase().contains("alternatives")
            || prompt.to_lowercase().contains("fallback"),
        "preamble missing fallback guidance:\n{prompt}"
    );
    // And must require verification before asserting facts about external
    // artifacts.
    assert!(
        prompt.to_lowercase().contains("verify"),
        "preamble missing verify-before-assert rule:\n{prompt}"
    );
}

// ── 6. Envelope cascade: parent's deny propagates to every persona ───────────

#[tokio::test]
async fn p10_06_envelope_cascades_unchanged_to_every_fanout_worker() {
    struct Capture {
        seen: std::sync::Mutex<Vec<PermissionEnvelope>>,
    }
    #[async_trait::async_trait]
    impl CritiqueRunner for Capture {
        async fn critique(
            &self,
            persona: &str,
            _prefix: &str,
            _plan: &str,
            env: &PermissionEnvelope,
        ) -> Result<Critique, anyhow::Error> {
            self.seen.lock().unwrap().push(env.clone());
            Ok(Critique {
                persona: persona.to_string(),
                severity: CritiqueSeverity::Info,
                findings: vec!["ok".into()],
                blocking: false,
            })
        }
    }
    let parent = PermissionEnvelope {
        read: PathAllowlist::open_all(),
        write: PathAllowlist {
            allow: vec!["src/**".into()],
            deny: vec!["src/secrets/**".into()],
            intercept_denied: false,
        },
        network: NetworkPolicy::disabled(),
        exec: ExecPolicy::disabled(),
        approval_cadence: ApprovalCadence::PerMajorStep,
        scope: EnvelopeScope::Task,
        treat_tool_output_as_untrusted: true,
        fanout_policy: FanoutPolicy::MultiPersona,
        skill_budget: 5,
        sensitive_write_paths: vec![],
        sensitive_write_exceptions: vec![],
    };
    let reg = PersonaRegistry::builtin_personas();
    let runner = Capture {
        seen: Default::default(),
    };
    let _ = spawn_critique_fanout(&parent, "plan", &["cloud", "qa"], &reg, &runner).await;
    let seen = runner.seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        3,
        "rubber-duck + cloud-architect + qa-strategist"
    );
    for child in seen.iter() {
        assert_eq!(*child, parent, "workers must see parent envelope verbatim");
    }
}

// ── 7. Scope-expansion under Autopilot: envelope denies writes outside allow

#[test]
fn p10_07_autopilot_still_blocks_writes_outside_envelope() {
    let mut env = PermissionEnvelope::autopilot_preset(vec!["src/**".into()], vec![]);
    // Approval cadence None == Autopilot, but deny-wins is structural.
    env.approval_cadence = ApprovalCadence::None;
    // Inside the grant: allowed.
    assert!(is_allow(
        &env.write.check(std::path::Path::new("src/main.rs"))
    ));
    // Outside the grant: denied. The engine's preflight emits a
    // ScopeExpansionRequested event (tested elsewhere) — here we just pin
    // the envelope decision.
    assert!(is_deny(
        &env.write.check(std::path::Path::new("secrets/key.pem"))
    ));
}

// ── 8. Research writes markdown only ─────────────────────────────────────────

#[test]
fn p10_08_research_allows_markdown_writes_only() {
    let env = PermissionEnvelope::research_preset();
    assert!(
        is_allow(&env.write.check(std::path::Path::new("notes.md"))),
        "Research must allow .md writes"
    );
    assert!(
        is_deny(&env.write.check(std::path::Path::new("hack.py"))),
        "Research must deny non-markdown writes"
    );
    assert!(
        is_deny(&env.write.check(std::path::Path::new("src/lib.rs"))),
        "Research must deny Rust source writes"
    );
}

// ── 9. Per-folder deny-wins beats allow at the same level ────────────────────

#[test]
fn p10_09_per_folder_deny_wins() {
    let env = PermissionEnvelope::act_preset(vec!["src/**".into()], vec!["src/secrets/**".into()]);
    assert!(is_allow(
        &env.write.check(std::path::Path::new("src/main.rs"))
    ));
    assert!(is_deny(
        &env.write.check(std::path::Path::new("src/secrets/key.pem"))
    ));
    // Nested deny still wins.
    assert!(is_deny(
        &env.write
            .check(std::path::Path::new("src/secrets/deep/nested/x.key"))
    ));
}

// ── 10. Skill auto-load uses budget and dir-based layout ─────────────────────

#[test]
fn p10_10_skill_budget_caps_activation_count() {
    // Build a tempdir with 5 tiny SKILL.md files.
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join(".caduceus/skills");
    for name in ["alpha", "beta", "gamma", "delta", "epsilon"] {
        let d = skills_dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        let body = format!(
            "---\nname: {name}\ndescription: '{name} triggers on {name}-keyword here'\ntriggers: ['{name}-keyword']\n---\n\n# {name}\n\nbody"
        );
        std::fs::write(d.join("SKILL.md"), body).unwrap();
    }
    let loader = InstructionLoader::new(tmp.path());
    let set = loader.load().unwrap();
    assert_eq!(set.available_skills.len(), 5);

    // Match text that activates all 5.
    let msg = "alpha-keyword beta-keyword gamma-keyword delta-keyword epsilon-keyword";
    let activated_small = loader.resolve_lazy_with_budget(&set, msg, 2);
    assert!(
        activated_small.activated.len() <= 2,
        "skill_budget=2 must cap activation count (got {})",
        activated_small.activated.len()
    );
    let activated_large = loader.resolve_lazy_with_budget(&set, msg, 10);
    assert_eq!(
        activated_large.activated.len(),
        5,
        "skill_budget >= total must not truncate (got {})",
        activated_large.activated.len()
    );
}

// ── 11. Fan-out envelope inheritance smoke test (different shape from #6) ────

#[tokio::test]
async fn p10_11_fanout_policy_off_yields_no_workers() {
    struct NoopRunner;
    #[async_trait::async_trait]
    impl CritiqueRunner for NoopRunner {
        async fn critique(
            &self,
            _persona: &str,
            _prefix: &str,
            _plan: &str,
            _env: &PermissionEnvelope,
        ) -> Result<Critique, anyhow::Error> {
            unreachable!("policy Off must not spawn any worker")
        }
    }
    let mut env = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
    env.fanout_policy = FanoutPolicy::Off;
    let reg = PersonaRegistry::builtin_personas();
    let out = spawn_critique_fanout(&env, "plan", &["cloud", "qa"], &reg, &NoopRunner).await;
    assert!(out.is_empty());
}

// ── 12. Legacy mode strings still deserialize (serde aliases) ────────────────

#[test]
fn p10_12_legacy_mode_names_deserialize_to_4_mode_world() {
    let arch: AgentMode = serde_json::from_str(r#""Architect""#).unwrap();
    assert_eq!(arch, AgentMode::Plan);
    let review: AgentMode = serde_json::from_str(r#""review""#).unwrap();
    assert_eq!(review, AgentMode::Act);
    let debug: AgentMode = serde_json::from_str(r#""Debug""#).unwrap();
    assert_eq!(debug, AgentMode::Act);
    let auto: AgentMode = serde_json::from_str(r#""auto""#).unwrap();
    assert_eq!(auto, AgentMode::Autopilot);
}

// ── 13. Dynamic mode catalog — rubber-duck fanout uses engine-side list ──────

#[test]
fn p10_13_critique_fanout_personas_are_derived_from_policy_not_hardcoded() {
    // The policy + domains drive persona selection; no hardcoded mode-catalog
    // in the selector.
    let empty = plan_critique_personas(FanoutPolicy::Off, &["cloud", "qa"]);
    assert!(empty.is_empty());
    let rd = plan_critique_personas(FanoutPolicy::RubberDuckOnly, &["cloud", "qa"]);
    assert_eq!(rd, vec!["rubber-duck"]);
    let multi = plan_critique_personas(FanoutPolicy::MultiPersona, &["cloud", "ml", "qa"]);
    assert_eq!(
        multi,
        vec![
            "rubber-duck",
            "cloud-architect",
            "ml-architect",
            "qa-strategist"
        ]
    );
}

// ── 14. Prompt-injection guard: preamble instructs "treat tool output as data"

#[test]
fn p10_14_preamble_contains_prompt_injection_guard() {
    let prompt = behavior_rules_and_mode_prompt(AgentMode::Plan, ActLens::Normal);
    let lc = prompt.to_lowercase();
    // The exact wording lives in lib.rs; we anchor on the semantic tokens
    // that cannot appear by accident.
    assert!(
        lc.contains("untrusted") || lc.contains("ignore any imperatives"),
        "preamble missing prompt-injection guard:\n{prompt}"
    );
}
