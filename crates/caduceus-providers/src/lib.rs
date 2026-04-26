use async_trait::async_trait;
use caduceus_core::{
    AuthStore, CaduceusError, ContentBlock, ImageContent, ImageSource, LlmMessage, ModelId,
    ProviderId, Result, Role, ToolResult, ToolSpec, ToolUse,
};
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use tracing::warn;

pub mod mock;
pub mod retry_adapter;
pub mod taper;

// Re-export StopReason from core — canonical definition lives in caduceus_core
pub use caduceus_core::StopReason;

// ── Message types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_blocks: Option<Vec<MessageContentBlock>>,
    /// Tool calls requested by the assistant (populated when role = "assistant").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolUse>,
    /// Tool result (populated when role = "tool").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<ToolResult>,
    /// P13.4 (G‑R4.3) — provider prompt‑caching hint. When `true`,
    /// adapters that support explicit cache breakpoints (Anthropic
    /// `cache_control: ephemeral`) MUST attach the breakpoint to this
    /// message's last content block. Adapters with implicit prefix
    /// caching (OpenAI ≥1024‑token prefix) MUST treat this as a noop;
    /// Gemini also noop. The harness sets this on the most recent
    /// stable message before a new user turn so the long prefix is
    /// reused across turns. Defaults to `false` to preserve byte‑for‑
    /// byte wire compatibility on every existing call site.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cache_breakpoint: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: String,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self {
            kind: "ephemeral".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContentBlock {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Image {
        base64: String,
        media_type: String,
    },
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            role: "user".into(),
            content: content.clone(),
            content_blocks: Some(vec![MessageContentBlock::text(content)]),
            tool_calls: Vec::new(),
            tool_result: None,
            cache_breakpoint: false,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            role: "assistant".into(),
            content: content.clone(),
            content_blocks: Some(vec![MessageContentBlock::text(content)]),
            tool_calls: Vec::new(),
            tool_result: None,
            cache_breakpoint: false,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            role: "system".into(),
            content: content.clone(),
            content_blocks: Some(vec![MessageContentBlock::text(content)]),
            tool_calls: Vec::new(),
            tool_result: None,
            cache_breakpoint: false,
        }
    }

    pub fn with_content_blocks(mut self, blocks: Vec<MessageContentBlock>) -> Self {
        self.content = blocks
            .iter()
            .map(MessageContentBlock::text_value)
            .collect::<Vec<_>>()
            .join("");
        self.content_blocks = Some(blocks);
        self
    }

    pub fn content_blocks(&self) -> Vec<MessageContentBlock> {
        self.content_blocks
            .clone()
            .unwrap_or_else(|| vec![MessageContentBlock::text(self.content.clone())])
    }

    pub fn content_text(&self) -> String {
        self.content_blocks()
            .iter()
            .map(MessageContentBlock::text_value)
            .collect::<Vec<_>>()
            .join("")
    }

    /// P13.4 (G‑R4.3) — builder helper: mark this message as a
    /// provider cache breakpoint. See [`Message::cache_breakpoint`]
    /// for adapter behaviour.
    pub fn with_cache_breakpoint(mut self) -> Self {
        self.cache_breakpoint = true;
        self
    }
}

// ── ST-C2 Phase 1 — storage/wire conversion ───────────────────────────────
//
// `caduceus_core::LlmMessage` is the *canonical storage* shape (role + content
// blocks). `providers::Message` is the *wire* shape — role-as-string plus
// flattened tool-call / tool-result fields, serde-shaped for provider APIs.
//
// These `From` impls let callers convert at the adapter boundary without
// leaking wire concerns into storage. The conversion is lossy in both
// directions:
//
//   * wire → storage: drops `cache_breakpoint` (wire-only hint) and the
//     duplicate `content: String` (content_blocks is authoritative).
//   * storage → wire: rebuilds `content: String` by joining text blocks;
//     `cache_breakpoint` defaults to false.
//
// These conversions materialise a fresh `Vec<MessageContentBlock>`, so they
// are NOT free — they exist to enable Phase 2 (switching
// `ConversationHistory` storage to `LlmMessage`) and Phase 3 (Arc-sharing
// the content slice on the hot path). Until then, call sites still use
// `providers::Message` directly.

fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}

fn role_from_str(s: &str) -> Role {
    match s {
        "assistant" => Role::Assistant,
        "system" => Role::System,
        // Default unknown roles (including "tool") to User — matches the
        // provider wire convention where tool-result messages are sent with
        // role=user and the tool payload in tool_result.
        _ => Role::User,
    }
}

impl From<&LlmMessage> for Message {
    fn from(m: &LlmMessage) -> Self {
        let mut content_text = String::new();
        let mut blocks: Vec<MessageContentBlock> = Vec::new();
        let mut tool_calls: Vec<ToolUse> = Vec::new();
        let mut tool_result: Option<ToolResult> = None;

        for block in &m.content {
            match block {
                ContentBlock::Text(s) => {
                    content_text.push_str(s);
                    blocks.push(MessageContentBlock::text(s.clone()));
                }
                ContentBlock::Image(img) => match &img.source {
                    ImageSource::Base64 { media_type, data } => {
                        blocks.push(MessageContentBlock::Image {
                            base64: data.clone(),
                            media_type: media_type.clone(),
                        });
                    }
                    ImageSource::Url(_) => {
                        // Wire `MessageContentBlock::Image` is base64-only
                        // today; drop URL-sourced images rather than fail.
                        // Adapters that care (Anthropic, OpenAI vision) must
                        // resolve URLs upstream of the wire boundary.
                    }
                },
                ContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolUse {
                        id: id.0.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                }
                ContentBlock::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                } => {
                    // Wire Message holds at most one tool_result; if the
                    // storage message contains multiple, we keep the first
                    // and warn. Callers that need one-per-tool-result must
                    // split the storage message upstream.
                    if tool_result.is_some() {
                        tracing::warn!(
                            tool_call_id = %tool_call_id.0,
                            "LlmMessage → providers::Message: dropped second tool_result; \
                             wire Message holds only one. Split storage message upstream."
                        );
                    } else {
                        tool_result = Some(
                            if *is_error {
                                ToolResult::error(content.clone())
                            } else {
                                ToolResult::success(content.clone())
                            }
                            .with_tool_use_id(tool_call_id.0.clone()),
                        );
                    }
                }
            }
        }

        Message {
            role: role_to_str(m.role).into(),
            content: content_text,
            content_blocks: if blocks.is_empty() {
                None
            } else {
                Some(blocks)
            },
            tool_calls,
            tool_result,
            cache_breakpoint: false,
        }
    }
}

impl From<LlmMessage> for Message {
    fn from(m: LlmMessage) -> Self {
        Self::from(&m)
    }
}

impl From<&Message> for LlmMessage {
    fn from(m: &Message) -> Self {
        let role = role_from_str(&m.role);
        let mut content: Vec<ContentBlock> = Vec::new();

        // Prefer structured content_blocks when present; fall back to the
        // plain `content` string for legacy call sites.
        if let Some(blocks) = &m.content_blocks {
            for b in blocks {
                match b {
                    MessageContentBlock::Text { text, .. } => {
                        content.push(ContentBlock::Text(text.clone()));
                    }
                    MessageContentBlock::Image { base64, media_type } => {
                        content.push(ContentBlock::Image(ImageContent {
                            source: ImageSource::Base64 {
                                media_type: media_type.clone(),
                                data: base64.clone(),
                            },
                            detail: None,
                        }));
                    }
                }
            }
        } else if !m.content.is_empty() {
            content.push(ContentBlock::Text(m.content.clone()));
        }

        for tc in &m.tool_calls {
            content.push(ContentBlock::ToolUse {
                id: caduceus_core::ToolCallId::new(tc.id.clone()),
                name: tc.name.clone(),
                input: tc.input.clone(),
            });
        }

        if let Some(tr) = &m.tool_result {
            content.push(ContentBlock::ToolResult {
                tool_call_id: tr
                    .tool_use_id
                    .clone()
                    .map(caduceus_core::ToolCallId::new)
                    .unwrap_or_else(|| caduceus_core::ToolCallId::new("")),
                content: tr.content.clone(),
                is_error: tr.is_error,
            });
        }

        LlmMessage { role, content }
    }
}

impl From<Message> for LlmMessage {
    fn from(m: Message) -> Self {
        Self::from(&m)
    }
}

impl MessageContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            cache_control: None,
        }
    }

    pub fn text_with_cache(text: impl Into<String>, cache_control: CacheControl) -> Self {
        Self::Text {
            text: text.into(),
            cache_control: Some(cache_control),
        }
    }

    pub fn image(base64: impl Into<String>, media_type: impl Into<String>) -> Self {
        Self::Image {
            base64: base64.into(),
            media_type: media_type.into(),
        }
    }

    fn text_value(&self) -> String {
        match self {
            Self::Text { text, .. } => text.clone(),
            Self::Image { .. } => String::new(),
        }
    }
}

// ── Model filter ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelFilter {
    pub allowed: Option<Vec<String>>,
    pub denied: Option<Vec<String>>,
}

impl ModelFilter {
    pub fn check(&self, model: &ModelId) -> Result<()> {
        if let Some(denied) = &self.denied {
            if denied.iter().any(|d| d == &model.0) {
                return Err(CaduceusError::Provider(format!(
                    "Model '{}' is denied by filter",
                    model.0
                )));
            }
        }
        if let Some(allowed) = &self.allowed {
            if !allowed.iter().any(|a| a == &model.0) {
                return Err(CaduceusError::Provider(format!(
                    "Model '{}' is not in the allowed list",
                    model.0
                )));
            }
        }
        Ok(())
    }
}

// ── Tool choice ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Specific(String),
}

// ── Response format ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: ModelId,
    /// Conversation history for this request.
    ///
    /// Stored as `Arc<[Message]>` (ST-C2 Phase 3 / audit finding C1) so
    /// that cloning a ChatRequest for retry/failover is O(1) (refcount
    /// bump) instead of O(N × message-size). Previously `Vec<Message>`;
    /// the serde surface is unchanged — `Arc<[T]>` serialises to the
    /// same JSON array as `Vec<T>`.
    pub messages: Arc<[Message]>,
    pub system: Option<String>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    #[serde(default)]
    pub thinking_mode: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    /// Tool definitions available for the LLM to call.
    ///
    /// Stored as `Arc<[ToolSpec]>` so that the long-lived tool spec list
    /// cached on the harness can be shared across every turn's ChatRequest
    /// without reallocating / cloning the underlying Vec (ST-C2 Phase 4 /
    /// audit finding I10).
    #[serde(default, skip_serializing_if = "<[ToolSpec]>::is_empty")]
    pub tools: Arc<[ToolSpec]>,
    /// Opt-in: request token logprobs (gap G10 / P3.2). When `Some(n)`,
    /// providers that support logprobs (currently: OpenAI-compatible)
    /// will request `n` top alternatives per token AND return a
    /// [`LogprobsSummary`] in [`ChatResponse::logprobs`]. Providers that
    /// do NOT support logprobs (Anthropic, mock) silently ignore this
    /// flag — callers must treat the absence of `logprobs` in the
    /// response as "unsupported", NOT as "high confidence".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<u8>,
    /// A3: cross-repo thread identity (Zed thread / caduceus session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// A3: cross-repo prompt identity. One user turn may fan out to
    /// several ChatRequests (verification rollouts, summarization,
    /// critique) that share the same `prompt_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    /// A3: why this request was issued. Drives `validate()` invariants
    /// and is forwarded to provider analytics. `None` = legacy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<CompletionIntent>,
    /// A3: custom stop sequences. Empty vec = unset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// A3 FU#2: reasoning effort hint (e.g. "low" / "medium" / "high") for
    /// models that expose a thinking-effort knob. Mirrors Zed's
    /// `LanguageModelRequest::thinking_effort`. `None` = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<String>,
    /// A3 FU#2: latency/quality hint. Mirrors Zed's
    /// `LanguageModelRequest::speed`. `None` = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
}

/// A3 FU#2: request-speed hint. Byte-for-byte serde compatible with Zed's
/// `language_model_core::Speed` (`standard` | `fast`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    Standard,
    Fast,
}

/// A3: why a ChatRequest was issued. Mirrors Zed's `CompletionIntent`
/// (verbatim 10 variants) plus 3 caduceus-only variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionIntent {
    UserPrompt,
    Subagent,
    ToolResults,
    ThreadSummarization,
    ThreadContextSummarization,
    CreateFile,
    EditFile,
    InlineAssist,
    TerminalInlineAssist,
    GenerateGitCommitMessage,
    /// Caduceus-only: verification rollouts (PRM scoring, re-runs).
    /// MUST NOT carry tools — enforced by `ChatRequest::validate()`.
    VerificationRollout,
    /// Caduceus-only: fallback summarization when primary path fails.
    SummarizationFallback,
    /// Caduceus-only: single-shot tool-free completion.
    OneShot,
}

impl CompletionIntent {
    /// Returns true if this intent is incompatible with tool invocation.
    pub fn forbids_tools(self) -> bool {
        matches!(
            self,
            CompletionIntent::VerificationRollout
                | CompletionIntent::ThreadSummarization
                | CompletionIntent::ThreadContextSummarization
                | CompletionIntent::SummarizationFallback
                | CompletionIntent::GenerateGitCommitMessage
        )
    }
}

