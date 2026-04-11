use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

pub mod keybindings;

pub use keybindings::{resolve_platform_shortcut, Keybinding, KeybindingConfig, KeybindingPreset};

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
    TextDelta {
        text: String,
    },
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
    PermissionRequest {
        id: String,
        capability: String,
        description: String,
    },
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
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            is_error: true,
        }
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

// ── Tests ──────────────────────────────────────────────────────────────────

// ── P0: Directory Conventions ──────────────────────────────────────────────────

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

    pub fn project_config_file(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".caduceus").join("config.toml")
    }

    /// Create all standard directories if they don't exist.
    pub fn ensure_dirs() -> std::io::Result<()> {
        std::fs::create_dir_all(Self::config_dir())?;
        std::fs::create_dir_all(Self::cache_dir())?;
        std::fs::create_dir_all(Self::logs_dir())?;
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

/// Thread-safe cancellation token wrapping an `Arc<AtomicBool>`.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signal cancellation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
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

    // ── P0: CaduceusPaths tests ────────────────────────────────────────────────

    #[test]
    fn caduceus_paths_structure() {
        let config_dir = CaduceusPaths::config_dir();
        assert!(config_dir.ends_with(".caduceus"));
        assert!(CaduceusPaths::config_file().ends_with("config.toml"));
        assert!(CaduceusPaths::db_file().ends_with("db.sqlite"));
        assert!(CaduceusPaths::cache_dir().ends_with("cache"));
        assert!(CaduceusPaths::logs_dir().ends_with("logs"));
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
}
