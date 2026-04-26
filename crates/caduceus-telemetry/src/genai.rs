//! G23 / P7.2 — OpenTelemetry **GenAI semantic conventions** mapper.
//!
//! Why this module exists
//! ----------------------
//! The audit identified that Caduceus emits rich `AgentEvent` data but
//! has no path to OTel GenAI semconv (`gen_ai.*` attributes), so any
//! external observability stack (Honeycomb, Datadog, Tempo, Phoenix,
//! Arize, …) sees the agent as opaque. P7.2 closes this by defining
//! the canonical event → attribute mapping in one place.
//!
//! Scope of *this* PR
//! ------------------
//! * Pure mapping layer — no OTLP transport, no `opentelemetry`
//!   crate dependency. Pulling the full SDK is a 300 KLoC dep tree
//!   and the team hasn't picked an exporter yet (OTLP-grpc vs
//!   OTLP-http vs Honeycomb-direct). Once the exporter choice is
//!   made, the [`GenAiSpanExporter`] trait is the single seam to
//!   wire the SDK against; nothing else in the crate has to change.
//! * Mapping covers the **stable** GenAI semconv subset (1.30+):
//!   `gen_ai.system`, `gen_ai.request.model`, `gen_ai.response.model`,
//!   `gen_ai.operation.name`, `gen_ai.usage.input_tokens`,
//!   `gen_ai.usage.output_tokens`, `gen_ai.response.finish_reasons`,
//!   `gen_ai.tool.name`, `gen_ai.tool.call.id`, `gen_ai.conversation.id`,
//!   plus a Caduceus-namespaced `caduceus.step_id` because GenAI
//!   semconv has no concept of a step.
//! * A [`JsonlGenAiExporter`] sink so unit tests can assert on the
//!   exact emitted JSON without needing a collector.
//!
//! Out of scope (deferred)
//! -----------------------
//! * Real OTLP push / pull (P7.2.1, follow-up).
//! * `gen_ai.system_prompt` / `gen_ai.completion` event bodies — these
//!   carry user content and need the `caduceus.telemetry.redaction`
//!   policy resolved before they can be safely emitted.

use caduceus_core::AgentEvent;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// One GenAI-semconv attribute value. The OTel API allows string,
/// int, double, bool, and arrays; we only emit the four scalar
/// shapes the Caduceus mapping actually needs. Adding `Array(Vec<...>)`
/// is straightforward when a new mapping requires it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GenAiValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl From<&str> for GenAiValue {
    fn from(s: &str) -> Self {
        GenAiValue::Str(s.to_string())
    }
}
impl From<String> for GenAiValue {
    fn from(s: String) -> Self {
        GenAiValue::Str(s)
    }
}
impl From<i64> for GenAiValue {
    fn from(v: i64) -> Self {
        GenAiValue::Int(v)
    }
}
impl From<u32> for GenAiValue {
    fn from(v: u32) -> Self {
        GenAiValue::Int(v as i64)
    }
}
impl From<u64> for GenAiValue {
    fn from(v: u64) -> Self {
        GenAiValue::Int(v as i64)
    }
}
impl From<usize> for GenAiValue {
    fn from(v: usize) -> Self {
        GenAiValue::Int(v as i64)
    }
}
impl From<bool> for GenAiValue {
    fn from(v: bool) -> Self {
        GenAiValue::Bool(v)
    }
}

/// A GenAI-shaped span ready for emission. Callers construct one via
/// [`GenAiMapper::map`]; exporters consume it via [`GenAiSpanExporter`].
///
/// `name` is the OTel span name. GenAI semconv recommends
/// `<operation> <model>` (e.g. `chat gpt-5.4`); for non-LLM
/// operations (tool calls, step boundaries) we use the operation
/// alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenAiSpan {
    pub name: String,
    pub attributes: BTreeMap<String, GenAiValue>,
}