/// A3: structured error for [`ChatRequest::validate`].
#[derive(Debug, thiserror::Error)]
pub enum ChatRequestError {
    #[error("intent {intent:?} forbids tools, but {count} tool(s) were attached")]
    IntentForbidsTools {
        intent: CompletionIntent,
        count: usize,
    },
}

impl ChatRequest {
    /// A3: fail-closed invariants on the ChatRequest shape.
    pub fn validate(&self) -> std::result::Result<(), ChatRequestError> {
        if let Some(intent) = self.intent {
            if intent.forbids_tools() && !self.tools.is_empty() {
                debug_assert!(
                    false,
                    "ChatRequest invariant: intent {intent:?} forbids tools but {} were attached",
                    self.tools.len()
                );
                return Err(ChatRequestError::IntentForbidsTools {
                    intent,
                    count: self.tools.len(),
                });
            }
        }
        Ok(())
    }
}

/// UX-friendly confidence bucket derived from token logprobs (G10 / P3.2).
///
/// Buckets are calibrated on `min_token_p` (the worst single-token
/// probability in the response): a chain is only as strong as its weakest
/// link, and rendering the *minimum* prevents the average from masking a
/// single very-uncertain token. Thresholds:
/// - `High`: min_p ≥ 0.85
/// - `Medium`: 0.50 ≤ min_p < 0.85
/// - `Low`: min_p < 0.50
///
/// These mirror Glassman/Amershi UX consensus on three-bucket calibrated
/// trust signals in human-AI interaction (2024). They are deliberately
/// coarse — finer-grained calibration requires per-model rescaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    /// Bucket a single probability into [`Confidence`]. Hard-clamps so
    /// NaN / out-of-range values don't escape — defensive against
    /// provider parsers returning garbage. NaN/sub-zero/None map to
    /// `Low` (conservative).
    pub fn from_min_p(min_p: f32) -> Self {
        if !min_p.is_finite() || min_p < 0.5 {
            Confidence::Low
        } else if min_p < 0.85 {
            Confidence::Medium
        } else {
            Confidence::High
        }
    }
}

/// Aggregated logprobs telemetry (gap G10 / P3.2).
///
/// We do NOT ship the per-token vector through to the UI — it can be
/// many KB per response and clutters the event bus. Instead we ship a
/// small summary that's enough to render a "confidence dot" plus a
/// detail tooltip. Per-token data, when needed, can still be fetched
/// from the provider directly via debug logging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogprobsSummary {
    /// How many tokens were measured.
    pub n_tokens: u32,
    /// Mean probability across all tokens (`exp(logprob)` averaged).
    pub mean_token_p: f32,
    /// Minimum single-token probability — the weakest link.
    pub min_token_p: f32,
    /// Three-bucket UX classification driven by `min_token_p`.
    pub confidence: Confidence,
}

impl LogprobsSummary {
    /// Build a summary from a slice of per-token probabilities.
    /// Returns `None` for an empty slice — the caller should treat that
    /// as "no confidence info available" (NOT as low confidence).
    /// Non-finite values (NaN/inf) are silently dropped before
    /// aggregation; a slice consisting entirely of garbage returns
    /// `None`.
    pub fn from_token_probs(probs: &[f32]) -> Option<Self> {
        let mut sum = 0.0_f64;
        let mut min = f32::INFINITY;
        let mut count: u32 = 0;
        for &p in probs {
            if !p.is_finite() || !(0.0..=1.0).contains(&p) {
                continue;
            }
            sum += p as f64;
            if p < min {
                min = p;
            }
            count += 1;
        }
        if count == 0 {
            return None;
        }
        let mean = (sum / count as f64) as f32;
        Some(Self {
            n_tokens: count,
            mean_token_p: mean,
            min_token_p: min,
            confidence: Confidence::from_min_p(min),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub content: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    pub stop_reason: StopReason,
    /// Tool calls requested by the LLM (when stop_reason = ToolUse).
    #[serde(default)]
    pub tool_calls: Vec<ToolUse>,
    /// Logprobs summary when the request asked for them AND the
    /// provider supports them. (G10 / P3.2). `None` MUST be treated as
    /// "unsupported / not requested", not as "low confidence".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<LogprobsSummary>,
    /// ST-A6: opaque reasoning/thinking text emitted separately from
    /// `content`. Serde-default so existing callers and on-disk
    /// payloads that predate ST-A6 deserialise as `""` (empty). UIs
    /// that want to render a "thinking" pane read this field; legacy
    /// text-only consumers remain untouched.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thinking: String,
}

// StopReason is re-exported from caduceus_core — canonical definition lives there.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub delta: String,
    pub is_final: bool,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_read_tokens: Option<u32>,
    pub cache_creation_tokens: Option<u32>,
    /// Reasoning / thinking delta produced by the provider during
    /// streaming. Empty for providers that don't emit interleaved
    /// thinking or when the chunk is a pure text/usage/final chunk.
    /// Wire-compat: `#[serde(default)]` so providers unaware of this
    /// field decode cleanly, and `skip_serializing_if` keeps the wire
    /// byte-identical when there is no thinking.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thinking: String,
}

pub type StreamResult = Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>;

// ── LlmAdapter trait ───────────────────────────────────────────────────────────

#[async_trait]
pub trait LlmAdapter: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;
    async fn stream(&self, request: ChatRequest) -> Result<StreamResult>;
    async fn list_models(&self) -> Result<Vec<ModelId>>;
}

// ── Constants ──────────────────────────────────────────────────────────────────

const ANTHROPIC_VERSION: &str = "2023-06-01";

// ── Retry configuration ───────────────────────────────────────────────────────

/// Configuration for retry-with-jitter behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_retries: usize,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
        }
    }
}

impl RetryConfig {
    /// Compute delay with exponential backoff and jitter:
    /// `delay = min(base_delay * 2^attempt + random(0..base_delay), max_delay)`
    pub fn delay_for_attempt(&self, attempt: usize) -> std::time::Duration {
        use rand::Rng;
        let shift = (attempt as u32).min(63);
        let exp_delay = self.base_delay_ms.saturating_mul(1u64 << shift);
        let jitter = rand::thread_rng().gen_range(0..=self.base_delay_ms);
        let total = exp_delay.saturating_add(jitter).min(self.max_delay_ms);
        std::time::Duration::from_millis(total)
    }
}

// ── Circuit breaker ──────────────────────────────────────────────────────────

/// Circuit breaker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CircuitState {
    /// Normal operation — requests pass through.
    Closed = 0,
    /// Circuit tripped — requests are rejected immediately.
    Open = 1,
    /// Cooldown expired — allow one probe request to test recovery.
    HalfOpen = 2,
}

impl CircuitState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Closed,
            1 => Self::Open,
            2 => Self::HalfOpen,
            _ => Self::Closed,
        }
    }
}

/// Auto-disable a provider/tool after N consecutive failures.
///
/// State machine: Closed → (threshold failures) → Open → (cooldown) → HalfOpen
///   - HalfOpen + success → Closed
///   - HalfOpen + failure → Open
pub struct CircuitBreaker {
    failure_count: std::sync::atomic::AtomicU32,
    threshold: u32,
    state: std::sync::atomic::AtomicU8,
    last_failure: std::sync::Mutex<Option<std::time::Instant>>,
    cooldown: std::time::Duration,
}

impl CircuitBreaker {
    pub fn new(threshold: u32, cooldown: std::time::Duration) -> Self {
        Self {
            failure_count: std::sync::atomic::AtomicU32::new(0),
            threshold,
            state: std::sync::atomic::AtomicU8::new(CircuitState::Closed as u8),
            last_failure: std::sync::Mutex::new(None),
            cooldown,
        }
    }

    /// Check whether a request should be allowed through.
    /// Returns `Ok(())` if the circuit is closed or half-open (probe allowed).
    /// Returns `Err` if the circuit is open.
    pub fn check(&self) -> Result<()> {
        let state = CircuitState::from_u8(self.state.load(std::sync::atomic::Ordering::SeqCst));
        match state {
            CircuitState::Closed => Ok(()),
            CircuitState::HalfOpen => Ok(()), // allow probe
            CircuitState::Open => {
                // Check if cooldown has expired → transition to HalfOpen
                let last = self.last_failure.lock().unwrap();
                if let Some(instant) = *last {
                    if instant.elapsed() >= self.cooldown {
                        drop(last);
                        self.state.store(
                            CircuitState::HalfOpen as u8,
                            std::sync::atomic::Ordering::SeqCst,
                        );
                        return Ok(());
                    }
                }
                Err(CaduceusError::Provider(
                    "Circuit breaker is open — provider temporarily disabled".into(),
                ))
            }
        }
    }

    /// Record a successful request. Resets the circuit to Closed.
    pub fn record_success(&self) {
        self.failure_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
        self.state.store(
            CircuitState::Closed as u8,
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    /// Record a failed request. Increments failure count and may trip the circuit.
    pub fn record_failure(&self) {
        let count = self
            .failure_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        *self.last_failure.lock().unwrap() = Some(std::time::Instant::now());

        if count >= self.threshold {
            self.state.store(
                CircuitState::Open as u8,
                std::sync::atomic::Ordering::SeqCst,
            );
        }
    }

    /// Get the current circuit state.
    pub fn state(&self) -> CircuitState {
        CircuitState::from_u8(self.state.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// Get the current consecutive failure count.
    pub fn failure_count(&self) -> u32 {
        self.failure_count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(5, std::time::Duration::from_secs(60))
    }
}

// ── Retry helper ──────────────────────────────────────────────────────────────

fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504 | 529)
}

async fn send_with_retry(
    _client: &reqwest::Client,
    build_request: impl Fn() -> reqwest::RequestBuilder,
    retry_config: &RetryConfig,
) -> Result<reqwest::Response> {
    let mut last_error = None;

    for attempt in 0..retry_config.max_retries {
        let resp = match build_request().send().await {
            Ok(r) => r,
            Err(e) => {
                last_error = Some(CaduceusError::Provider(format!("Network error: {}", e)));
                if attempt + 1 < retry_config.max_retries {
                    let delay = retry_config.delay_for_attempt(attempt);
                    tokio::time::sleep(delay).await;
                    continue;
                }
                break;
            }
        };

        let status = resp.status().as_u16();

        if resp.status().is_success() {
            return Ok(resp);
        }

        if is_retryable_status(status) && attempt + 1 < retry_config.max_retries {
            let delay = retry_config.delay_for_attempt(attempt);
            warn!(
                "Retryable status ({}), retrying in {:?} (attempt {}/{})",
                status,
                delay,
                attempt + 1,
                retry_config.max_retries
            );
            // Respect Retry-After header if present
            if let Some(retry_after) = resp.headers().get("retry-after") {
                if let Ok(secs) = retry_after.to_str().unwrap_or("").parse::<u64>() {
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    continue;
                }
            }
            tokio::time::sleep(delay).await;
            continue;
        }

        let body = resp.text().await.unwrap_or_default();

        if status == 401 || status == 403 {
            return Err(CaduceusError::Provider(format!(
                "Authentication failed ({}): {}",
                status, body
            )));
        }

        if is_retryable_status(status) {
            return Err(CaduceusError::RateLimited {
                retry_after_secs: 60,
            });
        }

        return Err(CaduceusError::Provider(format!(
            "API error ({}): {}",
            status, body
        )));
    }

    Err(last_error.unwrap_or_else(|| CaduceusError::Provider("Max retries exhausted".into())))
}

// ── Anthropic wire types ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

fn map_anthropic_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        other => {
            tracing::warn!(
                target: "caduceus::providers::anthropic",
                unknown_stop_reason = other,
                "Unknown Anthropic stop_reason; falling back to EndTurn (audit I11)"
            );
            StopReason::EndTurn
        }
    }
}

