use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

pub mod event_redact;
pub mod loop_detector;
pub mod path_norm;
pub mod process_reward;
pub mod sanitizer;
pub mod verification;

pub use event_redact::{redact_secrets_for_event, REDACTED_SENTINEL};
pub use loop_detector::{LoopCheckResult, LoopDetector};
pub use path_norm::{is_path_like_field, normalize_lex, PATH_LIKE_FIELDS};
pub use process_reward::{
    EnsembleCombiner, EnsembleStepVerifier, ObservedToolCall, OffStepVerifier, StepScore,
    StepVerifier, StepView,
};
pub use sanitizer::{
    SanitizationFlags, SanitizedOutput, ToolOutputSanitizer,
    DEFAULT_MAX_BYTES as DEFAULT_TOOL_OUTPUT_MAX_BYTES,
};
pub use verification::{majority_vote, weighted_majority_vote, VerificationStrategy, VoteOutcome};

// ── ID newtypes ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderId(pub String);

impl ProviderId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelId(pub String);

impl ModelId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToolCallId(pub String);

impl ToolCallId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

// ── Session types ──────────────────────────────────────────────────────────────

// ── G26 / P7.1: monotonic StepId ─────────────────────────────────────────────
//
// A `StepId` identifies one logical iteration of the agent loop: typically
// "one LLM call plus its associated tool batch". It is the join key that
// downstream consumers (OTel exporter — G23, trajectory recorder — G22)
// use to align tool calls with the LLM step that requested them. Without
// it, batched parallel tool calls cannot be reliably traced back to the
// step that produced them once they land out of order on the wire.
//
// Properties:
// * monotonic, per-session, never reused
// * starts at 0 — so an unstamped event implicitly belongs to "pre-loop"
//   work (history setup, system prompt assembly, etc.)
// * carried as a plain `u64` on `AgentEvent::StepStarted` /
//   `AgentEvent::StepCompleted` so wire decoders don't need a new type
// * the counter itself lives on `SessionState` and is shared with the
//   emitter via `Arc<AtomicU64>` so producer threads can fetch the
//   current step without locking
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, PartialOrd, Ord,
)]
pub struct StepId(pub u64);

impl StepId {
    pub const PRELOOP: StepId = StepId(0);

    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for StepId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "step#{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionPhase {
    Idle,
    Running,
    AwaitingPermission,
    /// P6.6 / G21 — verification step (rollout vote, PRM scoring) is
    /// running. Distinct from `Running` so the UI can show a "verifying"
    /// indicator and so cancellation hits the verifier `tokio::select!`
    /// arm rather than the main turn loop.
    Verifying,
    /// P6.6 / G21 — test-gate is executing the build / test suite to
    /// validate the candidate solution before committing.
    TestGating,
    Cancelling,
    Completed,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub id: SessionId,
    pub phase: SessionPhase,
    pub project_root: PathBuf,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub token_budget: TokenBudget,
    pub turn_count: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// G26 / P7.1 — monotonic step counter. Skipped from serde because
    /// it's a runtime clock; persisted sessions resume from `current()`
    /// at zero (replays use the recorded `StepStarted` events to drive
    /// progression deterministically). Wrapped in `Arc<AtomicU64>` so
    /// the orchestrator can hand a clone to the `AgentEventEmitter`
    /// for read-only "current step" queries from any task.
    #[serde(skip, default = "default_step_counter")]
    pub step_counter: Arc<AtomicU64>,
}

fn default_step_counter() -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(0))
}

impl SessionState {
    pub fn new(project_root: impl Into<PathBuf>, provider: ProviderId, model: ModelId) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: SessionId::new(),
            phase: SessionPhase::Idle,
            project_root: project_root.into(),
            provider_id: provider,
            model_id: model,
            token_budget: TokenBudget::default(),
            turn_count: 0,
            created_at: now,
            updated_at: now,
            step_counter: default_step_counter(),
        }
    }

    /// Atomically allocate the next `StepId`. Monotonic across the
    /// lifetime of the `SessionState`. Safe to call from any task; the
    /// underlying counter is shared with any emitter cloned via
    /// [`SessionState::step_counter`].
    pub fn next_step(&self) -> StepId {
        let n = self.step_counter.fetch_add(1, Ordering::Relaxed);
        // Reserve `0` for "pre-loop" work (StepId::PRELOOP). The first
        // real step the loop allocates is therefore `1`. We accomplish
        // this by treating `fetch_add`'s return as the *previous*
        // value and skipping zero on first allocation.
        StepId(n + 1)
    }

    /// Read the current (last-allocated) step id without advancing the
    /// counter. Returns `StepId::PRELOOP` before the first `next_step`.
    pub fn current_step(&self) -> StepId {
        StepId(self.step_counter.load(Ordering::Relaxed))
    }
}

// ── LLM Messages (provider-agnostic) ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl LlmMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text(text.into())],
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text(text.into())],
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::Text(text.into())],
        }
    }

    pub fn tool_result(
        tool_call_id: ToolCallId,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_call_id,
                content: content.into(),
                is_error,
            }],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentBlock {
    Text(String),
    ToolUse {
        id: ToolCallId,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_call_id: ToolCallId,
        content: String,
        is_error: bool,
    },
    Image(ImageContent),
}

// ── Vision types (Feature #72) ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageContent {
    pub source: ImageSource,
    pub detail: Option<String>, // "auto", "low", "high"
}

// ── LLM Response ──────────────────────────────────────────────────────────────

/// Severity classification for security findings. Canonical location for this
/// type is `caduceus-core` so that `caduceus-tools` (executor) does not need to
/// depend on `caduceus-permissions` (policy) — see audit I13. The permissions
/// crate re-exports this type for backwards compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VulnSeverity {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    /// The provider/network failed before a normal stop signal could be
    /// delivered. Carries no usage information; callers (e.g. emitters)
    /// use this to bracket the turn so subscribers always see a
    /// matching TurnComplete after Phase::Working — even on error.
    /// Round-2 audit finding (#27): without this variant the orchestrator
    /// could return Err without ever emitting TurnComplete, leaving UI
    /// listeners hung waiting for a turn-end boundary.
    Error,
    /// The orchestrator-side [`TurnBudget`] (gap G11) was exceeded before
    /// the model emitted a natural stop. Distinct from `MaxTokens`
    /// (provider-side context cap) and `Error` (transport failure) so the
    /// UI can surface a clear "rate limit / budget" message and offer a
    /// "raise budget and continue" affordance instead of restarting.
    BudgetExceeded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: TokenUsage,
}

impl LlmResponse {
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> Vec<&ContentBlock> {
        self.content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .collect()
    }
}

// ── Streaming Events (Orchestrator → Frontend) ────────────────────────────────

/// Outcome of a permission/approval request emitted as `PermissionDecision`.
/// The UI uses this to render an accurate post-prompt status (e.g. "timed out"
/// vs "denied") and the orchestrator can attribute telemetry per-outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermissionOutcome {
    /// User explicitly approved the action.
    Approved,
    /// User explicitly denied the action.
    Denied,
    /// No response received within the configured window. The orchestrator
    /// treats this as a denial for safety, but UIs should label it distinctly
    /// because the user didn't actually choose.
    TimedOut { waited_secs: u64 },
    /// Approval channel was closed (frontend gone, IPC torn down). Treated
    /// as a denial, surfaced separately so callers can distinguish from a
    /// real "no" or a dropped prompt.
    ChannelClosed,
    /// A decision arrived but its `id` did not match the request we were
    /// waiting on. Indicates an out-of-order or stale UI message; the
    /// orchestrator denies for safety. Carries the ids for diagnostics.
    MismatchedId { expected: String, got: String },
    /// G33 — forward-compat catch-all. Any tag this build doesn't
    /// recognise deserialises here so older readers don't crash on a
    /// newer wire payload. Treated as a denial by [`is_approved`] —
    /// fail-safe for permission decisions specifically.
    #[serde(other)]
    Unknown,
}

/// G27 / P10.4 — coarse decision bucket used by [`AgentEvent::ApprovalDecided`]
/// for analytics (approval rate, timeout rate, decision latency). Wider
/// classifications than [`PermissionOutcome`] collapse to `Denied` so
/// dashboards stay readable; the structured outcome stays available on
/// the sibling [`AgentEvent::PermissionDecision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Denied,
    TimedOut,
}

impl ApprovalDecision {
    /// Lossy projection from the harness's full [`PermissionOutcome`].
    /// Channel-closed, mismatched-id, and unknown collapse to `Denied`
    /// because, from a metrics standpoint, they are user-equivalent
    /// to "no" — the action did not run.
    pub fn from_outcome(o: &PermissionOutcome) -> Self {
        match o {
            PermissionOutcome::Approved => ApprovalDecision::Approved,
            PermissionOutcome::TimedOut { .. } => ApprovalDecision::TimedOut,
            PermissionOutcome::Denied
            | PermissionOutcome::ChannelClosed
            | PermissionOutcome::MismatchedId { .. }
            | PermissionOutcome::Unknown => ApprovalDecision::Denied,
        }
    }
}

impl PermissionOutcome {
    /// Returns true if the outcome should permit tool execution.
    pub fn is_approved(&self) -> bool {
        matches!(self, PermissionOutcome::Approved)
    }

    /// User-facing message embedded in the synthesized tool result when the
    /// action is skipped. Kept compact so it doesn't dilute the LLM context;
    /// `got` in `MismatchedId` is bounded so a malicious or buggy bridge
    /// cannot inject arbitrarily large strings into model context via the
    /// approval channel.
    pub fn skip_message(&self) -> String {
        match self {
            // Reachable only via direct construction; the orchestrator never
            // calls `skip_message` on Approved (it executes the tool instead).
            // Keep this arm so the match is exhaustive and the type is usable
            // in test fixtures, but flag misuse loudly in debug builds.
            PermissionOutcome::Approved => {
                debug_assert!(
                    false,
                    "skip_message called on Approved outcome; callers should execute the tool"
                );
                "Permission granted".to_string()
            }
            PermissionOutcome::Denied => "Permission denied by user".to_string(),
            PermissionOutcome::TimedOut { waited_secs } => format!(
                "Permission request timed out after {}s with no user response (treated as denied)",
                waited_secs
            ),
            PermissionOutcome::ChannelClosed => {
                "Permission channel closed before a decision was made (treated as denied)"
                    .to_string()
            }
            PermissionOutcome::MismatchedId { expected, got } => {
                // Bound `got` so an oversized id (from a malicious or buggy
                // bridge) can't bloat LLM context or smuggle attacker-controlled
                // content. 128 chars is comfortably above any well-formed id
                // (`perm_<tool_use.id>` is typically <40 chars).
                const MAX_GOT: usize = 128;
                let truncated: String = got.chars().take(MAX_GOT).collect();
                let suffix = if got.chars().count() > MAX_GOT {
                    "…(truncated)"
                } else {
                    ""
                };
                format!(
                    "Permission decision id mismatch (expected {}, got {}{}; treated as denied)",
                    expected, truncated, suffix
                )
            }
            PermissionOutcome::Unknown => {
                // G33 — wire payload from a newer producer; we can't
                // know what the user actually decided, so we surface
                // the unknown state explicitly and treat it as a
                // denial in `is_approved`. Distinct phrasing so an
                // operator triaging an incident can tell this apart
                // from a real "no".
                "Permission decision had an unrecognised outcome \
                 (treated as denied; client may be older than producer)"
                    .to_string()
            }
        }
    }
}

// Wire-protocol contract for `AgentEvent`:
//
// `AgentEvent` uses internally-tagged serde (`#[serde(tag = "type")]`).
// As of G33 the enum carries a `#[serde(other)]`-marked `Unknown` unit
// variant, so any `type` tag a reader doesn't recognise deserialises
// to `Unknown` instead of failing — this gives us forward-compatible
// rolling deploys (newer producer / older consumer) without breaking
// the IPC stream. Same treatment applies to [`PermissionOutcome`].
//
// Producers MUST NOT use the literal tag `"unknown"` for any new
// variant; doing so would short-circuit the catch-all on every reader
// and silently drop the new variant's fields.
//
// New variants on the producer side should still be paired with a
// wire-format roundtrip test (see `permission_decision_wire_format`)
// so we don't accidentally break the *backwards* direction (newer
// reader, older producer).
// --- G33 envelope ---

/// G33 — wire envelope wrapping an `AgentEvent` with an explicit
/// schema version. Use `VersionedAgentEvent` whenever events are
/// persisted (replay logs, telemetry sinks, recorded fixtures);
/// untagged in-process IPC streams can keep using bare `AgentEvent`
/// where the lockstep deploy assumption holds.
///
/// `v` is bumped only on a *breaking* change to a variant's payload
/// (renamed field, type change, removed field). Adding a new variant
/// is non-breaking thanks to the `Unknown` catch-all and does NOT
/// require a version bump.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionedAgentEvent {
    /// Schema version. Current value: [`AGENT_EVENT_SCHEMA_VERSION`].
    pub v: u16,
    /// The wrapped event. May deserialise to [`AgentEvent::Unknown`]
    /// if the producer is on a newer schema.
    pub event: AgentEvent,
    /// P13 — monotonic id within the session. Defaults to 0 on legacy
    /// payloads; new producers MUST assign a non-zero value. Clients use
    /// this to bootstrap live-state (`snapshot + events from last_event_id+1`).
    #[serde(default)]
    pub event_id: EventId,
    /// P13 — user-turn grouping. All events emitted in response to a
    /// single user turn share the same `turn_seq`. Defaults to 0.
    #[serde(default)]
    pub turn_seq: u64,
    /// P13 — causal parents by `event_id`. Multiple causes are legal
    /// (e.g. a `PlanAmended` synthesized from 3 critiques).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causal_parent_ids: Vec<EventId>,
}

/// Current schema version for [`VersionedAgentEvent`]. Bump on
/// *breaking* changes only; additive variant changes are absorbed by
/// the `Unknown` catch-all and do not require a bump.
pub const AGENT_EVENT_SCHEMA_VERSION: u16 = 1;

impl VersionedAgentEvent {
    /// Wrap an event with the current schema version. Causality fields are
    /// zeroed — use [`VersionedAgentEvent::with_causality`] when emitting
    /// new events that participate in the P13 introspection stream.
    pub fn current(event: AgentEvent) -> Self {
        Self {
            v: AGENT_EVENT_SCHEMA_VERSION,
            event,
            event_id: EventId(0),
            turn_seq: 0,
            causal_parent_ids: Vec::new(),
        }
    }

    /// P13 — wrap an event with full causal metadata.
    pub fn with_causality(
        event: AgentEvent,
        event_id: EventId,
        turn_seq: u64,
        causal_parent_ids: Vec<EventId>,
    ) -> Self {
        Self {
            v: AGENT_EVENT_SCHEMA_VERSION,
            event,
            event_id,
            turn_seq,
            causal_parent_ids,
        }
    }

    /// `true` iff this envelope was emitted by a newer producer than
    /// this build understands. Readers SHOULD log a warning and may
    /// surface a "client out of date" hint to the user.
    pub fn is_from_newer_producer(&self) -> bool {
        self.v > AGENT_EVENT_SCHEMA_VERSION
    }
}

/// Lightweight reference describing a message group that was evicted from
/// the active context window during compaction. Used in
/// [`AgentEvent::ContextGroupsEvicted`] (G31) so consumers can render *what*
/// was dropped without re-reading the full message body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvictedGroupRef {
    /// Free-form group kind: `"system"`, `"user"`, `"assistant_text"`,
    /// `"tool_call"`, `"summary"`, etc. Stringly-typed so callers in
    /// downstream crates (compaction, history) can stamp their own
    /// taxonomies without forcing a core-side enum dependency.
    pub kind: String,
    /// Number of underlying messages collapsed into this evicted unit.
    pub message_count: u32,
    /// Approximate token cost recovered by dropping this group.
    pub token_count: u32,
    /// Why this group was evicted. Examples: `"oldest-non-system"`,
    /// `"window-overflow"`, `"emergency-budget"`, `"tool-collapse"`.
    pub reason: String,
}

/// P8 — a single persona's critique of a plan or diff, surfaced inside an
/// [`AgentEvent::AwaitingApproval`] event for user triage. One of these is
/// emitted per persona that participated in a fan-out run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Critique {
    /// Persona name that produced this critique (e.g. `"rubber-duck"`,
    /// `"cloud-architect"`).
    pub persona: String,
    /// Coarse severity — informational vs. must-address.
    pub severity: CritiqueSeverity,
    /// Individual findings, one per line. Opaque prose; UIs render verbatim.
    pub findings: Vec<String>,
    /// If true, the critique flags an issue that SHOULD block approval
    /// until addressed. Non-blocking critiques are advisory.
    pub blocking: bool,
}

/// Severity bucket carried by [`Critique`]. Kept deliberately small so UIs
/// render it as a single colored pill.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CritiqueSeverity {
    /// No concerns worth flagging.
    Info,
    /// Worth addressing but does not block the plan.
    Warn,
    /// Must be addressed before proceeding.
    Critical,
}

/// P13 — stable identifier for a single execution of a step. Each attempt
/// (retry, critique persona, sub-agent spawn) gets its own `ExecutionId`.
/// Agents-DAG nodes are keyed by this, not by `StepId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ExecutionId(pub u64);

impl ExecutionId {
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ExecutionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "exec#{}", self.0)
    }
}

/// P13 — monotonic event id within a session. Assigned by the orchestrator
/// when an [`AgentEvent`] is wrapped in [`VersionedAgentEvent`]. Clients use
/// it to (a) reconstruct causal order under concurrent fan-out and (b)
/// bootstrap live-state queries (`snapshot.last_event_id` then subscribe
/// from `N+1`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, PartialOrd, Ord,
)]
pub struct EventId(pub u64);

impl EventId {
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "evt#{}", self.0)
    }
}

/// P13 — structured envelope summary. Exposed on the wire *instead of*
/// the rendered prompt text so clients can query and filter without
/// parsing prose. Rendered text is optional (`display_text`) and MUST NOT
/// be parsed by clients — only displayed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EnvelopeSummaryV1 {
    pub read_scope_count: u32,
    pub write_scope_count: u32,
    pub write_deny_count: u32,
    pub network_enabled: bool,
    pub exec_enabled: bool,
    /// One of: `"always"` | `"per_turn"` | `"never"`.
    pub approval_cadence: String,
    /// One of: `"preset:plan"` | `"preset:research"` | `"preset:act"` |
    /// `"preset:autopilot"` | `"custom"`.
    pub scope_source: String,
    /// Optional human-readable rendering. Clients MUST NOT parse this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
}

