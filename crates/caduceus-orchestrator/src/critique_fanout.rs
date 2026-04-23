//! P8 + P13 — critique fan-out with parallel execution and introspection.
//!
//! Shift-left critique of a plan or diff: spawn rubber-duck + domain-specialist
//! personas **concurrently** and collect their [`Critique`]s. Every worker sees
//! the parent [`PermissionEnvelope`] verbatim — no widening. Policy is encoded
//! on the envelope ([`FanoutPolicy`]); this module maps (policy, domains) →
//! persona list and runs them through a pluggable [`CritiqueRunner`].
//!
//! P13 — the driver also emits a live introspection event stream
//! (`FanoutStarted`, per-critic `StepAssigned` + `CritiqueEmitted` +
//! `AgentEdgeRecorded`, `FanoutCompleted`) through an optional
//! [`IntrospectionSink`] so UIs can render the Agents-DAG live.

use crate::modes::PersonaRegistry;
use crate::scoped_context::{ContextInjector, ScopeRequest};
use async_trait::async_trait;
use caduceus_core::{
    AgentEdgeKind, AssignmentSummaryV1, Critique, CritiqueSeverity, ExecutionId,
    IntrospectionEventV1, StepId,
};
use caduceus_permissions::envelope::{FanoutPolicy, PermissionEnvelope};
use std::sync::atomic::{AtomicU64, Ordering};

/// P13 — pluggable introspection event sink.
#[async_trait]
pub trait IntrospectionSink: Send + Sync {
    async fn emit(&self, event: IntrospectionEventV1);
}

pub struct FanoutIntrospectionCtx<'a> {
    pub sink: &'a dyn IntrospectionSink,
    pub primary_execution_id: ExecutionId,
    pub step_id: StepId,
    pub execution_id_allocator: &'a AtomicU64,
}

impl<'a> FanoutIntrospectionCtx<'a> {
    fn next_execution_id(&self) -> ExecutionId {
        ExecutionId(self.execution_id_allocator.fetch_add(1, Ordering::Relaxed))
    }
}

/// Pluggable critique worker.
///
/// **New scoped entry point.** [`critique_scoped`] takes a pre-sliced
/// [`ScopedContext`] built by a [`ContextInjector`]; runners that understand
/// the scoped shape should override it. The default impl transparently
/// forwards to the legacy [`critique`] method using the scoped fields, so
/// every existing runner keeps working unchanged.
#[async_trait]
pub trait CritiqueRunner: Send + Sync {
    async fn critique(
        &self,
        persona: &str,
        system_prompt_prefix: &str,
        plan_draft: &str,
        envelope: &PermissionEnvelope,
    ) -> Result<Critique, anyhow::Error>;

    /// Scoped variant — receives a narrowly-scoped, concise
    /// [`crate::scoped_context::ScopedContext`] instead of raw plan text.
    /// Override to consume the scoped fields directly; the default
    /// implementation flattens back to the legacy signature so existing
    /// runners keep working.
    async fn critique_scoped(
        &self,
        ctx: &crate::scoped_context::ScopedContext,
        envelope: &PermissionEnvelope,
    ) -> Result<Critique, anyhow::Error> {
        self.critique(&ctx.persona, &ctx.persona_role, &ctx.plan_slice, envelope)
            .await
    }

    fn model_metadata(&self, _persona: &str) -> (String, String) {
        ("unknown".to_string(), "unknown".to_string())
    }
}

pub fn plan_critique_personas(policy: FanoutPolicy, domains: &[&str]) -> Vec<&'static str> {
    match policy {
        FanoutPolicy::Off => Vec::new(),
        FanoutPolicy::RubberDuckOnly => vec!["rubber-duck"],
        FanoutPolicy::MultiPersona => {
            let mut out: Vec<&'static str> = vec!["rubber-duck"];
            for dom in domains {
                let p: Option<&'static str> = match *dom {
                    "cloud" | "infra" | "cloud-infra" => Some("cloud-architect"),
                    "algorithmic-ml" | "ml" | "ml-infra" => Some("ml-architect"),
                    "data-pipeline" => Some("data-engineer"),
                    "data-research" => Some("data-researcher"),
                    "data-model" | "statistics" | "experimentation" => Some("data-scientist"),
                    "qa" | "testing" | "test" => Some("qa-strategist"),
                    _ => None,
                };
                if let Some(name) = p {
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
            out
        }
    }
}

pub async fn spawn_critique_fanout(
    envelope: &PermissionEnvelope,
    plan_draft: &str,
    domains: &[&str],
    registry: &PersonaRegistry,
    runner: &dyn CritiqueRunner,
) -> Vec<Critique> {
    spawn_critique_fanout_with_introspection(envelope, plan_draft, domains, registry, runner, None)
        .await
}