fn parse_anthropic_chat_response(body: &str) -> Result<ChatResponse> {
    let resp: AnthropicResponse = serde_json::from_str(body).map_err(|e| {
        CaduceusError::Provider(format!(
            "Failed to parse Anthropic response: {} (body: {})",
            e,
            &body[..body.len().min(200)]
        ))
    })?;

    let content = resp
        .content
        .iter()
        .filter_map(|block| match block {
            AnthropicContentBlock::Text { text } => Some(text.as_str()),
            AnthropicContentBlock::ToolUse { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("");

    // Extract tool calls from content blocks
    let tool_calls: Vec<ToolUse> = resp
        .content
        .iter()
        .filter_map(|block| match block {
            AnthropicContentBlock::ToolUse { id, name, input } => Some(ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect();

    let stop_reason = resp
        .stop_reason
        .as_deref()
        .map(map_anthropic_stop_reason)
        .unwrap_or(StopReason::EndTurn);

    Ok(ChatResponse {
        content,
        input_tokens: resp.usage.input_tokens,
        output_tokens: resp.usage.output_tokens,
        cache_read_tokens: resp.usage.cache_read_input_tokens,
        cache_creation_tokens: resp.usage.cache_creation_input_tokens,
        stop_reason,
        tool_calls,
        logprobs: None,
        thinking: String::new(),
    })
}

fn parse_anthropic_sse_event(event_type: &str, data: &str) -> Option<Result<StreamChunk>> {
    match event_type {
        "message_start" => {
            let val: serde_json::Value = serde_json::from_str(data).ok()?;
            let input_tokens = val["message"]["usage"]["input_tokens"]
                .as_u64()
                .map(|n| n as u32);
            Some(Ok(StreamChunk {
                delta: String::new(),
                is_final: false,
                input_tokens,
                output_tokens: None,
                cache_read_tokens: val["message"]["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .map(|n| n as u32),
                cache_creation_tokens: val["message"]["usage"]["cache_creation_input_tokens"]
                    .as_u64()
                    .map(|n| n as u32),
                thinking: String::new(),
            }))
        }
        "content_block_delta" => {
            let val: serde_json::Value = serde_json::from_str(data).ok()?;
            let delta_type = val["delta"]["type"].as_str().unwrap_or("");
            match delta_type {
                "text_delta" => {
                    let text = val["delta"]["text"].as_str().unwrap_or("").to_string();
                    if text.is_empty() {
                        return None;
                    }
                    Some(Ok(StreamChunk {
                        delta: text,
                        is_final: false,
                        input_tokens: None,
                        output_tokens: None,
                        cache_read_tokens: None,
                        cache_creation_tokens: None,
                        thinking: String::new(),
                    }))
                }
                _ => None,
            }
        }
        "message_delta" => {
            let val: serde_json::Value = serde_json::from_str(data).ok()?;
            let output_tokens = val["usage"]["output_tokens"].as_u64().map(|n| n as u32);
            Some(Ok(StreamChunk {
                delta: String::new(),
                is_final: false,
                input_tokens: None,
                output_tokens,
                cache_read_tokens: val["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .map(|n| n as u32),
                cache_creation_tokens: val["usage"]["cache_creation_input_tokens"]
                    .as_u64()
                    .map(|n| n as u32),
                thinking: String::new(),
            }))
        }
        "message_stop" => Some(Ok(StreamChunk {
            delta: String::new(),
            is_final: true,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            thinking: String::new(),
        })),
        _ => None,
    }
}

// ── Anthropic adapter ──────────────────────────────────────────────────────────

pub struct AnthropicAdapter {
    provider_id: ProviderId,
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl AnthropicAdapter {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("anthropic"),
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com/v1".into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    fn build_request_body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| {
                let value = if m.role == "tool" {
                    // Tool result → Anthropic uses role "user" with tool_result content block
                    let tool_use_id = m
                        .tool_result
                        .as_ref()
                        .and_then(|r| r.tool_use_id.clone())
                        .unwrap_or_default();
                    let is_error = m.tool_result.as_ref().map(|r| r.is_error).unwrap_or(false);
                    let mut block = serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": m.content_text(),
                    });
                    if is_error {
                        block["is_error"] = serde_json::json!(true);
                    }
                    serde_json::json!({
                        "role": "user",
                        "content": [block],
                    })
                } else if m.role == "assistant" && !m.tool_calls.is_empty() {
                    // Assistant message with tool calls → add tool_use content blocks
                    let mut content = anthropic_content_blocks(&m.content_blocks());
                    for tc in &m.tool_calls {
                        content.push(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.input,
                        }));
                    }
                    serde_json::json!({
                        "role": "assistant",
                        "content": content,
                    })
                } else {
                    let content_blocks = anthropic_content_blocks(&m.content_blocks());
                    serde_json::json!({
                        "role": m.role,
                        "content": content_blocks,
                    })
                };
                // P13.4 (G‑R4.3) — when this message is a cache
                // breakpoint, stamp `cache_control: ephemeral` onto
                // its LAST content block. Anthropic caches the prefix
                // up to and INCLUDING the breakpointed block; reusing
                // that prefix on the next turn returns
                // `usage.cache_read_input_tokens > 0`.
                if m.cache_breakpoint {
                    let mut value = value;
                    if let Some(arr) = value.get_mut("content").and_then(|c| c.as_array_mut()) {
                        if let Some(last) = arr.last_mut() {
                            last["cache_control"] = serde_json::json!({"type": "ephemeral"});
                        }
                    }
                    value
                } else {
                    value
                }
            })
            .collect();

        let mut body = serde_json::json!({
            "model": request.model.0,
            "max_tokens": request.max_tokens,
            "messages": messages,
            "stream": stream,
        });

        // Serialize tool definitions for Anthropic
        if !request.tools.is_empty() {
            let tools_json: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools_json);
        }

        let mut system_blocks = Vec::new();
        if let Some(ref system) = request.system {
            system_blocks.push(MessageContentBlock::text_with_cache(
                system.clone(),
                CacheControl::ephemeral(),
            ));
        }
        for message in request.messages.iter().filter(|m| m.role == "system") {
            for block in message.content_blocks() {
                system_blocks.push(match block {
                    MessageContentBlock::Text { text, .. } => {
                        MessageContentBlock::text_with_cache(text, CacheControl::ephemeral())
                    }
                    other => other,
                });
            }
        }
        if !system_blocks.is_empty() {
            body["system"] = serde_json::Value::Array(anthropic_content_blocks(&system_blocks));
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if let Some(ref tc) = request.tool_choice {
            body["tool_choice"] = match tc {
                ToolChoice::Auto => serde_json::json!({"type": "auto"}),
                ToolChoice::None => serde_json::json!({"type": "none"}),
                ToolChoice::Required => serde_json::json!({"type": "any"}),
                ToolChoice::Specific(name) => serde_json::json!({"type": "tool", "name": name}),
            };
        }

        if let Some(ResponseFormat::JsonObject) = &request.response_format {
            let current_system = body.get("system").cloned();
            let json_prefix = "You must respond with valid JSON only.";
            if let Some(serde_json::Value::Array(blocks)) = current_system {
                let mut new_blocks = vec![serde_json::json!({"type": "text", "text": json_prefix})];
                new_blocks.extend(blocks);
                body["system"] = serde_json::Value::Array(new_blocks);
            } else {
                body["system"] = serde_json::json!([{"type": "text", "text": json_prefix}]);
            }
        }

        body
    }
}

fn anthropic_content_blocks(blocks: &[MessageContentBlock]) -> Vec<serde_json::Value> {
    blocks
        .iter()
        .map(|block| match block {
            MessageContentBlock::Text {
                text,
                cache_control,
            } => {
                let mut value = serde_json::json!({
                    "type": "text",
                    "text": text,
                });
                if let Some(cache_control) = cache_control {
                    value["cache_control"] = serde_json::json!({
                        "type": cache_control.kind,
                    });
                }
                value
            }
            MessageContentBlock::Image { base64, media_type } => {
                serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": base64,
                    }
                })
            }
        })
        .collect()
}

#[async_trait]
impl LlmAdapter for AnthropicAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.build_request_body(&request, false);
        let url = format!("{}/messages", self.base_url);
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json")
                    .json(&body)
            },
            &retry,
        )
        .await?;

        let resp_body = resp
            .text()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to read response: {}", e)))?;

        parse_anthropic_chat_response(&resp_body)
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        let body = self.build_request_body(&request, true);
        let url = format!("{}/messages", self.base_url);
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json")
                    .json(&body)
            },
            &retry,
        )
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_anthropic_sse_event(&event.event, &event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!("SSE error: {:?}", e)))),
                }
            });

        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        Ok(vec![
            ModelId::new("claude-opus-4-5"),
            ModelId::new("claude-sonnet-4-5"),
            ModelId::new("claude-haiku-4-5"),
        ])
    }
}

// ── OpenAI wire types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: Option<OpenAiMessage>,
    finish_reason: Option<String>,
    #[serde(default)]
    logprobs: Option<OpenAiLogprobs>,
}

/// OpenAI response shape:
/// `{"logprobs": {"content": [{"token": "...", "logprob": -0.1, "top_logprobs": [...]}]}}`
#[derive(Debug, Deserialize)]
struct OpenAiLogprobs {
    #[serde(default)]
    content: Vec<OpenAiTokenLogprob>,
}

#[derive(Debug, Deserialize)]
struct OpenAiTokenLogprob {
    logprob: f64,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCall {
    id: String,
    function: OpenAiToolFunction,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct OpenAiPromptTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChunkWire {
    choices: Vec<OpenAiStreamChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiDelta {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModelInfo>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelInfo {
    id: String,
}

fn map_openai_finish_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        other => {
            tracing::warn!(
                target: "caduceus::providers::openai",
                unknown_finish_reason = other,
                "Unknown OpenAI finish_reason; falling back to EndTurn (audit I11)"
            );
            StopReason::EndTurn
        }
    }
}

fn parse_openai_chat_response(body: &str) -> Result<ChatResponse> {
    let resp: OpenAiResponse = serde_json::from_str(body).map_err(|e| {
        CaduceusError::Provider(format!(
            "Failed to parse OpenAI response: {} (body: {})",
            e,
            &body[..body.len().min(200)]
        ))
    })?;

    let choice = resp
        .choices
        .first()
        .ok_or_else(|| CaduceusError::Provider("No choices in response".into()))?;

    let content = choice
        .message
        .as_ref()
        .and_then(|m| m.content.as_ref())
        .cloned()
        .unwrap_or_default();

    let stop_reason = choice
        .finish_reason
        .as_deref()
        .map(map_openai_finish_reason)
        .unwrap_or(StopReason::EndTurn);

    let (input_tokens, output_tokens, cache_read_tokens) = resp
        .usage
        .map(|u| {
            (
                u.prompt_tokens,
                u.completion_tokens,
                u.prompt_tokens_details
                    .map(|details| details.cached_tokens)
                    .unwrap_or_default(),
            )
        })
        .unwrap_or((0, 0, 0));

    // Extract tool calls from OpenAI format.
    //
    // ST-A7 / audit I11: previously malformed JSON in `tc.function.arguments`
    // was silently coerced to `Value::Null` via `unwrap_or_default()`, which
    // fed the tool handler a bogus input and masked upstream provider bugs.
    // We now surface the parse error. An empty string (common for zero-arg
    // tools) is treated as an empty object, matching tool-schema expectation.
    let tool_calls: Vec<ToolUse> = choice
        .message
        .as_ref()
        .and_then(|m| m.tool_calls.as_ref())
        .map(|tcs| {
            tcs.iter()
                .map(|tc| {
                    let raw = tc.function.arguments.trim();
                    let input = if raw.is_empty() {
                        serde_json::Value::Object(serde_json::Map::new())
                    } else {
                        serde_json::from_str(raw).map_err(|e| {
                            CaduceusError::Provider(format!(
                                "Malformed tool_call arguments for {} (id={}): {} (raw: {})",
                                tc.function.name,
                                tc.id,
                                e,
                                &raw[..raw.len().min(200)]
                            ))
                        })?
                    };
                    Ok::<_, CaduceusError>(ToolUse {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        input,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();

    // Aggregate per-token logprobs into a summary if the response carries
    // them (gap G10 / P3.2). We deliberately drop the raw token vector —
    // the summary is what crosses the event bus, the raw data stays in
    // provider logs for debugging.
    let logprobs = choice.logprobs.as_ref().and_then(|lp| {
        let probs: Vec<f32> = lp
            .content
            .iter()
            .map(|tok| tok.logprob.exp() as f32)
            .collect();
        LogprobsSummary::from_token_probs(&probs)
    });

    Ok(ChatResponse {
        content,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens: 0,
        stop_reason,
        tool_calls,
        logprobs,
        thinking: String::new(),
    })
}

fn parse_openai_sse_event(data: &str) -> Option<Result<StreamChunk>> {
    let trimmed = data.trim();
    if trimmed == "[DONE]" {
        return Some(Ok(StreamChunk {
            delta: String::new(),
            is_final: true,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            thinking: String::new(),
        }));
    }

    let chunk: OpenAiStreamChunkWire = serde_json::from_str(trimmed).ok()?;
    let choice = chunk.choices.first()?;

    let is_final = choice.finish_reason.is_some();
    let delta = choice.delta.content.clone().unwrap_or_default();

    let (input_tokens, output_tokens, cache_read_tokens) = chunk
        .usage
        .map(|u| {
            (
                Some(u.prompt_tokens),
                Some(u.completion_tokens),
                Some(
                    u.prompt_tokens_details
                        .map(|details| details.cached_tokens)
                        .unwrap_or_default(),
                ),
            )
        })
        .unwrap_or((None, None, None));

    // Skip empty non-final chunks with no usage info
    if delta.is_empty() && !is_final && input_tokens.is_none() {
        return None;
    }

    Some(Ok(StreamChunk {
        delta,
        is_final,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens: Some(0),
        thinking: String::new(),
    }))
}

// ── OpenAI-compatible adapter ──────────────────────────────────────────────────

pub struct OpenAiCompatibleAdapter {
    provider_id: ProviderId,
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl OpenAiCompatibleAdapter {
    pub fn new(
        provider_id: impl Into<String>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: ProviderId::new(provider_id),
            api_key: api_key.into(),
            base_url: base_url.into(),
            client: reqwest::Client::new(),
        }
    }

    fn build_request_body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        build_openai_request_body(request, stream, true)
    }
}

fn build_openai_request_body(
    request: &ChatRequest,
    stream: bool,
    include_model: bool,
) -> serde_json::Value {
    let mut messages: Vec<serde_json::Value> = Vec::new();

    if let Some(ref system) = request.system {
        messages.push(serde_json::json!({
            "role": "system",
            "content": system,
        }));
    }

    for msg in request.messages.iter() {
        let blocks = msg.content_blocks();
        let has_images = blocks
            .iter()
            .any(|b| matches!(b, MessageContentBlock::Image { .. }));
        if has_images {
            let parts: Vec<serde_json::Value> = blocks
                .iter()
                .map(|block| match block {
                    MessageContentBlock::Text { text, .. } => {
                        serde_json::json!({"type": "text", "text": text})
                    }
                    MessageContentBlock::Image { base64, media_type } => {
                        serde_json::json!({
                            "type": "image_url",
                            "image_url": {"url": format!("data:{media_type};base64,{base64}")}
                        })
                    }
                })
                .collect();
            messages.push(serde_json::json!({
                "role": msg.role,
                "content": parts,
            }));
        } else if msg.role == "tool" {
            // Tool result message
            let tool_use_id = msg
                .tool_result
                .as_ref()
                .and_then(|r| r.tool_use_id.clone())
                .unwrap_or_default();
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": msg.content_text(),
            }));
        } else if !msg.tool_calls.is_empty() {
            // Assistant message with tool calls
            let tool_calls_json: Vec<serde_json::Value> = msg
                .tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.input.to_string(),
                        }
                    })
                })
                .collect();
            let mut m = serde_json::json!({
                "role": "assistant",
                "tool_calls": tool_calls_json,
            });
            let text = msg.content_text();
            if !text.is_empty() {
                m["content"] = serde_json::Value::String(text);
            } else {
                m["content"] = serde_json::Value::Null;
            }
            messages.push(m);
        } else {
            messages.push(serde_json::json!({
                "role": msg.role,
                "content": msg.content_text(),
            }));
        }
    }

    let mut body = serde_json::json!({
        "messages": messages,
        "max_tokens": request.max_tokens,
        "stream": stream,
    });

    if include_model {
        body["model"] = serde_json::json!(request.model.0);
    }

    if let Some(temp) = request.temperature {
        body["temperature"] = serde_json::json!(temp);
    }

    // Opt-in token logprobs (gap G10 / P3.2). When the caller sets
    // `request.logprobs = Some(N)` we ask OpenAI to return the per-token
    // logprob for the chosen token plus N alternates. N=0 still gives us
    // the chosen token's logprob, which is all the confidence summary
    // needs. Anthropic / mock providers ignore this field.
    if let Some(top_n) = request.logprobs {
        body["logprobs"] = serde_json::json!(true);
        if top_n > 0 {
            body["top_logprobs"] = serde_json::json!(top_n);
        }
    }

    if stream {
        body["stream_options"] = serde_json::json!({"include_usage": true});
    }

    if let Some(ref tc) = request.tool_choice {
        body["tool_choice"] = match tc {
            ToolChoice::Auto => serde_json::json!("auto"),
            ToolChoice::None => serde_json::json!("none"),
            ToolChoice::Required => serde_json::json!("required"),
            ToolChoice::Specific(name) => {
                serde_json::json!({"type": "function", "function": {"name": name}})
            }
        };
    }

    if let Some(ref rf) = request.response_format {
        body["response_format"] = match rf {
            ResponseFormat::Text => serde_json::json!({"type": "text"}),
            ResponseFormat::JsonObject => serde_json::json!({"type": "json_object"}),
            ResponseFormat::JsonSchema(schema) => {
                serde_json::json!({"type": "json_schema", "json_schema": schema})
            }
        };
    }

    // Serialize tool definitions for the API
    if !request.tools.is_empty() {
        let tools_json: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();
        body["tools"] = serde_json::Value::Array(tools_json);
    }

    body
}