/// P13 — edge kinds inside the Agents DAG (who-ran-what topology).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentEdgeKind {
    /// Parent execution spawned a child to do part of its work.
    Delegation,
    /// Critic persona commented on an executor's output.
    Critique,
    /// Mode/persona transition from one executor to another.
    Handoff,
    /// Same step re-executed after failure.
    Retry,
    /// Fresh sub-session (distinct from Delegation: no parent-owns-child).
    Spawn,
}

/// P13 — cross-graph provenance edges linking an execution in the Agents
/// DAG to a mutation in the Features DAG.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceEdgeKind {
    /// Execution is the one doing the step.
    ExecutesStep,
    /// Execution produced a `PlanAmended` against the Features DAG.
    AmendsPlan,
    /// Execution emitted a `ScopeExpansionRequested`.
    ExpandsScope,
}

/// P13 — per-execution assignment row (a node in the Agents DAG).
///
/// One `AssignmentSummaryV1` is emitted for EVERY execution — the primary
/// executor of a step AND each critic persona that fans out. Do NOT collapse
/// critics into edge-only records; clients need the full (persona, model,
/// skills) tuple to answer "which model produced the blocking critique?".
///
/// Security: exact model id and skill/agent names are gated by
/// `include_sensitive` at the bridge layer. The wire shape carries both
/// coarse (`model_vendor` + `model_tier`, `*_count`) and exact fields;
/// the bridge redacts the exact fields for untrusted consumers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssignmentSummaryV1 {
    pub execution_id: ExecutionId,
    pub step_id: StepId,
    /// Stable persona id (e.g. `"rubber-duck"`, `"ml-architect"`).
    pub persona_id: String,
    /// E.g. `"anthropic"`, `"openai"`, `"local"`.
    pub model_vendor: String,
    /// E.g. `"opus"`, `"sonnet"`, `"haiku"`, `"mini"`.
    pub model_tier: String,
    /// Exact model id (e.g. `"claude-opus-4.7"`) — populated only when the
    /// consumer is trusted (`include_sensitive = true`). Omitted on the wire
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id_exact: Option<String>,
    pub activated_skills_count: u32,
    pub activated_agents_count: u32,
    /// Skill names — gated by `include_sensitive`. `None` = redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activated_skill_names: Option<Vec<String>>,
    /// Agent names — gated by `include_sensitive`. `None` = redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activated_agent_names: Option<Vec<String>>,
    /// 1 = first try, 2 = first retry, etc.
    pub attempt: u32,
}

/// P13 — introspection-surface events, versioned so the schema can evolve
/// without minting a new top-level [`AgentEvent`] variant per added field.
///
/// The top-level enum carries [`AgentEvent::Introspection`] as a single
/// variant; all schema churn happens here under the `v` tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntrospectionEventV1 {
    /// Fan-out has been dispatched: `critic_count` critics are running in
    /// parallel against the primary executor's output for `step_id`.
    /// Emitted ONCE before any [`IntrospectionEventV1::StepAssigned`] for
    /// the batch. Clients use this to render a "N critics running..." UI
    /// state and to pre-allocate DAG slots.
    FanoutStarted {
        step_id: StepId,
        parent_execution_id: ExecutionId,
        critic_count: u32,
        personas: Vec<String>,
    },
    /// Fan-out has completed: all critics reached terminal state (success
    /// or runner-error). Emitted AFTER the last
    /// [`IntrospectionEventV1::AgentEdgeRecorded`] for the batch. Carries
    /// the summary counts so a client can verify it saw everything.
    FanoutCompleted {
        step_id: StepId,
        parent_execution_id: ExecutionId,
        critic_count: u32,
        blocking_count: u32,
    },
    /// Envelope snapshot applied to the session/turn.
    EnvelopeApplied { summary: EnvelopeSummaryV1 },
    /// A new execution was assigned. One event per execution (primary + each
    /// critic). Clients build the Agents-DAG node set from these.
    StepAssigned { assignment: AssignmentSummaryV1 },
    /// Execution spawned a sub-session.
    SubAgentSpawned {
        parent_execution_id: ExecutionId,
        child_session_id: String,
        assignment: AssignmentSummaryV1,
    },
    /// Agents-DAG edge between two executions.
    AgentEdgeRecorded {
        edge: AgentEdgeKind,
        from_execution_id: ExecutionId,
        to_execution_id: ExecutionId,
    },
    /// Cross-graph provenance: an execution caused a Features-DAG change.
    ProvenanceRecorded {
        edge: ProvenanceEdgeKind,
        execution_id: ExecutionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_step_id: Option<StepId>,
    },
    /// A critique was emitted by `from_execution_id` about
    /// `target_execution_id`'s output. Emitted live in addition to the
    /// existing [`AgentEvent::AwaitingApproval`] snapshot.
    CritiqueEmitted {
        from_execution_id: ExecutionId,
        target_execution_id: ExecutionId,
        severity: CritiqueSeverity,
        blocking: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum AgentEvent {
    // ── Streaming text ────────────────────────────────────────────────────────
    TextDelta {
        text: String,
    },

    // ── Tool lifecycle ────────────────────────────────────────────────────────
    ToolCallStart {
        id: ToolCallId,
        name: String,
    },
    ToolCallInput {
        id: ToolCallId,
        delta: String,
    },
    ToolCallEnd {
        id: ToolCallId,
    },
    ToolResultStart {
        id: ToolCallId,
        name: String,
    },
    ToolResultEnd {
        id: ToolCallId,
        content: String,
        is_error: bool,
    },

    // ── Thinking / reasoning ──────────────────────────────────────────────────
    ThinkingStarted {
        iteration: u32,
    },
    ReasoningDelta {
        content: String,
    },
    ReasoningComplete {
        content: String,
        duration_ms: u64,
    },

    // ── Context management ────────────────────────────────────────────────────
    ContextWarning {
        level: String, // "warning_70", "warning_85", "critical_95"
        used_tokens: u32,
        max_tokens: u32,
    },
    ContextCompacted {
        freed_tokens: u32,
        before: u32,
        after: u32,
    },
    /// Emitted when one or more message groups are dropped from the active
    /// context window. Carries enough metadata for replay tooling and the UI
    /// to render *what* was lost (kind, role/message count, approximate
    /// tokens) and *why* (strategy name + per-group reason). G31.
    ///
    /// `strategy` identifies the eviction source (e.g. `"truncate-oldest"`,
    /// `"sliding-window"`, `"emergency-truncator"`). `groups` lists each
    /// dropped unit oldest-first. `total_tokens` is the sum of
    /// `EvictedGroupRef::token_count` across `groups` for cheap UI access.
    ContextGroupsEvicted {
        strategy: String,
        groups: Vec<EvictedGroupRef>,
        total_tokens: u32,
    },
    /// Emitted once per stale approval message that the orchestrator drained
    /// from the approval channel while looking for the response that matches
    /// the currently-pending [`AgentEvent::PermissionRequest`]. Surfaces
    /// otherwise-silent buffer-clearing so operators can correlate with UI
    /// double-click bugs, dropped websockets, or out-of-order replies.
    /// G28 — telemetry half (durable per-turn id rebuild deferred).
    DrainedStaleApproval {
        /// The id we *expected* on this turn.
        expected: String,
        /// The id actually pulled out of the channel.
        drained: String,
    },

    /// A new step has been recorded in Plan mode and is awaiting either
    /// (a) execution when the user switches to Act mode, or (b) an
    /// optional user amendment via the `amend_plan` IPC. (G4 / P3.1)
    /// The UI shows this as an editable row in the plan panel.
    PlanStepPending {
        /// 1-indexed step number within the plan (positional, may shift on
        /// plan amendment).
        step: usize,
        /// P13 — stable, revision-independent step id. Use THIS (not `step`)
        /// when storing inter-step relationships. Defaults to 0 for pre-P13
        /// producers; new emitters MUST set it.
        #[serde(default)]
        step_id: StepId,
        /// Per-step revision (starts at 0, bumps on each amendment).
        revision: u64,
        /// Plan-level revision after this step was added.
        plan_revision: u64,
        /// Tool that will be invoked.
        tool_name: String,
        /// Human-readable description (the rendered tool call args).
        description: String,
        /// P13 — Features-DAG edges: step ids this step depends on.
        /// Empty for root steps. Serde default for back-compat.
        #[serde(default)]
        depends_on: Vec<StepId>,
        /// P13 — optional parent step for sub-step decomposition.
        #[serde(default)]
        parent_step_id: Option<StepId>,
    },

    /// An external `amend_plan` IPC mutated the plan. Emitted on
    /// success AND failure so consumers can stay in sync. (G4 / P3.1)
    PlanAmended {
        /// What kind of amendment was attempted ("replace", "insert",
        /// "remove"). Snake_case to match `PlanAmendment` serde.
        kind: String,
        /// The step the amendment targeted (1-indexed). For Insert,
        /// this is the FINAL position of the new step on success.
        step: usize,
        /// `true` if applied, `false` if rejected.
        ok: bool,
        /// Human-readable summary on success, error message on failure.
        reason: String,
        /// Plan-level revision AFTER the amendment (unchanged on
        /// failure — UIs can use this to detect "no-op" reliably).
        plan_revision: u64,
    },

    /// Per-completion token-logprob summary (gap G10 / P3.2). Emitted
    /// once per `provider.chat()` round when the provider returned
    /// `ChatResponse.logprobs = Some(_)`. The UI uses `confidence` to
    /// render a tri-state dot next to the assistant message; the
    /// numeric fields are surfaced on hover. Absence of this event
    /// means "provider does not support logprobs OR they were not
    /// requested" — never treat as low confidence.
    TokenLogprobSummary {
        n_tokens: u32,
        min_token_p: f32,
        mean_token_p: f32,
        confidence: String,
    },

    /// A tool-batch checkpoint was created (gap G13 / P3.3). UI
    /// renders an entry in the timeline with a one-click revert
    /// button. `files` is the count of file snapshots captured.
    CheckpointCreated {
        id: u64,
        turn_index: u32,
        tool_summary: String,
        files: u32,
    },

    /// User invoked revert on a checkpoint (gap G13 / P3.3). `ok =
    /// false` means the checkpoint id was unknown / already reverted /
    /// still open; `reason` carries the diagnostic. UI greys out the
    /// entry on success and shows a toast on failure.
    CheckpointReverted {
        id: u64,
        ok: bool,
        files: u32,
        reason: String,
    },

    /// A background agent reached a terminal state (gap G15 / P3.4).
    /// Mirror of `BackgroundNotification` for consumers that prefer
    /// the `AgentEvent` stream over `subscribe_notifications()`.
    BackgroundAgentDone {
        agent_id: String,
        task_description: String,
        /// "completed" / "failed" / "cancelled".
        kind: String,
        detail: String,
    },

    /// The harness re-resolved the active token budget — typically
    /// when a new model was selected and `TokenBudget::for_model` was
    /// applied (gap G26 / P9.3). UI status bar shows the new ceiling.
    /// `model_id` is the resolved model name; `context_limit` and
    /// `reserved_output` are the new budget values in tokens.
    BudgetUpdated {
        model_id: String,
        context_limit: u32,
        reserved_output: u32,
    },

    // ── Loop / failure detection ──────────────────────────────────────────────
    LoopDetected {
        tool_name: String,
        consecutive_count: u32,
    },
    CircuitBreakerTriggered {
        consecutive_failures: u32,
        last_tools: Vec<String>,
    },

    // ── Execution tree ────────────────────────────────────────────────────────
    ExecutionTreeNode {
        id: String,
        parent_id: Option<String>,
        label: String,
        status: String, // "pending", "running", "completed", "failed"
    },
    ExecutionTreeUpdate {
        id: String,
        status: String,
        detail: Option<String>,
    },

    // ── Structured message parts (for AI Elements rendering) ──────────────────
    MessagePart {
        part_type: MessagePartType,
    },

    // ── Permission / approval ─────────────────────────────────────────────────
    PermissionRequest {
        id: String,
        capability: String,
        description: String,
        /// Raw tool-call input, with top-level secret-shaped keys redacted
        /// via [`redact_secrets_for_event`]. Consumed by zed-side
        /// always-allow matching (regex rules in
        /// `tool_permissions.<tool>.always_allow`) so the matcher can look
        /// at structured fields (`command`, `path`, `url`, ...) instead of
        /// guessing them out of the humanized `description`. `None` on old
        /// persisted events (backward-compat) and on paths that did not
        /// have structured input available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_input: Option<serde_json::Value>,
    },
    /// Emitted after a permission request is resolved (or fails to resolve).
    /// Lets the UI distinguish user-deny from timeout / channel-closed /
    /// id-mismatch scenarios so it can surface the right message and metric.
    PermissionDecision {
        id: String,
        capability: String,
        outcome: PermissionOutcome,
    },

    /// G27 / P10.4 — analytics-friendly companion to [`PermissionDecision`].
    /// Same information, plus the wall-clock latency between `PermissionRequest`
    /// and resolution. Emitted so downstream dashboards can compute
    /// approval-rate, time-to-decision, and timeout-rate without joining
    /// two event streams. Source of preference data for an offline
    /// "what-the-user-usually-approves" pre-filter (Constitutional AI / RLAIF).
    ApprovalDecided {
        /// Tool name (capability) the user was prompted on.
        tool: String,
        /// Resolution bucket — coarser than `PermissionOutcome` so the
        /// dashboard schema is stable across UI variants.
        decision: ApprovalDecision,
        /// Wall-clock milliseconds from prompt → decision; capped at u32::MAX.
        latency_ms: u32,
    },

    // ── Turn lifecycle ────────────────────────────────────────────────────────
    TurnComplete {
        stop_reason: StopReason,
        usage: TokenUsage,
    },
    Error {
        message: String,
    },
    SessionPhaseChanged {
        phase: SessionPhase,
    },

    // ── Routing decisions ─────────────────────────────────────────────────
    RoutingDecision {
        /// All scored candidates (name, kind, score)
        candidates: Vec<RoutingCandidate>,
        /// Which were activated (above threshold)
        activated: Vec<String>,
        /// The threshold used
        threshold: f64,
    },

    /// Synthetic event emitted by [`AgentEventEmitter`] when its mpsc buffer
    /// has been full and one or more events were dropped from the live
    /// channel (gap G27). The dropped events are still preserved in the
    /// emitter's retention ring (P1.4 / G14), so a UI that calls
    /// `replay()` after seeing this event can rebuild the missing slice.
    ///
    /// Emitted at most once per "drop streak": the counter resets to 0
    /// after a successful emit, and the next overflow re-arms the
    /// notification. UIs should treat this as a soft signal that they
    /// missed live events but did NOT lose data.
    EventBufferOverflow {
        dropped_since_last: u64,
    },

    /// A multi-agent coordinator's *critic* LLM call (gap G19). This
    /// surfaces what was previously a hidden third LLM round so the UI
    /// can render it, the budget guard can charge it, and observers
    /// can audit-trail it. Emitted by `Coordinator::critique_and_merge`
    /// once the critic returns; `denied=true` means the gateway hook
    /// refused the call and the coordinator fell back to plain synthesis.
    CritiqueCall {
        /// Model id of the critic (may differ from coordinator/team
        /// model — that's the whole point of having a critic).
        critic_model: String,
        /// How many leaf outputs the critic was shown.
        leaf_count: usize,
        /// Number of conflicts the critic flagged. `0` means agreement
        /// (or the critic missed the conflict — UI should still surface
        /// the count honestly).
        conflicts_found: usize,
        /// Tokens consumed by THIS specific call (not cumulative).
        /// Zero on `denied=true`.
        input_tokens: u32,
        output_tokens: u32,
        /// Wall-clock duration of the critic call in milliseconds.
        /// Zero on `denied=true`.
        duration_ms: u64,
        /// `true` iff a budget/HITL gateway hook refused the call;
        /// the coordinator silently downgraded to single-pass
        /// synthesis. UI should make this visible — silent denial of
        /// a critic is itself a confidence signal.
        denied: bool,
    },

    // ── Verification + test-gate phase events (gap G21) ───────────────────
    /// The post-loop verification phase has begun. Emitted before the
    /// first rollout / test invocation. `strategy` is a short label
    /// ("test_gated" | "rollout_vote") suitable for UI rendering;
    /// `sample_count` is the number of additional rollouts (0 for
    /// `TestGated`, `extra_samples` for `RolloutVote`).
    VerificationStarted {
        strategy: String,
        sample_count: usize,
    },
    /// Verification finished. `ballots_collected` includes the
    /// original answer; `agreed` is true iff a majority winner
    /// emerged AND it differed from the original answer (the only
    /// case where the user-visible answer actually changes). `cancelled`
    /// is true iff the user cancelled mid-verification.
    VerificationCompleted {
        ballots_collected: usize,
        agreed: bool,
        cancelled: bool,
    },
    /// A test-gate run is about to be spawned. The command is sent
    /// joined-as-it-would-be-displayed (NOT shell-parsed) so the UI
    /// can show it verbatim; sensitive args should be filtered by the
    /// caller before reaching this event.
    TestGateStarted {
        command_display: String,
        working_dir: String,
        timeout_secs: u64,
    },
    /// A test-gate run finished. `outcome` is one of
    /// `"pass" | "fail" | "timeout" | "spawn_error" | "cancelled"` —
    /// matches the [`crate::TestGateOutcome`] variants plus the
    /// new G21 cancel state. `exit_code` is `None` on timeout /
    /// spawn_error / cancelled.
    TestGateCompleted {
        outcome: String,
        exit_code: Option<i32>,
        duration_ms: u64,
    },

    // ── Parallel-tool batch diagnostics (gap G29) ─────────────────────────
    /// A parallel tool batch is starting. `parallelisable` is `false`
    /// when the dispatcher has downgraded the batch to sequential
    /// execution (for example because at least one tool is
    /// `ToolKind::Destructive`); UIs should surface that downgrade so
    /// the user can see why their batch isn't actually parallel.
    ParallelToolBatchStarted {
        tool_count: usize,
        parallelisable: bool,
    },
    /// A parallel tool batch finished. `ok_count + error_count ==
    /// tool_count` always holds; `duration_ms` is wall-clock for the
    /// whole batch (max of each tool's runtime, not the sum).
    ParallelToolBatchCompleted {
        tool_count: usize,
        ok_count: usize,
        error_count: usize,
        duration_ms: u64,
    },

    /// G34 / P11.2 — emitted once per individual tool that exceeded its
    /// per-tool wall-clock budget. Always paired with a `Tool` result
    /// carrying `is_error = true`; the dedicated event makes it cheap
    /// for dashboards to count timeouts without parsing tool result
    /// content. `timeout_secs` reflects the budget that was applied
    /// (per-tool override OR global default).
    ToolTimedOut {
        tool: String,
        timeout_secs: u64,
        elapsed_ms: u64,
    },

    /// P11.5 — emitted once per individual tool whose execution was
    /// aborted because the run-level cancellation token fired AFTER
    /// the tool already started. The tool's spawned future is
    /// dropped; the corresponding tool_result message carries
    /// `is_error = true` and a "Tool '...' cancelled" content so the
    /// model can observe the abort. `elapsed_ms` is the time the
    /// tool actually ran before cancellation hit.
    ToolCancelled {
        tool: String,
        elapsed_ms: u64,
    },

    // ── G26 / P7.1: step boundaries ──────────────────────────────────────
    //
    // Bracket every iteration of the agent loop. Downstream consumers
    // (OTel exporter — G23, trajectory replayer — G22) align tool calls
    // with the LLM step that requested them by reading the surrounding
    // `StepStarted` / `StepCompleted` pair. Pre-loop work (history
    // assembly, system prompt resolution) belongs to `StepId::PRELOOP`
    // and is not bracketed.
    /// A new agent loop iteration began. `step_id` is monotonic per
    /// session (allocated via `SessionState::next_step`). Always paired
    /// with a `StepCompleted` carrying the same id, even on error /
    /// cancellation, so consumers can balance the bracket.
    StepStarted {
        step_id: u64,
    },
    /// The current iteration finished. `ok` is `false` if the step
    /// terminated due to error, cancellation, or unmet verification —
    /// downstream telemetry uses this to compute step success rates.
    StepCompleted {
        step_id: u64,
        ok: bool,
    },

    /// P13.2 (G‑R5.2) — the harness reflected on a tool failure
    /// mid‑turn (Shinn et al., Reflexion 2023) and recorded a lesson
    /// that was both stored in `ReflexionMemory` AND inlined into the
    /// failing tool_result so the next provider call sees it in the
    /// same turn (no inter‑attempt delay). UIs SHOULD render this as
    /// an inline note attached to the failed tool, not as a separate
    /// event row, so the user sees what the agent learned.
    ReflexionRecorded {
        /// The tool whose failure triggered the reflection.
        tool: String,
        /// The lesson string that was both stored and shown to the
        /// model. Truncated to 512 chars by emitter contract.
        lesson: String,
    },

    /// P13.6 (G‑R10.1) — the per‑turn critic produced a verdict on
    /// the candidate final response. `accepted=true` means the
    /// harness emitted `TurnComplete`; `accepted=false` means the
    /// harness appended `feedback` as a synthetic user message and
    /// is running a revision turn. `iteration` is the zero‑indexed
    /// revision count BEFORE this verdict (so the first verdict on
    /// a turn is always `iteration=0`).
    ///
    /// Inspired by Self‑Refine (Madaan et al., NeurIPS 2023) and
    /// CRITIC (Gou et al., ICLR 2024). UIs SHOULD render rejects
    /// distinctly so users can see why a turn ran twice.
    CriticVerdict {
        accepted: bool,
        feedback: String,
        iteration: u32,
    },

    // ── Permission envelope (P1b) ─────────────────────────────────────────────
    /// Agent attempted an action outside its PermissionEnvelope. The
    /// orchestrator (or user-facing UI) MAY grant a scope expansion and
    /// resume; otherwise the agent should treat this as a hard stop and ask
    /// the user. This event fires regardless of approval_cadence, including
    /// under Autopilot — scope expansion is the one thing that always
    /// re-prompts.
    ScopeExpansionRequested {
        /// One of: "read" | "write" | "network" | "exec".
        capability: String,
        /// Path, URL, host, or command string depending on capability.
        resource: String,
        /// Machine-readable deny reason, e.g. "NotInAllowList".
        reason: String,
        /// Name of the tool that triggered the check.
        tool: String,
    },

    /// P8 — a plan draft has been critiqued by a fan-out of domain-specialist
    /// personas (and optionally rubber-duck) and is now pending user approval.
    /// UIs SHOULD render the critiques inline with the plan and offer
    /// Accept / Amend / Reject. This is a separate channel from
    /// `PlanAmendment` so fan-out results don't silently mutate the plan.
    AwaitingApproval {
        /// The plan text the critiques are about. Verbatim from the agent.
        plan_revision: String,
        /// One entry per persona that actually ran (rubber-duck + N domain
        /// specialists depending on `FanoutPolicy`).
        critiques: Vec<Critique>,
    },

    /// P13 — the active mode or lens changed on the running session. First-
    /// class (not under `Introspection`) because the mode catalog is stable
    /// and client code often gates UI affordances on it. `from_lens` /
    /// `to_lens` are `Option<String>` because non-Act modes have no lens.
    /// Lens values are the serde-name strings (e.g. `"fast"`, `"normal"`,
    /// `"slow"`); mode values are serde-name strings on `AgentMode`
    /// (e.g. `"plan"`, `"research"`, `"act"`, `"autopilot"`, plus legacy
    /// aliases for back-compat).
    ModeChanged {
        from_mode: String,
        to_mode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_lens: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_lens: Option<String>,
    },

    /// P13 — versioned introspection surface. All envelope/assignment/edge
    /// events ship inside here so schema churn doesn't mint new top-level
    /// variants. Older clients see this under [`AgentEvent::Unknown`].
    Introspection(IntrospectionEventV1),

    /// G33 — forward-compat catch-all. Any `type` tag this build
    /// doesn't recognise deserialises here, so an older reader doesn't
    /// hard-fail on a newer wire payload. UIs SHOULD render this as a
    /// neutral "unrecognised event (upgrade your client)" placeholder
    /// rather than dropping it silently — silent drops would let new
    /// safety-relevant events go invisible on stale clients.
    #[serde(other)]
    Unknown,
}

// G25 — defensive contracts. The retention ring (P1.4) clones every
// `AgentEvent` it stores and broadcasts it across tasks via tokio
// channels, so the type MUST be `Clone + Send + Sync + 'static`.
// These bounds are enforced today by transitive use sites (the code
// won't compile if they're broken), but a contributor adding e.g. a
// `Box<dyn Trait>` field could subtly weaken `Send`/`Sync` and the
// regression would only surface deep inside an async test failure.
// `assert_impl_all!` makes the contract a compile-time error at the
// definition site, with a clear message pointing at this module.
//
// Same contract for `PermissionOutcome` (carried across the IPC
// boundary by the same channels) and `VersionedAgentEvent` (the
// persisted-stream wrapper introduced in G33).
static_assertions::assert_impl_all!(AgentEvent: Clone, Send, Sync);
static_assertions::assert_impl_all!(PermissionOutcome: Clone, Send, Sync);
static_assertions::assert_impl_all!(VersionedAgentEvent: Clone, Send, Sync);

/// A candidate agent/skill evaluated during semantic routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingCandidate {
    pub name: String,
    pub kind: String, // "agent" or "skill"
    pub score: f64,
    pub activated: bool,
}

