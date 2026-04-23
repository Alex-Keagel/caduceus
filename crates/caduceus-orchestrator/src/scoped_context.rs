//! P13 follow-up — **lazy scoped context injection** for sub-agents.
//!
//! ## Problem
//!
//! Historically the critique fan-out handed every critic the *same* sprawling
//! context: the full `plan_draft` string and the full persona
//! `system_prompt_prefix`. Every persona saw every word — maximum
//! hallucination surface, minimum focus.
//!
//! ## Design — manager-owned scoping, evaluated lazily
//!
//! The orchestrator is the **single owner of full context**. Each sub-agent
//! (critique persona, delegated task worker, retry attempt) receives only a
//! narrowly-scoped bundle built by a [`ContextInjector`] at *dispatch time*:
//!
//! 1. The manager holds the full plan + full envelope + full domain tags.
//! 2. When the fan-out driver is about to invoke a critic, it calls
//!    [`ContextInjector::scope_for`] **inside that critic's async task** —
//!    after `join_all` has already parallelised the workers. No work is
//!    wasted for critics that are never scheduled (e.g. shutdown), and each
//!    scope is computed in parallel with the others.
//! 3. The injector returns a [`ScopedContext`]: a concise persona role, a
//!    plan slice tailored to the persona's focus area, an envelope *summary*
//!    (not the full envelope), and an optional task excerpt.
//! 4. The sub-agent's runner consumes the [`ScopedContext`] directly via the
//!    new [`CritiqueRunner::critique_scoped`] entry point. If the runner only
//!    implements the legacy `critique(name, prefix, plan, env)`, the default
//!    `critique_scoped` shim forwards to it using the scoped fields — so
//!    existing runners keep working with zero changes.
//!
//! ## Why lazy
//!
//! * **Concurrency-safe cost model.** If you pre-compute N scopes eagerly,
//!   you pay N × scoping cost even for a fan-out that never runs (policy
//!   off, crash mid-flight, …). Lazy = "pay only for work that executes".
//! * **Per-task-local view.** Each critic task owns its `ScopedContext`
//!   on its stack. No shared mutable bundle → no cross-contamination.
//! * **Hallucination reduction.** Smaller prompt = smaller attack surface
//!   for the LLM to hallucinate off of. The persona stays anchored to its
//!   narrow brief.
//!
//! ## Backward compatibility
//!
//! The `ContextInjector` parameter on the fan-out driver is `Option<…>`.
//! When `None`, the driver takes the legacy path and passes
//! `(persona, persona.system_prompt_prefix, plan_draft, envelope)` verbatim
//! to `CritiqueRunner::critique` — the exact pre-P13b behavior. Every
//! current test therefore keeps passing unchanged.

use caduceus_core::StepId;
use caduceus_permissions::envelope::PermissionEnvelope;

/// All the raw information the manager can hand to a scoper. Inputs are
/// borrowed — the injector should return owned strings.
pub struct ScopeRequest<'a> {
    /// Persona identifier (e.g. `"rubber-duck"`, `"cloud-architect"`).
    pub persona: &'a str,
    /// The persona's full `system_prompt_prefix` as registered in
    /// [`crate::modes::PersonaRegistry`]. The injector is free to *shorten*
    /// this into a concise role line for the sub-agent.
    pub persona_prefix: &'a str,
    /// Step this sub-agent is critiquing/delegating for.
    pub step_id: StepId,
    /// The full plan text owned by the manager. The injector's job is to
    /// SLICE — return only what this persona actually needs.
    pub plan_draft: &'a str,
    /// Full permission envelope. The injector should summarise, not forward
    /// verbatim, to avoid leaking approval/exec/network flags that the
    /// sub-agent does not enforce.
    pub envelope: &'a PermissionEnvelope,
    /// Domain tags that drove persona selection (e.g. `["cloud", "qa"]`).
    pub domains: &'a [&'a str],
}

/// Narrow, per-sub-agent context. All fields are owned strings so the
/// sub-agent task can hold it on the stack and drop the borrow on the
/// manager's data structures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedContext {
    /// Persona identifier. Echoed back from the request for convenience.
    pub persona: String,
    /// Concise one-line role ("Critique as a rubber-duck reviewer…").
    /// This replaces the full `system_prompt_prefix` at the wire.
    pub persona_role: String,
    /// Focus area for this persona — e.g. `"cloud-infra concerns"`,
    /// `"test coverage gaps"`. Drives prompt scaffolding.
    pub focus_area: String,
    /// Only-relevant slice of the plan. MUST be non-empty in production;
    /// an empty slice usually indicates a scoping bug.
    pub plan_slice: String,
    /// Envelope summary (counts, booleans, cadence) — no display_text.
    pub envelope_summary: String,
    /// Optional specific task excerpt a delegation/retry flow carries
    /// beyond the plan slice.
    pub task_excerpt: Option<String>,
}