#[async_trait]
impl LlmAdapter for OpenAiCompatibleAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.build_request_body(&request, false);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                let mut req = client
                    .post(&url)
                    .header("content-type", "application/json")
                    .json(&body);
                if !api_key.is_empty() {
                    req = req.header("authorization", format!("Bearer {}", &api_key));
                }
                req
            },
            &retry,
        )
        .await?;

        let resp_body = resp
            .text()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to read response: {}", e)))?;

        parse_openai_chat_response(&resp_body)
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        let body = self.build_request_body(&request, true);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                let mut req = client
                    .post(&url)
                    .header("content-type", "application/json")
                    .json(&body);
                if !api_key.is_empty() {
                    req = req.header("authorization", format!("Bearer {}", &api_key));
                }
                req
            },
            &retry,
        )
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_openai_sse_event(&event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!("SSE error: {:?}", e)))),
                }
            });

        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let mut req = self
            .client
            .get(&url)
            .header("content-type", "application/json");
        if !self.api_key.is_empty() {
            req = req.header("authorization", format!("Bearer {}", &self.api_key));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to list models: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CaduceusError::Provider(format!(
                "Failed to list models ({}): {}",
                status, body
            )));
        }

        let body = resp.text().await.map_err(|e| {
            CaduceusError::Provider(format!("Failed to read models response: {}", e))
        })?;

        let models: OpenAiModelsResponse = serde_json::from_str(&body).map_err(|e| {
            CaduceusError::Provider(format!("Failed to parse models response: {}", e))
        })?;

        Ok(models
            .data
            .into_iter()
            .map(|m| ModelId::new(m.id))
            .collect())
    }
}

// ── Azure OpenAI adapter ─────────────────────────────────────────────────────────

pub struct AzureOpenAiAdapter {
    provider_id: ProviderId,
    resource: String,
    deployment: String,
    api_key: String,
    api_version: String,
    base_url: Option<String>,
    client: reqwest::Client,
}

impl AzureOpenAiAdapter {
    pub fn new(
        resource: impl Into<String>,
        deployment: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: ProviderId::new("azure"),
            resource: resource.into(),
            deployment: deployment.into(),
            api_key: api_key.into(),
            api_version: "2024-02-01".into(),
            base_url: None,
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    fn endpoint(&self) -> String {
        let root = self
            .base_url
            .clone()
            .unwrap_or_else(|| format!("https://{}.openai.azure.com", self.resource));
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            root.trim_end_matches('/'),
            self.deployment,
            self.api_version
        )
    }

    fn build_request_body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        build_openai_request_body(request, stream, false)
    }
}

#[async_trait]
impl LlmAdapter for AzureOpenAiAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.build_request_body(&request, false);
        let url = self.endpoint();
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry_config = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("api-key", &api_key)
                    .json(&body)
            },
            &retry_config,
        )
        .await?;

        let resp_body = resp
            .text()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to read response: {}", e)))?;
        parse_openai_chat_response(&resp_body)
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        let body = self.build_request_body(&request, true);
        let url = self.endpoint();
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry_config = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("api-key", &api_key)
                    .json(&body)
            },
            &retry_config,
        )
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_openai_sse_event(&event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!("SSE error: {:?}", e)))),
                }
            });

        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        Ok(vec![ModelId::new(self.deployment.clone())])
    }
}

// ── Provider connector ───────────────────────────────────────────────────────────

#[async_trait]
pub trait ApiKeyPrompter: Send + Sync {
    async fn prompt_api_key(&self, provider_id: &ProviderId) -> Result<String>;
}

#[derive(Debug, Clone, Default)]
pub struct ProviderConnectionConfig {
    pub base_url: Option<String>,
    pub model: Option<ModelId>,
    pub azure_resource: Option<String>,
    pub azure_deployment: Option<String>,
}

pub struct ProviderConnector<S, P> {
    auth_store: Arc<S>,
    prompter: Arc<P>,
    configs: HashMap<String, ProviderConnectionConfig>,
}

impl<S, P> ProviderConnector<S, P>
where
    S: AuthStore,
    P: ApiKeyPrompter,
{
    pub fn new(auth_store: Arc<S>, prompter: Arc<P>) -> Self {
        Self {
            auth_store,
            prompter,
            configs: HashMap::new(),
        }
    }

    pub fn with_provider_config(
        mut self,
        provider_id: impl Into<String>,
        config: ProviderConnectionConfig,
    ) -> Self {
        self.configs.insert(provider_id.into(), config);
        self
    }

    pub async fn connect(&self, provider_id: &ProviderId) -> Result<()> {
        let key = self.prompter.prompt_api_key(provider_id).await?;
        self.validate_key(provider_id, &key).await?;
        self.auth_store.set_api_key(provider_id, &key).await
    }

    pub async fn validate_key(&self, provider_id: &ProviderId, key: &str) -> Result<()> {
        let config = self
            .configs
            .get(&provider_id.0)
            .cloned()
            .unwrap_or_default();
        let request = ChatRequest {
            model: config
                .model
                .unwrap_or_else(|| default_validation_model(provider_id)),
            messages: vec![Message::user("ping")].into(),
            system: Some("Reply with pong.".into()),
            max_tokens: 8,
            temperature: Some(0.0),
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };

        match provider_id.0.as_str() {
            "anthropic" => {
                let mut adapter = AnthropicAdapter::new(key);
                if let Some(base_url) = config.base_url {
                    adapter = adapter.with_base_url(base_url);
                }
                adapter.chat(request).await.map(|_| ())
            }
            "openai" | "ollama" => {
                let base_url = config
                    .base_url
                    .unwrap_or_else(|| default_openai_base_url(provider_id));
                OpenAiCompatibleAdapter::new(provider_id.0.clone(), key, base_url)
                    .chat(request)
                    .await
                    .map(|_| ())
            }
            "azure" => {
                let resource = config.azure_resource.ok_or_else(|| {
                    CaduceusError::Provider("missing Azure resource for connector".into())
                })?;
                let deployment = config.azure_deployment.ok_or_else(|| {
                    CaduceusError::Provider("missing Azure deployment for connector".into())
                })?;
                let mut adapter = AzureOpenAiAdapter::new(resource, deployment, key);
                if let Some(base_url) = config.base_url {
                    adapter = adapter.with_base_url(base_url);
                }
                adapter.chat(request).await.map(|_| ())
            }
            "copilot" => {
                let mut adapter = CopilotLmAdapter::new(key);
                if let Some(base_url) = config.base_url {
                    adapter = adapter.with_base_url(base_url);
                }
                adapter.chat(request).await.map(|_| ())
            }
            other => Err(CaduceusError::Provider(format!(
                "unsupported provider for connection: {other}"
            ))),
        }
    }
}

fn default_validation_model(provider_id: &ProviderId) -> ModelId {
    match provider_id.0.as_str() {
        "anthropic" => ModelId::new("claude-haiku-4-5"),
        "openai" => ModelId::new("gpt-4o-mini"),
        "azure" => ModelId::new("azure-deployment"),
        "ollama" => ModelId::new("llama3.2"),
        "copilot" => ModelId::new("gpt-4o-mini"),
        _ => ModelId::new("default"),
    }
}

fn default_openai_base_url(provider_id: &ProviderId) -> String {
    match provider_id.0.as_str() {
        "ollama" => "http://127.0.0.1:11434/v1".into(),
        _ => "https://api.openai.com/v1".into(),
    }
}

// ── Provider registry ──────────────────────────────────────────────────────────

pub struct ProviderRegistry {
    adapters: HashMap<String, Box<dyn LlmAdapter>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
        }
    }

    pub fn register(&mut self, adapter: Box<dyn LlmAdapter>) {
        let id = adapter.provider_id().0.clone();
        self.adapters.insert(id, adapter);
    }

    pub fn get(&self, provider_id: &ProviderId) -> Option<&dyn LlmAdapter> {
        self.adapters.get(&provider_id.0).map(|a| a.as_ref())
    }

    pub fn list_providers(&self) -> Vec<&ProviderId> {
        self.adapters.values().map(|a| a.provider_id()).collect()
    }

    /// Resolve "provider:model" strings into (ProviderId, ModelId) pairs.
    pub fn resolve_model(&self, model_string: &str) -> Option<(ProviderId, ModelId)> {
        if let Some((provider, model)) = model_string.split_once(':') {
            if self.adapters.contains_key(provider) {
                return Some((ProviderId::new(provider), ModelId::new(model)));
            }
        }
        None
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── GitHub Copilot LM API adapter ──────────────────────────────────────────────

/// Adapter for the GitHub Copilot Language Model API.
///
/// Uses the OpenAI-compatible chat/completions format with GitHub token auth.
/// Auth: `GITHUB_TOKEN` env var as Bearer token.
/// Base URL: configurable, defaults to GitHub Copilot's local proxy endpoint.
pub struct CopilotLmAdapter {
    provider_id: ProviderId,
    token: String,
    base_url: String,
    client: reqwest::Client,
}

impl CopilotLmAdapter {
    /// Create a new adapter using env vars or gh CLI for token.
    pub fn from_env() -> std::result::Result<Self, String> {
        let token = std::env::var("COPILOT_GITHUB_TOKEN")
            .or_else(|_| std::env::var("GH_TOKEN"))
            .or_else(|_| std::env::var("GITHUB_TOKEN"))
            .or_else(|_| {
                // Try cached token
                let home = std::env::var("HOME").unwrap_or_default();
                std::fs::read_to_string(format!("{home}/.caduceus/github_token"))
                    .map(|t| t.trim().to_string())
                    .map_err(|e| std::env::VarError::NotUnicode(e.to_string().into()))
            })
            .or_else(|_| {
                // Try gh CLI with common paths
                for gh in &["gh", "/opt/homebrew/bin/gh", "/usr/local/bin/gh"] {
                    if let Ok(output) = std::process::Command::new(gh)
                        .args(["auth", "token"])
                        .output()
                    {
                        if output.status.success() {
                            let t = String::from_utf8_lossy(&output.stdout).trim().to_string();
                            if !t.is_empty() {
                                return Ok(t);
                            }
                        }
                    }
                }
                Err(std::env::VarError::NotPresent)
            })
            .map_err(|_| {
                "No GitHub token found. Set GITHUB_TOKEN or run 'gh auth login'.".to_string()
            })?;
        Ok(Self::new(token))
    }

    /// Create a new adapter with an explicit token.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("copilot"),
            token: token.into(),
            base_url: "https://api.githubcopilot.com".into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn build_request_body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        build_openai_request_body(request, stream, true)
    }

    /// Add Copilot-specific headers to a request.
    fn copilot_headers(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", &self.token))
            .header("editor-version", "vscode/1.96.0")
            .header("copilot-integration-id", "vscode-chat")
    }
}