/// Structured message part types for rich chat rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagePartType {
    Text {
        content: String,
    },
    Reasoning {
        content: String,
        duration_ms: u64,
        is_complete: bool,
    },
    ToolInvocation {
        tool_use_id: String,
        name: String,
        params: serde_json::Value,
        status: String, // "pending", "running", "complete", "error"
        result: Option<String>,
        error: Option<String>,
    },
    CodeArtifact {
        filename: String,
        language: String,
        content: String,
        diff: Option<String>,
    },
    Source {
        href: String,
        title: String,
    },
    Suggestion {
        text: String,
    },
}

// ── Token tracking ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
}

impl TokenUsage {
    pub fn total(&self) -> u32 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    pub context_limit: u32,
    pub used_input: u32,
    pub used_output: u32,
    pub reserved_output: u32,
}

impl TokenBudget {
    /// Default reserved-output headroom. Single source of truth shared with
    /// the storage migration in caduceus-storage (sessions.reserved_output
    /// DEFAULT). Audit finding round-3 (#r3-st-5).
    pub const DEFAULT_RESERVED_OUTPUT: u32 = 8_192;
    /// Default context window assumed when a model isn't recognized.
    pub const DEFAULT_CONTEXT_LIMIT: u32 = 200_000;

    /// Per-model context-aware defaults (gap G12 / P4.4). Returns the
    /// `(context_limit, reserved_output)` tuple for the given model
    /// id; falls back to the conservative defaults above if the id
    /// isn't recognised. Match is by case-insensitive substring on
    /// the model name so both bare ids ("gpt-4o") and provider-
    /// prefixed forms ("openai/gpt-4o-mini-2024-07-18") resolve.
    ///
    /// Sources (Nov 2025): vendor docs for context windows and
    /// max_tokens defaults. We pick a reserved_output that is roughly
    /// the model's documented max output, capped at 1/4 of the
    /// context window to leave room for the prompt.
    pub fn model_spec(model_id: &str) -> (u32, u32) {
        let m = model_id.to_ascii_lowercase();

        // Helper: cap reserved at ¼ of context_limit so a tiny model
        // can't reserve >25% of its window for output.
        let pick = |ctx: u32, max_out: u32| -> (u32, u32) {
            let cap = ctx / 4;
            (ctx, max_out.min(cap.max(1)))
        };

        // Anthropic
        if m.contains("opus-4") {
            return pick(200_000, 32_000);
        }
        if m.contains("sonnet-4") {
            return pick(200_000, 16_000);
        }
        if m.contains("haiku-4") {
            return pick(200_000, 8_192);
        }
        if m.contains("claude-3-5-sonnet") || m.contains("claude-3.5-sonnet") {
            return pick(200_000, 8_192);
        }
        if m.contains("claude-3-5-haiku") || m.contains("claude-3.5-haiku") {
            return pick(200_000, 8_192);
        }
        if m.contains("claude-3-opus") {
            return pick(200_000, 4_096);
        }

        // OpenAI
        if m.contains("gpt-4o-mini") {
            return pick(128_000, 16_384);
        }
        if m.contains("gpt-4o") {
            return pick(128_000, 16_384);
        }
        if m.contains("gpt-4-turbo") || m.contains("gpt-4.1") {
            return pick(128_000, 8_192);
        }
        if m.contains("o1-mini") {
            return pick(128_000, 65_536);
        }
        if m.contains("o1") {
            return pick(200_000, 100_000);
        }
        if m.contains("gpt-3.5") {
            return pick(16_385, 4_096);
        }

        // Google
        if m.contains("gemini-1.5-pro") {
            return pick(2_000_000, 8_192);
        }
        if m.contains("gemini-1.5-flash") {
            return pick(1_000_000, 8_192);
        }
        if m.contains("gemini-2") {
            return pick(1_000_000, 8_192);
        }

        // Mistral
        if m.contains("mistral-large") {
            return pick(128_000, 8_192);
        }

        (Self::DEFAULT_CONTEXT_LIMIT, Self::DEFAULT_RESERVED_OUTPUT)
    }

    /// Build a `TokenBudget` sized for the named model. Per-model
    /// `context_limit` and `reserved_output` come from `model_spec`.
    pub fn for_model(model_id: &str) -> Self {
        let (ctx, reserved) = Self::model_spec(model_id);
        Self {
            context_limit: ctx,
            used_input: 0,
            used_output: 0,
            reserved_output: reserved,
        }
    }
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            context_limit: Self::DEFAULT_CONTEXT_LIMIT,
            used_input: 0,
            used_output: 0,
            reserved_output: Self::DEFAULT_RESERVED_OUTPUT,
        }
    }
}

impl TokenBudget {
    pub fn remaining(&self) -> u32 {
        let used = self.used_input.saturating_add(self.used_output);
        let reserved = used.saturating_add(self.reserved_output);
        self.context_limit.saturating_sub(reserved)
    }

    pub fn fill_fraction(&self) -> f64 {
        let used = self.used_input.saturating_add(self.used_output);
        if self.context_limit == 0 {
            return 0.0;
        }
        used as f64 / self.context_limit as f64
    }

    pub fn needs_compaction(&self) -> bool {
        self.fill_fraction() > 0.85
    }

    /// Return the current warning level based on context utilization.
    pub fn warning_level(&self) -> WarningLevel {
        let frac = self.fill_fraction();
        if frac >= 0.95 {
            WarningLevel::Critical95
        } else if frac >= 0.85 {
            WarningLevel::Warning85
        } else if frac >= 0.70 {
            WarningLevel::Warning70
        } else {
            WarningLevel::None
        }
    }
}

// ── Tool types ─────────────────────────────────────────────────────────────────

/// Side-effect classification for a tool. Used by the parallel
/// dispatcher (gap G20) to decide whether two queued tool calls may
/// safely run concurrently.
///
/// Defaults to [`ToolKind::Destructive`] under serde so an older
/// persisted spec (or a tool that forgot to set this) is treated as
/// the most dangerous case — fail safe, never fail open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Pure read; never mutates state on the local machine, the
    /// filesystem, or any remote service. Two `ReadOnly` calls may
    /// always run in parallel, even on the same resource.
    ReadOnly,
    /// Mutates state, but the operation is idempotent: applying it
    /// twice yields the same result as applying it once. Safe to
    /// retry; *not* generally safe to run concurrently with another
    /// tool on the same resource.
    Idempotent,
    /// Mutates state with no idempotency guarantee. Must serialise
    /// against everything else in the same batch — a destructive
    /// task in a batch downgrades the whole batch to sequential
    /// execution.
    #[default]
    Destructive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub required_capability: Option<String>,
}

/// An LLM's request to invoke a tool (extracted from the API response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    /// The tool_use id this result corresponds to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
}

impl ToolResult {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            tool_use_id: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            is_error: true,
            tool_use_id: None,
        }
    }

    pub fn with_tool_use_id(mut self, id: impl Into<String>) -> Self {
        self.tool_use_id = Some(id.into());
        self
    }
}

// ── Turn budget (gap G11) ─────────────────────────────────────────────────────

/// Per-turn execution budget. Bounds tool-call count, cumulative tool wall-
/// clock, and total bytes of tool output a single agent turn may consume,
/// so a runaway loop can't fire 50 iterations × 4 parallel calls × 100 KiB
/// each before the model itself decides to stop.
///
/// This is **orchestrator-side** rate limiting, distinct from
/// `TokenBudget` (provider context) and the `tool_timeout` per single call.
/// The full picture is:
///
/// | Layer            | Bounds                                      |
/// |------------------|---------------------------------------------|
/// | `TokenBudget`    | input + output tokens vs context window     |
/// | `tool_timeout`   | wall-clock of one tool call                 |
/// | `TurnBudget`     | call count + cumulative wall-clock + bytes  |
///
/// Defaults are sized to the conservative "deeply automated refactor"
/// envelope used by the reference workflows; raise them per-host (CI,
/// long-running batch agents) via the builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnBudget {
    pub max_tool_calls: u32,
    pub max_total_tool_seconds: u64,
    pub max_total_bytes_read: u64,
}

impl TurnBudget {
    pub const DEFAULT_MAX_TOOL_CALLS: u32 = 200;
    pub const DEFAULT_MAX_TOTAL_TOOL_SECONDS: u64 = 600; // 10 min
    pub const DEFAULT_MAX_TOTAL_BYTES_READ: u64 = 50 * 1024 * 1024; // 50 MiB

    pub fn unlimited() -> Self {
        Self {
            max_tool_calls: u32::MAX,
            max_total_tool_seconds: u64::MAX,
            max_total_bytes_read: u64::MAX,
        }
    }
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self {
            max_tool_calls: Self::DEFAULT_MAX_TOOL_CALLS,
            max_total_tool_seconds: Self::DEFAULT_MAX_TOTAL_TOOL_SECONDS,
            max_total_bytes_read: Self::DEFAULT_MAX_TOTAL_BYTES_READ,
        }
    }
}

/// Mutable accumulator paired with a [`TurnBudget`] for a single turn.
/// Created at turn start, updated after every tool call, queried before
/// each new round.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnBudgetUsage {
    pub tool_calls: u32,
    pub total_tool_seconds: u64,
    pub total_bytes_read: u64,
}

/// Reason the budget tripped, surfaced in events / logs / tool-result
/// synth so the model can see exactly why we stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BudgetBreach {
    ToolCalls { used: u32, limit: u32 },
    ToolSeconds { used: u64, limit: u64 },
    BytesRead { used: u64, limit: u64 },
}

impl BudgetBreach {
    /// Single-line message suitable for both the synthesised tool result
    /// and a UI toast. Stable wording — assert on it from tests.
    pub fn message(&self) -> String {
        match self {
            BudgetBreach::ToolCalls { used, limit } => format!(
                "Turn budget exceeded: {used} tool calls (limit {limit}). \
                 Stopping turn; raise turn_budget.max_tool_calls to continue."
            ),
            BudgetBreach::ToolSeconds { used, limit } => format!(
                "Turn budget exceeded: {used}s of tool wall-clock (limit {limit}s). \
                 Stopping turn; raise turn_budget.max_total_tool_seconds to continue."
            ),
            BudgetBreach::BytesRead { used, limit } => format!(
                "Turn budget exceeded: {used} bytes of tool output (limit {limit}). \
                 Stopping turn; raise turn_budget.max_total_bytes_read to continue."
            ),
        }
    }
}

impl TurnBudgetUsage {
    /// Add one tool call's bookkeeping. Saturating arithmetic so a buggy
    /// caller can't wrap around and silently re-arm the budget.
    pub fn record(&mut self, wall_seconds: u64, bytes_read: u64) {
        self.tool_calls = self.tool_calls.saturating_add(1);
        self.total_tool_seconds = self.total_tool_seconds.saturating_add(wall_seconds);
        self.total_bytes_read = self.total_bytes_read.saturating_add(bytes_read);
    }

    /// Returns `Some(BudgetBreach)` if the current usage exceeds any limit.
    /// Caller should bail and emit `StopReason::BudgetExceeded`.
    pub fn check(&self, budget: &TurnBudget) -> Option<BudgetBreach> {
        if self.tool_calls > budget.max_tool_calls {
            return Some(BudgetBreach::ToolCalls {
                used: self.tool_calls,
                limit: budget.max_tool_calls,
            });
        }
        if self.total_tool_seconds > budget.max_total_tool_seconds {
            return Some(BudgetBreach::ToolSeconds {
                used: self.total_tool_seconds,
                limit: budget.max_total_tool_seconds,
            });
        }
        if self.total_bytes_read > budget.max_total_bytes_read {
            return Some(BudgetBreach::BytesRead {
                used: self.total_bytes_read,
                limit: budget.max_total_bytes_read,
            });
        }
        None
    }
}

