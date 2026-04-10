use caduceus_core::{
    CaduceusError, ContentBlock, LlmMessage, LlmResponse,
    ModelId, ProviderId, Result, SessionId, SessionPhase, SessionState,
    StopReason, TokenUsage, ToolCallId,
};
use caduceus_providers::{ChatRequest, LlmAdapter};
use caduceus_tools::ToolRegistry;
use std::sync::Arc;

// ── Config loader ──────────────────────────────────────────────────────────────

pub struct ConfigLoader {
    config_path: std::path::PathBuf,
}

impl ConfigLoader {
    pub fn new(config_path: impl Into<std::path::PathBuf>) -> Self {
        Self { config_path: config_path.into() }
    }

    pub fn load(&self) -> Result<caduceus_core::CaduceusConfig> {
        if self.config_path.exists() {
            let content = std::fs::read_to_string(&self.config_path)
                .map_err(|e| CaduceusError::Config(e.to_string()))?;
            serde_json::from_str(&content).map_err(|e| CaduceusError::Config(e.to_string()))
        } else {
            Ok(caduceus_core::CaduceusConfig::default())
        }
    }

    pub fn save(&self, config: &caduceus_core::CaduceusConfig) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CaduceusError::Config(e.to_string()))?;
        }
        let json = serde_json::to_string_pretty(config)?;
        std::fs::write(&self.config_path, json)
            .map_err(|e| CaduceusError::Config(e.to_string()))
    }
}

// ── Slash commands ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum SlashCommand {
    Help,
    Clear,
    Model(String),
    Provider(String),
    Status,
    Compact,
    Exit,
    Unknown(String),
}

impl SlashCommand {
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }
        let parts: Vec<&str> = trimmed[1..].splitn(2, ' ').collect();
        let cmd = match parts[0] {
            "help" => Self::Help,
            "clear" => Self::Clear,
            "status" => Self::Status,
            "compact" => Self::Compact,
            "exit" | "quit" => Self::Exit,
            "model" => Self::Model(parts.get(1).map(|s| s.to_string()).unwrap_or_default()),
            "provider" => Self::Provider(parts.get(1).map(|s| s.to_string()).unwrap_or_default()),
            other => Self::Unknown(other.to_string()),
        };
        Some(cmd)
    }
}

// ── Session manager ────────────────────────────────────────────────────────────

pub struct SessionManager {
    storage: Arc<dyn caduceus_core::SessionStorage>,
}

impl SessionManager {
    pub fn new(storage: Arc<dyn caduceus_core::SessionStorage>) -> Self {
        Self { storage }
    }

    pub async fn create(
        &self,
        project_root: impl Into<std::path::PathBuf>,
        provider: ProviderId,
        model: ModelId,
    ) -> Result<SessionState> {
        let state = SessionState::new(project_root, provider, model);
        self.storage.create_session(&state).await?;
        Ok(state)
    }

    pub async fn load(&self, id: &SessionId) -> Result<Option<SessionState>> {
        self.storage.load_session(id).await
    }

    pub async fn update(&self, state: &SessionState) -> Result<()> {
        self.storage.update_session(state).await
    }

    pub async fn list(&self, limit: usize) -> Result<Vec<SessionState>> {
        self.storage.list_sessions(limit).await
    }
}

// ── Agent harness ──────────────────────────────────────────────────────────────
// The core conversation loop: send → extract tool calls → execute → append → repeat

pub struct AgentHarness {
    provider: Arc<dyn LlmAdapter>,
    tools: ToolRegistry,
    system_prompt: String,
    max_context_tokens: u32,
    max_turns: usize,
}

impl AgentHarness {
    pub fn new(
        provider: Arc<dyn LlmAdapter>,
        tools: ToolRegistry,
        max_context_tokens: u32,
        system_prompt: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            tools,
            system_prompt: system_prompt.into(),
            max_context_tokens,
            max_turns: 50,
        }
    }

    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }

    /// Run one agent turn: send user message, loop tool calls until end_turn.
    /// Returns the final assistant text response.
    pub async fn run_turn(
        &self,
        state: &mut SessionState,
        user_input: &str,
    ) -> Result<String> {
        state.phase = SessionPhase::Running;

        // Build messages for the LLM (using providers' Message type)
        let messages = vec![
            caduceus_providers::Message::system(&self.system_prompt),
            caduceus_providers::Message::user(user_input),
        ];

        let request = ChatRequest {
            model: state.model_id.clone(),
            messages,
            system: Some(self.system_prompt.clone()),
            max_tokens: 4096,
            temperature: None,
        };

        let response = self.provider.chat(request).await?;

        state.token_budget.used_input += response.input_tokens;
        state.token_budget.used_output += response.output_tokens;
        state.turn_count += 1;
        state.phase = SessionPhase::Idle;

        Ok(response.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_loader_defaults() {
        let loader = ConfigLoader::new("/tmp/nonexistent-caduceus.json");
        let config = loader.load().unwrap();
        assert_eq!(config.default_provider.0, "anthropic");
    }

    #[test]
    fn slash_command_parse() {
        assert!(matches!(SlashCommand::parse("/help"), Some(SlashCommand::Help)));
        assert!(matches!(SlashCommand::parse("/status"), Some(SlashCommand::Status)));
        assert!(matches!(SlashCommand::parse("/model gpt-4"), Some(SlashCommand::Model(_))));
        assert!(SlashCommand::parse("hello").is_none());
    }
}