/// ST-B1 / contract `harness-sink-v1` — convenience call site that pulls
/// the [`IntrospectionSink`] and [`PermissionEnvelope`] from an
/// [`crate::AgentHarness`] so the caller doesn't have to unpack them
/// manually. This is the **only** fan-out entry point the bridge and IDE
/// should use — keeping the two DAGs wired end-to-end is a precondition
/// for the Agents-DAG to ever render.
///
/// Behaviour:
///   - If the harness has no envelope, falls back to
///     [`PermissionEnvelope::plan_preset`] (safest default — no writes).
///   - If the harness has no sink, drops introspection events silently
///     (legacy no-op).
///   - Otherwise wires sink + envelope + allocates an
///     [`ExecutionId`]/[`StepId`] pair for this fan-out invocation.
pub async fn spawn_critique_fanout_via_harness(
    harness: &crate::AgentHarness,
    plan_draft: &str,
    domains: &[&str],
    registry: &PersonaRegistry,
    runner: &dyn CritiqueRunner,
) -> Vec<Critique> {
    use caduceus_core::{ExecutionId, StepId};

    let default_env;
    let envelope = match harness.permission_envelope() {
        Some(e) => e,
        None => {
            tracing::warn!(
                "spawn_critique_fanout_via_harness called on harness with no permission \
                 envelope; falling back to plan_preset() (no writes). Callers should \
                 configure an envelope explicitly."
            );
            default_env = PermissionEnvelope::plan_preset();
            &default_env
        }
    };

    let execution_id_allocator = AtomicU64::new(1);
    let primary_execution_id = ExecutionId(execution_id_allocator.fetch_add(1, Ordering::Relaxed));

    let sink = harness.introspection_sink();
    let injector = harness.context_injector().map(|arc| arc.as_ref());
    match sink {
        Some(sink_arc) => {
            let ctx = FanoutIntrospectionCtx {
                sink: sink_arc.as_ref(),
                primary_execution_id,
                step_id: StepId(0),
                execution_id_allocator: &execution_id_allocator,
            };
            spawn_critique_fanout_with_injection(
                envelope,
                plan_draft,
                domains,
                registry,
                runner,
                Some(&ctx),
                injector,
            )
            .await
        }
        None => {
            spawn_critique_fanout_with_injection(
                envelope, plan_draft, domains, registry, runner, None, injector,
            )
            .await
        }
    }
}

pub async fn spawn_critique_fanout_with_introspection(
    envelope: &PermissionEnvelope,
    plan_draft: &str,
    domains: &[&str],
    registry: &PersonaRegistry,
    runner: &dyn CritiqueRunner,
    introspection: Option<&FanoutIntrospectionCtx<'_>>,
) -> Vec<Critique> {
    spawn_critique_fanout_with_injection(
        envelope,
        plan_draft,
        domains,
        registry,
        runner,
        introspection,
        None,
    )
    .await
}