// ── Project context ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectContext {
    pub root: PathBuf,
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub file_count: usize,
    pub token_estimate: u32,
    pub context_summary: String,
}

// ── Config ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: ProviderId,
    pub display_name: String,
    pub base_url: Option<String>,
    pub default_model: ModelId,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaduceusConfig {
    pub default_provider: ProviderId,
    pub default_model: ModelId,
    pub storage_path: PathBuf,
    pub log_level: String,
    pub max_context_tokens: u32,
    pub providers: HashMap<String, ProviderConfig>,
    pub permissions: PermissionDefaults,
}

impl Default for CaduceusConfig {
    fn default() -> Self {
        Self {
            default_provider: ProviderId::new("anthropic"),
            default_model: ModelId::new("claude-sonnet-4-6"),
            storage_path: PathBuf::from("~/.caduceus/db.sqlite"),
            log_level: "info".into(),
            max_context_tokens: 200_000,
            providers: HashMap::new(),
            permissions: PermissionDefaults::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionDefaults {
    pub fs_read: bool,
    pub fs_write: PermissionMode,
    pub process_exec: PermissionMode,
    pub network_http: PermissionMode,
    pub git_mutate: PermissionMode,
}

impl Default for PermissionDefaults {
    fn default() -> Self {
        Self {
            fs_read: true,
            fs_write: PermissionMode::PromptPerSession,
            process_exec: PermissionMode::PromptPerAction,
            network_http: PermissionMode::PromptPerSession,
            git_mutate: PermissionMode::PromptPerAction,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    Allow,
    Deny,
    PromptPerSession,
    PromptPerAction,
}

// ── Audit log ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub session_id: SessionId,
    pub capability: String,
    pub tool_name: String,
    pub args_redacted: String,
    pub decision: AuditDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditDecision {
    Allowed,
    Denied,
    UserApproved,
    UserDenied,
}

// ── Memory ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: Uuid,
    pub session_id: SessionId,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl MemoryEntry {
    pub fn new(session_id: SessionId, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            content: content.into(),
            tags: Vec::new(),
            created_at: chrono::Utc::now(),
        }
    }
}

// ── Errors ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum CaduceusError {
    #[error("Storage error: {0}")]
    Storage(String),
    #[error("Provider error: {0}")]
    Provider(String),
    /// Provider call exceeded its time budget. Treated as transient
    /// by `RetryAdapter` (mirrors rate-limit/network-class errors)
    /// so the caller can re-run the turn; distinct variant so UI and
    /// telemetry can render a timeout-specific diagnostic.
    #[error("Provider timeout after {elapsed_ms}ms (limit {limit_ms}ms): {context}")]
    ProviderTimeout {
        elapsed_ms: u64,
        limit_ms: u64,
        context: String,
    },
    #[error("Rate limited: retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("Context overflow: {used} tokens used, limit is {limit}")]
    ContextOverflow { used: u32, limit: u32 },
    #[error("Permission denied: {capability} for {tool}")]
    PermissionDenied { capability: String, tool: String },
    #[error("Tool error in {tool}: {message}")]
    Tool { tool: String, message: String },
    #[error("Config error: {0}")]
    Config(String),
    #[error("Session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("Cancelled by user")]
    Cancelled,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, CaduceusError>;

// ── ST7: SubAgent failure taxonomy ────────────────────────────────────────────

/// Coarse phase a sub-agent task is in when an error / timeout fires.
/// Computed locally from observed [`AgentEvent`] traffic on the sub-agent's
/// emitter; see plan v3.1 Fix 2 for the transition table. Stable wire-shape
/// (`#[serde(tag="phase")]`) so downstream consumers can match on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SubAgentPhase {
    /// Default at spawn entry, before any provider/tool/context event.
    ModelSelection,
    /// Provider call active: at least one of `ThinkingStarted`,
    /// `ReasoningDelta`, `TextDelta` has been observed since the last
    /// `ToolCallStart`.
    ProviderCall,
    /// A tool is executing. Set on `ToolCallStart`. Per v3.1 Fix 2,
    /// `ToolResultEnd` does NOT transition out of this phase — only the
    /// next provider-side event does.
    ToolExecution,
    /// `ContextWarning` / `ContextCompacted` / `ContextGroupsEvicted`
    /// observed.
    ContextManagement,
    /// No phase signal observed yet (or signal not classifiable).
    Unknown,
}

/// Retry hint emitted alongside `SubAgentFailure::ProviderError`. Master
/// orchestrator uses this to decide reroute / immediate retry / surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RetryClass {
    /// Transient transport blip — retry immediately, same provider.
    Immediate,
    /// Rate-limit / overload — wait `retry_after_secs` then retry.
    Backoff,
    /// Auth / malformed / policy — do not retry.
    NonRetriable,
}

/// Detail payload for [`SubAgentFailure::Timeout`]. Newtype-wrapped
/// (`#[non_exhaustive]`) so adding fields is non-breaking. See plan v2 B7.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TimeoutFailure {
    pub elapsed_secs: u64,
    pub timeout_secs: u64,
    pub last_phase: SubAgentPhase,
    pub tools_started: bool,
}

impl TimeoutFailure {
    pub fn new(
        elapsed_secs: u64,
        timeout_secs: u64,
        last_phase: SubAgentPhase,
        tools_started: bool,
    ) -> Self {
        Self {
            elapsed_secs,
            timeout_secs,
            last_phase,
            tools_started,
        }
    }
}

/// Detail payload for [`SubAgentFailure::ProviderError`]. Provider/model
/// are `Option` because at the zed-tool boundary the typed `CaduceusError`
/// is already collapsed to an `anyhow::Error`; classification done in
/// caduceus-orchestrator preserves the typed values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProviderErrorFailure {
    pub provider: Option<ProviderId>,
    pub model: Option<ModelId>,
    pub message: String,
    pub retry_class: RetryClass,
    pub retry_after_secs: Option<u64>,
    pub http_status: Option<u16>,
}

impl ProviderErrorFailure {
    pub fn new(message: impl Into<String>, retry_class: RetryClass) -> Self {
        Self {
            provider: None,
            model: None,
            message: message.into(),
            retry_class,
            retry_after_secs: None,
            http_status: None,
        }
    }
}

/// Structured outcome of a failed sub-agent spawn. Tagged on the wire
/// (`#[serde(tag="failure_type")]`) so the LLM-visible JSON in
/// `SpawnAgentToolOutput::Error` carries an explicit discriminant.
///
/// Per plan v3.1: `ContextExhausted` is intentionally absent — folded into
/// ST7-followup-A (StopReason discriminant lift).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "failure_type", content = "details")]
#[non_exhaustive]
pub enum SubAgentFailure {
    Timeout(TimeoutFailure),
    ProviderError(ProviderErrorFailure),
    ToolError {
        tool_name: String,
        message: String,
    },
    UserCancel,
    PolicyDenied {
        capability: String,
        tool: String,
        reason: String,
    },
    RecursionLimitExceeded {
        current_depth: u8,
        max_depth: u8,
    },
    ModelRefusal {
        refusal_text: String,
    },
    /// Catch-all for shapes we cannot classify (e.g. `anyhow::Other`,
    /// channel-closed pre-ST7-prereq). `kind` is a stable string so the
    /// LLM can branch without reading `message`.
    InternalError {
        kind: String,
        message: String,
    },
}

impl SubAgentFailure {
    /// Stable string discriminant matching the `failure_type` serde tag.
    /// Useful for telemetry / classification without serde round-trip.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Timeout(_) => "Timeout",
            Self::ProviderError(_) => "ProviderError",
            Self::ToolError { .. } => "ToolError",
            Self::UserCancel => "UserCancel",
            Self::PolicyDenied { .. } => "PolicyDenied",
            Self::RecursionLimitExceeded { .. } => "RecursionLimitExceeded",
            Self::ModelRefusal { .. } => "ModelRefusal",
            Self::InternalError { .. } => "InternalError",
        }
    }
}

impl fmt::Display for SubAgentFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout(t) => write!(
                f,
                "sub-agent timeout: {}s elapsed, limit {}s, last_phase={:?}",
                t.elapsed_secs, t.timeout_secs, t.last_phase
            ),
            Self::ProviderError(p) => write!(f, "provider error: {}", p.message),
            Self::ToolError { tool_name, message } => {
                write!(f, "tool error in {tool_name}: {message}")
            }
            Self::UserCancel => write!(f, "cancelled by user"),
            Self::PolicyDenied {
                capability,
                tool,
                reason,
            } => write!(f, "policy denied: {capability} for {tool}: {reason}"),
            Self::RecursionLimitExceeded {
                current_depth,
                max_depth,
            } => write!(
                f,
                "Maximum subagent depth ({max_depth}) reached (current depth {current_depth})"
            ),
            Self::ModelRefusal { refusal_text } => write!(f, "model refusal: {refusal_text}"),
            Self::InternalError { kind, message } => write!(f, "internal error [{kind}]: {message}"),
        }
    }
}

impl std::error::Error for SubAgentFailure {}

/// Caller-supplied context for [`classify_caduceus_error`]. Carries the
/// `(provider, model)` pair that the typed `CaduceusError` itself does not
/// preserve (because variants like `Provider(String)` / `RateLimited` /
/// `ProviderTimeout` are intentionally provider-agnostic).
///
/// Per ST7 must-fix #2 / plan v3 §A: `ProviderErrorFailure` MUST surface
/// `provider`/`model` whenever the call-site has them, so ST8's
/// vendor-rerouting decision can branch correctly. Adding fields to every
/// `CaduceusError::Provider*` variant would touch dozens of construction
/// sites across the workspace; threading a context struct from the
/// classifier call site (which always has the in-flight model and provider
/// in scope — `Dispatcher`, `Harness`, etc.) is the surgical alternative.
#[derive(Debug, Clone, Default)]
pub struct ClassifyContext {
    pub provider: Option<ProviderId>,
    pub model: Option<ModelId>,
}

impl ClassifyContext {
    pub fn new(provider: Option<ProviderId>, model: Option<ModelId>) -> Self {
        Self { provider, model }
    }

    /// Empty context — use only at boundaries where provider/model are not
    /// in scope (e.g. zed `spawn_agent_tool` after the `anyhow::Error`
    /// collapse). Prefer threading a populated context whenever possible.
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Classify a typed [`CaduceusError`] into a [`SubAgentFailure`]. The
/// `ctx` argument supplies `(provider, model)` for `ProviderError`
/// variants (ST7 must-fix #2 — populated for ST8's vendor-rerouting
/// decision).
pub fn classify_caduceus_error(
    err: &CaduceusError,
    ctx: &ClassifyContext,
    last_phase: SubAgentPhase,
    tools_started: bool,
    elapsed_secs: u64,
    timeout_secs: u64,
) -> SubAgentFailure {
    match err {
        CaduceusError::Cancelled => SubAgentFailure::UserCancel,
        CaduceusError::PermissionDenied { capability, tool } => SubAgentFailure::PolicyDenied {
            capability: capability.clone(),
            tool: tool.clone(),
            reason: "permission denied".into(),
        },
        CaduceusError::Tool { tool, message } => SubAgentFailure::ToolError {
            tool_name: tool.clone(),
            message: message.clone(),
        },
        CaduceusError::RateLimited { retry_after_secs } => {
            SubAgentFailure::ProviderError(ProviderErrorFailure {
                provider: ctx.provider.clone(),
                model: ctx.model.clone(),
                message: format!("rate limited: retry after {retry_after_secs}s"),
                retry_class: RetryClass::Backoff,
                retry_after_secs: Some(*retry_after_secs),
                http_status: Some(429),
            })
        }
        CaduceusError::ProviderTimeout {
            elapsed_ms,
            limit_ms,
            context,
        } => SubAgentFailure::ProviderError(ProviderErrorFailure {
            provider: ctx.provider.clone(),
            model: ctx.model.clone(),
            message: format!(
                "provider timeout after {elapsed_ms}ms (limit {limit_ms}ms): {context}"
            ),
            retry_class: RetryClass::Backoff,
            retry_after_secs: None,
            http_status: None,
        }),
        CaduceusError::Provider(msg) => SubAgentFailure::ProviderError(ProviderErrorFailure {
            provider: ctx.provider.clone(),
            model: ctx.model.clone(),
            message: msg.clone(),
            retry_class: RetryClass::Immediate,
            retry_after_secs: None,
            http_status: None,
        }),
        CaduceusError::ContextOverflow { used, limit } => {
            // Pre-ST7-followup-A: surfaced as InternalError so callers
            // can branch without prematurely committing to a public
            // ContextExhausted shape we can't reliably populate yet.
            let _ = (last_phase, tools_started, elapsed_secs, timeout_secs);
            SubAgentFailure::InternalError {
                kind: "context_overflow".into(),
                message: format!("{used} tokens used, limit {limit}"),
            }
        }
        other => SubAgentFailure::InternalError {
            kind: "caduceus_error".into(),
            message: other.to_string(),
        },
    }
}

impl SubAgentPhase {
    /// Apply v3.1 Fix 2 transition table. Returns the new phase given the
    /// observed [`AgentEvent`]. Unhandled variants leave the phase
    /// unchanged.
    ///
    /// Critical invariant: `ToolResultEnd` does NOT exit `ToolExecution`.
    /// Only the next provider-side event (`ThinkingStarted` /
    /// `ReasoningDelta` / `TextDelta`) does.
    pub fn next_phase(self, event: &AgentEvent) -> Self {
        match event {
            AgentEvent::ContextWarning { .. }
            | AgentEvent::ContextCompacted { .. }
            | AgentEvent::ContextGroupsEvicted { .. } => Self::ContextManagement,
            AgentEvent::ToolCallStart { .. } => Self::ToolExecution,
            AgentEvent::ThinkingStarted { .. }
            | AgentEvent::ReasoningDelta { .. }
            | AgentEvent::TextDelta { .. } => Self::ProviderCall,
            // ToolResultStart / ToolResultEnd: stay in ToolExecution.
            // RoutingDecision: emitted while in ModelSelection, no transition.
            // SessionPhaseChanged / others: leave phase untouched.
            _ => self,
        }
    }
}

// ── Traits ─────────────────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait SessionStorage: Send + Sync {
    async fn create_session(&self, state: &SessionState) -> Result<()>;
    async fn load_session(&self, id: &SessionId) -> Result<Option<SessionState>>;
    async fn update_session(&self, state: &SessionState) -> Result<()>;
    async fn list_sessions(&self, limit: usize) -> Result<Vec<SessionState>>;
    async fn delete_session(&self, id: &SessionId) -> Result<()>;
}

#[async_trait::async_trait]
pub trait AuthStore: Send + Sync {
    async fn get_api_key(&self, provider_id: &ProviderId) -> Result<Option<String>>;
    async fn set_api_key(&self, provider_id: &ProviderId, key: &str) -> Result<()>;
    async fn delete_api_key(&self, provider_id: &ProviderId) -> Result<()>;
}

// ── Tests ──────────────────────────────────────────────────────────────────

// ── P0: Directory Conventions ──────────────────────────────────────────────────

/// Runtime feature toggles — read from `CADUCEUS_FEATURE_*` environment variables.
/// These control which subsystems are active in the current process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlags {
    pub proactive_mode: bool,
    pub voice_input: bool,
    pub team_sync: bool,
    pub mcp_runtime: bool,
    pub crdt_collab: bool,
    pub auto_memory: bool,
    pub otel_tracing: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            proactive_mode: false,
            voice_input: false,
            team_sync: false,
            mcp_runtime: true,
            crdt_collab: true,
            auto_memory: true,
            otel_tracing: false,
        }
    }
}

impl FeatureFlags {
    pub fn from_env() -> Self {
        Self {
            proactive_mode: std::env::var("CADUCEUS_FEATURE_PROACTIVE")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
            voice_input: std::env::var("CADUCEUS_FEATURE_VOICE")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
            team_sync: std::env::var("CADUCEUS_FEATURE_TEAM_SYNC")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
            mcp_runtime: std::env::var("CADUCEUS_FEATURE_MCP")
                .map(|v| v != "0" && v != "false")
                .unwrap_or(true),
            crdt_collab: std::env::var("CADUCEUS_FEATURE_CRDT")
                .map(|v| v != "0" && v != "false")
                .unwrap_or(true),
            auto_memory: std::env::var("CADUCEUS_FEATURE_AUTO_MEMORY")
                .map(|v| v != "0" && v != "false")
                .unwrap_or(true),
            otel_tracing: std::env::var("CADUCEUS_FEATURE_OTEL")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
        }
    }
}

/// Standardized paths for Caduceus configuration, storage, and cache.
pub struct CaduceusPaths;

impl CaduceusPaths {
    fn home_dir() -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn config_dir() -> PathBuf {
        Self::home_dir().join(".caduceus")
    }

    pub fn config_file() -> PathBuf {
        Self::config_dir().join("config.toml")
    }

    pub fn db_file() -> PathBuf {
        Self::config_dir().join("db.sqlite")
    }

    pub fn cache_dir() -> PathBuf {
        Self::config_dir().join("cache")
    }

    pub fn logs_dir() -> PathBuf {
        Self::config_dir().join("logs")
    }

    pub fn guidelines_dir() -> PathBuf {
        Self::config_dir().join("guidelines")
    }

    pub fn project_config_file(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".caduceus").join("config.toml")
    }

    /// Create all standard directories if they don't exist.
    pub fn ensure_dirs() -> std::io::Result<()> {
        std::fs::create_dir_all(Self::config_dir())?;
        std::fs::create_dir_all(Self::cache_dir())?;
        std::fs::create_dir_all(Self::logs_dir())?;
        std::fs::create_dir_all(Self::guidelines_dir())?;
        Self::seed_guidelines()?;
        Ok(())
    }

    /// Seed built-in guideline templates into `~/.caduceus/guidelines/`.
    ///
    /// Only writes files that don't already exist — user edits are never
    /// overwritten. New templates shipped in future releases will appear on
    /// next launch; removed/renamed templates stay behind (user owns them).
    pub fn seed_guidelines() -> std::io::Result<()> {
        let dir = Self::guidelines_dir();
        let templates: &[(&str, &str)] = &[(
            "strategy-selection.md",
            include_str!("../../../resources/guidelines/strategy-selection.md"),
        )];
        for (name, body) in templates {
            let path = dir.join(name);
            if !path.exists() {
                std::fs::write(&path, body)?;
            }
        }
        Ok(())
    }
}