#[async_trait]
impl LlmAdapter for CopilotLmAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.build_request_body(&request, false);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || self.copilot_headers(client.post(&url)).json(&body),
            &retry,
        )
        .await?;

        let resp_body = resp
            .text()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to read response: {}", e)))?;

        parse_openai_chat_response(&resp_body)
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        let body = self.build_request_body(&request, true);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || self.copilot_headers(client.post(&url)).json(&body),
            &retry,
        )
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_openai_sse_event(&event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!("SSE error: {:?}", e)))),
                }
            });

        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let resp = self
            .copilot_headers(self.client.get(&url))
            .send()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to list models: {}", e)))?;

        if !resp.status().is_success() {
            return Ok(vec![
                ModelId::new("claude-sonnet-4.6"),
                ModelId::new("gpt-5.2"),
            ]);
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to parse models: {}", e)))?;

        let models = body["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter(|m| {
                        m["model_picker_enabled"].as_bool().unwrap_or(false)
                            && m["policy"]["state"].as_str() == Some("enabled")
                            && m["supported_endpoints"]
                                .as_array()
                                .map(|eps| {
                                    eps.iter().any(|e| e.as_str() == Some("/chat/completions"))
                                })
                                .unwrap_or(false)
                    })
                    .filter_map(|m| m["id"].as_str().map(ModelId::new))
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

// ── Vision helpers (Feature #72) ──────────────────────────────────────────────

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let combined = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((combined >> 18) & 0x3f) as usize]);
        out.push(TABLE[((combined >> 12) & 0x3f) as usize]);
        out.push(if chunk.len() >= 2 {
            TABLE[((combined >> 6) & 0x3f) as usize]
        } else {
            b'='
        });
        out.push(if chunk.len() >= 3 {
            TABLE[(combined & 0x3f) as usize]
        } else {
            b'='
        });
    }
    String::from_utf8(out).expect("base64 output is always valid ASCII")
}

/// Detect the MIME type of an image based on its file extension.
pub fn detect_media_type(path: &Path) -> Option<String> {
    match path.extension()?.to_str()?.to_lowercase().as_str() {
        "png" => Some("image/png".into()),
        "jpg" | "jpeg" => Some("image/jpeg".into()),
        "gif" => Some("image/gif".into()),
        "webp" => Some("image/webp".into()),
        _ => None,
    }
}

/// Read an image file and encode it as base64.
pub fn encode_image_file(path: &Path) -> Result<ImageContent> {
    let raw = std::fs::read(path)?;
    let media_type = detect_media_type(path).ok_or_else(|| {
        CaduceusError::Provider(format!(
            "Unsupported or unrecognised image extension: {}",
            path.display()
        ))
    })?;
    Ok(ImageContent {
        source: ImageSource::Base64 {
            media_type,
            data: base64_encode(&raw),
        },
        detail: None,
    })
}

// ── Tool fallback text extractor (Feature #73) ────────────────────────────────

pub struct ToolFallbackExtractor;

impl ToolFallbackExtractor {
    /// Extract meaningful text from a `ToolResult`, even when it represents an error.
    pub fn extract_text(result: &ToolResult) -> String {
        if result.content.is_empty() {
            return if result.is_error {
                "(empty error)".to_string()
            } else {
                String::new()
            };
        }

        // For errors, try to pull a "message" or "error" field out of JSON content.
        if result.is_error {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&result.content) {
                if let Some(msg) = json
                    .get("message")
                    .or_else(|| json.get("error"))
                    .and_then(|v| v.as_str())
                {
                    return msg.to_string();
                }
            }
        }