impl ScopedContext {
    /// Rough size heuristic — sum of string lengths. Callers use it to
    /// enforce per-persona token budgets without pulling in a tokenizer.
    pub fn approx_byte_len(&self) -> usize {
        self.persona.len()
            + self.persona_role.len()
            + self.focus_area.len()
            + self.plan_slice.len()
            + self.envelope_summary.len()
            + self.task_excerpt.as_deref().map(str::len).unwrap_or(0)
    }
}

/// Pluggable lazy scoper. Every implementation MUST be `Send + Sync`
/// because it is invoked from parallel critic tasks.
pub trait ContextInjector: Send + Sync {
    fn scope_for(&self, req: ScopeRequest<'_>) -> ScopedContext;
}

// ── Built-in implementations ──────────────────────────────────────────────

/// Degenerate injector: returns the full plan + full prefix unchanged.
/// Equivalent to pre-P13 behaviour; useful as a test fallback or a
/// migration bridge.
pub struct PassthroughContextInjector;

impl ContextInjector for PassthroughContextInjector {
    fn scope_for(&self, req: ScopeRequest<'_>) -> ScopedContext {
        ScopedContext {
            persona: req.persona.to_string(),
            persona_role: req.persona_prefix.to_string(),
            focus_area: req.domains.join(","),
            plan_slice: req.plan_draft.to_string(),
            envelope_summary: summarise_envelope(req.envelope),
            task_excerpt: None,
        }
    }
}

/// Production default — concise persona role + keyword-sliced plan.
///
/// The injector shortens the `system_prompt_prefix` to its first sentence
/// (or first `max_role_chars` characters), picks a persona-appropriate
/// focus area from a built-in table, and trims the plan to the first
/// `max_plan_chars` characters UNLESS it finds persona-relevant keywords,
/// in which case it keeps lines containing those keywords plus a short
/// head/tail of the plan for orientation.
pub struct BuiltinScopedContextInjector {
    pub max_role_chars: usize,
    pub max_plan_chars: usize,
    /// When `true`, keeps plan lines mentioning persona-domain keywords
    /// even if they would be cut by the char limit.
    pub prefer_keyword_lines: bool,
}

impl Default for BuiltinScopedContextInjector {
    fn default() -> Self {
        Self {
            max_role_chars: 240,
            max_plan_chars: 1_200,
            prefer_keyword_lines: true,
        }
    }
}

impl BuiltinScopedContextInjector {
    /// Map a persona to its focus area + keyword set.
    ///
    /// Hardcoded table → zero runtime cost; covers every built-in persona
    /// the fan-out driver can spawn (`plan_critique_personas`). Unknown
    /// personas fall through to a generic "review" scope.
    fn persona_profile(persona: &str) -> (&'static str, &'static [&'static str]) {
        match persona {
            "rubber-duck" => (
                "Critique reasoning gaps, hidden assumptions, edge cases",
                &[
                    "assumption",
                    "edge case",
                    "unless",
                    "except",
                    "fail",
                    "error",
                ],
            ),
            "cloud-architect" => (
                "Critique cloud, infra, scalability, cost concerns",
                &[
                    "cloud", "azure", "aws", "gcp", "region", "vm", "scale", "latency", "cost",
                    "sla",
                ],
            ),
            "ml-architect" => (
                "Critique ML pipeline, training, inference, model risk",
                &[
                    "model",
                    "train",
                    "inference",
                    "dataset",
                    "gpu",
                    "embedding",
                    "drift",
                    "bias",
                    "ml",
                ],
            ),
            "data-engineer" => (
                "Critique data pipelines, schemas, ingestion, contracts",
                &[
                    "pipeline", "schema", "etl", "ingest", "contract", "backfill", "stream",
                    "batch",
                ],
            ),
            "data-researcher" => (
                "Critique research methodology, sources, reproducibility",
                &[
                    "paper",
                    "research",
                    "cite",
                    "benchmark",
                    "state-of-the-art",
                    "sota",
                ],
            ),
            "data-scientist" => (
                "Critique statistics, experiment design, significance",
                &[
                    "experiment",
                    "a/b",
                    "sample",
                    "power",
                    "significance",
                    "distribution",
                    "bias",
                ],
            ),
            "qa-strategist" => (
                "Critique test coverage, acceptance criteria, flakiness",
                &[
                    "test",
                    "coverage",
                    "flaky",
                    "hermetic",
                    "regression",
                    "acceptance",
                    "fuzz",
                ],
            ),
            _ => ("Generic reviewer — surface issues and unclarities", &[]),
        }
    }

    fn concise_role(&self, prefix: &str) -> String {
        let trimmed = prefix.trim();
        // First sentence or first char-budget, whichever comes first.
        let end_of_sentence = trimmed
            .find(|c| matches!(c, '.' | '!' | '?' | '\n'))
            .map(|i| i + 1)
            .unwrap_or(trimmed.len());
        let cap = end_of_sentence.min(self.max_role_chars).min(trimmed.len());
        trimmed[..cap].trim().to_string()
    }

    fn slice_plan(&self, plan: &str, keywords: &[&str]) -> String {
        if plan.len() <= self.max_plan_chars {
            return plan.to_string();
        }
        if !self.prefer_keyword_lines || keywords.is_empty() {
            // Head truncation with ellipsis marker.
            let cut = byte_boundary_le(plan, self.max_plan_chars);
            let mut out = String::with_capacity(cut + 6);
            out.push_str(&plan[..cut]);
            out.push_str("\n…");
            return out;
        }
        // Keyword-preferring slicer: keep ALL lines matching any keyword
        // (case-insensitive substring), plus the first ~200 chars of the
        // plan as orientation.
        let lower_plan: String = plan.to_lowercase();
        let head_cap = (self.max_plan_chars / 6).min(plan.len());
        let head = &plan[..byte_boundary_le(plan, head_cap)];
        let mut kept: Vec<&str> = Vec::new();
        let mut used = head.len();
        for line in plan.lines() {
            if used + line.len() + 1 > self.max_plan_chars {
                break;
            }
            let lower_line = &lower_plan[line_range(plan, line)];
            if keywords
                .iter()
                .any(|k| lower_line.contains(&k.to_lowercase()))
            {
                kept.push(line);
                used += line.len() + 1;
            }
        }
        let body = kept.join("\n");
        if body.is_empty() {
            // No keyword hits — fall back to head+ellipsis.
            let cut = byte_boundary_le(plan, self.max_plan_chars);
            let mut out = String::with_capacity(cut + 6);
            out.push_str(&plan[..cut]);
            out.push_str("\n…");
            return out;
        }
        format!("{head}\n…\n{body}")
    }
}

impl ContextInjector for BuiltinScopedContextInjector {
    fn scope_for(&self, req: ScopeRequest<'_>) -> ScopedContext {
        let (focus_area, keywords) = Self::persona_profile(req.persona);
        let persona_role = self.concise_role(req.persona_prefix);
        let plan_slice = self.slice_plan(req.plan_draft, keywords);
        ScopedContext {
            persona: req.persona.to_string(),
            persona_role,
            focus_area: focus_area.to_string(),
            plan_slice,
            envelope_summary: summarise_envelope(req.envelope),
            task_excerpt: None,
        }
    }
}

/// Compact, hallucination-starving envelope description.
pub fn summarise_envelope(env: &PermissionEnvelope) -> String {
    // Layout: "r=<allow>/<deny>,w=<allow>/<deny>,net=<bool>,exec=<bool>,cadence=<str>,fanout=<str>"
    format!(
        "r={}/{},w={}/{},net={},exec={},cadence={:?},fanout={:?}",
        env.read.allow.len(),
        env.read.deny.len(),
        env.write.allow.len(),
        env.write.deny.len(),
        env.network.enabled,
        env.exec.enabled,
        env.approval_cadence,
        env.fanout_policy,
    )
}

// Find the largest byte index ≤ `cap` that lies on a UTF-8 char boundary.
fn byte_boundary_le(s: &str, cap: usize) -> usize {
    if cap >= s.len() {
        return s.len();
    }
    let mut i = cap;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// Map a `&str` line produced by `str::lines()` back to its byte range in
// the owning string. Relies on `line.as_ptr()` lying inside `owner`.
fn line_range(owner: &str, line: &str) -> std::ops::Range<usize> {
    let base = owner.as_ptr() as usize;
    let start = line.as_ptr() as usize - base;
    start..start + line.len()
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> PermissionEnvelope {
        PermissionEnvelope::research_preset()
    }

    fn req<'a>(
        persona: &'a str,
        prefix: &'a str,
        plan: &'a str,
        envelope: &'a PermissionEnvelope,
        domains: &'a [&'a str],
    ) -> ScopeRequest<'a> {
        ScopeRequest {
            persona,
            persona_prefix: prefix,
            step_id: StepId(1),
            plan_draft: plan,
            envelope,
            domains,
        }
    }