// ── P0: Configuration Layering ─────────────────────────────────────────────────

/// Partial config for layered merging. All fields are optional so partial
/// TOML files can be deserialized without providing every field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialConfig {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub storage_path: Option<String>,
    pub log_level: Option<String>,
    pub max_context_tokens: Option<u32>,
    pub providers: Option<HashMap<String, ProviderConfig>>,
    pub permissions: Option<PermissionDefaults>,
}

/// Loads and merges configuration from multiple sources in priority order:
/// 1. CLI overrides
/// 2. Environment variables
/// 3. Project config (.caduceus/config.toml in workspace root)
/// 4. Global config (~/.caduceus/config.toml)
/// 5. Defaults
pub struct ConfigLoader {
    cli_overrides: HashMap<String, String>,
    workspace_root: Option<PathBuf>,
}

impl ConfigLoader {
    pub fn new() -> Self {
        Self {
            cli_overrides: HashMap::new(),
            workspace_root: None,
        }
    }

    pub fn with_cli_overrides(mut self, overrides: HashMap<String, String>) -> Self {
        self.cli_overrides = overrides;
        self
    }

    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    fn load_toml_file(path: &Path) -> Option<PartialConfig> {
        let content = std::fs::read_to_string(path).ok()?;
        toml::from_str(&content).ok()
    }

    fn load_env() -> PartialConfig {
        PartialConfig {
            default_provider: std::env::var("CADUCEUS_PROVIDER").ok(),
            default_model: std::env::var("CADUCEUS_MODEL").ok(),
            storage_path: std::env::var("CADUCEUS_STORAGE_PATH").ok(),
            log_level: std::env::var("CADUCEUS_LOG_LEVEL").ok(),
            max_context_tokens: std::env::var("CADUCEUS_MAX_CONTEXT_TOKENS")
                .ok()
                .and_then(|v| v.parse().ok()),
            providers: None,
            permissions: None,
        }
    }

    fn cli_to_partial(overrides: &HashMap<String, String>) -> PartialConfig {
        PartialConfig {
            default_provider: overrides.get("provider").cloned(),
            default_model: overrides.get("model").cloned(),
            storage_path: overrides.get("storage_path").cloned(),
            log_level: overrides.get("log_level").cloned(),
            max_context_tokens: overrides
                .get("max_context_tokens")
                .and_then(|v| v.parse().ok()),
            providers: None,
            permissions: None,
        }
    }

    fn merge_partial(base: &mut CaduceusConfig, partial: &PartialConfig) {
        if let Some(ref p) = partial.default_provider {
            base.default_provider = ProviderId::new(p);
        }
        if let Some(ref m) = partial.default_model {
            base.default_model = ModelId::new(m);
        }
        if let Some(ref s) = partial.storage_path {
            base.storage_path = PathBuf::from(s);
        }
        if let Some(ref l) = partial.log_level {
            base.log_level.clone_from(l);
        }
        if let Some(t) = partial.max_context_tokens {
            base.max_context_tokens = t;
        }
        if let Some(ref providers) = partial.providers {
            for (k, v) in providers {
                base.providers.insert(k.clone(), v.clone());
            }
        }
        if let Some(ref perms) = partial.permissions {
            base.permissions = perms.clone();
        }
    }

    /// Load and merge config from all sources. Priority: CLI > env > project > global > defaults.
    pub fn load(&self) -> CaduceusConfig {
        let mut config = CaduceusConfig::default();

        // Layer 5: defaults (already set)

        // Layer 4: global config
        let global_path = CaduceusPaths::config_file();
        if let Some(global) = Self::load_toml_file(&global_path) {
            Self::merge_partial(&mut config, &global);
        }

        // Layer 3: project config
        if let Some(ref root) = self.workspace_root {
            let project_path = CaduceusPaths::project_config_file(root);
            if let Some(project) = Self::load_toml_file(&project_path) {
                Self::merge_partial(&mut config, &project);
            }
        }

        // Layer 2: environment variables
        let env_config = Self::load_env();
        Self::merge_partial(&mut config, &env_config);

        // Layer 1: CLI overrides (highest priority)
        let cli_config = Self::cli_to_partial(&self.cli_overrides);
        Self::merge_partial(&mut config, &cli_config);

        config
    }
}

impl Default for ConfigLoader {
    fn default() -> Self {
        Self::new()
    }
}

// ── P0: Cancellation Token ─────────────────────────────────────────────────────

/// Thread-safe cancellation token wrapping an `Arc<AtomicBool>` plus a
/// monotonic generation counter (ST-A2b).
///
/// The generation counter binds a cancel signal to the turn that requested
/// it. A long-lived harness that serves many turns can bump the generation
/// at the start of each turn, then UI callers cancel *for a specific
/// generation*. A late-arriving cancel from turn N cannot poison turn N+1
/// because its generation no longer matches.
///
/// Legacy `cancel()` / `is_cancelled()` / `check()` / `reset()` remain
/// generation-agnostic for paths that never reuse a token across turns.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Signal cancellation (generation-agnostic).
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Signal cancellation *only* if the current generation equals `gen`.
    /// Returns `true` when the cancel took effect.
    ///
    /// This is the ST-A2b API: UI captures the generation at turn start
    /// and passes it back when the user clicks stop. A stale cancel
    /// request for a prior turn is silently no-op'd here.
    pub fn cancel_for_generation(&self, gen: u64) -> bool {
        if self.current_generation() == gen {
            self.cancelled.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// Check if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Return `Err(CaduceusError::Cancelled)` if cancellation has been requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(CaduceusError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Current generation. Callers snapshot this at turn start and pass
    /// it back via `cancel_for_generation` to bind a cancel to the turn.
    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Increment the generation and return the new value. Also clears any
    /// lingering cancel flag so the new generation starts clean. Call at
    /// the start of every turn on a long-lived harness.
    pub fn bump_generation(&self) -> u64 {
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.cancelled.store(false, Ordering::SeqCst);
        next
    }

    /// Clear the cancelled flag so the token can be reused for a fresh run.
    ///
    /// Without this, a long-lived `AgentHarness` that gets cancelled once
    /// would refuse every subsequent `run` (audit finding #9). Callers
    /// reusing a harness across user requests should reset between runs;
    /// `AgentHarness::reset_cancellation` automates the bookkeeping.
    ///
    /// Note: `reset()` does NOT bump the generation. Use `bump_generation`
    /// when entering a new turn on a shared harness.
    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

// ── P1: Token Warning Levels ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WarningLevel {
    None,
    Warning70,
    Warning85,
    Critical95,
}

// ── Feature Flags (Feature #50) ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlag {
    pub name: String,
    pub enabled: bool,
    pub description: String,
    pub rollout_percentage: Option<u8>, // 0-100
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeatureFlagRegistry {
    flags: HashMap<String, FeatureFlag>,
}

impl FeatureFlagRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: &str, desc: &str, default: bool) {
        self.flags.insert(
            name.to_string(),
            FeatureFlag {
                name: name.to_string(),
                enabled: default,
                description: desc.to_string(),
                rollout_percentage: None,
            },
        );
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.flags.get(name).map(|f| f.enabled).unwrap_or(false)
    }

    pub fn set(&mut self, name: &str, enabled: bool) {
        if let Some(flag) = self.flags.get_mut(name) {
            flag.enabled = enabled;
        }
    }

    pub fn set_rollout(&mut self, name: &str, percentage: u8) {
        if let Some(flag) = self.flags.get_mut(name) {
            flag.rollout_percentage = Some(percentage.min(100));
        }
    }

    /// Deterministic per-user rollout: returns `true` if `user_hash % 100 < percentage`.
    pub fn check_rollout(&self, name: &str, user_hash: u64) -> bool {
        let Some(flag) = self.flags.get(name) else {
            return false;
        };
        if !flag.enabled {
            return false;
        }
        match flag.rollout_percentage {
            None => flag.enabled,
            Some(0) => false,
            Some(pct) if pct >= 100 => true,
            Some(pct) => (user_hash % 100) < pct as u64,
        }
    }

    pub fn all_flags(&self) -> Vec<&FeatureFlag> {
        self.flags.values().collect()
    }
}

// ── Feature #188: Agent Identity (DID) ────────────────────────────────────────

fn fnv1a_hash(s: &str) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

#[derive(Debug, Clone)]
pub struct AgentIdentity {
    pub did: String,
    /// WARNING: FNV-1a is NOT cryptographic. This provides tamper-detection only, not security. For production use, replace with ed25519.
    pub verification_hash: String,
    pub created_at: u64,
    pub metadata: HashMap<String, String>,
}

impl AgentIdentity {
    pub fn generate() -> Self {
        let seed = Uuid::new_v4().to_string();
        let hex = format!(
            "{:016x}{:016x}",
            fnv1a_hash(&seed),
            fnv1a_hash(&(seed.clone() + "2"))
        );
        let did = format!("did:caduceus:{}", hex);
        let verification_hash = format!("{:016x}", fnv1a_hash(&did));
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            did,
            verification_hash,
            created_at,
            metadata: HashMap::new(),
        }
    }

    pub fn did(&self) -> &str {
        &self.did
    }

    /// WARNING: FNV-1a is NOT cryptographic. This provides tamper-detection only, not security. For production use, replace with ed25519.
    pub fn sign(&self, message: &str) -> String {
        let input = format!("{}:{}", self.verification_hash, message);
        format!("{:016x}", fnv1a_hash(&input))
    }

    /// WARNING: FNV-1a is NOT cryptographic. This provides tamper-detection only, not security. For production use, replace with ed25519.
    pub fn verify_signature(&self, message: &str, signature: &str) -> bool {
        self.sign(message) == signature
    }
}

pub struct AgentIdentityRegistry {
    identities: HashMap<String, AgentIdentity>,
}

impl AgentIdentityRegistry {
    pub fn new() -> Self {
        Self {
            identities: HashMap::new(),
        }
    }

    pub fn register(&mut self, identity: AgentIdentity) {
        self.identities.insert(identity.did.clone(), identity);
    }

    pub fn lookup(&self, did: &str) -> Option<&AgentIdentity> {
        self.identities.get(did)
    }

    pub fn verify(&self, did: &str, message: &str, signature: &str) -> bool {
        self.identities
            .get(did)
            .map(|id| id.verify_signature(message, signature))
            .unwrap_or(false)
    }

    pub fn list(&self) -> Vec<&AgentIdentity> {
        self.identities.values().collect()
    }
}

impl Default for AgentIdentityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Feature #128: Bridge / Remote Control (WebSocket) ─────────────────────────

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub auth_token: Option<String>,
    pub max_connections: usize,
}

#[derive(Debug, Clone)]
pub struct BridgeMessage {
    pub msg_type: BridgeMessageType,
    pub payload: String,
    pub sender: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeMessageType {
    Command,
    Response,
    Event,
    Error,
}

#[derive(Debug, Clone)]
pub struct BridgeSession {
    pub id: String,
    pub connected_at: u64,
    pub last_activity: u64,
    pub authenticated: bool,
}

impl BridgeConfig {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            tls: true,
            auth_token: None,
            max_connections: 100,
        }
    }

    pub fn websocket_url(&self) -> String {
        let scheme = if self.tls { "wss" } else { "ws" };
        format!("{}://{}:{}", scheme, self.host, self.port)
    }

    pub fn with_auth(mut self, token: &str) -> Self {
        self.auth_token = Some(token.to_string());
        self
    }
}

// ── Feature #129: SSH Sessions ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SshSessionConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: SshAuthMethod,
}

#[derive(Clone)]
pub enum SshAuthMethod {
    /// WARNING: Password stored in plaintext. In production, use a secret manager or zeroize-on-drop wrapper.
    Password(String),
    PrivateKey(String),
    Agent,
}

impl fmt::Debug for SshAuthMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password(_) => write!(f, "Password(***REDACTED***)"),
            Self::PrivateKey(path) => f.debug_tuple("PrivateKey").field(path).finish(),
            Self::Agent => write!(f, "Agent"),
        }
    }
}

impl SshSessionConfig {
    pub fn new(host: &str, username: &str) -> Self {
        Self {
            host: host.to_string(),
            port: 22,
            username: username.to_string(),
            auth_method: SshAuthMethod::Agent,
        }
    }

    pub fn with_key(mut self, key_path: &str) -> Self {
        self.auth_method = SshAuthMethod::PrivateKey(key_path.to_string());
        self
    }

    pub fn connection_string(&self) -> String {
        format!("{}@{}:{}", self.username, self.host, self.port)
    }
}

// ── Feature #130: ACP Protocol ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpMessage {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<serde_json::Value>,
    pub id: Option<u64>,
}

impl AcpMessage {
    pub fn request(method: &str, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params: Some(params),
            id: Some(fnv1a_hash(&Uuid::new_v4().to_string())),
        }
    }

    pub fn notification(method: &str, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params: Some(params),
            id: None,
        }
    }

    /// By design, this returns a serialized JSON-RPC response string for the ACP wire format instead of `Self`.
    pub fn response(id: u64, result: serde_json::Value) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        })
        .to_string()
    }

    /// By design, this returns a serialized JSON-RPC error string for the ACP wire format instead of `Self`.
    pub fn error(id: u64, code: i32, message: &str) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        })
        .to_string()
    }

    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(CaduceusError::Serialization)
    }
}

// ── Feature #131: Collaboration Sync ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpType {
    Insert,
    Delete,
    Replace,
}

#[derive(Debug, Clone)]
pub struct DeferredOp {
    pub op_type: OpType,
    pub path: String,
    pub content: Option<String>,
    pub timestamp: u64,
    pub author: String,
}

pub struct OpLog {
    ops: Vec<DeferredOp>,
}

impl OpLog {
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    pub fn append(&mut self, op: DeferredOp) {
        self.ops.push(op);
    }

    pub fn replay(&self) -> Vec<&DeferredOp> {
        self.ops.iter().collect()
    }

    pub fn ops_since(&self, timestamp: u64) -> Vec<&DeferredOp> {
        self.ops
            .iter()
            .filter(|op| op.timestamp > timestamp)
            .collect()
    }

    pub fn merge(&mut self, other: &OpLog) {
        for op in &other.ops {
            let is_dup = self.ops.iter().any(|e| {
                e.timestamp == op.timestamp
                    && e.author == op.author
                    && e.path == op.path
                    && e.op_type == op.op_type
            });
            if !is_dup {
                self.ops.push(op.clone());
            }
        }
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

impl Default for OpLog {
    fn default() -> Self {
        Self::new()
    }
}

// ── Feature #132: Remote Selections / AI Cursors ──────────────────────────────

#[derive(Debug, Clone)]
pub struct RemoteCursor {
    pub user_id: String,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub color: String,
}

pub struct CursorTracker {
    cursors: HashMap<String, RemoteCursor>,
}

impl CursorTracker {
    pub fn new() -> Self {
        Self {
            cursors: HashMap::new(),
        }
    }

    pub fn update(&mut self, cursor: RemoteCursor) {
        self.cursors.insert(cursor.user_id.clone(), cursor);
    }

    pub fn remove(&mut self, user_id: &str) {
        self.cursors.remove(user_id);
    }

    pub fn get(&self, user_id: &str) -> Option<&RemoteCursor> {
        self.cursors.get(user_id)
    }

    pub fn cursors_in_file(&self, file: &str) -> Vec<&RemoteCursor> {
        self.cursors.values().filter(|c| c.file == file).collect()
    }

    pub fn all_cursors(&self) -> Vec<&RemoteCursor> {
        self.cursors.values().collect()
    }
}

impl Default for CursorTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ── Config migration ───────────────────────────────────────────────────────────

/// Describes a single config migration step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigMigration {
    pub version: u32,
    pub description: String,
}

/// Runs config migrations against `.caduceus/version`.
pub struct ConfigMigrator {
    migrations: Vec<ConfigMigration>,
}

impl ConfigMigrator {
    pub fn new() -> Self {
        Self {
            migrations: vec![ConfigMigration {
                version: 1,
                description: "Initial config version".to_string(),
            }],
        }
    }

    /// The latest config version this binary understands.
    pub fn current_version() -> u32 {
        1
    }

    /// Read the persisted version from `<config_path>/version`.
    fn read_version(config_path: &Path) -> u32 {
        let version_file = config_path.join("version");
        if !version_file.exists() {
            return 0;
        }
        std::fs::read_to_string(&version_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0)
    }

    /// Write the version to `<config_path>/version`.
    fn write_version(config_path: &Path, version: u32) -> Result<()> {
        std::fs::create_dir_all(config_path)
            .map_err(|e| CaduceusError::Config(format!("failed to create config dir: {e}")))?;
        std::fs::write(config_path.join("version"), version.to_string())
            .map_err(|e| CaduceusError::Config(format!("failed to write version file: {e}")))?;
        Ok(())
    }

    /// Returns `true` if the on-disk version is behind `current_version()`.
    pub fn needs_migration(config_path: &Path) -> bool {
        Self::read_version(config_path) < Self::current_version()
    }

    /// Run all outstanding migrations and return the new version.
    pub fn migrate(config_path: &Path) -> Result<u32> {
        let migrator = Self::new();
        let current = Self::read_version(config_path);
        let target = Self::current_version();

        if current >= target {
            return Ok(current);
        }

        for migration in &migrator.migrations {
            if migration.version > current && migration.version <= target {
                tracing::info!(
                    version = migration.version,
                    description = %migration.description,
                    "applying config migration"
                );
            }
        }

        Self::write_version(config_path, target)?;
        Ok(target)
    }
}

impl Default for ConfigMigrator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ST7: SubAgentFailure / SubAgentPhase / classifier ─────────────────────

    #[test]
    fn sub_agent_failure_kind_str_stable() {
        assert_eq!(
            SubAgentFailure::Timeout(TimeoutFailure::new(1, 900, SubAgentPhase::Unknown, false))
                .kind_str(),
            "Timeout"
        );
        assert_eq!(SubAgentFailure::UserCancel.kind_str(), "UserCancel");
        assert_eq!(
            SubAgentFailure::ModelRefusal {
                refusal_text: "x".into()
            }
            .kind_str(),
            "ModelRefusal"
        );
    }

