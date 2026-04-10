use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionPhase {
    Idle,
    Running,
    AwaitingPermission,
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
        }
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

    pub fn tool_result(tool_call_id: ToolCallId, content: impl Into<String>, is_error: bool) -> Self {
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
}

// ── LLM Response ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    TextDelta { text: String },
    ToolCallStart { id: ToolCallId, name: String },
    ToolCallInput { id: ToolCallId, delta: String },
    ToolCallEnd { id: ToolCallId },
    ToolResultStart { id: ToolCallId, name: String },
    ToolResultEnd { id: ToolCallId, content: String, is_error: bool },
    PermissionRequest { id: String, capability: String, description: String },
    TurnComplete { stop_reason: StopReason, usage: TokenUsage },
    Error { message: String },
    SessionPhaseChanged { phase: SessionPhase },
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
        self.input_tokens + self.output_tokens
    }

    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    pub context_limit: u32,
    pub used_input: u32,
    pub used_output: u32,
    pub reserved_output: u32,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            context_limit: 200_000,
            used_input: 0,
            used_output: 0,
            reserved_output: 8_192,
        }
    }
}

impl TokenBudget {
    pub fn remaining(&self) -> u32 {
        self.context_limit
            .saturating_sub(self.used_input + self.used_output + self.reserved_output)
    }

    pub fn fill_fraction(&self) -> f64 {
        let used = self.used_input + self.used_output;
        if self.context_limit == 0 {
            return 0.0;
        }
        used as f64 / self.context_limit as f64
    }

    pub fn needs_compaction(&self) -> bool {
        self.fill_fraction() > 0.85
    }
}

// ── Tool types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub required_capability: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self { content: message.into(), is_error: true }
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

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
        let turn = TokenUsage { input_tokens: 100, output_tokens: 50, ..Default::default() };
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

    #[test]
    fn config_defaults_are_sane() {
        let config = CaduceusConfig::default();
        assert_eq!(config.default_provider.0, "anthropic");
        assert_eq!(config.max_context_tokens, 200_000);
        assert!(config.permissions.fs_read);
    }
}