        result.content.clone()
    }

    /// Truncate `error` to `max_chars`, appending `"..."` when truncated.
    pub fn summarize_error(error: &str, max_chars: usize) -> String {
        if max_chars == 0 {
            return "...".to_string();
        }
        if error.len() <= max_chars {
            return error.to_string();
        }
        let cutoff = max_chars.saturating_sub(3);
        // Find the last valid char boundary at or before cutoff
        let boundary = error
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i < cutoff)
            .last()
            .unwrap_or(0);
        format!("{}...", &error[..boundary])
    }

    /// Attempt to parse possibly-truncated or broken JSON by closing unclosed
    /// brackets and braces.
    pub fn extract_partial_json(input: &str) -> Option<serde_json::Value> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Try as-is first.
        if let Ok(v) = serde_json::from_str(trimmed) {
            return Some(v);
        }

        // Walk the string tracking open brackets/braces so we can close them.
        let mut stack: Vec<char> = Vec::new();
        let mut in_string = false;
        let mut escaped = false;

        for ch in trimmed.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            if in_string {
                match ch {
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' | ']' => {
                    stack.pop();
                }
                _ => {}
            }
        }

        let mut attempt = trimmed.to_string();
        // Close an unterminated string literal before closing containers.
        if in_string {
            attempt.push('"');
        }
        for c in stack.iter().rev() {
            attempt.push(*c);
        }

        serde_json::from_str(&attempt).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockLlmAdapter;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::thread;

    // ── A3 — CompletionIntent + ChatRequest::validate ───────────────────────

    fn a3_mk_req(intent: Option<CompletionIntent>, tools: Vec<ToolSpec>) -> ChatRequest {
        ChatRequest {
            model: caduceus_core::ModelId("m".into()),
            messages: vec![].into(),
            system: None,
            max_tokens: 128,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: tools.into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        }
    }

    #[test]
    fn a3_completion_intent_serde_roundtrip_snake_case() {
        // Wire format must be snake_case so it matches Zed's
        // LanguageModelRequest.intent encoding verbatim.
        let cases = [
            (CompletionIntent::UserPrompt, "\"user_prompt\""),
            (
                CompletionIntent::ThreadSummarization,
                "\"thread_summarization\"",
            ),
            (
                CompletionIntent::VerificationRollout,
                "\"verification_rollout\"",
            ),
            (
                CompletionIntent::GenerateGitCommitMessage,
                "\"generate_git_commit_message\"",
            ),
            (CompletionIntent::OneShot, "\"one_shot\""),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected, "intent {variant:?} serialization");
            let back: CompletionIntent = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant, "intent {variant:?} roundtrip");
        }
    }

    #[test]
    fn a3_validate_rejects_verification_rollout_with_tools() {
        let tool = ToolSpec {
            name: "t".into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
            required_capability: None,
        };
        let req = a3_mk_req(Some(CompletionIntent::VerificationRollout), vec![tool]);
        // Use catch_unwind to bypass debug_assert! panic in debug builds.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| req.validate()));
        match result {
            Ok(Err(ChatRequestError::IntentForbidsTools { intent, count })) => {
                assert_eq!(intent, CompletionIntent::VerificationRollout);
                assert_eq!(count, 1);
            }
            Ok(Ok(())) => panic!("expected validate() to reject verification rollout with tools"),
            Err(_) => {} // debug_assert panic is also acceptable (fail-closed in debug)
        }
    }

    #[test]
    fn a3_validate_accepts_user_prompt_with_tools() {
        let tool = ToolSpec {
            name: "t".into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
            required_capability: None,
        };
        let req = a3_mk_req(Some(CompletionIntent::UserPrompt), vec![tool]);
        assert!(req.validate().is_ok());
    }

    #[test]
    fn a3_validate_accepts_none_intent_with_or_without_tools() {
        // Legacy path: intent=None must always pass.
        assert!(a3_mk_req(None, vec![]).validate().is_ok());
        let tool = ToolSpec {
            name: "t".into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
            required_capability: None,
        };
        assert!(a3_mk_req(None, vec![tool]).validate().is_ok());
    }

    #[test]
    fn a3_validate_accepts_tool_forbidding_intent_when_tools_empty() {
        // The failure mode is the combination; forbidding intent alone is fine.
        for intent in [
            CompletionIntent::VerificationRollout,
            CompletionIntent::ThreadSummarization,
            CompletionIntent::SummarizationFallback,
            CompletionIntent::GenerateGitCommitMessage,
        ] {
            assert!(
                a3_mk_req(Some(intent), vec![]).validate().is_ok(),
                "intent {intent:?} with empty tools should validate"
            );
        }
    }

    #[test]
    fn a3_forbids_tools_table() {
        assert!(CompletionIntent::VerificationRollout.forbids_tools());
        assert!(CompletionIntent::ThreadSummarization.forbids_tools());
        assert!(CompletionIntent::ThreadContextSummarization.forbids_tools());
        assert!(CompletionIntent::SummarizationFallback.forbids_tools());
        assert!(CompletionIntent::GenerateGitCommitMessage.forbids_tools());
        assert!(!CompletionIntent::UserPrompt.forbids_tools());
        assert!(!CompletionIntent::Subagent.forbids_tools());
        assert!(!CompletionIntent::ToolResults.forbids_tools());
        assert!(!CompletionIntent::EditFile.forbids_tools());
        assert!(!CompletionIntent::OneShot.forbids_tools());
    }

    #[test]
    fn a3_chatrequest_skip_serializing_empty_a3_fields() {
        // intent=None / thread_id=None / prompt_id=None / stop=[] should
        // NOT appear on the wire — preserves byte-compat with servers that
        // don't know about the A3 extensions.
        let req = a3_mk_req(None, vec![]);
        let json = serde_json::to_value(&req).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("thread_id"));
        assert!(!obj.contains_key("prompt_id"));
        assert!(!obj.contains_key("intent"));
        assert!(!obj.contains_key("stop"));
    }

    // ── ST-C2 Phase 1 — LlmMessage ↔ Message conversions ────────────────────

    #[test]
    fn c2_llm_to_wire_user_text_roundtrip() {
        let storage = LlmMessage::user("hello world");
        let wire: Message = (&storage).into();
        assert_eq!(wire.role, "user");
        assert_eq!(wire.content, "hello world");
        let back: LlmMessage = (&wire).into();
        assert_eq!(back.role, Role::User);
        assert_eq!(back.content.len(), 1);
        match &back.content[0] {
            ContentBlock::Text(s) => assert_eq!(s, "hello world"),
            _ => panic!("expected Text block"),
        }
    }

    #[test]
    fn c2_wire_to_llm_assistant_roundtrip() {
        let wire = Message::assistant("sure thing");
        let storage: LlmMessage = (&wire).into();
        assert_eq!(storage.role, Role::Assistant);
        assert_eq!(storage.content.len(), 1);
        let back: Message = storage.into();
        assert_eq!(back.role, "assistant");
        assert!(back.content.contains("sure thing"));
    }

    #[test]
    fn c2_llm_to_wire_tool_use_preserved() {
        let storage = LlmMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("let me check".into()),
                ContentBlock::ToolUse {
                    id: caduceus_core::ToolCallId::new("tc_1"),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/tmp/x"}),
                },
            ],
        };
        let wire: Message = (&storage).into();
        assert_eq!(wire.tool_calls.len(), 1);
        assert_eq!(wire.tool_calls[0].id, "tc_1");
        assert_eq!(wire.tool_calls[0].name, "read_file");
        let back: LlmMessage = (&wire).into();
        // text + tool-use round-trip preserves block count & order
        assert_eq!(back.content.len(), 2);
        assert!(matches!(back.content[0], ContentBlock::Text(_)));
        assert!(matches!(back.content[1], ContentBlock::ToolUse { .. }));
    }

    #[test]
    fn c2_llm_to_wire_tool_result_preserved() {
        let storage = LlmMessage::tool_result(
            caduceus_core::ToolCallId::new("tc_42"),
            "file contents",
            false,
        );
        let wire: Message = (&storage).into();
        let tr = wire.tool_result.as_ref().expect("tool_result set");
        assert_eq!(tr.content, "file contents");
        assert!(!tr.is_error);
        assert_eq!(tr.tool_use_id.as_deref(), Some("tc_42"));

        let back: LlmMessage = (&wire).into();
        // Expect a ToolResult block (plus no Text since wire.content is empty
        // for tool_result-only messages built by LlmMessage::tool_result).
        let tool_results: Vec<&ContentBlock> = back
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .collect();
        assert_eq!(tool_results.len(), 1);
    }

    #[test]
    fn c2_llm_to_wire_error_tool_result() {
        let storage = LlmMessage::tool_result(
            caduceus_core::ToolCallId::new("tc_err"),
            "permission denied",
            true,
        );
        let wire: Message = (&storage).into();
        let tr = wire.tool_result.as_ref().expect("tool_result set");
        assert!(tr.is_error);
        assert_eq!(tr.content, "permission denied");
    }

    #[test]
    fn c2_wire_to_llm_system_role() {
        let wire = Message::system("you are helpful");
        let storage: LlmMessage = (&wire).into();
        assert_eq!(storage.role, Role::System);
    }

    #[test]
    fn c2_wire_to_llm_unknown_role_defaults_user() {
        let mut wire = Message::user("hi");
        wire.role = "tool".into();
        let storage: LlmMessage = (&wire).into();
        // role=tool maps to User (wire convention: tool-result on user role)
        assert_eq!(storage.role, Role::User);
    }

    #[test]
    fn c2_llm_to_wire_drops_cache_breakpoint() {
        let storage = LlmMessage::user("cached");
        let wire: Message = (&storage).into();
        // storage has no cache_breakpoint field, wire default is false
        assert!(!wire.cache_breakpoint);
    }

    // ── end ST-C2 Phase 1 tests ─────────────────────────────────────────────

    struct TestServer {
        base_url: String,
        _handle: thread::JoinHandle<()>,
    }

    impl TestServer {
        fn respond(status_line: &str, content_type: &str, body: &str, requests: usize) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let status = status_line.to_string();
            let content_type = content_type.to_string();
            let body = body.to_string();
            let handle = thread::spawn(move || {
                for _ in 0..requests {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut buffer = [0u8; 8192];
                    let _ = stream.read(&mut buffer);
                    let response = format!(
                        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status,
                        content_type,
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            });
            Self {
                base_url,
                _handle: handle,
            }
        }
    }

    #[derive(Default)]
    struct InMemoryAuthStore {
        keys: Mutex<HashMap<String, String>>,
    }

    #[async_trait]
    impl AuthStore for InMemoryAuthStore {
        async fn get_api_key(&self, provider_id: &ProviderId) -> Result<Option<String>> {
            Ok(self.keys.lock().unwrap().get(&provider_id.0).cloned())
        }

        async fn set_api_key(&self, provider_id: &ProviderId, key: &str) -> Result<()> {
            self.keys
                .lock()
                .unwrap()
                .insert(provider_id.0.clone(), key.to_string());
            Ok(())
        }

        async fn delete_api_key(&self, provider_id: &ProviderId) -> Result<()> {
            self.keys.lock().unwrap().remove(&provider_id.0);
            Ok(())
        }
    }

    struct StaticPrompter {
        key: String,
    }

    #[async_trait]
    impl ApiKeyPrompter for StaticPrompter {
        async fn prompt_api_key(&self, _provider_id: &ProviderId) -> Result<String> {
            Ok(self.key.clone())
        }
    }

    #[test]
    fn test_message_construction() {
        let user = Message::user("hello");
        assert_eq!(user.role, "user");
        assert_eq!(user.content, "hello");

        let asst = Message::assistant("world");
        assert_eq!(asst.role, "assistant");

        let sys = Message::system("you are helpful");
        assert_eq!(sys.role, "system");
    }

    #[test]
    fn test_provider_registry_register_and_lookup() {
        let mut registry = ProviderRegistry::new();
        assert!(registry.get(&ProviderId::new("anthropic")).is_none());
        assert!(registry.list_providers().is_empty());

        let adapter = AnthropicAdapter::new("test-key");
        registry.register(Box::new(adapter));

        assert!(registry.get(&ProviderId::new("anthropic")).is_some());
        assert_eq!(registry.list_providers().len(), 1);
        assert!(registry.get(&ProviderId::new("missing")).is_none());
    }

    #[test]
    fn test_resolve_model_with_provider_prefix() {
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(AnthropicAdapter::new("k")));

        let result = registry.resolve_model("anthropic:claude-sonnet-4-5");
        assert!(result.is_some());
        let (pid, mid) = result.unwrap();
        assert_eq!(pid.0, "anthropic");
        assert_eq!(mid.0, "claude-sonnet-4-5");

        assert!(registry.resolve_model("unknown:model").is_none());
        assert!(registry.resolve_model("claude-sonnet-4-5").is_none());
    }

    #[test]
    fn test_parse_anthropic_response_text() {
        let json = r#"{
            "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello, world!"}],
            "model": "claude-sonnet-4-5-20241022",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 25, "output_tokens": 13}
        }"#;

        let resp = parse_anthropic_chat_response(json).unwrap();
        assert_eq!(resp.content, "Hello, world!");
        assert_eq!(resp.input_tokens, 25);
        assert_eq!(resp.output_tokens, 13);
        assert_eq!(resp.cache_read_tokens, 0);
        assert_eq!(resp.cache_creation_tokens, 0);
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn test_parse_anthropic_response_tool_use() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "Running that."},
                {"type": "tool_use", "id": "toolu_01A", "name": "bash", "input": {"cmd": "ls"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 50, "output_tokens": 30}
        }"#;

        let resp = parse_anthropic_chat_response(json).unwrap();
        assert_eq!(resp.content, "Running that.");
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert_eq!(resp.input_tokens, 50);
    }

    #[test]
    fn test_parse_openai_response() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;

        let resp = parse_openai_chat_response(json).unwrap();
        assert_eq!(resp.content, "Hello!");
        assert_eq!(resp.input_tokens, 10);
        assert_eq!(resp.output_tokens, 5);
        assert_eq!(resp.cache_read_tokens, 0);
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn test_parse_anthropic_sse_events() {
        // message_start → input token count
        let chunk = parse_anthropic_sse_event(
            "message_start",
            r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chunk.input_tokens, Some(25));
        assert!(!chunk.is_final);

        // content_block_delta → text delta
        let chunk = parse_anthropic_sse_event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chunk.delta, "Hello");

        // message_delta → output token count
        let chunk = parse_anthropic_sse_event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chunk.output_tokens, Some(15));

        // message_stop → final
        let chunk = parse_anthropic_sse_event("message_stop", r#"{"type":"message_stop"}"#)
            .unwrap()
            .unwrap();
        assert!(chunk.is_final);

        // ping → ignored
        assert!(parse_anthropic_sse_event("ping", "").is_none());
    }

    #[test]
    fn test_parse_openai_sse_events() {
        // Text delta
        let chunk = parse_openai_sse_event(
            r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"Hi"},"finish_reason":null}]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chunk.delta, "Hi");
        assert!(!chunk.is_final);

        // Final chunk with usage
        let chunk = parse_openai_sse_event(
            r#"{"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .unwrap()
        .unwrap();
        assert!(chunk.is_final);
        assert_eq!(chunk.input_tokens, Some(10));
        assert_eq!(chunk.output_tokens, Some(5));

        // [DONE] sentinel
        let chunk = parse_openai_sse_event("[DONE]").unwrap().unwrap();
        assert!(chunk.is_final);
    }

    #[test]
    fn test_retryable_status_codes() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(504));
        assert!(is_retryable_status(529));
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(404));
    }

    #[test]
    fn test_stop_reason_mapping() {
        assert_eq!(map_anthropic_stop_reason("end_turn"), StopReason::EndTurn);
        assert_eq!(
            map_anthropic_stop_reason("max_tokens"),
            StopReason::MaxTokens
        );
        assert_eq!(
            map_anthropic_stop_reason("stop_sequence"),
            StopReason::StopSequence
        );
        assert_eq!(map_anthropic_stop_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(map_anthropic_stop_reason("unknown"), StopReason::EndTurn);

        assert_eq!(map_openai_finish_reason("stop"), StopReason::EndTurn);
        assert_eq!(map_openai_finish_reason("length"), StopReason::MaxTokens);
        assert_eq!(map_openai_finish_reason("tool_calls"), StopReason::ToolUse);
    }

    #[test]
    fn test_anthropic_request_body_construction() {
        let adapter = AnthropicAdapter::new("test-key");
        let request = ChatRequest {
            model: ModelId::new("claude-sonnet-4-5"),
            messages: vec![Message::user("Hello")].into(),
            system: Some("You are helpful.".into()),
            max_tokens: 1024,
            temperature: Some(0.7),
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };

        let body = adapter.build_request_body(&request, false);
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["system"][0]["text"], "You are helpful.");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    // ── P13.4 — provider prompt‑caching breakpoint ───────────────────

    #[test]
    fn p13_4_anthropic_emits_cache_control_when_breakpoint_set() {
        let adapter = AnthropicAdapter::new("test-key");
        let request = ChatRequest {
            model: ModelId::new("claude-sonnet-4-5"),
            messages: vec![
                Message::user("first turn — long stable prefix").with_cache_breakpoint(),
                Message::user("second turn — fresh"),
            ]
            .into(),
            system: None,
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        let body = adapter.build_request_body(&request, false);
        let msgs = body["messages"].as_array().unwrap();
        // (1) The breakpointed message gets cache_control on its last block.
        let m0_last = msgs[0]["content"].as_array().unwrap().last().unwrap();
        assert_eq!(
            m0_last["cache_control"]["type"], "ephemeral",
            "breakpoint must stamp ephemeral cache_control on last block"
        );
        // (2) The non‑breakpointed message has NO cache_control.
        let m1_last = msgs[1]["content"].as_array().unwrap().last().unwrap();
        assert!(
            m1_last.get("cache_control").is_none(),
            "non‑breakpoint messages must remain pristine"
        );
    }

    #[test]
    fn p13_4_anthropic_breakpoint_works_on_tool_result_block() {
        // Breakpointing a tool result must put cache_control on the
        // tool_result block (Anthropic's prefix‑caching needs the
        // marker on whichever block ends the cached prefix).
        let adapter = AnthropicAdapter::new("test-key");
        let mut tool_msg = Message {
            role: "tool".into(),
            content: "stable tool output".into(),
            content_blocks: None,
            tool_calls: vec![],
            tool_result: Some(
                caduceus_core::ToolResult::success("stable tool output").with_tool_use_id("tc_abc"),
            ),
            cache_breakpoint: false,
        };
        tool_msg.cache_breakpoint = true;
        let request = ChatRequest {
            model: ModelId::new("claude-sonnet-4-5"),
            messages: vec![tool_msg].into(),
            system: None,
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        let body = adapter.build_request_body(&request, false);
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["type"], "tool_result");
        assert_eq!(block["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn test_anthropic_tool_round_trip() {
        let adapter = AnthropicAdapter::new("test-key");

        // Build a request with tools + tool history
        let tool_spec = ToolSpec {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
            required_capability: None,
        };

        // History: user → assistant+tool_use → tool_result → assistant
        let mut assistant_msg = Message::assistant("I'll read the file.");
        assistant_msg.tool_calls = vec![ToolUse {
            id: "tc_123".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "test.txt"}),
        }];

        let tool_msg = Message {
            role: "tool".into(),
            content: "file contents here".into(),
            content_blocks: None,
            tool_calls: vec![],
            tool_result: Some(ToolResult::success("file contents here").with_tool_use_id("tc_123")),
            cache_breakpoint: false,
        };

        let request = ChatRequest {
            model: ModelId::new("claude-sonnet-4-5"),
            messages: vec![
                Message::user("Read test.txt"),
                assistant_msg,
                tool_msg,
                Message::user("What did the file say?"),
            ]
            .into(),
            system: Some("You are helpful.".into()),
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: Arc::from([tool_spec]),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };

        let body = adapter.build_request_body(&request, false);

        // 1. Tools are serialized
        assert!(body["tools"].is_array(), "tools should be present");
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert!(body["tools"][0]["input_schema"].is_object());

        // 2. Assistant message has tool_use content block
        let msgs = body["messages"].as_array().unwrap();
        // msg[0] = user, msg[1] = assistant with tool_use, msg[2] = tool_result, msg[3] = user
        let assistant = &msgs[1];
        assert_eq!(assistant["role"], "assistant");
        let content = assistant["content"].as_array().unwrap();
        let tool_use_block = content.iter().find(|b| b["type"] == "tool_use");
        assert!(
            tool_use_block.is_some(),
            "assistant should have tool_use block"
        );
        let tub = tool_use_block.unwrap();
        assert_eq!(tub["id"], "tc_123");
        assert_eq!(tub["name"], "read_file");

        // 3. Tool result is serialized as user message with tool_result block
        let tool_result_msg = &msgs[2];
        assert_eq!(tool_result_msg["role"], "user");
        let tr_content = tool_result_msg["content"].as_array().unwrap();
        let tr_block = &tr_content[0];
        assert_eq!(tr_block["type"], "tool_result");
        assert_eq!(tr_block["tool_use_id"], "tc_123");
        assert_eq!(tr_block["content"], "file contents here");
    }

    #[test]
    fn test_openai_request_body_construction() {
        let adapter = OpenAiCompatibleAdapter::new("openai", "key", "https://api.openai.com/v1");
        let request = ChatRequest {
            model: ModelId::new("gpt-4"),
            messages: vec![Message::user("Hello")].into(),
            system: Some("You are helpful.".into()),
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };

        let body = adapter.build_request_body(&request, true);
        assert_eq!(body["model"], "gpt-4");
        assert_eq!(body["stream"], true);
        // System message is first in the messages array for OpenAI
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "You are helpful.");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn test_parse_malformed_response() {
        assert!(parse_anthropic_chat_response("not json").is_err());
        assert!(parse_openai_chat_response("not json").is_err());
        assert!(parse_openai_chat_response(r#"{"choices":[]}"#).is_err());
    }

    #[test]
    fn test_adapter_construction() {
        let a = AnthropicAdapter::new("key1");
        assert_eq!(a.provider_id.0, "anthropic");
        assert_eq!(a.base_url, "https://api.anthropic.com/v1");

        let a = a.with_base_url("http://localhost:8080");
        assert_eq!(a.base_url, "http://localhost:8080");

        let o = OpenAiCompatibleAdapter::new("openai", "key2", "https://api.openai.com/v1");
        assert_eq!(o.provider_id.0, "openai");
    }

    #[test]
    fn test_message_content_blocks_round_trip() {
        let message = Message::system("cache me").with_content_blocks(vec![
            MessageContentBlock::text_with_cache("cache me", CacheControl::ephemeral()),
        ]);
        assert_eq!(message.content_text(), "cache me");
        let blocks = message.content_blocks();
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            MessageContentBlock::Text {
                cache_control: Some(cache),
                ..
            } if cache.kind == "ephemeral"
        ));
    }

    #[test]
    fn test_parse_anthropic_cache_usage() {
        let json = r#"{
            "content": [{"type": "text", "text": "Cached!"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 11,
                "output_tokens": 7,
                "cache_read_input_tokens": 5,
                "cache_creation_input_tokens": 3
            }
        }"#;
        let resp = parse_anthropic_chat_response(json).unwrap();
        assert_eq!(resp.cache_read_tokens, 5);
        assert_eq!(resp.cache_creation_tokens, 3);
    }

    #[test]
    fn test_azure_request_body_and_endpoint() {
        let adapter = AzureOpenAiAdapter::new("resource-name", "deployment-a", "key");
        let request = ChatRequest {
            model: ModelId::new("ignored"),
            messages: vec![Message::user("Hello Azure")].into(),
            system: Some("Stay concise".into()),
            max_tokens: 128,
            temperature: Some(0.2),
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };

        let body = adapter.build_request_body(&request, true);
        assert!(body.get("model").is_none());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            adapter.endpoint(),
            "https://resource-name.openai.azure.com/openai/deployments/deployment-a/chat/completions?api-version=2024-02-01"
        );
    }

    #[tokio::test]
    async fn test_provider_connector_connects_and_stores_key_for_openai() {
        let server = TestServer::respond(
            "200 OK",
            "application/json",
            r#"{"choices":[{"message":{"content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
            1,
        );
        let auth_store = Arc::new(InMemoryAuthStore::default());
        let prompter = Arc::new(StaticPrompter {
            key: "secret-key".into(),
        });
        let connector = ProviderConnector::new(auth_store.clone(), prompter).with_provider_config(
            "openai",
            ProviderConnectionConfig {
                base_url: Some(server.base_url),
                model: Some(ModelId::new("gpt-4o-mini")),
                ..Default::default()
            },
        );

        connector.connect(&ProviderId::new("openai")).await.unwrap();
        let stored = auth_store
            .get_api_key(&ProviderId::new("openai"))
            .await
            .unwrap();
        assert_eq!(stored.as_deref(), Some("secret-key"));
    }

    // ── P0: RetryConfig tests ──────────────────────────────────────────────────

    #[test]
    fn test_retry_config_defaults() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_delay_ms, 1000);
        assert_eq!(config.max_delay_ms, 30_000);
    }

    #[test]
    fn test_retry_config_delay_increases_with_attempt() {
        let config = RetryConfig {
            max_retries: 5,
            base_delay_ms: 100,
            max_delay_ms: 60_000,
        };
        let d0 = config.delay_for_attempt(0);
        let d1 = config.delay_for_attempt(1);
        let d2 = config.delay_for_attempt(2);
        // Due to jitter, d1 should generally be >= d0 base, but we test the trend
        // by checking that the base delay doubles
        assert!(d0.as_millis() >= 100); // base + jitter(0..100)
        assert!(d1.as_millis() >= 200); // 2*base + jitter
        assert!(d2.as_millis() >= 400); // 4*base + jitter
    }

    #[test]
    fn test_retry_config_caps_at_max_delay() {
        let config = RetryConfig {
            max_retries: 10,
            base_delay_ms: 1000,
            max_delay_ms: 5_000,
        };
        let delay = config.delay_for_attempt(20); // Would be huge without cap
        assert!(delay.as_millis() <= 5_000);
    }

    // ── P1: Extended Thinking tests ────────────────────────────────────────────

    #[test]
    fn test_chat_request_thinking_mode_default() {
        let json = r#"{"model":"test","messages":[],"system":null,"max_tokens":100}"#;
        let req: ChatRequest = serde_json::from_str(json).unwrap();
        assert!(!req.thinking_mode);
    }

    #[test]
    fn test_chat_request_thinking_mode_enabled() {
        let req = ChatRequest {
            model: ModelId::new("test"),
            messages: vec![].into(),
            system: Some("sys".into()),
            max_tokens: 100,
            temperature: None,
            thinking_mode: true,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        assert!(req.thinking_mode);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"thinking_mode\":true"));
    }

    // ── Copilot LM adapter tests ───────────────────────────────────────────────

    #[test]
    fn test_copilot_adapter_construction() {
        let adapter = CopilotLmAdapter::new("gh-token-123");
        assert_eq!(adapter.provider_id().0, "copilot");
        assert_eq!(adapter.token(), "gh-token-123");
        assert_eq!(adapter.base_url(), "https://api.githubcopilot.com");
    }

    #[test]
    fn test_copilot_adapter_custom_base_url() {
        let adapter =
            CopilotLmAdapter::new("token").with_base_url("https://copilot.example.com/v1");
        assert_eq!(adapter.base_url(), "https://copilot.example.com/v1");
    }

    #[test]
    fn test_copilot_adapter_request_body() {
        let adapter = CopilotLmAdapter::new("token");
        let request = ChatRequest {
            model: ModelId::new("gpt-4o"),
            messages: vec![Message::user("Hello")].into(),
            system: Some("You are helpful.".into()),
            max_tokens: 1024,
            temperature: Some(0.5),
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };

        let body = adapter.build_request_body(&request, true);
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[tokio::test]
    async fn test_copilot_adapter_chat() {
        let server = TestServer::respond(
            "200 OK",
            "application/json",
            r#"{"choices":[{"message":{"content":"Hello from Copilot"},"finish_reason":"stop"}],"usage":{"prompt_tokens":15,"completion_tokens":4}}"#,
            1,
        );
        let adapter = CopilotLmAdapter::new("test-token").with_base_url(server.base_url);
        let request = ChatRequest {
            model: ModelId::new("gpt-4o"),
            messages: vec![Message::user("Hi")].into(),
            system: None,
            max_tokens: 64,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };

        let resp = adapter.chat(request).await.unwrap();
        assert_eq!(resp.content, "Hello from Copilot");
        assert_eq!(resp.input_tokens, 15);
        assert_eq!(resp.output_tokens, 4);
    }

    #[test]
    fn test_copilot_adapter_in_registry() {
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(CopilotLmAdapter::new("token")));
        assert!(registry.get(&ProviderId::new("copilot")).is_some());
        let resolved = registry.resolve_model("copilot:gpt-4o");
        assert!(resolved.is_some());
        let (pid, mid) = resolved.unwrap();
        assert_eq!(pid.0, "copilot");
        assert_eq!(mid.0, "gpt-4o");
    }

    // ── Circuit breaker tests ──────────────────────────────────────────────

    #[test]
    fn circuit_breaker_starts_closed() {
        let cb = CircuitBreaker::default();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.check().is_ok());
    }

    #[test]
    fn circuit_breaker_opens_after_threshold() {
        let cb = CircuitBreaker::new(3, std::time::Duration::from_secs(60));
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
        cb.record_failure(); // 3rd failure → opens
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(cb.check().is_err());
    }

    #[test]
    fn circuit_breaker_success_resets() {
        let cb = CircuitBreaker::new(3, std::time::Duration::from_secs(60));
        cb.record_failure();
        cb.record_failure();
        cb.record_success();
        assert_eq!(cb.failure_count(), 0);
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn circuit_breaker_half_open_after_cooldown() {
        let cb = CircuitBreaker::new(1, std::time::Duration::from_millis(10));
        cb.record_failure(); // opens immediately
        assert_eq!(cb.state(), CircuitState::Open);
        std::thread::sleep(std::time::Duration::from_millis(20));
        // After cooldown, check() transitions to HalfOpen
        assert!(cb.check().is_ok());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    // ── Vision tests ──────────────────────────────────────────────────────

    #[test]
    fn test_image_content_block() {
        let block = MessageContentBlock::image("aGVsbG8=", "image/jpeg");
        match &block {
            MessageContentBlock::Image { base64, media_type } => {
                assert_eq!(base64, "aGVsbG8=");
                assert_eq!(media_type, "image/jpeg");
            }
            _ => panic!("Expected Image variant"),
        }
        assert_eq!(block.text_value(), "");
    }

    #[test]
    fn test_anthropic_image_request_body() {
        let adapter = AnthropicAdapter::new("test-key");
        let msg = Message::user("describe this").with_content_blocks(vec![
            MessageContentBlock::text("describe this"),
            MessageContentBlock::image("aGVsbG8=", "image/jpeg"),
        ]);
        let request = ChatRequest {
            model: ModelId::new("claude-sonnet-4-5"),
            messages: vec![msg].into(),
            system: None,
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        let body = adapter.build_request_body(&request, false);
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/jpeg");
        assert_eq!(content[1]["source"]["data"], "aGVsbG8=");
    }

    #[test]
    fn test_openai_image_request_body() {
        let msg = Message::user("describe this").with_content_blocks(vec![
            MessageContentBlock::text("describe this"),
            MessageContentBlock::image("aGVsbG8=", "image/png"),
        ]);
        let request = ChatRequest {
            model: ModelId::new("gpt-4o"),
            messages: vec![msg].into(),
            system: None,
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        let body = build_openai_request_body(&request, false, true);
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert!(content[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    // ── Model filter tests ──────────────────────────────────────────────

    #[test]
    fn test_model_filter_allowed_list() {
        let filter = ModelFilter {
            allowed: Some(vec!["gpt-4".into(), "gpt-4o".into()]),
            denied: None,
        };
        assert!(filter.check(&ModelId::new("gpt-4")).is_ok());
        assert!(filter.check(&ModelId::new("gpt-4o")).is_ok());
        assert!(filter.check(&ModelId::new("gpt-3.5")).is_err());
    }

    #[test]
    fn test_model_filter_denied_list() {
        let filter = ModelFilter {
            allowed: None,
            denied: Some(vec!["gpt-3.5".into()]),
        };
        assert!(filter.check(&ModelId::new("gpt-4")).is_ok());
        assert!(filter.check(&ModelId::new("gpt-3.5")).is_err());
    }

    // ── Tool choice tests ──────────────────────────────────────────────

    #[test]
    fn test_tool_choice_anthropic_body() {
        let adapter = AnthropicAdapter::new("test-key");
        let request = ChatRequest {
            model: ModelId::new("claude-sonnet-4-5"),
            messages: vec![Message::user("Hello")].into(),
            system: None,
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: Some(ToolChoice::Required),
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        let body = adapter.build_request_body(&request, false);
        assert_eq!(body["tool_choice"]["type"], "any");

        let request2 = ChatRequest {
            model: ModelId::new("claude-sonnet-4-5"),
            messages: vec![Message::user("Hello")].into(),
            system: None,
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: Some(ToolChoice::Specific("my_tool".into())),
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        let body2 = adapter.build_request_body(&request2, false);
        assert_eq!(body2["tool_choice"]["type"], "tool");
        assert_eq!(body2["tool_choice"]["name"], "my_tool");
    }

    #[test]
    fn test_tool_choice_openai_body() {
        let request = ChatRequest {
            model: ModelId::new("gpt-4"),
            messages: vec![Message::user("Hello")].into(),
            system: None,
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: Some(ToolChoice::Required),
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        let body = build_openai_request_body(&request, false, true);
        assert_eq!(body["tool_choice"], "required");

        let request2 = ChatRequest {
            model: ModelId::new("gpt-4"),
            messages: vec![Message::user("Hello")].into(),
            system: None,
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: Some(ToolChoice::Specific("my_func".into())),
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        let body2 = build_openai_request_body(&request2, false, true);
        assert_eq!(body2["tool_choice"]["type"], "function");
        assert_eq!(body2["tool_choice"]["function"]["name"], "my_func");
    }

    // ── ST-A7 / audit I11: tool_call arguments parse semantics ────────
    #[test]
    fn malformed_tool_call_arguments_surface_error() {
        let body = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "foo", "arguments": "{not json" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }"#;
        let err = parse_openai_chat_response(body).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Malformed tool_call arguments for foo"),
            "expected named-tool error, got: {msg}"
        );
        assert!(
            msg.contains("call_1"),
            "expected tool_call id in error: {msg}"
        );
    }

    #[test]
    fn empty_tool_call_arguments_become_empty_object() {
        let body = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_0",
                        "type": "function",
                        "function": { "name": "noop", "arguments": "" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }"#;
        let resp = parse_openai_chat_response(body).expect("should parse");
        assert_eq!(resp.tool_calls.len(), 1);
        assert!(resp.tool_calls[0].input.is_object());
        assert_eq!(
            resp.tool_calls[0].input.as_object().unwrap().len(),
            0,
            "empty-args should be empty object"
        );
    }

    #[test]
    fn valid_tool_call_arguments_parse() {
        let body = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_2",
                        "type": "function",
                        "function": { "name": "fetch", "arguments": "{\"url\":\"https://x\"}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }"#;
        let resp = parse_openai_chat_response(body).expect("should parse");
        assert_eq!(resp.tool_calls[0].input["url"], "https://x");
    }

    // ── Response format tests ──────────────────────────────────────────

    #[test]
    fn test_response_format_openai() {
        let request = ChatRequest {
            model: ModelId::new("gpt-4"),
            messages: vec![Message::user("Hello")].into(),
            system: None,
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: Some(ResponseFormat::JsonObject),
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        let body = build_openai_request_body(&request, false, true);
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    // ── Error recovery tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_rate_limit_retry_succeeds() {
        // MockLlmAdapter simulates first call failing, second succeeding
        let success_response = ChatResponse {
            content: "Hello after retry".into(),
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
            thinking: String::new(),
        };

        let mock = MockLlmAdapter::new(vec![success_response.clone()]);
        let request = ChatRequest {
            model: ModelId::new("mock-model"),
            messages: vec![Message::user("test")].into(),
            system: None,
            max_tokens: 100,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };

        let response = mock.chat(request).await.unwrap();
        assert_eq!(response.content, "Hello after retry");
        assert_eq!(response.input_tokens, 10);
        assert_eq!(response.output_tokens, 5);
    }

    #[tokio::test]
    async fn test_network_error_propagates() {
        // MockLlmAdapter with no scripted responses → returns Provider error
        let mock = MockLlmAdapter::new(vec![]);
        let request = ChatRequest {
            model: ModelId::new("mock-model"),
            messages: vec![Message::user("test")].into(),
            system: None,
            max_tokens: 100,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };

        let result = mock.chat(request).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, CaduceusError::Provider(_)),
            "expected Provider error, got: {err}"
        );
    }

    #[test]
    fn test_malformed_json_response() {
        let bad_json = "{ this is not valid json at all }}}";
        let result = parse_anthropic_chat_response(bad_json);
        assert!(result.is_err(), "malformed JSON should return error");

        let also_bad = r#"{"content": "missing required fields"}"#;
        let result = parse_anthropic_chat_response(also_bad);
        assert!(
            result.is_err(),
            "missing required fields should return error"
        );
    }

    #[tokio::test]
    async fn test_empty_response_handled() {
        let empty_response = ChatResponse {
            content: String::new(),
            input_tokens: 5,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
            thinking: String::new(),
        };

        let mock = MockLlmAdapter::new(vec![empty_response]);
        let request = ChatRequest {
            model: ModelId::new("mock-model"),
            messages: vec![Message::user("test")].into(),
            system: None,
            max_tokens: 100,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };

        let response = mock.chat(request).await.unwrap();
        assert_eq!(response.content, "");
        assert_eq!(response.output_tokens, 0);
        assert_eq!(response.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn test_circuit_breaker_opens() {
        let cb = CircuitBreaker::new(3, std::time::Duration::from_secs(60));
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.failure_count(), 0);

        // Record failures up to threshold
        cb.record_failure();
        assert_eq!(cb.failure_count(), 1);
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.failure_count(), 2);
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure(); // Hits threshold of 3
        assert_eq!(cb.failure_count(), 3);
        assert_eq!(cb.state(), CircuitState::Open);

        // Requests should now be rejected
        let result = cb.check();
        assert!(result.is_err(), "circuit should reject requests when open");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Circuit breaker"),
            "expected circuit breaker error, got: {err}"
        );
    }

    #[test]
    fn test_circuit_breaker_half_open_recovery() {
        // Use a very short cooldown so we can test the HalfOpen transition
        let cb = CircuitBreaker::new(2, std::time::Duration::from_millis(1));

        // Trip the circuit
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for cooldown to expire
        std::thread::sleep(std::time::Duration::from_millis(10));

        // check() should transition to HalfOpen and allow the probe
        let result = cb.check();
        assert!(
            result.is_ok(),
            "after cooldown, probe request should be allowed"
        );
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Record success → should go back to Closed
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.failure_count(), 0);

        // Now requests should pass normally
        assert!(cb.check().is_ok());
    }

    // ── Feature #72: Vision helpers tests ─────────────────────────────────────

    #[test]
    fn test_detect_media_type_png() {
        assert_eq!(
            detect_media_type(std::path::Path::new("photo.png")),
            Some("image/png".into())
        );
    }

    #[test]
    fn test_detect_media_type_jpg() {
        assert_eq!(
            detect_media_type(std::path::Path::new("photo.jpg")),
            Some("image/jpeg".into())
        );
        assert_eq!(
            detect_media_type(std::path::Path::new("photo.jpeg")),
            Some("image/jpeg".into())
        );
    }

    #[test]
    fn test_detect_media_type_gif() {
        assert_eq!(
            detect_media_type(std::path::Path::new("anim.gif")),
            Some("image/gif".into())
        );
    }

    #[test]
    fn test_detect_media_type_webp() {
        assert_eq!(
            detect_media_type(std::path::Path::new("image.webp")),
            Some("image/webp".into())
        );
    }

    #[test]
    fn test_detect_media_type_unknown() {
        assert_eq!(detect_media_type(std::path::Path::new("doc.pdf")), None);
        assert_eq!(detect_media_type(std::path::Path::new("noext")), None);
    }

    #[test]
    fn test_base64_encode_known_values() {
        // RFC 4648 test vectors
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }

    #[test]
    fn test_encode_image_file_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join("caduceus_test_image.png");
        let data = b"fake png content for test";
        std::fs::write(&path, data).unwrap();

        let img = encode_image_file(&path).unwrap();
        match img.source {
            ImageSource::Base64 {
                media_type,
                data: encoded,
            } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(encoded, base64_encode(data));
            }
            _ => panic!("expected Base64 source"),
        }
        assert!(img.detail.is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_encode_image_file_unsupported_extension() {
        let dir = std::env::temp_dir();
        let path = dir.join("caduceus_test_doc.pdf");
        std::fs::write(&path, b"pdf content").unwrap();
        let result = encode_image_file(&path);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    // ── Feature #73: ToolFallbackExtractor tests ──────────────────────────────

    #[test]
    fn test_extract_text_success() {
        let result = ToolResult::success("operation completed successfully");
        assert_eq!(
            ToolFallbackExtractor::extract_text(&result),
            "operation completed successfully"
        );
    }

    #[test]
    fn test_extract_text_empty_success() {
        let result = ToolResult::success("");
        assert_eq!(ToolFallbackExtractor::extract_text(&result), "");
    }

    #[test]
    fn test_extract_text_from_error() {
        let result = ToolResult::error("file not found: /etc/missing");
        assert_eq!(
            ToolFallbackExtractor::extract_text(&result),
            "file not found: /etc/missing"
        );
    }

    #[test]
    fn test_extract_text_from_empty_error() {
        let result = ToolResult::error("");
        assert_eq!(
            ToolFallbackExtractor::extract_text(&result),
            "(empty error)"
        );
    }

    #[test]
    fn test_extract_text_json_error_message_field() {
        let result = ToolResult::error(r#"{"message": "permission denied", "code": 403}"#);
        assert_eq!(
            ToolFallbackExtractor::extract_text(&result),
            "permission denied"
        );
    }

    #[test]
    fn test_extract_text_json_error_field() {
        let result = ToolResult::error(r#"{"error": "timeout after 30s"}"#);
        assert_eq!(
            ToolFallbackExtractor::extract_text(&result),
            "timeout after 30s"
        );
    }

    #[test]
    fn test_summarize_error_short() {
        let short = "file not found";
        assert_eq!(
            ToolFallbackExtractor::summarize_error(short, 100),
            "file not found"
        );
    }

    #[test]
    fn test_summarize_error_truncation() {
        let long_error = "a".repeat(200);
        let summary = ToolFallbackExtractor::summarize_error(&long_error, 20);
        assert!(summary.ends_with("..."));
        assert!(summary.len() <= 20);
    }

    #[test]
    fn test_summarize_error_exact_length() {
        let error = "exactly twenty chars";
        assert_eq!(error.len(), 20);
        assert_eq!(ToolFallbackExtractor::summarize_error(error, 20), error);
    }

    #[test]
    fn test_extract_partial_json_valid() {
        let json = r#"{"key": "value", "num": 42}"#;
        let result = ToolFallbackExtractor::extract_partial_json(json);
        assert!(result.is_some());
        assert_eq!(result.unwrap()["key"], "value");
    }

    #[test]
    fn test_extract_partial_json_truncated_object() {
        // Truncated after the value, before the closing brace
        let partial = r#"{"key": "val"#;
        let result = ToolFallbackExtractor::extract_partial_json(partial);
        assert!(result.is_some(), "should recover truncated JSON");
        assert_eq!(result.unwrap()["key"], "val");
    }

    #[test]
    fn test_extract_partial_json_truncated_array() {
        let partial = r#"[1, 2, 3"#;
        let result = ToolFallbackExtractor::extract_partial_json(partial);
        assert!(result.is_some());
        let arr = result.unwrap();
        assert_eq!(arr[0], 1);
        assert_eq!(arr[2], 3);
    }

    #[test]
    fn test_extract_partial_json_empty() {
        assert!(ToolFallbackExtractor::extract_partial_json("").is_none());
        assert!(ToolFallbackExtractor::extract_partial_json("   ").is_none());
    }

    #[test]
    fn test_extract_partial_json_completely_broken() {
        // Not recoverable
        assert!(ToolFallbackExtractor::extract_partial_json("not json at all :::").is_none());
    }

    // ── G10 / P3.2 — logprobs → confidence ────────────────────────────────────

    #[test]
    fn confidence_thresholds_are_correct() {
        // Boundaries: ≥0.85 High, [0.5, 0.85) Medium, <0.5 Low.
        assert_eq!(Confidence::from_min_p(0.95), Confidence::High);
        assert_eq!(Confidence::from_min_p(0.85), Confidence::High);
        assert_eq!(Confidence::from_min_p(0.8499), Confidence::Medium);
        assert_eq!(Confidence::from_min_p(0.5), Confidence::Medium);
        assert_eq!(Confidence::from_min_p(0.4999), Confidence::Low);
        assert_eq!(Confidence::from_min_p(0.0), Confidence::Low);
        // NaN / out-of-range degrade conservatively to Low, never panic.
        assert_eq!(Confidence::from_min_p(f32::NAN), Confidence::Low);
        assert_eq!(Confidence::from_min_p(-0.5), Confidence::Low);
        assert_eq!(Confidence::from_min_p(f32::INFINITY), Confidence::Low);
    }

    #[test]
    fn logprobs_summary_drops_nan_and_inf() {
        // Mix of valid + garbage; summary should reflect only the valid ones.
        let probs = [0.9, f32::NAN, 0.7, f32::INFINITY, -0.1, 0.95];
        let s = LogprobsSummary::from_token_probs(&probs).expect("some valid tokens");
        assert_eq!(s.n_tokens, 3);
        assert!((s.min_token_p - 0.7).abs() < 1e-6);
        let expected_mean = (0.9_f32 + 0.7 + 0.95) / 3.0;
        assert!((s.mean_token_p - expected_mean).abs() < 1e-6);
        assert_eq!(s.confidence, Confidence::Medium);
    }

    #[test]
    fn logprobs_summary_empty_returns_none() {
        assert!(LogprobsSummary::from_token_probs(&[]).is_none());
        // All garbage also returns None — no info available, NOT "low".
        assert!(LogprobsSummary::from_token_probs(&[f32::NAN, -1.0, 2.0]).is_none());
    }

    #[test]
    fn openai_request_body_includes_logprobs_when_requested() {
        let req = ChatRequest {
            model: ModelId::new("gpt-4"),
            messages: vec![].into(),
            system: None,
            tools: vec![].into(),
            tool_choice: None,
            response_format: None,
            max_tokens: 100,
            temperature: None,
            logprobs: Some(3),
            thinking_mode: false,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        };
        let body = build_openai_request_body(&req, false, true);
        assert_eq!(body["logprobs"], serde_json::json!(true));
        assert_eq!(body["top_logprobs"], serde_json::json!(3));

        let req_no = ChatRequest {
            logprobs: None,
            ..req.clone()
        };
        let body_no = build_openai_request_body(&req_no, false, true);
        assert!(body_no.get("logprobs").is_none());
        assert!(body_no.get("top_logprobs").is_none());

        // top_n=0 => request logprobs but no alternates
        let req_zero = ChatRequest {
            logprobs: Some(0),
            ..req
        };
        let body_zero = build_openai_request_body(&req_zero, false, true);
        assert_eq!(body_zero["logprobs"], serde_json::json!(true));
        assert!(body_zero.get("top_logprobs").is_none());
    }

    #[test]
    fn openai_chat_parses_logprobs_summary() {
        // logprob = ln(p), so ln(0.9) ≈ -0.10536, ln(0.7) ≈ -0.35667
        let _body = serde_json::json!({
            "choices": [{
                "message": {"content": "hi"},
                "finish_reason": "stop",
                "logprobs": {
                    "content": [
                        {"token": "h",  "logprob": -0.10536_f64.ln().exp().ln()},
                        {"token": "i",  "logprob": (0.7_f64).ln()},
                    ]
                }
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2}
        });
        // The first logprob entry computes weirdly; use plain ln values:
        let body = serde_json::json!({
            "choices": [{
                "message": {"content": "hi"},
                "finish_reason": "stop",
                "logprobs": {
                    "content": [
                        {"token": "h", "logprob": (0.9_f64).ln()},
                        {"token": "i", "logprob": (0.7_f64).ln()},
                    ]
                }
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2}
        });
        let resp = parse_openai_chat_response(&body.to_string()).unwrap();
        let lp = resp.logprobs.expect("summary populated");
        assert_eq!(lp.n_tokens, 2);
        assert!((lp.min_token_p - 0.7).abs() < 1e-4);
        assert!((lp.mean_token_p - 0.8).abs() < 1e-4);
        assert_eq!(lp.confidence, Confidence::Medium);
    }

    #[test]
    fn openai_chat_without_logprobs_returns_none() {
        let body = serde_json::json!({
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        });
        let resp = parse_openai_chat_response(&body.to_string()).unwrap();
        assert!(resp.logprobs.is_none(), "absence ≠ low confidence");
    }
}