    #[test]
    fn sub_agent_failure_serde_tag_shape() {
        let f = SubAgentFailure::Timeout(TimeoutFailure::new(
            901,
            900,
            SubAgentPhase::ToolExecution,
            true,
        ));
        let v = serde_json::to_value(&f).expect("serialize");
        assert_eq!(v["failure_type"], "Timeout");
        assert_eq!(v["details"]["elapsed_secs"], 901);
        assert_eq!(v["details"]["last_phase"], "ToolExecution");

        // Round-trip via tagged shape.
        let back: SubAgentFailure = serde_json::from_value(v).unwrap();
        assert_eq!(back.kind_str(), "Timeout");
    }

    #[test]
    fn classify_caduceus_error_ratelimit_to_backoff() {
        let err = CaduceusError::RateLimited {
            retry_after_secs: 30,
        };
        match classify_caduceus_error(&err, &ClassifyContext::empty(), SubAgentPhase::ProviderCall, false, 0, 900) {
            SubAgentFailure::ProviderError(p) => {
                assert_eq!(p.retry_class, RetryClass::Backoff);
                assert_eq!(p.retry_after_secs, Some(30));
                assert_eq!(p.http_status, Some(429));
            }
            other => panic!("expected ProviderError, got {other:?}"),
        }
    }

    #[test]
    fn classify_caduceus_error_provider_timeout_to_backoff() {
        let err = CaduceusError::ProviderTimeout {
            elapsed_ms: 6000,
            limit_ms: 5000,
            context: "chat".into(),
        };
        match classify_caduceus_error(&err, &ClassifyContext::empty(), SubAgentPhase::ProviderCall, false, 6, 900) {
            SubAgentFailure::ProviderError(p) => {
                assert_eq!(p.retry_class, RetryClass::Backoff);
            }
            other => panic!("expected ProviderError, got {other:?}"),
        }
    }

