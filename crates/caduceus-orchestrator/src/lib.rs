use caduceus_core::{CaduceusError, ModelId, ProviderId, Result, Role, SessionId, SessionState, TranscriptEntry};
use caduceus_providers::{ChatRequest, LlmAdapter, Message};
use caduceus_tools::ToolRegistry;
use serde::{Deserialize, Serialize};
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

// ── Context assembler ──────────────────────────────────────────────────────────

pub struct ContextAssembler {
    max_tokens: u32,
}

impl ContextAssembler {
    pub fn new(max_tokens: u32) -> Self {
        Self { max_tokens }
    }

    pub fn assemble_messages(&self, state: &SessionState) -> Vec<Message> {
        let mut messages = Vec::new();
        let mut token_estimate = 0u32;

        // Walk transcript in reverse to include most recent first, then reverse
        for entry in state.transcript.iter().rev() {
            let est = (entry.content.len() / 4) as u32;
            if token_estimate + est > self.max_tokens {
                break;
            }
            token_estimate += est;
            let role = match entry.role {
                Role::User => "user".to_string(),
                Role::Assistant => "assistant".to_string(),
                _ => continue,
            };
            messages.push(Message { role, content: entry.content.clone() });
        }

        messages.reverse();
        messages
    }
}

// ── Slash command ──────────────────────────────────────────────────────────────

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
            "provider" => {
                Self::Provider(parts.get(1).map(|s| s.to_string()).unwrap_or_default())
            }
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
        project_root: impl Into<String>,
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

pub struct AgentHarness {
    provider: Arc<dyn LlmAdapter>,
    tools: ToolRegistry,
    context_assembler: ContextAssembler,
    system_prompt: String,
    max_turns: usize,
}

impl AgentHarness {
    pub fn new(
        provider: Arc<dyn LlmAdapter>,
        tools: ToolRegistry,
        max_tokens: u32,
        system_prompt: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            tools,
            context_assembler: ContextAssembler::new(max_tokens),
            system_prompt: system_prompt.into(),
            max_turns: 50,
        }
    }

    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }

    pub async fn run_turn(&self, state: &mut SessionState, user_input: &str) -> Result<String> {
        // Handle slash commands
        if let Some(cmd) = SlashCommand::parse(user_input) {
            return self.handle_slash_command(cmd, state).await;
        }

        // Append user message
        state.transcript.push(TranscriptEntry::new(Role::User, user_input));
        state.phase = caduceus_core::SessionPhase::Executing;

        let messages = self.context_assembler.assemble_messages(state);

        let request = ChatRequest {
            model: state.model_id.clone(),
            messages,
            system: Some(self.system_prompt.clone()),
            max_tokens: 4096,
            temperature: None,
        };

        let response = self.provider.chat(request).await?;

        state.transcript.push(TranscriptEntry::new(
            Role::Assistant,
            response.content.clone(),
        ));
        state.phase = caduceus_core::SessionPhase::Idle;

        Ok(response.content)
    }

    async fn handle_slash_command(
        &self,
        cmd: SlashCommand,
        state: &mut SessionState,
    ) -> Result<String> {
        Ok(match cmd {
            SlashCommand::Help => "Available commands: /help /clear /status /model <id> /provider <id> /compact /exit".into(),
            SlashCommand::Status => format!(
                "Session: {} | Phase: {:?} | Messages: {}",
                state.id,
                state.phase,
                state.transcript.len()
            ),
            SlashCommand::Clear => {
                state.transcript.clear();
                "Transcript cleared.".into()
            }
            SlashCommand::Model(m) => {
                state.model_id = ModelId::new(m.clone());
                format!("Model set to {m}")
            }
            SlashCommand::Provider(p) => {
                state.provider_id = ProviderId::new(p.clone());
                format!("Provider set to {p}")
            }
            SlashCommand::Compact => "Compaction not yet implemented.".into(),
            SlashCommand::Exit => "Goodbye.".into(),
            SlashCommand::Unknown(s) => format!("Unknown command: /{s}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let loader = ConfigLoader::new("/tmp/caduceus.json");
        let config = loader.load().unwrap();
        assert_eq!(config.default_provider.0, "anthropic");
    }

    #[test]
    fn slash_command_parse() {
        assert!(matches!(SlashCommand::parse("/help"), Some(SlashCommand::Help)));
        assert!(matches!(SlashCommand::parse("/status"), Some(SlashCommand::Status)));
        assert!(SlashCommand::parse("hello").is_none());
    }
}