    #[test]
    fn passthrough_does_not_modify_fields() {
        let e = env();
        let r = req(
            "rubber-duck",
            "Very long prefix. Has multiple sentences. Third.",
            "body body body",
            &e,
            &["qa"],
        );
        let sc = PassthroughContextInjector.scope_for(r);
        assert_eq!(sc.persona, "rubber-duck");
        assert_eq!(sc.plan_slice, "body body body");
        assert_eq!(
            sc.persona_role,
            "Very long prefix. Has multiple sentences. Third."
        );
        assert_eq!(sc.focus_area, "qa");
        assert!(sc.envelope_summary.starts_with("r="));
    }

    #[test]
    fn builtin_shortens_persona_prefix_to_first_sentence() {
        let inj = BuiltinScopedContextInjector::default();
        let e = env();
        let sc = inj.scope_for(req(
            "rubber-duck",
            "Think like a rubber duck. Surface unstated assumptions. Challenge edge cases.",
            "plan body",
            &e,
            &["qa"],
        ));
        assert_eq!(sc.persona_role, "Think like a rubber duck.");
    }

    #[test]
    fn builtin_slices_large_plan_keeping_keyword_lines() {
        let mut plan = String::new();
        for i in 0..500 {
            plan.push_str(&format!("filler line {i}\n"));
        }
        plan.push_str("this line mentions cost and scale\n");
        plan.push_str("this line mentions region and latency\n");
        for i in 0..200 {
            plan.push_str(&format!("more filler {i}\n"));
        }
        let inj = BuiltinScopedContextInjector::default();
        let e = env();
        let sc = inj.scope_for(req("cloud-architect", "prefix.", &plan, &e, &["cloud"]));
        assert!(
            sc.plan_slice.len() <= inj.max_plan_chars + 8,
            "len={}",
            sc.plan_slice.len()
        );
        assert!(sc.plan_slice.contains("cost and scale"));
        assert!(sc.plan_slice.contains("region and latency"));
    }