    #[test]
    fn classify_caduceus_error_tool_to_tool_error() {
        let err = CaduceusError::Tool {
            tool: "read_file".into(),
            message: "boom".into(),
        };
        match classify_caduceus_error(&err, &ClassifyContext::empty(), SubAgentPhase::ToolExecution, true, 1, 900) {
            SubAgentFailure::ToolError { tool_name, message } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(message, "boom");
            }
            other => panic!("expected ToolError, got {other:?}"),
        }
    }

    #[test]
    fn classify_caduceus_error_permission_to_policy_denied() {
        let err = CaduceusError::PermissionDenied {
            capability: "fs.write".into(),
            tool: "write_file".into(),
        };
        match classify_caduceus_error(&err, &ClassifyContext::empty(), SubAgentPhase::ToolExecution, true, 1, 900) {
            SubAgentFailure::PolicyDenied {
                capability, tool, ..
            } => {
                assert_eq!(capability, "fs.write");
                assert_eq!(tool, "write_file");
            }
            other => panic!("expected PolicyDenied, got {other:?}"),
        }
    }

    #[test]
    fn classify_caduceus_error_cancelled_to_user_cancel() {
        let err = CaduceusError::Cancelled;
        match classify_caduceus_error(&err, &ClassifyContext::empty(), SubAgentPhase::ProviderCall, false, 0, 900) {
            SubAgentFailure::UserCancel => {}
            other => panic!("expected UserCancel, got {other:?}"),
        }
    }

    #[test]
    fn classify_caduceus_error_provider_string_to_immediate() {
        let err = CaduceusError::Provider("transient blip".into());
        match classify_caduceus_error(&err, &ClassifyContext::empty(), SubAgentPhase::ProviderCall, false, 0, 900) {
            SubAgentFailure::ProviderError(p) => {
                assert_eq!(p.retry_class, RetryClass::Immediate);
                assert!(p.retry_after_secs.is_none());
            }
            other => panic!("expected ProviderError, got {other:?}"),
        }
    }

    #[test]
    fn classify_caduceus_error_populates_provider_and_model_for_429() {
        // ST7 must-fix #2: ProviderErrorFailure MUST carry provider/model
        // when the call-site supplies them (ST8 vendor-rerouting input).
        let err = CaduceusError::RateLimited { retry_after_secs: 30 };
        let ctx = ClassifyContext::new(
            Some(ProviderId::new("anthropic")),
            Some(ModelId::new("claude-opus-4.7")),
        );
        match classify_caduceus_error(
            &err,
            &ctx,
            SubAgentPhase::ProviderCall,
            false,
            0,
            900,
        ) {
            SubAgentFailure::ProviderError(p) => {
                assert_eq!(p.provider, Some(ProviderId::new("anthropic")));
                assert_eq!(p.model, Some(ModelId::new("claude-opus-4.7")));
                assert_eq!(p.http_status, Some(429));
                assert_eq!(p.retry_class, RetryClass::Backoff);
                assert_eq!(p.retry_after_secs, Some(30));
            }
            other => panic!("expected ProviderError, got {other:?}"),
        }

        // Also exercise the ProviderTimeout + Provider(String) arms.
        let err = CaduceusError::ProviderTimeout {
            elapsed_ms: 6000,
            limit_ms: 5000,
            context: "chat".into(),
        };
        match classify_caduceus_error(
            &err,
            &ctx,
            SubAgentPhase::ProviderCall,
            false,
            6,
            900,
        ) {
            SubAgentFailure::ProviderError(p) => {
                assert_eq!(p.provider, Some(ProviderId::new("anthropic")));
                assert_eq!(p.model, Some(ModelId::new("claude-opus-4.7")));
            }
            other => panic!("expected ProviderError, got {other:?}"),
        }

        let err = CaduceusError::Provider("503 service unavailable".into());
        match classify_caduceus_error(
            &err,
            &ctx,
            SubAgentPhase::ProviderCall,
            false,
            0,
            900,
        ) {
            SubAgentFailure::ProviderError(p) => {
                assert_eq!(p.provider, Some(ProviderId::new("anthropic")));
                assert_eq!(p.model, Some(ModelId::new("claude-opus-4.7")));
                assert_eq!(p.retry_class, RetryClass::Immediate);
            }
            other => panic!("expected ProviderError, got {other:?}"),
        }
    }

    #[test]
    fn classify_caduceus_error_empty_context_yields_none_provider_model() {
        // Backward-compat: empty context (zed-tool boundary) leaves
        // provider/model None so consumers can branch on Option.
        let err = CaduceusError::RateLimited { retry_after_secs: 30 };
        match classify_caduceus_error(
            &err,
            &ClassifyContext::empty(),
            SubAgentPhase::ProviderCall,
            false,
            0,
            900,
        ) {
            SubAgentFailure::ProviderError(p) => {
                assert!(p.provider.is_none());
                assert!(p.model.is_none());
            }
            other => panic!("expected ProviderError, got {other:?}"),
        }
    }

    #[test]
    fn classify_caduceus_error_other_falls_back_to_internal() {
        let err = CaduceusError::Other(anyhow::anyhow!("synthetic"));
        match classify_caduceus_error(&err, &ClassifyContext::empty(), SubAgentPhase::Unknown, false, 0, 900) {
            SubAgentFailure::InternalError { kind, .. } => {
                assert_eq!(kind, "caduceus_error");
            }
            other => panic!("expected InternalError, got {other:?}"),
        }
    }

    // ── Phase transition table (v3.1 Fix 2) ──────────────────────────────────

    #[test]
    fn phase_initial_is_model_selection() {
        // ModelSelection is the spawn-entry default. RoutingDecision does
        // not transition.
        let p = SubAgentPhase::ModelSelection.next_phase(&AgentEvent::RoutingDecision {
            candidates: vec![],
            activated: vec![],
            threshold: 0.5,
        });
        assert_eq!(p, SubAgentPhase::ModelSelection);
    }

    #[test]
    fn phase_provider_call_then_tool_execution_then_back() {
        let p = SubAgentPhase::ModelSelection.next_phase(&AgentEvent::TextDelta {
            text: "hi".into(),
        });
        assert_eq!(p, SubAgentPhase::ProviderCall);

        let p = p.next_phase(&AgentEvent::ToolCallStart {
            id: ToolCallId::new("1"),
            name: "read_file".into(),
        });
        assert_eq!(p, SubAgentPhase::ToolExecution);

        // ToolResultEnd MUST NOT exit ToolExecution (v3.1 Fix 2).
        let p = p.next_phase(&AgentEvent::ToolResultEnd {
            id: ToolCallId::new("1"),
            content: String::new(),
            is_error: false,
        });
        assert_eq!(p, SubAgentPhase::ToolExecution);

        // The next provider-side event finally transitions back.
        let p = p.next_phase(&AgentEvent::ReasoningDelta {
            content: "think".into(),
        });
        assert_eq!(p, SubAgentPhase::ProviderCall);
    }

    #[test]
    fn phase_context_management_on_context_events() {
        for ev in [
            AgentEvent::ContextWarning {
                level: "warning_85".into(),
                used_tokens: 85,
                max_tokens: 100,
            },
            AgentEvent::ContextCompacted {
                freed_tokens: 10,
                before: 100,
                after: 90,
            },
        ] {
            let p = SubAgentPhase::ProviderCall.next_phase(&ev);
            assert_eq!(p, SubAgentPhase::ContextManagement);
        }
    }

    #[test]
    fn session_id_unique() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn token_budget_remaining() {
        let budget = TokenBudget {
            context_limit: 1000,
            used_input: 300,
            used_output: 100,
            reserved_output: 200,
        };
        assert_eq!(budget.remaining(), 400);
    }

    #[test]
    fn token_budget_needs_compaction() {
        let budget = TokenBudget {
            context_limit: 1000,
            used_input: 800,
            used_output: 60,
            reserved_output: 100,
        };
        assert!(budget.needs_compaction());
    }

    #[test]
    fn token_usage_accumulate() {
        let mut total = TokenUsage::default();
        let turn = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };
        total.accumulate(&turn);
        total.accumulate(&turn);
        assert_eq!(total.input_tokens, 200);
        assert_eq!(total.output_tokens, 100);
    }

    #[test]
    fn tool_result_success_and_error() {
        let ok = ToolResult::success("done");
        assert!(!ok.is_error);
        let err = ToolResult::error("failed");
        assert!(err.is_error);
    }

    #[test]
    fn turn_budget_default_has_sane_limits() {
        let b = TurnBudget::default();
        assert!(b.max_tool_calls >= 50);
        assert!(b.max_total_tool_seconds >= 60);
        assert!(b.max_total_bytes_read >= 1024 * 1024);
    }

    #[test]
    fn turn_budget_unlimited_never_trips() {
        let b = TurnBudget::unlimited();
        let mut u = TurnBudgetUsage::default();
        for _ in 0..1000 {
            u.record(10, 100_000);
        }
        assert!(u.check(&b).is_none());
    }

    #[test]
    fn turn_budget_trips_on_call_count() {
        let b = TurnBudget {
            max_tool_calls: 3,
            ..TurnBudget::unlimited()
        };
        let mut u = TurnBudgetUsage::default();
        for _ in 0..3 {
            u.record(0, 0);
        }
        assert!(u.check(&b).is_none(), "exactly limit should NOT trip");
        u.record(0, 0);
        match u.check(&b) {
            Some(BudgetBreach::ToolCalls { used: 4, limit: 3 }) => {}
            other => panic!("expected ToolCalls breach, got {other:?}"),
        }
    }

    #[test]
    fn turn_budget_trips_on_seconds() {
        let b = TurnBudget {
            max_total_tool_seconds: 5,
            ..TurnBudget::unlimited()
        };
        let mut u = TurnBudgetUsage::default();
        u.record(6, 0);
        assert!(matches!(
            u.check(&b),
            Some(BudgetBreach::ToolSeconds { used: 6, limit: 5 })
        ));
    }

    #[test]
    fn turn_budget_trips_on_bytes() {
        let b = TurnBudget {
            max_total_bytes_read: 1000,
            ..TurnBudget::unlimited()
        };
        let mut u = TurnBudgetUsage::default();
        u.record(0, 1500);
        assert!(matches!(
            u.check(&b),
            Some(BudgetBreach::BytesRead {
                used: 1500,
                limit: 1000
            })
        ));
    }

    #[test]
    fn turn_budget_record_saturates_on_overflow() {
        let mut u = TurnBudgetUsage {
            tool_calls: u32::MAX,
            total_tool_seconds: u64::MAX,
            total_bytes_read: u64::MAX,
        };
        u.record(100, 100);
        // Saturated, not wrapped — the breach check must still see MAX,
        // not 99 (which would silently re-arm the budget).
        assert_eq!(u.tool_calls, u32::MAX);
        assert_eq!(u.total_tool_seconds, u64::MAX);
        assert_eq!(u.total_bytes_read, u64::MAX);
    }

    #[test]
    fn budget_breach_messages_are_actionable() {
        let m = BudgetBreach::ToolCalls {
            used: 100,
            limit: 50,
        }
        .message();
        assert!(m.contains("100"));
        assert!(m.contains("50"));
        assert!(m.contains("max_tool_calls"));
    }

    #[test]
    fn budget_breach_serde_round_trip() {
        let breach = BudgetBreach::BytesRead {
            used: 999,
            limit: 100,
        };
        let json = serde_json::to_string(&breach).unwrap();
        assert!(json.contains("bytes_read"));
        let back: BudgetBreach = serde_json::from_str(&json).unwrap();
        assert_eq!(breach, back);
    }

    #[test]
    fn llm_response_extracts_text_and_tools() {
        let resp = LlmResponse {
            content: vec![
                ContentBlock::Text("Hello".into()),
                ContentBlock::ToolUse {
                    id: ToolCallId::new("t1"),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "ls"}),
                },
                ContentBlock::Text(" world".into()),
            ],
            stop_reason: StopReason::ToolUse,
            usage: TokenUsage::default(),
        };
        assert_eq!(resp.text_content(), "Hello world");
        assert_eq!(resp.tool_calls().len(), 1);
    }

    #[test]
    fn agent_event_serializes_as_tagged() {
        let event = AgentEvent::TextDelta { text: "hi".into() };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"TextDelta\""));
    }

    /// Pin the `PermissionDecision` wire format so an accidental change to
    /// `tag = "type"` or to `PermissionOutcome`'s `tag = "kind"` /
    /// `rename_all = "snake_case"` is caught by the test suite. Any consumer
    /// (the bridge, telemetry, replay logging) depends on the exact layout
    /// asserted here.
    #[test]
    fn permission_decision_wire_format() {
        // TimedOut variant — payload field carried.
        let event = AgentEvent::PermissionDecision {
            id: "perm_x".into(),
            capability: "write_file".into(),
            outcome: PermissionOutcome::TimedOut { waited_secs: 300 },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"type\":\"PermissionDecision\""),
            "AgentEvent tag must be PermissionDecision, got: {json}"
        );
        assert!(
            json.contains("\"kind\":\"timed_out\""),
            "PermissionOutcome should serialize TimedOut as snake_case, got: {json}"
        );
        assert!(
            json.contains("\"waited_secs\":300"),
            "TimedOut payload must include waited_secs, got: {json}"
        );

        // Denied variant — pure unit, no payload.
        let denied = AgentEvent::PermissionDecision {
            id: "perm_x".into(),
            capability: "write_file".into(),
            outcome: PermissionOutcome::Denied,
        };
        let json2 = serde_json::to_string(&denied).unwrap();
        assert!(
            json2.contains("\"kind\":\"denied\""),
            "Denied should serialize as kind=denied, got: {json2}"
        );

        // MismatchedId — both ids preserved on the wire.
        let mismatched = AgentEvent::PermissionDecision {
            id: "perm_x".into(),
            capability: "write_file".into(),
            outcome: PermissionOutcome::MismatchedId {
                expected: "perm_x".into(),
                got: "perm_y".into(),
            },
        };
        let json3 = serde_json::to_string(&mismatched).unwrap();
        assert!(json3.contains("\"kind\":\"mismatched_id\""), "got: {json3}");
        assert!(json3.contains("\"expected\":\"perm_x\""), "got: {json3}");
        assert!(json3.contains("\"got\":\"perm_y\""), "got: {json3}");
    }

    /// G33 — after introducing `#[serde(other)] Unknown`, the contract
    /// flipped: unknown `AgentEvent` tags now deserialise to `Unknown`
    /// instead of erroring. This preserves forward-compat across rolling
    /// deploys (newer producer / older consumer). If serde ever changes
    /// `#[serde(other)]` semantics on internally-tagged enums, this test
    /// breaks loudly so we can re-evaluate the compat story.
    #[test]
    fn agent_event_unknown_variant_errors_cleanly() {
        let json = r#"{"type":"FutureUnknownVariant","data":"x"}"#;
        let parsed: AgentEvent = serde_json::from_str(json)
            .expect("unknown tag should fall through to Unknown, not error");
        assert!(matches!(parsed, AgentEvent::Unknown));
    }

    /// Bound-check on `MismatchedId.skip_message`: an oversized `got` is
    /// truncated so a malicious bridge can't bloat LLM context with arbitrary
    /// content via the approval channel.
    #[test]
    fn mismatched_id_skip_message_truncates_oversized_got() {
        let huge = "x".repeat(5_000);
        let outcome = PermissionOutcome::MismatchedId {
            expected: "perm_real".into(),
            got: huge,
        };
        let msg = outcome.skip_message();
        assert!(
            msg.len() < 300,
            "skip_message must truncate huge `got`; got len={}",
            msg.len()
        );
        assert!(
            msg.contains("(truncated)"),
            "skip_message should signal truncation, got: {msg}"
        );
    }

    #[test]
    fn config_defaults_are_sane() {
        let config = CaduceusConfig::default();
        assert_eq!(config.default_provider.0, "anthropic");
        assert_eq!(config.max_context_tokens, 200_000);
        assert!(config.permissions.fs_read);
    }

    // ── P0: CaduceusPaths tests ────────────────────────────────────────────────

    #[test]
    fn caduceus_paths_structure() {
        let config_dir = CaduceusPaths::config_dir();
        assert!(config_dir.ends_with(".caduceus"));
        assert!(CaduceusPaths::config_file().ends_with("config.toml"));
        assert!(CaduceusPaths::db_file().ends_with("db.sqlite"));
        assert!(CaduceusPaths::cache_dir().ends_with("cache"));
        assert!(CaduceusPaths::logs_dir().ends_with("logs"));
        assert!(CaduceusPaths::guidelines_dir().ends_with("guidelines"));
    }

    #[test]
    fn caduceus_paths_project_config() {
        let root = PathBuf::from("/workspace/my-project");
        let project_config = CaduceusPaths::project_config_file(&root);
        assert_eq!(
            project_config,
            PathBuf::from("/workspace/my-project/.caduceus/config.toml")
        );
    }

    #[test]
    fn seed_guidelines_does_not_overwrite_user_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let g = tmp.path().join("guidelines");
        std::fs::create_dir_all(&g).unwrap();
        let tpl = g.join("strategy-selection.md");
        std::fs::write(&tpl, "USER EDITED").unwrap();
        // Simulate the seed loop with a user-owned file present.
        let body = "DEFAULT";
        if !tpl.exists() {
            std::fs::write(&tpl, body).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&tpl).unwrap(), "USER EDITED");
    }

    // ── P0: ConfigLoader tests ─────────────────────────────────────────────────

    #[test]
    fn config_loader_defaults_without_files() {
        let loader = ConfigLoader::new();
        let config = loader.load();
        assert_eq!(config.default_provider.0, "anthropic");
        assert_eq!(config.default_model.0, "claude-sonnet-4-6");
    }

    #[test]
    fn config_loader_cli_overrides() {
        let mut overrides = HashMap::new();
        overrides.insert("provider".into(), "openai".into());
        overrides.insert("model".into(), "gpt-4".into());
        overrides.insert("max_context_tokens".into(), "100000".into());

        let loader = ConfigLoader::new().with_cli_overrides(overrides);
        let config = loader.load();
        assert_eq!(config.default_provider.0, "openai");
        assert_eq!(config.default_model.0, "gpt-4");
        assert_eq!(config.max_context_tokens, 100_000);
    }

    #[test]
    fn config_loader_merge_partial() {
        let partial = PartialConfig {
            default_provider: Some("openai".into()),
            log_level: Some("debug".into()),
            ..Default::default()
        };
        let mut config = CaduceusConfig::default();
        ConfigLoader::merge_partial(&mut config, &partial);
        assert_eq!(config.default_provider.0, "openai");
        assert_eq!(config.log_level, "debug");
        // Unset fields should keep defaults
        assert_eq!(config.default_model.0, "claude-sonnet-4-6");
    }

    #[test]
    fn partial_config_toml_roundtrip() {
        let toml_str = r#"
default_provider = "openai"
default_model = "gpt-4"
log_level = "debug"
"#;
        let partial: PartialConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(partial.default_provider.as_deref(), Some("openai"));
        assert_eq!(partial.default_model.as_deref(), Some("gpt-4"));
        assert!(partial.max_context_tokens.is_none());
    }

    // ── P0: CancellationToken tests ────────────────────────────────────────────

    #[test]
    fn cancellation_token_default_not_cancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        assert!(token.check().is_ok());
    }

    #[test]
    fn cancellation_token_cancel() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(token.is_cancelled());
        assert!(token.check().is_err());
    }

    #[test]
    fn cancellation_token_shared_across_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        token.cancel();
        assert!(clone.is_cancelled());
    }

    /// Audit finding #9: reset() must clear the flag on every clone of the
    /// shared Arc, not just the local handle, so a harness reusing the
    /// same token across runs actually un-cancels.
    #[test]
    fn cancellation_token_reset_clears_flag_on_all_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        token.cancel();
        assert!(clone.is_cancelled());
        clone.reset();
        assert!(!token.is_cancelled(), "reset on clone must affect original");
        assert!(token.check().is_ok());
    }

    // ── ST-A2b: Generation-bound cancellation tests ───────────────────────────

    #[test]
    fn cancellation_token_starts_at_generation_zero() {
        let token = CancellationToken::new();
        assert_eq!(token.current_generation(), 0);
    }

    #[test]
    fn cancellation_token_bump_generation_increments_and_clears_flag() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(token.is_cancelled());
        let gen = token.bump_generation();
        assert_eq!(gen, 1);
        assert_eq!(token.current_generation(), 1);
        assert!(
            !token.is_cancelled(),
            "bump_generation must clear the cancel flag"
        );
    }

    #[test]
    fn cancellation_token_bump_generation_shared_across_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert_eq!(token.bump_generation(), 1);
        assert_eq!(clone.current_generation(), 1);
    }

    #[test]
    fn cancellation_token_cancel_for_generation_current_succeeds() {
        let token = CancellationToken::new();
        let gen = token.current_generation();
        assert!(token.cancel_for_generation(gen));
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancellation_token_cancel_for_stale_generation_noop() {
        let token = CancellationToken::new();
        let stale_gen = token.current_generation();
        token.bump_generation();
        assert!(!token.cancel_for_generation(stale_gen));
        assert!(
            !token.is_cancelled(),
            "stale-generation cancel must not poison new turn"
        );
    }

    #[test]
    fn cancellation_token_cancel_for_generation_after_bump_works() {
        let token = CancellationToken::new();
        let gen0 = token.current_generation();
        assert!(token.cancel_for_generation(gen0));
        let gen1 = token.bump_generation();
        assert_eq!(gen1, 1);
        assert!(!token.is_cancelled());
        assert!(token.cancel_for_generation(gen1));
        assert!(token.is_cancelled());
    }

    // ── P1: Token Warning Levels tests ─────────────────────────────────────────

    #[test]
    fn token_budget_warning_none() {
        let budget = TokenBudget {
            context_limit: 1000,
            used_input: 100,
            used_output: 50,
            reserved_output: 100,
        };
        assert_eq!(budget.warning_level(), WarningLevel::None);
    }

    #[test]
    fn token_budget_warning_70() {
        let budget = TokenBudget {
            context_limit: 1000,
            used_input: 600,
            used_output: 100,
            reserved_output: 100,
        };
        assert_eq!(budget.warning_level(), WarningLevel::Warning70);
    }

    #[test]
    fn token_budget_warning_85() {
        let budget = TokenBudget {
            context_limit: 1000,
            used_input: 750,
            used_output: 100,
            reserved_output: 100,
        };
        assert_eq!(budget.warning_level(), WarningLevel::Warning85);
    }

    #[test]
    fn token_budget_warning_critical_95() {
        let budget = TokenBudget {
            context_limit: 1000,
            used_input: 900,
            used_output: 60,
            reserved_output: 100,
        };
        assert_eq!(budget.warning_level(), WarningLevel::Critical95);
    }

    // ── Feature #50: FeatureFlags tests ────────────────────────────────────────

    #[test]
    fn feature_flags_register_and_check() {
        let mut flags = FeatureFlagRegistry::new();
        flags.register("dark-mode", "Enable dark mode UI", false);
        flags.register("beta-search", "New search engine", true);

        assert!(!flags.is_enabled("dark-mode"));
        assert!(flags.is_enabled("beta-search"));
        assert!(!flags.is_enabled("nonexistent"));
    }

    #[test]
    fn feature_flags_enable_disable() {
        let mut flags = FeatureFlagRegistry::new();
        flags.register("my-feature", "desc", false);

        assert!(!flags.is_enabled("my-feature"));
        flags.set("my-feature", true);
        assert!(flags.is_enabled("my-feature"));
        flags.set("my-feature", false);
        assert!(!flags.is_enabled("my-feature"));
    }

    #[test]
    fn feature_flags_set_on_unknown_is_noop() {
        let mut flags = FeatureFlagRegistry::new();
        flags.set("ghost", true); // should not panic
        assert!(!flags.is_enabled("ghost"));
    }

    #[test]
    fn feature_flags_rollout_zero() {
        let mut flags = FeatureFlagRegistry::new();
        flags.register("rollout-zero", "0% rollout", true);
        flags.set_rollout("rollout-zero", 0);
        // No user should get this
        for hash in 0u64..200 {
            assert!(!flags.check_rollout("rollout-zero", hash));
        }
    }

    #[test]
    fn feature_flags_rollout_hundred() {
        let mut flags = FeatureFlagRegistry::new();
        flags.register("rollout-full", "100% rollout", true);
        flags.set_rollout("rollout-full", 100);
        // Every user should get this
        for hash in 0u64..200 {
            assert!(flags.check_rollout("rollout-full", hash));
        }
    }

    #[test]
    fn feature_flags_rollout_fifty() {
        let mut flags = FeatureFlagRegistry::new();
        flags.register("rollout-half", "50% rollout", true);
        flags.set_rollout("rollout-half", 50);
        // Users 0-49 get it, 50-99 don't (deterministic)
        let enabled: usize = (0u64..100)
            .filter(|&h| flags.check_rollout("rollout-half", h))
            .count();
        assert_eq!(enabled, 50);
    }

    #[test]
    fn feature_flags_rollout_respects_disabled() {
        let mut flags = FeatureFlagRegistry::new();
        flags.register("feat", "desc", false);
        flags.set_rollout("feat", 100);
        // Even 100% rollout should return false when feature is disabled
        assert!(!flags.check_rollout("feat", 0));
    }

    #[test]
    fn feature_flags_all_flags() {
        let mut flags = FeatureFlagRegistry::new();
        flags.register("a", "desc a", true);
        flags.register("b", "desc b", false);
        let all = flags.all_flags();
        assert_eq!(all.len(), 2);
    }

    // ── Feature #72: Vision types tests ────────────────────────────────────────

    #[test]
    fn image_source_base64_variant() {
        let src = ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        };
        match src {
            ImageSource::Base64 { media_type, data } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, "aGVsbG8=");
            }
            _ => panic!("expected Base64 variant"),
        }
    }

    #[test]
    fn image_source_url_variant() {
        let src = ImageSource::Url("https://example.com/img.png".into());
        match src {
            ImageSource::Url(url) => assert!(url.contains("example.com")),
            _ => panic!("expected Url variant"),
        }
    }

    #[test]
    fn image_content_block_in_content_block_enum() {
        let img = ImageContent {
            source: ImageSource::Base64 {
                media_type: "image/jpeg".into(),
                data: "dGVzdA==".into(),
            },
            detail: Some("auto".into()),
        };
        let block = ContentBlock::Image(img);
        // text_content should skip images
        let resp = LlmResponse {
            content: vec![ContentBlock::Text("hi".into()), block],
            stop_reason: StopReason::EndTurn,
            usage: TokenUsage::default(),
        };
        assert_eq!(resp.text_content(), "hi");
    }

    // ── Feature #188: Agent Identity tests ─────────────────────────────────────

    #[test]
    fn agent_identity_generate_has_did_prefix() {
        let id = AgentIdentity::generate();
        assert!(id.did().starts_with("did:caduceus:"));
    }

    #[test]
    fn agent_identity_sign_and_verify() {
        let id = AgentIdentity::generate();
        let sig = id.sign("hello world");
        assert!(id.verify_signature("hello world", &sig));
        assert!(!id.verify_signature("other msg", &sig));
    }

    #[test]
    fn agent_identity_unique_dids() {
        let a = AgentIdentity::generate();
        let b = AgentIdentity::generate();
        assert_ne!(a.did(), b.did());
    }

    #[test]
    fn agent_identity_registry_register_and_lookup() {
        let mut reg = AgentIdentityRegistry::new();
        let id = AgentIdentity::generate();
        let did = id.did().to_string();
        reg.register(id);
        assert!(reg.lookup(&did).is_some());
        assert!(reg.lookup("did:caduceus:nonexistent").is_none());
    }

    #[test]
    fn agent_identity_registry_verify() {
        let mut reg = AgentIdentityRegistry::new();
        let id = AgentIdentity::generate();
        let did = id.did().to_string();
        let sig = id.sign("test");
        reg.register(id);
        assert!(reg.verify(&did, "test", &sig));
        assert!(!reg.verify(&did, "test", "badsig"));
        assert!(!reg.verify("did:caduceus:ghost", "test", &sig));
    }

    #[test]
    fn agent_identity_registry_list() {
        let mut reg = AgentIdentityRegistry::new();
        reg.register(AgentIdentity::generate());
        reg.register(AgentIdentity::generate());
        assert_eq!(reg.list().len(), 2);
    }

    // ── Feature #128: Bridge tests ──────────────────────────────────────────────

    #[test]
    fn bridge_config_websocket_url() {
        let cfg = BridgeConfig::new("localhost", 8080);
        assert_eq!(cfg.websocket_url(), "wss://localhost:8080");
    }

    #[test]
    fn bridge_config_websocket_url_without_tls() {
        let mut cfg = BridgeConfig::new("localhost", 8080);
        cfg.tls = false;
        assert_eq!(cfg.websocket_url(), "ws://localhost:8080");
    }

    #[test]
    fn bridge_config_with_auth() {
        let cfg = BridgeConfig::new("host", 9000).with_auth("secret");
        assert_eq!(cfg.auth_token, Some("secret".to_string()));
    }

    #[test]
    fn bridge_config_defaults() {
        let cfg = BridgeConfig::new("host", 80);
        assert!(cfg.tls);
        assert!(cfg.auth_token.is_none());
        assert_eq!(cfg.max_connections, 100);
    }

    #[test]
    fn bridge_message_type_variants() {
        let _ = BridgeMessageType::Command;
        let _ = BridgeMessageType::Response;
        let _ = BridgeMessageType::Event;
        let _ = BridgeMessageType::Error;
    }

    // ── Feature #129: SSH Session tests ────────────────────────────────────────

    #[test]
    fn ssh_session_config_defaults() {
        let cfg = SshSessionConfig::new("example.com", "alice");
        assert_eq!(cfg.host, "example.com");
        assert_eq!(cfg.username, "alice");
        assert_eq!(cfg.port, 22);
        assert!(matches!(cfg.auth_method, SshAuthMethod::Agent));
    }

    #[test]
    fn ssh_session_config_with_key() {
        let cfg = SshSessionConfig::new("host", "bob").with_key("/home/bob/.ssh/id_rsa");
        assert!(matches!(cfg.auth_method, SshAuthMethod::PrivateKey(_)));
    }

    #[test]
    fn ssh_session_config_connection_string() {
        let cfg = SshSessionConfig::new("myhost", "user");
        assert_eq!(cfg.connection_string(), "user@myhost:22");
    }

    // ── Feature #130: ACP Protocol tests ───────────────────────────────────────

    #[test]
    fn acp_message_request_has_id() {
        let msg = AcpMessage::request("tools/list", serde_json::json!({}));
        assert_eq!(msg.jsonrpc, "2.0");
        assert_eq!(msg.method, "tools/list");
        assert!(msg.id.is_some());
        assert!(msg.params.is_some());
    }

    #[test]
    fn acp_message_notification_has_no_id() {
        let msg = AcpMessage::notification("event/fired", serde_json::json!({"key": "val"}));
        assert!(msg.id.is_none());
        assert_eq!(msg.method, "event/fired");
    }

    #[test]
    fn acp_message_response_serializes() {
        let resp = AcpMessage::response(42, serde_json::json!({"ok": true}));
        assert!(resp.contains("\"jsonrpc\":\"2.0\""));
        assert!(resp.contains("\"id\":42"));
    }

    #[test]
    fn acp_message_error_serializes() {
        let err = AcpMessage::error(1, -32600, "Invalid Request");
        assert!(err.contains("\"error\""));
        assert!(err.contains("-32600"));
    }

    #[test]
    fn acp_message_parse_roundtrip() {
        let msg = AcpMessage::request("ping", serde_json::json!(null));
        let json = serde_json::to_string(&msg).unwrap();
        let parsed = AcpMessage::parse(&json).unwrap();
        assert_eq!(parsed.method, "ping");
        assert_eq!(parsed.jsonrpc, "2.0");
    }

    // ── Feature #131: Collaboration Sync tests ─────────────────────────────────

    #[test]
    fn oplog_append_and_len() {
        let mut log = OpLog::new();
        assert_eq!(log.len(), 0);
        log.append(DeferredOp {
            op_type: OpType::Insert,
            path: "file.rs".to_string(),
            content: Some("fn main() {}".to_string()),
            timestamp: 1,
            author: "alice".to_string(),
        });
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn oplog_replay_order() {
        let mut log = OpLog::new();
        for i in 0u64..3 {
            log.append(DeferredOp {
                op_type: OpType::Insert,
                path: format!("f{}.rs", i),
                content: None,
                timestamp: i,
                author: "bob".to_string(),
            });
        }
        let replayed = log.replay();
        assert_eq!(replayed.len(), 3);
        assert_eq!(replayed[0].timestamp, 0);
        assert_eq!(replayed[2].timestamp, 2);
    }

    #[test]
    fn oplog_ops_since() {
        let mut log = OpLog::new();
        for ts in [1u64, 5, 10] {
            log.append(DeferredOp {
                op_type: OpType::Replace,
                path: "x".to_string(),
                content: None,
                timestamp: ts,
                author: "carol".to_string(),
            });
        }
        let recent = log.ops_since(5);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].timestamp, 10);
    }

    #[test]
    fn oplog_merge_deduplicates() {
        let mut log_a = OpLog::new();
        let mut log_b = OpLog::new();
        let op = DeferredOp {
            op_type: OpType::Delete,
            path: "shared.rs".to_string(),
            content: None,
            timestamp: 42,
            author: "dave".to_string(),
        };
        log_a.append(op.clone());
        log_b.append(op);
        log_b.append(DeferredOp {
            op_type: OpType::Insert,
            path: "new.rs".to_string(),
            content: Some("x".to_string()),
            timestamp: 43,
            author: "dave".to_string(),
        });
        log_a.merge(&log_b);
        assert_eq!(log_a.len(), 2); // duplicate not added
    }

    #[test]
    fn oplog_merge_keeps_same_time_different_type_ops() {
        let mut log_a = OpLog::new();
        let mut log_b = OpLog::new();
        log_a.append(DeferredOp {
            op_type: OpType::Insert,
            path: "shared.rs".to_string(),
            content: Some("before".to_string()),
            timestamp: 42,
            author: "dave".to_string(),
        });
        log_b.append(DeferredOp {
            op_type: OpType::Replace,
            path: "shared.rs".to_string(),
            content: Some("after".to_string()),
            timestamp: 42,
            author: "dave".to_string(),
        });

        log_a.merge(&log_b);

        assert_eq!(log_a.len(), 2);
    }

    // ── Feature #132: Remote Cursors tests ─────────────────────────────────────

    #[test]
    fn cursor_tracker_update_and_get() {
        let mut tracker = CursorTracker::new();
        tracker.update(RemoteCursor {
            user_id: "u1".to_string(),
            file: "main.rs".to_string(),
            line: 10,
            column: 5,
            color: "#ff0000".to_string(),
        });
        let c = tracker.get("u1").unwrap();
        assert_eq!(c.line, 10);
    }

    #[test]
    fn cursor_tracker_remove() {
        let mut tracker = CursorTracker::new();
        tracker.update(RemoteCursor {
            user_id: "u2".to_string(),
            file: "lib.rs".to_string(),
            line: 1,
            column: 0,
            color: "#00ff00".to_string(),
        });
        tracker.remove("u2");
        assert!(tracker.get("u2").is_none());
    }

    #[test]
    fn cursor_tracker_cursors_in_file() {
        let mut tracker = CursorTracker::new();
        tracker.update(RemoteCursor {
            user_id: "u1".to_string(),
            file: "a.rs".to_string(),
            line: 1,
            column: 0,
            color: "red".to_string(),
        });
        tracker.update(RemoteCursor {
            user_id: "u2".to_string(),
            file: "b.rs".to_string(),
            line: 2,
            column: 0,
            color: "blue".to_string(),
        });
        let in_a = tracker.cursors_in_file("a.rs");
        assert_eq!(in_a.len(), 1);
        assert_eq!(in_a[0].user_id, "u1");
    }

    #[test]
    fn cursor_tracker_all_cursors() {
        let mut tracker = CursorTracker::new();
        tracker.update(RemoteCursor {
            user_id: "u1".to_string(),
            file: "f.rs".to_string(),
            line: 0,
            column: 0,
            color: "red".to_string(),
        });
        tracker.update(RemoteCursor {
            user_id: "u2".to_string(),
            file: "g.rs".to_string(),
            line: 0,
            column: 0,
            color: "green".to_string(),
        });
        assert_eq!(tracker.all_cursors().len(), 2);
    }

    // ── G33: forward-compat catch-all + schema version envelope ──────────

    #[test]
    fn agent_event_unknown_variant_absorbs_unrecognised_tag() {
        let payload = r#"{"type":"some_future_event_we_dont_know","field":42}"#;
        let parsed: AgentEvent = serde_json::from_str(payload).unwrap();
        assert!(matches!(parsed, AgentEvent::Unknown));
    }

    #[test]
    fn agent_event_known_variant_still_parses_post_unknown() {
        let payload = r#"{"type":"TextDelta","text":"hello"}"#;
        let parsed: AgentEvent = serde_json::from_str(payload).unwrap();
        match parsed {
            AgentEvent::TextDelta { text } => assert_eq!(text, "hello"),
            other => panic!("wrong variant: {:?}", other),
        }
    }

    #[test]
    fn permission_outcome_unknown_absorbs_future_kind() {
        let payload = r#"{"kind":"some_future_outcome"}"#;
        let parsed: PermissionOutcome = serde_json::from_str(payload).unwrap();
        assert!(matches!(parsed, PermissionOutcome::Unknown));
        // Critical safety property: unknown outcomes MUST NOT be
        // treated as approval. If this ever flips we lose the fail-
        // safe and a producer could silently authorise a tool.
        assert!(!parsed.is_approved());
        assert!(parsed.skip_message().contains("unrecognised"));
    }

    #[test]
    fn permission_outcome_approved_still_parses() {
        let payload = r#"{"kind":"approved"}"#;
        let parsed: PermissionOutcome = serde_json::from_str(payload).unwrap();
        assert!(parsed.is_approved());
    }

    #[test]
    fn versioned_agent_event_roundtrips_at_current_version() {
        let env = VersionedAgentEvent::current(AgentEvent::TextDelta { text: "x".into() });
        let s = serde_json::to_string(&env).unwrap();
        let back: VersionedAgentEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.v, AGENT_EVENT_SCHEMA_VERSION);
        assert!(!back.is_from_newer_producer());
        assert!(matches!(back.event, AgentEvent::TextDelta { .. }));
    }

    #[test]
    fn versioned_agent_event_detects_newer_producer() {
        let payload = format!(
            r#"{{"v":{},"event":{{"type":"TextDelta","text":"x"}}}}"#,
            AGENT_EVENT_SCHEMA_VERSION + 1
        );
        let env: VersionedAgentEvent = serde_json::from_str(&payload).unwrap();
        assert!(env.is_from_newer_producer());
    }

    // ── G26 / P7.1: StepId tests ─────────────────────────────────────────

    #[test]
    fn step_id_starts_at_one_and_is_monotonic() {
        let session = SessionState::new(
            std::path::PathBuf::from("/tmp/x"),
            ProviderId::new("p"),
            ModelId::new("m"),
        );
        assert_eq!(session.current_step(), StepId::PRELOOP);
        let s1 = session.next_step();
        let s2 = session.next_step();
        let s3 = session.next_step();
        assert_eq!(s1.raw(), 1);
        assert_eq!(s2.raw(), 2);
        assert_eq!(s3.raw(), 3);
        assert_eq!(session.current_step(), StepId(3));
    }

    #[test]
    fn step_id_counter_is_shared_across_clones() {
        let session = SessionState::new(
            std::path::PathBuf::from("/tmp/x"),
            ProviderId::new("p"),
            ModelId::new("m"),
        );
        let cloned = session.clone();
        let _ = session.next_step();
        let _ = cloned.next_step();
        // Both views observe the same monotonic clock — clone shares
        // the Arc, not a fresh counter, so step ids cannot collide
        // between an orchestrator and an emitter that holds a clone.
        assert_eq!(session.current_step().raw(), 2);
        assert_eq!(cloned.current_step().raw(), 2);
    }

    #[test]
    fn step_id_skipped_after_serde_roundtrip_is_safe() {
        let session = SessionState::new(
            std::path::PathBuf::from("/tmp/x"),
            ProviderId::new("p"),
            ModelId::new("m"),
        );
        let _ = session.next_step();
        let _ = session.next_step();
        let json = serde_json::to_string(&session).unwrap();
        // step_counter is #[serde(skip)], so a restored session
        // resumes from 0 — replays drive progression deterministically
        // via recorded StepStarted events.
        let restored: SessionState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.current_step(), StepId::PRELOOP);
        assert_eq!(restored.next_step().raw(), 1);
    }

    #[test]
    fn step_started_completed_serde_roundtrip() {
        let started = AgentEvent::StepStarted { step_id: 7 };
        let json = serde_json::to_string(&started).unwrap();
        assert!(json.contains("\"step_id\":7"));
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, AgentEvent::StepStarted { step_id: 7 }));

        let completed = AgentEvent::StepCompleted {
            step_id: 7,
            ok: false,
        };
        let json = serde_json::to_string(&completed).unwrap();
        assert!(json.contains("\"ok\":false"));
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            AgentEvent::StepCompleted {
                step_id: 7,
                ok: false
            }
        ));
    }

    #[test]
    fn token_budget_for_model_picks_anthropic_specs() {
        let b = TokenBudget::for_model("claude-opus-4.6");
        assert_eq!(b.context_limit, 200_000);
        assert_eq!(b.reserved_output, 32_000);

        let b = TokenBudget::for_model("anthropic/claude-haiku-4-5");
        assert_eq!(b.context_limit, 200_000);
        assert_eq!(b.reserved_output, 8_192);
    }

    #[test]
    fn token_budget_for_model_picks_openai_specs() {
        let b = TokenBudget::for_model("gpt-4o-mini");
        assert_eq!(b.context_limit, 128_000);
        assert_eq!(b.reserved_output, 16_384);

        let b = TokenBudget::for_model("openai/gpt-3.5-turbo");
        assert_eq!(b.context_limit, 16_385);
        // Cap kicks in: 4_096 vs ¼ of 16_385 (4_096) → equal.
        assert!(b.reserved_output <= b.context_limit / 4 + 1);
    }

    #[test]
    fn token_budget_for_model_picks_gemini_specs() {
        let b = TokenBudget::for_model("gemini-1.5-pro-001");
        assert_eq!(b.context_limit, 2_000_000);
        assert_eq!(b.reserved_output, 8_192);
    }

    #[test]
    fn token_budget_for_model_unknown_falls_back_to_defaults() {
        let b = TokenBudget::for_model("totally-bogus-model-xyz");
        assert_eq!(b.context_limit, TokenBudget::DEFAULT_CONTEXT_LIMIT);
        assert_eq!(b.reserved_output, TokenBudget::DEFAULT_RESERVED_OUTPUT);
    }

    #[test]
    fn token_budget_for_model_is_case_insensitive() {
        let lo = TokenBudget::for_model("claude-sonnet-4-5");
        let mixed = TokenBudget::for_model("Claude-Sonnet-4-5");
        assert_eq!(lo.context_limit, mixed.context_limit);
        assert_eq!(lo.reserved_output, mixed.reserved_output);
    }

    // ── P13 — introspection surface wire-format pinning ──────────────────

    #[test]
    fn p13_plan_step_pending_backwards_compatible_missing_new_fields() {
        // Legacy producers without P13 fields must still deserialize —
        // step_id defaults to 0, depends_on to [], parent_step_id to None.
        let legacy = r#"{"type":"PlanStepPending","step":1,"revision":0,"plan_revision":1,"tool_name":"edit","description":"x"}"#;
        let parsed: AgentEvent = serde_json::from_str(legacy).unwrap();
        match parsed {
            AgentEvent::PlanStepPending {
                step_id,
                depends_on,
                parent_step_id,
                ..
            } => {
                assert_eq!(step_id, StepId(0));
                assert!(depends_on.is_empty());
                assert!(parent_step_id.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn p13_plan_step_pending_with_deps_roundtrips() {
        let ev = AgentEvent::PlanStepPending {
            step: 3,
            step_id: StepId(30),
            revision: 0,
            plan_revision: 7,
            tool_name: "edit".into(),
            description: "apply fix".into(),
            depends_on: vec![StepId(10), StepId(20)],
            parent_step_id: Some(StepId(5)),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"step_id\":30"));
        assert!(s.contains("\"depends_on\":[10,20]"));
        assert!(s.contains("\"parent_step_id\":5"));
        let _: AgentEvent = serde_json::from_str(&s).unwrap();
    }

    #[test]
    fn p13_mode_changed_roundtrip_with_and_without_lens() {
        let with_lens = AgentEvent::ModeChanged {
            from_mode: "plan".into(),
            to_mode: "act".into(),
            from_lens: None,
            to_lens: Some("normal".into()),
        };
        let s = serde_json::to_string(&with_lens).unwrap();
        assert!(s.contains("\"type\":\"ModeChanged\""));
        assert!(s.contains("\"to_lens\":\"normal\""));
        assert!(
            !s.contains("\"from_lens\""),
            "None lens must be omitted, got {s}"
        );
        let _: AgentEvent = serde_json::from_str(&s).unwrap();
    }

    #[test]
    fn p13_introspection_envelope_applied_wire_shape() {
        let summary = EnvelopeSummaryV1 {
            read_scope_count: 4,
            write_scope_count: 2,
            write_deny_count: 1,
            network_enabled: false,
            exec_enabled: false,
            approval_cadence: "per_turn".into(),
            scope_source: "preset:plan".into(),
            display_text: None,
        };
        let ev = AgentEvent::Introspection(IntrospectionEventV1::EnvelopeApplied { summary });
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"type\":\"Introspection\""));
        assert!(s.contains("\"kind\":\"envelope_applied\""));
        assert!(s.contains("\"approval_cadence\":\"per_turn\""));
        // Security: display_text must be omitted when None (don't leak
        // prompt-text-as-API by accident).
        assert!(!s.contains("display_text"));
        let _: AgentEvent = serde_json::from_str(&s).unwrap();
    }

    #[test]
    fn p13_step_assigned_default_redacts_exact_names() {
        let assignment = AssignmentSummaryV1 {
            execution_id: ExecutionId(42),
            step_id: StepId(7),
            persona_id: "ml-architect".into(),
            model_vendor: "anthropic".into(),
            model_tier: "opus".into(),
            model_id_exact: None,
            activated_skills_count: 2,
            activated_agents_count: 1,
            activated_skill_names: None,
            activated_agent_names: None,
            attempt: 1,
        };
        let ev = AgentEvent::Introspection(IntrospectionEventV1::StepAssigned { assignment });
        let s = serde_json::to_string(&ev).unwrap();
        // The redaction-by-default contract: exact names must be absent
        // from the default wire payload.
        assert!(
            !s.contains("model_id_exact"),
            "model_id_exact leaked in default wire format: {s}"
        );
        assert!(!s.contains("activated_skill_names"));
        assert!(!s.contains("activated_agent_names"));
        // But counts must be present.
        assert!(s.contains("\"activated_skills_count\":2"));
        assert!(s.contains("\"activated_agents_count\":1"));
        assert!(s.contains("\"attempt\":1"));
        let _: AgentEvent = serde_json::from_str(&s).unwrap();
    }

    #[test]
    fn p13_agent_edge_kinds_cover_rubber_duck_findings() {
        // Enumerate every kind the critique called out so a future
        // contributor can't silently drop one.
        use AgentEdgeKind::*;
        for k in [Delegation, Critique, Handoff, Retry, Spawn] {
            let s = serde_json::to_string(&k).unwrap();
            // snake_case on the wire.
            assert!(s.chars().all(|c| c.is_lowercase() || c == '_' || c == '"'));
            let back: AgentEdgeKind = serde_json::from_str(&s).unwrap();
            assert_eq!(k, back);
        }
        use ProvenanceEdgeKind::*;
        for k in [ExecutesStep, AmendsPlan, ExpandsScope] {
            let s = serde_json::to_string(&k).unwrap();
            let back: ProvenanceEdgeKind = serde_json::from_str(&s).unwrap();
            assert_eq!(k, back);
        }
    }

    #[test]
    fn p13_critique_emitted_carries_target_execution() {
        let ev = AgentEvent::Introspection(IntrospectionEventV1::CritiqueEmitted {
            from_execution_id: ExecutionId(2),
            target_execution_id: ExecutionId(1),
            severity: CritiqueSeverity::Critical,
            blocking: true,
        });
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"critique_emitted\""));
        assert!(s.contains("\"from_execution_id\":2"));
        assert!(s.contains("\"target_execution_id\":1"));
        assert!(s.contains("\"blocking\":true"));
        let _: AgentEvent = serde_json::from_str(&s).unwrap();
    }

    #[test]
    fn p13_versioned_event_carries_causal_metadata() {
        let ev = AgentEvent::TextDelta { text: "x".into() };
        let wrapped =
            VersionedAgentEvent::with_causality(ev, EventId(42), 7, vec![EventId(40), EventId(41)]);
        let s = serde_json::to_string(&wrapped).unwrap();
        assert!(s.contains("\"event_id\":42"));
        assert!(s.contains("\"turn_seq\":7"));
        assert!(s.contains("\"causal_parent_ids\":[40,41]"));
        let back: VersionedAgentEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.event_id, EventId(42));
        assert_eq!(back.turn_seq, 7);
        assert_eq!(back.causal_parent_ids.len(), 2);
    }

    #[test]
    fn p13_versioned_event_legacy_payload_defaults_causality() {
        // Pre-P13 producer — no event_id / turn_seq / causal_parent_ids.
        let legacy = r#"{"v":1,"event":{"type":"TextDelta","text":"hi"}}"#;
        let back: VersionedAgentEvent = serde_json::from_str(legacy).unwrap();
        assert_eq!(back.event_id, EventId(0));
        assert_eq!(back.turn_seq, 0);
        assert!(back.causal_parent_ids.is_empty());
    }

    #[test]
    fn p13_older_client_absorbs_introspection_as_unknown() {
        // Client on pre-P13 schema would see the new Introspection tag
        // via #[serde(other)] Unknown. Simulate: feed a NEW-style payload,
        // but assert parsed type.
        let payload = r#"{"type":"Introspection","kind":"envelope_applied","summary":{"read_scope_count":0,"write_scope_count":0,"write_deny_count":0,"network_enabled":false,"exec_enabled":false,"approval_cadence":"never","scope_source":"custom"}}"#;
        let ev: AgentEvent = serde_json::from_str(payload).unwrap();
        // On THIS build (new schema) it parses into Introspection, not Unknown.
        assert!(
            matches!(ev, AgentEvent::Introspection(_)),
            "new build must parse Introspection; got {ev:?}"
        );
    }

    // ── PermissionRequest.raw_input (A1): wire-compat ────────────────────

    /// Old persisted/legacy JSON (without `raw_input`) must deserialize
    /// into `PermissionRequest` with `raw_input: None`. Guards against
    /// inadvertent removal of `#[serde(default)]` on the new field.
    #[test]
    fn permission_request_old_payload_deserializes_to_none() {
        let legacy = r#"{
            "type": "PermissionRequest",
            "id": "perm_t1",
            "capability": "bash",
            "description": "bash with args: {\"command\":\"ls\"}"
        }"#;
        let ev: AgentEvent = serde_json::from_str(legacy).unwrap();
        match ev {
            AgentEvent::PermissionRequest {
                id,
                capability,
                description: _,
                raw_input,
            } => {
                assert_eq!(id, "perm_t1");
                assert_eq!(capability, "bash");
                assert!(raw_input.is_none(), "raw_input must default to None");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    /// `raw_input: None` must NOT appear in the serialized JSON; a
    /// downstream consumer reading old bytes shouldn't see a new
    /// key pop up once this build roundtrips the event.
    #[test]
    fn permission_request_none_raw_input_skipped_in_serialization() {
        let ev = AgentEvent::PermissionRequest {
            id: "perm_t1".into(),
            capability: "bash".into(),
            description: "bash with args: {}".into(),
            raw_input: None,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(
            !s.contains("raw_input"),
            "None raw_input must be skipped; got {s}",
        );
    }

    /// `Some(value)` roundtrips preserving structure — this is the
    /// contract always-allow matching relies on.
    #[test]
    fn permission_request_some_raw_input_roundtrips() {
        use serde_json::json;
        let ev = AgentEvent::PermissionRequest {
            id: "perm_t1".into(),
            capability: "bash".into(),
            description: "bash with args: {\"command\":\"ls\"}".into(),
            raw_input: Some(json!({"command": "ls -la", "cwd": "/tmp"})),
        };
        let s = serde_json::to_string(&ev).unwrap();
        let parsed: AgentEvent = serde_json::from_str(&s).unwrap();
        match parsed {
            AgentEvent::PermissionRequest { raw_input, .. } => {
                let v = raw_input.expect("must be Some");
                assert_eq!(v["command"].as_str(), Some("ls -la"));
                assert_eq!(v["cwd"].as_str(), Some("/tmp"));
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }
}
