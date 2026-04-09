use serde::{Deserialize, Serialize};
use std::fmt;
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelId(pub String);

impl ModelId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BufferId(pub Uuid);

impl BufferId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for BufferId {
    fn default() -> Self {
        Self::new()
    }
}

// ── Session types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionPhase {
    Idle,
    Planning,
    Executing,
    AwaitingPermission,
    Summarizing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub id: SessionId,
    pub phase: SessionPhase,
    pub project_root: String,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub token_budget: TokenBudget,
    pub transcript: Vec<TranscriptEntry>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl SessionState {
    pub fn new(project_root: impl Into<String>, provider: ProviderId, model: ModelId) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: SessionId::new(),
            phase: SessionPhase::Idle,
            project_root: project_root.into(),
            provider_id: provider,
            model_id: model,
            token_budget: TokenBudget::default(),
            transcript: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

// ── Transcript ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub role: Role,
    pub content: String,
    pub tokens: Option<u32>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl TranscriptEntry {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tokens: None,
            timestamp: chrono::Utc::now(),
        }
    }
}

// ── Token budget ───────────────────────────────────────────────────────────────

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
}

// ── Config ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaduceusConfig {
    pub default_provider: ProviderId,
    pub default_model: ModelId,
    pub storage_path: String,
    pub log_level: String,
    pub max_context_tokens: u32,
}

impl Default for CaduceusConfig {
    fn default() -> Self {
        Self {
            default_provider: ProviderId::new("anthropic"),
            default_model: ModelId::new("claude-opus-4-5"),
            storage_path: "~/.caduceus/db.sqlite".into(),
            log_level: "info".into(),
            max_context_tokens: 200_000,
        }
    }
}

// ── Memory ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: Uuid,
    pub session_id: SessionId,
    pub content: String,
    pub embedding: Option<Vec<f32>>,
    pub tags: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl MemoryEntry {
    pub fn new(session_id: SessionId, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            content: content.into(),
            embedding: None,
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
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("Tool error: {0}")]
    Tool(String),
    #[error("Config error: {0}")]
    Config(String),
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
pub trait ConfigSource: Send + Sync {
    async fn load(&self) -> Result<CaduceusConfig>;
    async fn save(&self, config: &CaduceusConfig) -> Result<()>;
}

#[async_trait::async_trait]
pub trait SessionStorage: Send + Sync {
    async fn create_session(&self, state: &SessionState) -> Result<()>;
    async fn load_session(&self, id: &SessionId) -> Result<Option<SessionState>>;
    async fn update_session(&self, state: &SessionState) -> Result<()>;
    async fn list_sessions(&self, limit: usize) -> Result<Vec<SessionState>>;
    async fn delete_session(&self, id: &SessionId) -> Result<()>;
    async fn append_entry(&self, session_id: &SessionId, entry: &TranscriptEntry) -> Result<()>;
}

#[async_trait::async_trait]
pub trait AuthStore: Send + Sync {
    async fn get_api_key(&self, provider_id: &ProviderId) -> Result<Option<String>>;
    async fn set_api_key(&self, provider_id: &ProviderId, key: &str) -> Result<()>;
    async fn delete_api_key(&self, provider_id: &ProviderId) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let id = SessionId::new();
        assert_ne!(id.0, Uuid::nil());
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
}