impl GenAiSpan {
    /// Convenience: borrow a specific attribute, returning `None` if
    /// the attribute wasn't set. Useful in tests and in exporters
    /// that want to derive secondary fields (e.g. resource attributes
    /// from `gen_ai.system`).
    pub fn attr(&self, key: &str) -> Option<&GenAiValue> {
        self.attributes.get(key)
    }
}

/// Stable session/conversation context carried across every span
/// emitted for one agent run. The mapper accepts this as input
/// because GenAI semconv requires `gen_ai.system` and
/// `gen_ai.conversation.id` on every span — values that the event
/// stream itself does not duplicate (they live on `SessionState`).
#[derive(Debug, Clone)]
pub struct GenAiContext {
    /// e.g. `"openai"`, `"anthropic"`, `"local"`. Maps to
    /// `gen_ai.system`.
    pub system: String,
    /// Stable id for the multi-turn conversation. Maps to
    /// `gen_ai.conversation.id`. Use `SessionState::id`.
    pub conversation_id: String,
    /// Default model id used when the event doesn't specify one
    /// (e.g. lifecycle events that aren't tied to a single LLM
    /// call). Maps to `gen_ai.request.model` when no override is
    /// available on the event itself.
    pub default_model: Option<String>,
    /// Current step id from `SessionState::current_step()`. Stamped
    /// onto every span as `caduceus.step_id` so tool calls join
    /// back to the LLM step that requested them (G26 / P7.1).
    pub current_step: u64,
}

impl GenAiContext {
    pub fn new(system: impl Into<String>, conversation_id: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            conversation_id: conversation_id.into(),
            default_model: None,
            current_step: 0,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = Some(model.into());
        self
    }

    pub fn with_step(mut self, step: u64) -> Self {
        self.current_step = step;
        self
    }
}

/// Maps `AgentEvent` payloads to GenAI semconv spans.
///
/// The mapper is **pure** — no I/O, no clock reads, no allocations
/// beyond the returned span — so it's cheap to call on the hot path
/// and easy to test exhaustively.
pub struct GenAiMapper;