    #[test]
    fn builtin_unknown_persona_falls_back_generic() {
        let inj = BuiltinScopedContextInjector::default();
        let e = env();
        let sc = inj.scope_for(req("mystery-persona", "prefix.", "tiny plan", &e, &[]));
        assert_eq!(
            sc.focus_area,
            "Generic reviewer — surface issues and unclarities"
        );
        assert_eq!(sc.plan_slice, "tiny plan");
    }

    #[test]
    fn builtin_small_plan_not_truncated() {
        let inj = BuiltinScopedContextInjector::default();
        let e = env();
        let sc = inj.scope_for(req(
            "qa-strategist",
            "prefix.",
            "test plan body",
            &e,
            &["qa"],
        ));
        assert_eq!(sc.plan_slice, "test plan body");
    }

    #[test]
    fn envelope_summary_is_compact_and_stable() {
        let e = env();
        let s = summarise_envelope(&e);
        // No display_text or raw prompt → no leak.
        assert!(!s.contains("display_text"));
        assert!(s.starts_with("r="));
        assert!(s.contains("net="));
    }

    #[test]
    fn approx_byte_len_sums_all_fields() {
        let sc = ScopedContext {
            persona: "p".into(),
            persona_role: "role".into(),
            focus_area: "focus".into(),
            plan_slice: "plan".into(),
            envelope_summary: "env".into(),
            task_excerpt: Some("extra".into()),
        };
        assert_eq!(sc.approx_byte_len(), 1 + 4 + 5 + 4 + 3 + 5);
    }

    #[test]
    fn builtin_respects_utf8_char_boundary_when_truncating() {
        let inj = BuiltinScopedContextInjector {
            max_plan_chars: 10,
            ..Default::default()
        };
        let e = env();
        // "éé" is 2 chars / 4 bytes; "🚀" is 1 char / 4 bytes.
        let plan = "ééééé🚀🚀🚀🚀filler text that definitely exceeds ten chars";
        let sc = inj.scope_for(req("rubber-duck", "prefix.", plan, &e, &[]));
        // Must not panic; must still be valid UTF-8.
        assert!(sc.plan_slice.is_char_boundary(sc.plan_slice.len()));
    }
}