/// Full-fidelity entry point: lets the caller pass a [`ContextInjector`]
/// that the driver calls **lazily inside each parallel critic task** to
/// produce a per-persona [`crate::scoped_context::ScopedContext`]. When
/// `injector` is `None` the driver takes the legacy path (full plan +
/// full prefix to every critic) so existing callers are unaffected.
pub async fn spawn_critique_fanout_with_injection(
    envelope: &PermissionEnvelope,
    plan_draft: &str,
    domains: &[&str],
    registry: &PersonaRegistry,
    runner: &dyn CritiqueRunner,
    introspection: Option<&FanoutIntrospectionCtx<'_>>,
    injector: Option<&dyn ContextInjector>,
) -> Vec<Critique> {
    let personas = plan_critique_personas(envelope.fanout_policy, domains);

    enum Slot {
        Runnable { name: &'static str, prefix: String },
        Skipped { name: &'static str },
    }
    let slots: Vec<Slot> = personas
        .iter()
        .map(|&name| match registry.get(name) {
            Some(p) => Slot::Runnable {
                name,
                prefix: p.system_prompt_prefix.clone(),
            },
            None => Slot::Skipped { name },
        })
        .collect();

    let runnable_personas: Vec<String> = slots
        .iter()
        .filter_map(|s| match s {
            Slot::Runnable { name, .. } => Some(name.to_string()),
            Slot::Skipped { .. } => None,
        })
        .collect();
    let runnable_count = runnable_personas.len() as u32;

    if let Some(ctx) = introspection {
        ctx.sink
            .emit(IntrospectionEventV1::FanoutStarted {
                step_id: ctx.step_id,
                parent_execution_id: ctx.primary_execution_id,
                critic_count: runnable_count,
                personas: runnable_personas,
            })
            .await;
    }

    let prepared: Vec<(Slot, Option<ExecutionId>)> = slots
        .into_iter()
        .map(|s| {
            let eid = match (&s, introspection) {
                (Slot::Runnable { .. }, Some(ctx)) => Some(ctx.next_execution_id()),
                _ => None,
            };
            (s, eid)
        })
        .collect();

    let futs = prepared.into_iter().map(|(slot, eid)| async move {
        match slot {
            Slot::Runnable { name, prefix } => {
                if let (Some(ctx), Some(critic_eid)) = (introspection, eid) {
                    let (vendor, tier) = runner.model_metadata(name);
                    ctx.sink
                        .emit(IntrospectionEventV1::StepAssigned {
                            assignment: AssignmentSummaryV1 {
                                execution_id: critic_eid,
                                step_id: ctx.step_id,
                                persona_id: name.to_string(),
                                model_vendor: vendor,
                                model_tier: tier,
                                model_id_exact: None,
                                activated_skills_count: 0,
                                activated_agents_count: 0,
                                activated_skill_names: None,
                                activated_agent_names: None,
                                attempt: 1,
                            },
                        })
                        .await;
                }

                // Lazy scoped-context injection: if an injector is present,
                // build the persona-specific context RIGHT NOW, inside this
                // task, after join_all has parallelised us. Feed the runner
                // through its scoped entry point so it sees only the
                // narrow slice. Without an injector we take the legacy
                // path unchanged.
                let critique_result = match injector {
                    Some(inj) => {
                        let scoped = inj.scope_for(ScopeRequest {
                            persona: name,
                            persona_prefix: &prefix,
                            step_id: introspection.map(|c| c.step_id).unwrap_or(StepId(0)),
                            plan_draft,
                            envelope,
                            domains,
                        });
                        runner.critique_scoped(&scoped, envelope).await
                    }
                    None => runner.critique(name, &prefix, plan_draft, envelope).await,
                };

                let critique = match critique_result {
                    Ok(c) => c,
                    Err(e) => Critique {
                        persona: name.to_string(),
                        severity: CritiqueSeverity::Critical,
                        findings: vec![format!("critique runner failed: {e}")],
                        blocking: true,
                    },
                };

                if let (Some(ctx), Some(critic_eid)) = (introspection, eid) {
                    ctx.sink
                        .emit(IntrospectionEventV1::CritiqueEmitted {
                            from_execution_id: critic_eid,
                            target_execution_id: ctx.primary_execution_id,
                            severity: critique.severity,
                            blocking: critique.blocking,
                        })
                        .await;
                    ctx.sink
                        .emit(IntrospectionEventV1::AgentEdgeRecorded {
                            edge: AgentEdgeKind::Critique,
                            from_execution_id: critic_eid,
                            to_execution_id: ctx.primary_execution_id,
                        })
                        .await;
                }

                critique
            }
            Slot::Skipped { name } => Critique {
                persona: name.to_string(),
                severity: CritiqueSeverity::Info,
                findings: vec![format!("persona '{name}' not registered in this build")],
                blocking: false,
            },
        }
    });

    let out: Vec<Critique> = futures::future::join_all(futs).await;

    if let Some(ctx) = introspection {
        let blocking_count = out.iter().filter(|c| c.blocking).count() as u32;
        ctx.sink
            .emit(IntrospectionEventV1::FanoutCompleted {
                step_id: ctx.step_id,
                parent_execution_id: ctx.primary_execution_id,
                critic_count: runnable_count,
                blocking_count,
            })
            .await;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_permissions::envelope::PermissionEnvelope;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    struct StubRunner;
    #[async_trait]
    impl CritiqueRunner for StubRunner {
        async fn critique(
            &self,
            persona: &str,
            _prefix: &str,
            plan_draft: &str,
            envelope: &PermissionEnvelope,
        ) -> Result<Critique, anyhow::Error> {
            Ok(Critique {
                persona: persona.to_string(),
                severity: CritiqueSeverity::Warn,
                findings: vec![
                    format!("saw plan of length {}", plan_draft.len()),
                    format!("envelope.fanout={:?}", envelope.fanout_policy),
                ],
                blocking: false,
            })
        }
        fn model_metadata(&self, _persona: &str) -> (String, String) {
            ("anthropic".into(), "opus".into())
        }
    }

    #[derive(Default)]
    struct CaptureSink {
        events: Mutex<Vec<IntrospectionEventV1>>,
    }
    #[async_trait]
    impl IntrospectionSink for CaptureSink {
        async fn emit(&self, event: IntrospectionEventV1) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn policy_off_returns_no_personas() {
        let v = plan_critique_personas(FanoutPolicy::Off, &["algorithmic-ml"]);
        assert!(v.is_empty());
    }

    #[test]
    fn policy_rubber_duck_only_is_just_rubber_duck() {
        let v = plan_critique_personas(FanoutPolicy::RubberDuckOnly, &["cloud", "qa"]);
        assert_eq!(v, vec!["rubber-duck"]);
    }

    #[test]
    fn policy_multi_persona_adds_domain_specialists_stable_and_deduped() {
        let v = plan_critique_personas(
            FanoutPolicy::MultiPersona,
            &["cloud", "algorithmic-ml", "cloud", "qa"],
        );
        assert_eq!(
            v,
            vec![
                "rubber-duck",
                "cloud-architect",
                "ml-architect",
                "qa-strategist"
            ]
        );
    }

    #[test]
    fn unknown_domain_tag_is_silently_ignored() {
        let v = plan_critique_personas(FanoutPolicy::MultiPersona, &["quantum-alchemy"]);
        assert_eq!(v, vec!["rubber-duck"]);
    }

    #[tokio::test]
    async fn fanout_cascades_envelope_to_every_worker() {
        let env = PermissionEnvelope::research_preset();
        let reg = PersonaRegistry::builtin_personas();
        let got = spawn_critique_fanout(
            &env,
            "plan body",
            &["cloud", "algorithmic-ml", "qa"],
            &reg,
            &StubRunner,
        )
        .await;
        assert_eq!(got.len(), 4);
        for c in &got {
            assert!(c
                .findings
                .iter()
                .any(|f| f.contains(&format!("{:?}", env.fanout_policy))));
            assert!(c.findings.iter().any(|f| f.contains("plan of length 9")));
        }
        let names: Vec<&str> = got.iter().map(|c| c.persona.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "rubber-duck",
                "cloud-architect",
                "ml-architect",
                "qa-strategist"
            ]
        );
    }

    #[tokio::test]
    async fn policy_off_yields_empty_fanout() {
        let mut env = PermissionEnvelope::plan_preset();
        env.fanout_policy = FanoutPolicy::Off;
        let reg = PersonaRegistry::builtin_personas();
        let got = spawn_critique_fanout(&env, "plan", &["cloud"], &reg, &StubRunner).await;
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn p13_fanout_emits_start_then_per_critic_then_complete() {
        let env = PermissionEnvelope::research_preset();
        let reg = PersonaRegistry::builtin_personas();
        let sink = CaptureSink::default();
        let alloc = AtomicU64::new(100);
        let ctx = FanoutIntrospectionCtx {
            sink: &sink,
            primary_execution_id: ExecutionId(1),
            step_id: StepId(42),
            execution_id_allocator: &alloc,
        };

        let got = spawn_critique_fanout_with_introspection(
            &env,
            "plan body",
            &["cloud", "qa"],
            &reg,
            &StubRunner,
            Some(&ctx),
        )
        .await;
        assert_eq!(got.len(), 3);

        let events = sink.events.lock().unwrap().clone();
        assert_eq!(events.len(), 11, "got {events:?}");

        match &events[0] {
            IntrospectionEventV1::FanoutStarted {
                step_id,
                parent_execution_id,
                critic_count,
                personas,
            } => {
                assert_eq!(*step_id, StepId(42));
                assert_eq!(*parent_execution_id, ExecutionId(1));
                assert_eq!(*critic_count, 3);
                assert_eq!(
                    personas,
                    &vec![
                        "rubber-duck".to_string(),
                        "cloud-architect".to_string(),
                        "qa-strategist".to_string(),
                    ]
                );
            }
            other => panic!("expected FanoutStarted first, got {other:?}"),
        }

        match events.last().unwrap() {
            IntrospectionEventV1::FanoutCompleted {
                step_id,
                critic_count,
                blocking_count,
                ..
            } => {
                assert_eq!(*step_id, StepId(42));
                assert_eq!(*critic_count, 3);
                assert_eq!(*blocking_count, 0);
            }
            other => panic!("expected FanoutCompleted last, got {other:?}"),
        }

        let middle = &events[1..events.len() - 1];
        assert_eq!(middle.len(), 9);
        let mut per_critic: std::collections::HashMap<u64, Vec<&IntrospectionEventV1>> =
            std::collections::HashMap::new();
        for ev in middle {
            let eid = match ev {
                IntrospectionEventV1::StepAssigned { assignment } => assignment.execution_id.0,
                IntrospectionEventV1::CritiqueEmitted {
                    from_execution_id, ..
                }
                | IntrospectionEventV1::AgentEdgeRecorded {
                    from_execution_id, ..
                } => from_execution_id.0,
                other => panic!("unexpected mid-batch event {other:?}"),
            };
            per_critic.entry(eid).or_default().push(ev);
        }
        assert_eq!(per_critic.len(), 3);
        for expected in [100u64, 101, 102] {
            let evs = per_critic
                .get(&expected)
                .unwrap_or_else(|| panic!("missing execution_id {expected}"));
            assert_eq!(evs.len(), 3);
            assert!(matches!(evs[0], IntrospectionEventV1::StepAssigned { .. }));
            assert!(matches!(
                evs[1],
                IntrospectionEventV1::CritiqueEmitted { .. }
            ));
            assert!(matches!(
                evs[2],
                IntrospectionEventV1::AgentEdgeRecorded { .. }
            ));
        }
        assert_eq!(alloc.load(Ordering::Relaxed), 103);
    }

    #[tokio::test]
    async fn p13_fanout_no_sink_emits_nothing() {
        let env = PermissionEnvelope::research_preset();
        let reg = PersonaRegistry::builtin_personas();
        let got = spawn_critique_fanout_with_introspection(
            &env,
            "plan",
            &["cloud"],
            &reg,
            &StubRunner,
            None,
        )
        .await;
        assert_eq!(got.len(), 2);
    }

    struct FailingRunner;
    #[async_trait]
    impl CritiqueRunner for FailingRunner {
        async fn critique(
            &self,
            _persona: &str,
            _prefix: &str,
            _plan: &str,
            _env: &PermissionEnvelope,
        ) -> Result<Critique, anyhow::Error> {
            Err(anyhow::anyhow!("synthetic failure"))
        }
    }

    #[tokio::test]
    async fn p13_failed_critique_still_emits_events_with_blocking_critical() {
        let env = PermissionEnvelope::research_preset();
        let reg = PersonaRegistry::builtin_personas();
        let sink = CaptureSink::default();
        let alloc = AtomicU64::new(1);
        let ctx = FanoutIntrospectionCtx {
            sink: &sink,
            primary_execution_id: ExecutionId(0),
            step_id: StepId(1),
            execution_id_allocator: &alloc,
        };

        let got = spawn_critique_fanout_with_introspection(
            &env,
            "plan",
            &[],
            &reg,
            &FailingRunner,
            Some(&ctx),
        )
        .await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].severity, CritiqueSeverity::Critical);
        assert!(got[0].blocking);

        let events = sink.events.lock().unwrap().clone();
        assert_eq!(events.len(), 5);
        match &events[0] {
            IntrospectionEventV1::FanoutStarted { critic_count, .. } => {
                assert_eq!(*critic_count, 1);
            }
            other => panic!("expected FanoutStarted, got {other:?}"),
        }
        match events.last().unwrap() {
            IntrospectionEventV1::FanoutCompleted { blocking_count, .. } => {
                assert_eq!(*blocking_count, 1);
            }
            other => panic!("expected FanoutCompleted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn p13_critics_actually_run_in_parallel() {
        struct SleepRunner;
        #[async_trait]
        impl CritiqueRunner for SleepRunner {
            async fn critique(
                &self,
                persona: &str,
                _prefix: &str,
                _plan: &str,
                _env: &PermissionEnvelope,
            ) -> Result<Critique, anyhow::Error> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(Critique {
                    persona: persona.to_string(),
                    severity: CritiqueSeverity::Info,
                    findings: vec!["slept 200ms".into()],
                    blocking: false,
                })
            }
        }

        let env = PermissionEnvelope::research_preset();
        let reg = PersonaRegistry::builtin_personas();
        let start = Instant::now();
        let got = spawn_critique_fanout(
            &env,
            "plan",
            &["cloud", "algorithmic-ml", "qa"],
            &reg,
            &SleepRunner,
        )
        .await;
        let elapsed = start.elapsed();
        assert_eq!(got.len(), 4);
        assert!(
            elapsed < Duration::from_millis(500),
            "4 critics × 200ms ran in {elapsed:?} — not parallel (serial ≥800ms)"
        );
    }

    #[tokio::test]
    async fn p13_returned_critique_order_is_stable_under_parallel_completion() {
        struct VariableSleepRunner;
        #[async_trait]
        impl CritiqueRunner for VariableSleepRunner {
            async fn critique(
                &self,
                persona: &str,
                _prefix: &str,
                _plan: &str,
                _env: &PermissionEnvelope,
            ) -> Result<Critique, anyhow::Error> {
                let ms = match persona {
                    "rubber-duck" => 150,
                    "cloud-architect" => 30,
                    "ml-architect" => 60,
                    "qa-strategist" => 10,
                    _ => 0,
                };
                tokio::time::sleep(Duration::from_millis(ms)).await;
                Ok(Critique {
                    persona: persona.to_string(),
                    severity: CritiqueSeverity::Info,
                    findings: vec![],
                    blocking: false,
                })
            }
        }

        let env = PermissionEnvelope::research_preset();
        let reg = PersonaRegistry::builtin_personas();
        let got = spawn_critique_fanout(
            &env,
            "plan",
            &["cloud", "algorithmic-ml", "qa"],
            &reg,
            &VariableSleepRunner,
        )
        .await;
        let names: Vec<&str> = got.iter().map(|c| c.persona.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "rubber-duck",
                "cloud-architect",
                "ml-architect",
                "qa-strategist"
            ],
            "output order must match plan_critique_personas() regardless of timing"
        );
    }

    // ── Lazy scoped-context injection tests ────────────────────────────────

    /// Runner that asserts it ALWAYS receives a ScopedContext (never the
    /// legacy path) and captures every scope it sees.
    struct ScopedCapturingRunner {
        seen: Mutex<Vec<crate::scoped_context::ScopedContext>>,
        legacy_called: std::sync::atomic::AtomicBool,
    }
    impl ScopedCapturingRunner {
        fn new() -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
                legacy_called: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }
    #[async_trait]
    impl CritiqueRunner for ScopedCapturingRunner {
        async fn critique(
            &self,
            persona: &str,
            _prefix: &str,
            _plan: &str,
            _env: &PermissionEnvelope,
        ) -> Result<Critique, anyhow::Error> {
            self.legacy_called
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Critique {
                persona: persona.to_string(),
                severity: CritiqueSeverity::Info,
                findings: vec!["legacy path".into()],
                blocking: false,
            })
        }
        async fn critique_scoped(
            &self,
            ctx: &crate::scoped_context::ScopedContext,
            _env: &PermissionEnvelope,
        ) -> Result<Critique, anyhow::Error> {
            self.seen.lock().unwrap().push(ctx.clone());
            Ok(Critique {
                persona: ctx.persona.clone(),
                severity: CritiqueSeverity::Info,
                findings: vec![format!("scoped role={}", ctx.persona_role)],
                blocking: false,
            })
        }
        fn model_metadata(&self, _persona: &str) -> (String, String) {
            ("anthropic".into(), "opus".into())
        }
    }

    #[tokio::test]
    async fn p13_injector_routes_runner_through_critique_scoped_not_legacy() {
        use crate::scoped_context::BuiltinScopedContextInjector;
        let env = {
            let mut e = PermissionEnvelope::research_preset();
            e.fanout_policy = FanoutPolicy::MultiPersona;
            e
        };
        let reg = PersonaRegistry::builtin_personas();
        let runner = ScopedCapturingRunner::new();
        let inj = BuiltinScopedContextInjector::default();
        let plan = "cost scaling azure region latency plan body";
        let out = spawn_critique_fanout_with_injection(
            &env,
            plan,
            &["cloud"],
            &reg,
            &runner,
            None,
            Some(&inj),
        )
        .await;
        assert_eq!(out.len(), 2, "rubber-duck + cloud-architect");
        assert!(
            !runner
                .legacy_called
                .load(std::sync::atomic::Ordering::SeqCst),
            "legacy critique() must NOT be called when injector is provided"
        );
        let seen = runner.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        // The cloud-architect persona's scoped context carries its own
        // focus area and persona role — no cross-contamination.
        let cloud = seen
            .iter()
            .find(|s| s.persona == "cloud-architect")
            .unwrap();
        let duck = seen.iter().find(|s| s.persona == "rubber-duck").unwrap();
        assert!(cloud.focus_area.to_lowercase().contains("cloud"));
        assert!(duck.focus_area.to_lowercase().contains("reasoning"));
        assert_ne!(cloud.persona_role, duck.persona_role);
    }

    #[tokio::test]
    async fn p13_injector_absent_means_legacy_path_unchanged() {
        let env = {
            let mut e = PermissionEnvelope::research_preset();
            e.fanout_policy = FanoutPolicy::MultiPersona;
            e
        };
        let reg = PersonaRegistry::builtin_personas();
        let runner = ScopedCapturingRunner::new();
        let out = spawn_critique_fanout_with_injection(
            &env,
            "plan body",
            &["qa"],
            &reg,
            &runner,
            None,
            None, // no injector
        )
        .await;
        assert!(!out.is_empty());
        assert!(
            runner
                .legacy_called
                .load(std::sync::atomic::Ordering::SeqCst),
            "legacy critique() MUST be called when no injector is provided"
        );
        assert!(
            runner.seen.lock().unwrap().is_empty(),
            "critique_scoped() must NOT be called without an injector"
        );
    }

    /// Lazy-eval proof: the injector MUST be called exactly once per runnable
    /// persona and NEVER for skipped personas. We also prove the injector
    /// sees per-critic inputs (persona name + prefix differ across calls).
    #[tokio::test]
    async fn p13_injector_called_lazily_once_per_runnable_persona() {
        struct CountingInjector {
            count: std::sync::atomic::AtomicUsize,
            personas: Mutex<Vec<String>>,
        }
        impl ContextInjector for CountingInjector {
            fn scope_for(&self, req: ScopeRequest<'_>) -> crate::scoped_context::ScopedContext {
                self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.personas.lock().unwrap().push(req.persona.to_string());
                crate::scoped_context::PassthroughContextInjector.scope_for(req)
            }
        }
        let inj = CountingInjector {
            count: std::sync::atomic::AtomicUsize::new(0),
            personas: Mutex::new(Vec::new()),
        };

        let env = {
            let mut e = PermissionEnvelope::research_preset();
            e.fanout_policy = FanoutPolicy::MultiPersona;
            e
        };
        let reg = PersonaRegistry::builtin_personas();
        let runner = StubRunner;
        let _ = spawn_critique_fanout_with_injection(
            &env,
            "body",
            &["cloud", "qa"],
            &reg,
            &runner,
            None,
            Some(&inj),
        )
        .await;
        // 3 runnable personas: rubber-duck + cloud-architect + qa-strategist.
        assert_eq!(
            inj.count.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "injector must run exactly once per runnable persona"
        );
        let mut names = inj.personas.lock().unwrap().clone();
        names.sort();
        assert_eq!(
            names,
            vec!["cloud-architect", "qa-strategist", "rubber-duck"]
        );
    }

    /// Parallelism proof for the injected path: 3 critics each with a
    /// 150ms scoped sleep must complete in well under 300ms.
    #[tokio::test]
    async fn p13_injected_critics_still_run_in_parallel() {
        struct SlowScopedRunner;
        #[async_trait]
        impl CritiqueRunner for SlowScopedRunner {
            async fn critique(
                &self,
                _persona: &str,
                _prefix: &str,
                _plan: &str,
                _env: &PermissionEnvelope,
            ) -> Result<Critique, anyhow::Error> {
                unreachable!("injector path should always hit critique_scoped");
            }
            async fn critique_scoped(
                &self,
                ctx: &crate::scoped_context::ScopedContext,
                _env: &PermissionEnvelope,
            ) -> Result<Critique, anyhow::Error> {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok(Critique {
                    persona: ctx.persona.clone(),
                    severity: CritiqueSeverity::Info,
                    findings: vec![],
                    blocking: false,
                })
            }
            fn model_metadata(&self, _persona: &str) -> (String, String) {
                ("anthropic".into(), "opus".into())
            }
        }
        let env = {
            let mut e = PermissionEnvelope::research_preset();
            e.fanout_policy = FanoutPolicy::MultiPersona;
            e
        };
        let reg = PersonaRegistry::builtin_personas();
        let inj = crate::scoped_context::PassthroughContextInjector;
        let start = Instant::now();
        let _ = spawn_critique_fanout_with_injection(
            &env,
            "body",
            &["cloud", "qa"],
            &reg,
            &SlowScopedRunner,
            None,
            Some(&inj),
        )
        .await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(400),
            "3 × 150ms parallel critics should finish well under 400ms, got {elapsed:?}"
        );
    }

    // ── ST-B1 — harness-aware fan-out ────────────────────────────────────

    /// Contract `harness-sink-v1` (decomposition §5): when the harness
    /// carries an introspection sink, `spawn_critique_fanout_via_harness`
    /// must route every `IntrospectionEventV1` variant through that sink.
    /// This is the seam that makes the Agents-DAG render in the IDE.
    #[tokio::test]
    async fn via_harness_routes_through_installed_sink() {
        use caduceus_providers::mock::MockLlmAdapter;
        use caduceus_tools::ToolRegistry;

        let sink = Arc::new(CaptureSink::default());
        let provider = Arc::new(MockLlmAdapter::new(vec![]));
        let tools = ToolRegistry::new();
        let env = {
            let mut e = PermissionEnvelope::research_preset();
            e.fanout_policy = FanoutPolicy::MultiPersona;
            e
        };
        let harness = crate::AgentHarness::new(provider, tools, 8192, "test")
            .with_permission_envelope(env)
            .with_introspection_sink(sink.clone() as Arc<dyn IntrospectionSink>);

        let reg = PersonaRegistry::builtin_personas();
        let got = spawn_critique_fanout_via_harness(
            &harness,
            "plan body",
            &["cloud", "qa"],
            &reg,
            &StubRunner,
        )
        .await;

        // 3 runnable personas: rubber-duck + cloud-architect + qa-strategist.
        assert_eq!(got.len(), 3);

        let events = sink.events.lock().unwrap();
        assert!(
            matches!(
                events.first(),
                Some(IntrospectionEventV1::FanoutStarted { .. })
            ),
            "first event must be FanoutStarted, got {:?}",
            events.first()
        );
        assert!(
            matches!(
                events.last(),
                Some(IntrospectionEventV1::FanoutCompleted { .. })
            ),
            "last event must be FanoutCompleted, got {:?}",
            events.last()
        );
        let per_critic_events = events.len() - 2; // minus Started/Completed
                                                  // Each critic should emit at least StepAssigned + CritiqueEmitted + AgentEdgeRecorded.
        assert!(
            per_critic_events >= 3 * 3,
            "expected ≥ 9 per-critic events for 3 critics, got {per_critic_events}"
        );
    }

    /// When the harness carries no sink, `spawn_critique_fanout_via_harness`
    /// must still run the fan-out and return results — introspection is a
    /// non-blocking side channel.
    #[tokio::test]
    async fn via_harness_no_sink_still_runs_fanout() {
        use caduceus_providers::mock::MockLlmAdapter;
        use caduceus_tools::ToolRegistry;

        let provider = Arc::new(MockLlmAdapter::new(vec![]));
        let tools = ToolRegistry::new();
        let env = {
            let mut e = PermissionEnvelope::research_preset();
            e.fanout_policy = FanoutPolicy::RubberDuckOnly;
            e
        };
        let harness =
            crate::AgentHarness::new(provider, tools, 8192, "test").with_permission_envelope(env);
        // No sink installed.

        let reg = PersonaRegistry::builtin_personas();
        let got =
            spawn_critique_fanout_via_harness(&harness, "plan body", &["cloud"], &reg, &StubRunner)
                .await;

        assert_eq!(
            got.len(),
            1,
            "RubberDuckOnly should produce exactly one critique"
        );
        assert_eq!(got[0].persona, "rubber-duck");
    }

    /// When the harness has no envelope, `spawn_critique_fanout_via_harness`
    /// falls back to `plan_preset` (RubberDuckOnly by default). This guards
    /// against a regression where a misconfigured harness would silently
    /// skip fan-out entirely.
    #[tokio::test]
    async fn via_harness_defaults_to_plan_preset_when_envelope_missing() {
        use caduceus_providers::mock::MockLlmAdapter;
        use caduceus_tools::ToolRegistry;

        let provider = Arc::new(MockLlmAdapter::new(vec![]));
        let tools = ToolRegistry::new();
        let harness = crate::AgentHarness::new(provider, tools, 8192, "test");
        // No envelope, no sink.

        let reg = PersonaRegistry::builtin_personas();
        let got =
            spawn_critique_fanout_via_harness(&harness, "plan body", &["cloud"], &reg, &StubRunner)
                .await;

        // plan_preset = RubberDuckOnly → exactly one critique.
        assert_eq!(got.len(), 1);
    }

    // ── ST-B3 — context-injector-v1 ──────────────────────────────────────

    /// Contract `context-injector-v1`: when the harness carries a
    /// context injector, `spawn_critique_fanout_via_harness` MUST hand it
    /// to every parallel critic task. The driver already proves this
    /// via `spawn_critique_fanout_with_injection` — this test closes the
    /// harness-aware loop so the bridge/IDE path doesn't silently drop
    /// the injector.
    #[tokio::test]
    async fn via_harness_routes_through_installed_injector() {
        use crate::scoped_context::{ContextInjector, ScopeRequest, ScopedContext};
        use caduceus_providers::mock::MockLlmAdapter;
        use caduceus_tools::ToolRegistry;
        use std::sync::atomic::AtomicUsize;

        struct CountingInjector {
            count: AtomicUsize,
            personas: Mutex<Vec<String>>,
        }
        impl ContextInjector for CountingInjector {
            fn scope_for(&self, req: ScopeRequest<'_>) -> ScopedContext {
                self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.personas.lock().unwrap().push(req.persona.to_string());
                crate::scoped_context::PassthroughContextInjector.scope_for(req)
            }
        }
        let inj = Arc::new(CountingInjector {
            count: AtomicUsize::new(0),
            personas: Mutex::new(vec![]),
        });

        let provider = Arc::new(MockLlmAdapter::new(vec![]));
        let tools = ToolRegistry::new();
        let env = {
            let mut e = PermissionEnvelope::research_preset();
            e.fanout_policy = FanoutPolicy::MultiPersona;
            e
        };
        let harness = crate::AgentHarness::new(provider, tools, 8192, "test")
            .with_permission_envelope(env)
            .with_context_injector(inj.clone() as Arc<dyn ContextInjector>);

        let reg = PersonaRegistry::builtin_personas();
        let got = spawn_critique_fanout_via_harness(
            &harness,
            "plan body",
            &["cloud", "qa"],
            &reg,
            &StubRunner,
        )
        .await;
        assert_eq!(got.len(), 3);
        assert_eq!(
            inj.count.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "injector must run exactly once per runnable persona"
        );
        let mut names = inj.personas.lock().unwrap().clone();
        names.sort();
        assert_eq!(
            names,
            vec!["cloud-architect", "qa-strategist", "rubber-duck"]
        );
    }

    /// Contract `context-injector-v1`: when no injector is installed,
    /// the driver falls back to the legacy "full plan to every critic"
    /// path. This is the byte-for-byte preserved behaviour the
    /// decomposition promises ("additive; default preserves current
    /// behavior"). A regression here would silently scope context
    /// differently from before and break every caller that doesn't
    /// opt in.
    #[tokio::test]
    async fn via_harness_without_injector_preserves_legacy_full_plan_path() {
        use caduceus_providers::mock::MockLlmAdapter;
        use caduceus_tools::ToolRegistry;

        // The StubRunner used here records the full plan draft into the
        // findings; when no injector is installed every critic should
        // see the SAME full body. This mirrors `fanout_cascades_envelope_…`
        // but exercises the via-harness path so we don't just retest the
        // direct driver.
        let provider = Arc::new(MockLlmAdapter::new(vec![]));
        let tools = ToolRegistry::new();
        let env = {
            let mut e = PermissionEnvelope::research_preset();
            e.fanout_policy = FanoutPolicy::MultiPersona;
            e
        };
        let harness =
            crate::AgentHarness::new(provider, tools, 8192, "test").with_permission_envelope(env);
        // No injector installed.

        let reg = PersonaRegistry::builtin_personas();
        let got = spawn_critique_fanout_via_harness(
            &harness,
            "plan body", // len 9
            &["cloud", "qa"],
            &reg,
            &StubRunner,
        )
        .await;
        assert_eq!(got.len(), 3);
        for c in &got {
            assert!(
                c.findings.iter().any(|f| f.contains("plan of length 9")),
                "legacy path must feed full plan body to every critic; got {:?}",
                c.findings
            );
        }
    }
}