impl GenAiMapper {
    /// Convert one event into a span. Returns `None` for events that
    /// have no semantic meaning under GenAI conventions (e.g. UI tree
    /// nodes, routing-decision diagnostics) — exporters skip these
    /// rather than emit no-op spans that would clutter traces.
    pub fn map(event: &AgentEvent, ctx: &GenAiContext) -> Option<GenAiSpan> {
        let mut attrs = BTreeMap::new();

        // Apply the always-on context attributes upfront. The mapper
        // returns None *after* this check for events with no GenAI
        // mapping, so we never emit a span containing only context.
        let span = match event {
            // ── LLM calls ────────────────────────────────────────────
            AgentEvent::ThinkingStarted { iteration } => {
                attrs.insert("gen_ai.operation.name".into(), "chat".into());
                attrs.insert(
                    "caduceus.iteration".into(),
                    GenAiValue::Int(*iteration as i64),
                );
                GenAiSpan {
                    name: format!("chat {}", ctx.default_model.as_deref().unwrap_or("unknown")),
                    attributes: attrs,
                }
            }

            AgentEvent::TurnComplete { usage, stop_reason } => {
                attrs.insert("gen_ai.operation.name".into(), "chat".into());
                attrs.insert(
                    "gen_ai.usage.input_tokens".into(),
                    GenAiValue::Int(usage.input_tokens as i64),
                );
                attrs.insert(
                    "gen_ai.usage.output_tokens".into(),
                    GenAiValue::Int(usage.output_tokens as i64),
                );
                // GenAI semconv `response.finish_reasons` is an array
                // of strings; we only know one stop reason per
                // TurnComplete so we emit a single-element JSON array
                // serialised as a string. Real OTLP exporters can
                // re-parse to an array attribute.
                attrs.insert(
                    "gen_ai.response.finish_reasons".into(),
                    format!("[\"{}\"]", finish_reason_label(stop_reason)).into(),
                );
                GenAiSpan {
                    name: "chat.complete".into(),
                    attributes: attrs,
                }
            }

            // ── Tool calls ───────────────────────────────────────────
            AgentEvent::ToolCallStart { id, name } => {
                attrs.insert("gen_ai.operation.name".into(), "execute_tool".into());
                attrs.insert("gen_ai.tool.name".into(), name.clone().into());
                attrs.insert("gen_ai.tool.call.id".into(), id.0.clone().into());
                GenAiSpan {
                    name: format!("execute_tool {name}"),
                    attributes: attrs,
                }
            }

            AgentEvent::ToolCallEnd { id } => {
                attrs.insert("gen_ai.operation.name".into(), "execute_tool".into());
                attrs.insert("gen_ai.tool.call.id".into(), id.0.clone().into());
                GenAiSpan {
                    name: "execute_tool.complete".into(),
                    attributes: attrs,
                }
            }

            AgentEvent::ToolResultEnd { id, is_error, .. } => {
                attrs.insert("gen_ai.operation.name".into(), "execute_tool".into());
                attrs.insert("gen_ai.tool.call.id".into(), id.0.clone().into());
                attrs.insert("caduceus.tool.is_error".into(), GenAiValue::Bool(*is_error));
                GenAiSpan {
                    name: "execute_tool.result".into(),
                    attributes: attrs,
                }
            }

            // ── Step boundaries (G26 / P7.1) ─────────────────────────
            AgentEvent::StepStarted { step_id } => {
                // We override the context step so the attribute below
                // reflects the *new* step, not the previous one the
                // caller passed in.
                attrs.insert("caduceus.step_id".into(), GenAiValue::Int(*step_id as i64));
                attrs.insert("caduceus.step.event".into(), "started".into());
                return Some(finalise(
                    GenAiSpan {
                        name: format!("step {step_id}"),
                        attributes: attrs,
                    },
                    ctx,
                    /* respect_step_override = */ true,
                ));
            }

            AgentEvent::StepCompleted { step_id, ok } => {
                attrs.insert("caduceus.step_id".into(), GenAiValue::Int(*step_id as i64));
                attrs.insert("caduceus.step.event".into(), "completed".into());
                attrs.insert("caduceus.step.ok".into(), GenAiValue::Bool(*ok));
                return Some(finalise(
                    GenAiSpan {
                        name: format!("step {step_id}.complete"),
                        attributes: attrs,
                    },
                    ctx,
                    /* respect_step_override = */ true,
                ));
            }

            // ── Permission / HITL ────────────────────────────────────
            AgentEvent::PermissionDecision {
                id,
                capability,
                outcome,
            } => {
                attrs.insert("gen_ai.operation.name".into(), "human_in_the_loop".into());
                attrs.insert("caduceus.permission.id".into(), id.clone().into());
                attrs.insert(
                    "caduceus.permission.capability".into(),
                    capability.clone().into(),
                );
                attrs.insert(
                    "caduceus.permission.outcome".into(),
                    outcome_label(outcome).into(),
                );
                GenAiSpan {
                    name: "human_in_the_loop.decision".into(),
                    attributes: attrs,
                }
            }

            // ── Critique (G19 / P6.5) ────────────────────────────────
            AgentEvent::CritiqueCall {
                critic_model,
                input_tokens,
                output_tokens,
                duration_ms,
                denied,
                ..
            } => {
                attrs.insert("gen_ai.operation.name".into(), "chat".into());
                attrs.insert("gen_ai.request.model".into(), critic_model.clone().into());
                attrs.insert("caduceus.critique.denied".into(), GenAiValue::Bool(*denied));
                if !*denied {
                    attrs.insert(
                        "gen_ai.usage.input_tokens".into(),
                        GenAiValue::Int(*input_tokens as i64),
                    );
                    attrs.insert(
                        "gen_ai.usage.output_tokens".into(),
                        GenAiValue::Int(*output_tokens as i64),
                    );
                    attrs.insert(
                        "caduceus.critique.duration_ms".into(),
                        GenAiValue::Int(*duration_ms as i64),
                    );
                }
                // Override default_model for this span so a critique
                // run with a different model is correctly attributed.
                let span = GenAiSpan {
                    name: format!("chat {critic_model}"),
                    attributes: attrs,
                };
                return Some(finalise_with_model_override(span, ctx, critic_model));
            }

            // Everything else has no GenAI mapping today (UI tree,
            // routing diagnostics, buffer-overflow notices, etc.).
            _ => return None,
        };

        Some(finalise(
            span, ctx, /* respect_step_override = */ false,
        ))
    }
}

/// Stamp the always-on context attributes onto the span and return.
fn finalise(mut span: GenAiSpan, ctx: &GenAiContext, respect_step_override: bool) -> GenAiSpan {
    span.attributes
        .insert("gen_ai.system".into(), ctx.system.clone().into());
    span.attributes.insert(
        "gen_ai.conversation.id".into(),
        ctx.conversation_id.clone().into(),
    );
    if let Some(ref m) = ctx.default_model {
        // Don't clobber an event-specific request.model (e.g. critique
        // call uses a different critic_model).
        span.attributes
            .entry("gen_ai.request.model".into())
            .or_insert_with(|| m.clone().into());
    }
    if !respect_step_override {
        span.attributes.insert(
            "caduceus.step_id".into(),
            GenAiValue::Int(ctx.current_step as i64),
        );
    }
    span
}

/// Same as [`finalise`] but uses an event-supplied model id instead
/// of the context default. Used by the critique mapping where the
/// critic model differs from the agent's main model.
fn finalise_with_model_override(
    mut span: GenAiSpan,
    ctx: &GenAiContext,
    _model_override: &str,
) -> GenAiSpan {
    span.attributes
        .insert("gen_ai.system".into(), ctx.system.clone().into());
    span.attributes.insert(
        "gen_ai.conversation.id".into(),
        ctx.conversation_id.clone().into(),
    );
    span.attributes.insert(
        "caduceus.step_id".into(),
        GenAiValue::Int(ctx.current_step as i64),
    );
    // gen_ai.request.model was set by the caller to the override value.
    span
}

fn finish_reason_label(stop: &caduceus_core::StopReason) -> &'static str {
    use caduceus_core::StopReason::*;
    match stop {
        EndTurn => "stop",
        MaxTokens => "length",
        ToolUse => "tool_calls",
        StopSequence => "stop_sequence",
        BudgetExceeded => "budget_exceeded",
        Error => "error",
    }
}

fn outcome_label(o: &caduceus_core::PermissionOutcome) -> &'static str {
    use caduceus_core::PermissionOutcome::*;
    match o {
        Approved => "approved",
        Denied => "denied",
        TimedOut { .. } => "timed_out",
        ChannelClosed => "channel_closed",
        MismatchedId { .. } => "mismatched_id",
        Unknown => "unknown",
    }
}

/// Plug-point for a real OTel SDK. Implementors translate
/// [`GenAiSpan`] into their backend's span/event/log type. The trait
/// is intentionally infallible — exporters that need to fail (e.g.
/// network error) should buffer internally and surface drops via
/// their own metrics, mirroring the `AgentEventEmitter` G27 pattern.
pub trait GenAiSpanExporter: Send + Sync {
    fn export(&self, span: &GenAiSpan);
}

/// Test / debug exporter that captures spans in memory as JSON Lines.
/// Production code should use a real OTLP exporter; this one is
/// shipped so unit tests can assert on the exact wire payload without
/// pulling the SDK.
pub struct JsonlGenAiExporter {
    buf: Mutex<Vec<String>>,
}

impl JsonlGenAiExporter {
    pub fn new() -> Self {
        Self {
            buf: Mutex::new(Vec::new()),
        }
    }

    /// Snapshot of all exported spans, in order. Cheap clone.
    pub fn snapshot(&self) -> Vec<String> {
        match self.buf.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    /// Number of spans exported so far. Avoids the snapshot clone
    /// when callers only want the count.
    pub fn len(&self) -> usize {
        self.buf.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for JsonlGenAiExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl GenAiSpanExporter for JsonlGenAiExporter {
    fn export(&self, span: &GenAiSpan) {
        let line = serde_json::to_string(span).unwrap_or_else(|_| "{}".into());
        if let Ok(mut g) = self.buf.lock() {
            g.push(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::{PermissionOutcome, StopReason, TokenUsage};

    fn ctx() -> GenAiContext {
        GenAiContext::new("anthropic", "conv_42")
            .with_model("claude-opus-4.6")
            .with_step(3)
    }

    #[test]
    fn maps_thinking_started_to_chat_span() {
        let span = GenAiMapper::map(&AgentEvent::ThinkingStarted { iteration: 0 }, &ctx()).unwrap();
        assert_eq!(span.name, "chat claude-opus-4.6");
        assert_eq!(span.attr("gen_ai.operation.name"), Some(&"chat".into()));
        assert_eq!(span.attr("gen_ai.system"), Some(&"anthropic".into()));
        assert_eq!(span.attr("gen_ai.conversation.id"), Some(&"conv_42".into()));
        assert_eq!(
            span.attr("gen_ai.request.model"),
            Some(&"claude-opus-4.6".into())
        );
        assert_eq!(span.attr("caduceus.step_id"), Some(&GenAiValue::Int(3)));
    }

    #[test]
    fn maps_turn_complete_token_usage_and_finish_reason() {
        let span = GenAiMapper::map(
            &AgentEvent::TurnComplete {
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage {
                    input_tokens: 100,
                    output_tokens: 200,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    context_limit: None,
                },
            },
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            span.attr("gen_ai.usage.input_tokens"),
            Some(&GenAiValue::Int(100))
        );
        assert_eq!(
            span.attr("gen_ai.usage.output_tokens"),
            Some(&GenAiValue::Int(200))
        );
        assert_eq!(
            span.attr("gen_ai.response.finish_reasons"),
            Some(&"[\"stop\"]".into())
        );
    }

    #[test]
    fn maps_tool_call_start_with_tool_name() {
        let span = GenAiMapper::map(
            &AgentEvent::ToolCallStart {
                id: caduceus_core::ToolCallId::new("call_abc"),
                name: "bash".into(),
            },
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            span.attr("gen_ai.operation.name"),
            Some(&"execute_tool".into())
        );
        assert_eq!(span.attr("gen_ai.tool.name"), Some(&"bash".into()));
        assert_eq!(span.attr("gen_ai.tool.call.id"), Some(&"call_abc".into()));
        assert_eq!(span.name, "execute_tool bash");
    }

    #[test]
    fn maps_step_started_uses_event_step_not_context_step() {
        let span = GenAiMapper::map(&AgentEvent::StepStarted { step_id: 9 }, &ctx()).unwrap();
        // Critical: the step *event itself* carries the new step id;
        // we must NOT clobber it with the context's stale value (3).
        // Otherwise OTel traces would show every step span pinned to
        // the most-recently-allocated step rather than its own.
        assert_eq!(span.attr("caduceus.step_id"), Some(&GenAiValue::Int(9)));
    }

    #[test]
    fn maps_step_completed_carries_ok_flag() {
        let span = GenAiMapper::map(
            &AgentEvent::StepCompleted {
                step_id: 5,
                ok: false,
            },
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            span.attr("caduceus.step.ok"),
            Some(&GenAiValue::Bool(false))
        );
        assert_eq!(span.attr("caduceus.step_id"), Some(&GenAiValue::Int(5)));
    }

    #[test]
    fn maps_permission_decision_outcome() {
        let span = GenAiMapper::map(
            &AgentEvent::PermissionDecision {
                id: "perm_x".into(),
                capability: "bash".into(),
                outcome: PermissionOutcome::Approved,
            },
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            span.attr("gen_ai.operation.name"),
            Some(&"human_in_the_loop".into())
        );
        assert_eq!(
            span.attr("caduceus.permission.outcome"),
            Some(&"approved".into())
        );
    }

    #[test]
    fn maps_critique_uses_critic_model_not_default() {
        let span = GenAiMapper::map(
            &AgentEvent::CritiqueCall {
                critic_model: "gpt-5.4".into(),
                leaf_count: 3,
                conflicts_found: 1,
                input_tokens: 50,
                output_tokens: 75,
                duration_ms: 1200,
                denied: false,
            },
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            span.attr("gen_ai.request.model"),
            Some(&"gpt-5.4".into()),
            "critic_model must override default_model on the span"
        );
        assert_eq!(span.name, "chat gpt-5.4");
        assert_eq!(
            span.attr("caduceus.critique.denied"),
            Some(&GenAiValue::Bool(false))
        );
        assert_eq!(
            span.attr("gen_ai.usage.input_tokens"),
            Some(&GenAiValue::Int(50))
        );
    }

    #[test]
    fn maps_critique_denied_omits_token_usage() {
        let span = GenAiMapper::map(
            &AgentEvent::CritiqueCall {
                critic_model: "gpt-5.4".into(),
                leaf_count: 3,
                conflicts_found: 0,
                input_tokens: 0,
                output_tokens: 0,
                duration_ms: 0,
                denied: true,
            },
            &ctx(),
        )
        .unwrap();
        assert!(
            span.attr("gen_ai.usage.input_tokens").is_none(),
            "denied critiques didn't actually call the LLM, so no token usage should be reported"
        );
        assert_eq!(
            span.attr("caduceus.critique.denied"),
            Some(&GenAiValue::Bool(true))
        );
    }

    #[test]
    fn unmapped_events_return_none() {
        // RoutingDecision is a UI-side diagnostic with no GenAI shape;
        // the mapper must not synthesise an empty span for it.
        let span = GenAiMapper::map(
            &AgentEvent::RoutingDecision {
                candidates: vec![],
                activated: vec![],
                threshold: 0.5,
            },
            &ctx(),
        );
        assert!(span.is_none());
    }

    #[test]
    fn jsonl_exporter_captures_spans_in_order() {
        let exp = JsonlGenAiExporter::new();
        let s1 = GenAiMapper::map(&AgentEvent::StepStarted { step_id: 1 }, &ctx()).unwrap();
        let s2 = GenAiMapper::map(
            &AgentEvent::StepCompleted {
                step_id: 1,
                ok: true,
            },
            &ctx(),
        )
        .unwrap();
        exp.export(&s1);
        exp.export(&s2);
        let snap = exp.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap[0].contains("\"step 1\""));
        assert!(snap[1].contains("\"step 1.complete\""));
    }

    /// Stable attribute keys MUST match GenAI semconv exactly. A typo
    /// here would silently break every downstream dashboard. Lock the
    /// canonical list with a single test so any future rename trips
    /// CI and forces a deliberate review.
    #[test]
    fn stable_genai_attribute_keys_lock() {
        let span = GenAiMapper::map(
            &AgentEvent::TurnComplete {
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    context_limit: None,
                },
            },
            &ctx(),
        )
        .unwrap();
        for key in [
            "gen_ai.system",
            "gen_ai.conversation.id",
            "gen_ai.operation.name",
            "gen_ai.usage.input_tokens",
            "gen_ai.usage.output_tokens",
            "gen_ai.response.finish_reasons",
        ] {
            assert!(
                span.attr(key).is_some(),
                "missing GenAI semconv attribute `{key}`"
            );
        }
    }
}
