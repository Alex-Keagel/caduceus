use caduceus_core::{
    CaduceusError, ModelId, ProviderId, Result, SessionId, SessionPhase, SessionState,
};
use caduceus_providers::{ChatRequest, LlmAdapter, Message, StopReason as ProviderStopReason};
use caduceus_tools::ToolRegistry;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
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
    denied_capabilities: HashSet<String>,
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
            denied_capabilities: HashSet::new(),
        }
    }

    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }

    pub fn deny_capability(mut self, capability: impl Into<String>) -> Self {
        self.denied_capabilities.insert(capability.into());
        self
    }

    fn parse_tool_calls(content: &str) -> Result<Vec<ScriptedToolCall>> {
        let parsed = serde_json::from_str::<Value>(content)
            .map_err(|e| CaduceusError::Provider(format!("invalid tool_use payload: {e}")))?;

        let mut calls = Vec::new();
        match parsed {
            Value::Array(items) => {
                for item in items {
                    calls.push(serde_json::from_value::<ScriptedToolCall>(item).map_err(|e| {
                        CaduceusError::Provider(format!("invalid tool_use entry: {e}"))
                    })?);
                }
            }
            Value::Object(obj) => {
                if obj.contains_key("tool_calls") {
                    let wrapped =
                        serde_json::from_value::<ToolCallEnvelope>(Value::Object(obj)).map_err(
                            |e| CaduceusError::Provider(format!("invalid tool_calls payload: {e}")),
                        )?;
                    calls = wrapped.tool_calls;
                } else {
                    calls.push(
                        serde_json::from_value::<ScriptedToolCall>(Value::Object(obj)).map_err(
                            |e| CaduceusError::Provider(format!("invalid tool_use entry: {e}")),
                        )?,
                    );
                }
            }
            _ => {
                return Err(CaduceusError::Provider(
                    "tool_use payload must be an object or array".into(),
                ))
            }
        }

        if calls.is_empty() {
            return Err(CaduceusError::Provider(
                "tool_use stop reason returned no tool calls".into(),
            ));
        }

        Ok(calls)
    }

    fn ensure_capability_allowed(&self, tool_name: &str) -> Result<()> {
        if let Some(tool) = self.tools.get(tool_name) {
            if let Some(required) = tool.spec().required_capability {
                if self.denied_capabilities.contains(&required) {
                    return Err(CaduceusError::PermissionDenied {
                        capability: required,
                        tool: tool_name.to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Run one agent turn: send user message, loop tool calls until end_turn.
    /// Returns the final assistant text response.
    pub async fn run_turn(
        &self,
        state: &mut SessionState,
        user_input: &str,
    ) -> Result<String> {
        if user_input.trim().is_empty() {
            state.phase = SessionPhase::Idle;
            return Ok(String::new());
        }

        if self.max_context_tokens > 0 && state.token_budget.remaining() == 0 {
            return Err(CaduceusError::ContextOverflow {
                used: state.token_budget.used_input + state.token_budget.used_output,
                limit: self.max_context_tokens,
            });
        }

        state.phase = SessionPhase::Running;

        let mut messages = vec![Message::system(&self.system_prompt), Message::user(user_input)];
        let mut llm_calls = 0usize;

        loop {
            if llm_calls >= self.max_turns {
                state.phase = SessionPhase::Error;
                return Err(CaduceusError::Provider("max turns exceeded".into()));
            }

            let request = ChatRequest {
                model: state.model_id.clone(),
                messages: messages.clone(),
                system: Some(self.system_prompt.clone()),
                max_tokens: 4096,
                temperature: None,
            };

            let response = self.provider.chat(request).await?;
            llm_calls += 1;

            state.token_budget.used_input += response.input_tokens;
            state.token_budget.used_output += response.output_tokens;
            state.turn_count += 1;
            messages.push(Message::assistant(response.content.clone()));

            match response.stop_reason {
                ProviderStopReason::ToolUse => {
                    let calls = Self::parse_tool_calls(&response.content)?;
                    for call in calls {
                        self.ensure_capability_allowed(&call.name)?;
                        let result = self.tools.execute(&call.name, call.input.clone()).await?;
                        let tool_result_payload = json!({
                            "tool_call_id": call.id,
                            "name": call.name,
                            "content": result.content,
                            "is_error": result.is_error,
                        });
                        messages.push(Message::user(tool_result_payload.to_string()));
                    }
                }
                _ => {
                    state.phase = SessionPhase::Idle;
                    return Ok(response.content);
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ToolCallEnvelope {
    #[serde(default)]
    tool_calls: Vec<ScriptedToolCall>,
}

#[derive(Debug, Deserialize)]
struct ScriptedToolCall {
    id: String,
    name: String,
    input: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_providers::mock::MockLlmAdapter;
    use caduceus_providers::{ChatResponse, StopReason as ProviderStopReason, StreamChunk};
    use caduceus_scanner::ProjectScanner;
    use caduceus_storage::SqliteStorage;
    use caduceus_tools::default_registry_with_root;
    use futures::StreamExt;
    use tempfile::TempDir;
    use tokio::fs;

    #[tokio::test]
    async fn mock_adapter_returns_scripted_chat_and_stream_and_records_requests() {
        let adapter = Arc::new(
            MockLlmAdapter::new(vec![ChatResponse {
                content: "Hi".into(),
                input_tokens: 3,
                output_tokens: 1,
                stop_reason: ProviderStopReason::EndTurn,
            }])
            .with_stream_chunks(vec![vec![
                StreamChunk {
                    delta: "A".into(),
                    is_final: false,
                    input_tokens: Some(1),
                    output_tokens: None,
                },
                StreamChunk {
                    delta: "B".into(),
                    is_final: true,
                    input_tokens: None,
                    output_tokens: Some(2),
                },
            ]]),
        );

        let req = ChatRequest {
            model: ModelId::new("mock-model"),
            messages: vec![Message::user("hello")],
            system: None,
            max_tokens: 10,
            temperature: None,
        };
        let response = adapter.chat(req.clone()).await.unwrap();
        assert_eq!(response.content, "Hi");

        let mut stream = adapter.stream(req).await.unwrap();
        let chunks: Vec<_> = stream.by_ref().collect().await;
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].as_ref().unwrap().delta, "A");
        assert!(chunks[1].as_ref().unwrap().is_final);

        let requests = adapter.recorded_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].messages[0].content, "hello");
    }

    #[tokio::test]
    async fn simple_text_response() {
        let adapter = Arc::new(MockLlmAdapter::new(vec![ChatResponse {
            content: "Hi there!".into(),
            input_tokens: 10,
            output_tokens: 5,
            stop_reason: ProviderStopReason::EndTurn,
        }]));
        let root = TempDir::new().unwrap();
        let harness = AgentHarness::new(
            adapter,
            default_registry_with_root(root.path()),
            200_000,
            "You are helpful",
        );
        let mut session = SessionState::new(
            root.path(),
            ProviderId::new("mock"),
            ModelId::new("mock-model"),
        );
        let output = harness.run_turn(&mut session, "hello").await.unwrap();
        assert_eq!(output, "Hi there!");
        assert_eq!(session.turn_count, 1);
    }

    #[tokio::test]
    async fn tool_call_roundtrip() {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("hello.txt"), "from tool").await.unwrap();

        let adapter = Arc::new(MockLlmAdapter::new(vec![
            ChatResponse {
                content: r#"{"tool_calls":[{"id":"tool-1","name":"read_file","input":{"path":"hello.txt"}}]}"#.into(),
                input_tokens: 10,
                output_tokens: 4,
                stop_reason: ProviderStopReason::ToolUse,
            },
            ChatResponse {
                content: "Done reading.".into(),
                input_tokens: 8,
                output_tokens: 3,
                stop_reason: ProviderStopReason::EndTurn,
            },
        ]));

        let harness = AgentHarness::new(
            adapter.clone(),
            default_registry_with_root(root.path()),
            200_000,
            "system",
        );
        let mut session = SessionState::new(
            root.path(),
            ProviderId::new("mock"),
            ModelId::new("mock-model"),
        );

        let output = harness.run_turn(&mut session, "read file").await.unwrap();
        assert_eq!(output, "Done reading.");
        let requests = adapter.recorded_requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1]
            .messages
            .iter()
            .any(|m| m.content.contains("from tool")));
    }

    #[tokio::test]
    async fn permission_denial() {
        let root = TempDir::new().unwrap();
        let adapter = Arc::new(MockLlmAdapter::new(vec![ChatResponse {
            content: r#"{"tool_calls":[{"id":"tool-1","name":"write_file","input":{"path":"x.txt","content":"x"}}]}"#.into(),
            input_tokens: 5,
            output_tokens: 2,
            stop_reason: ProviderStopReason::ToolUse,
        }]));

        let harness = AgentHarness::new(
            adapter,
            default_registry_with_root(root.path()),
            200_000,
            "system",
        )
        .deny_capability("fs_write");

        let mut session = SessionState::new(
            root.path(),
            ProviderId::new("mock"),
            ModelId::new("mock-model"),
        );
        let err = harness.run_turn(&mut session, "write").await.err().unwrap();
        match err {
            CaduceusError::PermissionDenied { capability, tool } => {
                assert_eq!(capability, "fs_write");
                assert_eq!(tool, "write_file");
            }
            other => panic!("expected permission denied, got {other}"),
        }
    }

    #[tokio::test]
    async fn max_turns_exceeded() {
        let root = TempDir::new().unwrap();
        let always_tool = ChatResponse {
            content: r#"{"tool_calls":[{"id":"tool-1","name":"list_files","input":{"path":".","recursive":false}}]}"#
                .into(),
            input_tokens: 1,
            output_tokens: 1,
            stop_reason: ProviderStopReason::ToolUse,
        };
        let adapter = Arc::new(MockLlmAdapter::new(vec![
            always_tool.clone(),
            always_tool.clone(),
            always_tool,
        ]));
        let harness = AgentHarness::new(
            adapter,
            default_registry_with_root(root.path()),
            200_000,
            "system",
        )
        .with_max_turns(2);
        let mut session = SessionState::new(
            root.path(),
            ProviderId::new("mock"),
            ModelId::new("mock-model"),
        );
        let err = harness.run_turn(&mut session, "loop").await.err().unwrap();
        assert!(err.to_string().contains("max turns exceeded"));
    }

    #[tokio::test]
    async fn empty_input_handling() {
        let root = TempDir::new().unwrap();
        let adapter = Arc::new(MockLlmAdapter::new(vec![]));
        let harness = AgentHarness::new(
            adapter,
            default_registry_with_root(root.path()),
            200_000,
            "system",
        );
        let mut session = SessionState::new(
            root.path(),
            ProviderId::new("mock"),
            ModelId::new("mock-model"),
        );
        let output = harness.run_turn(&mut session, "   ").await.unwrap();
        assert_eq!(output, "");
        assert_eq!(session.turn_count, 0);
    }

    #[tokio::test]
    async fn session_persistence() {
        let dir = TempDir::new().unwrap();
        let storage = Arc::new(SqliteStorage::open(dir.path().join("sessions.sqlite")).unwrap());
        let manager = SessionManager::new(storage.clone());
        let created = manager
            .create(
                dir.path(),
                ProviderId::new("mock"),
                ModelId::new("mock-model"),
            )
            .await
            .unwrap();
        let loaded = manager.load(&created.id).await.unwrap().unwrap();
        assert_eq!(loaded.id.to_string(), created.id.to_string());
        assert_eq!(loaded.project_root, created.project_root);
        assert_eq!(loaded.model_id.0, "mock-model");
    }

    #[tokio::test]
    async fn config_load_save_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("caduceus-config.json");
        let loader = ConfigLoader::new(&path);
        let mut config = loader.load().unwrap();
        config.default_model = ModelId::new("mock-model");
        config.max_context_tokens = 123_456;
        loader.save(&config).unwrap();
        let reloaded = loader.load().unwrap();
        assert_eq!(reloaded.default_model.0, "mock-model");
        assert_eq!(reloaded.max_context_tokens, 123_456);
    }

    #[tokio::test]
    async fn token_budget_tracking() {
        let root = TempDir::new().unwrap();
        let adapter = Arc::new(MockLlmAdapter::new(vec![
            ChatResponse {
                content: "first".into(),
                input_tokens: 11,
                output_tokens: 7,
                stop_reason: ProviderStopReason::EndTurn,
            },
            ChatResponse {
                content: "second".into(),
                input_tokens: 5,
                output_tokens: 3,
                stop_reason: ProviderStopReason::EndTurn,
            },
        ]));

        let harness = AgentHarness::new(
            adapter,
            default_registry_with_root(root.path()),
            200_000,
            "system",
        );
        let mut session = SessionState::new(
            root.path(),
            ProviderId::new("mock"),
            ModelId::new("mock-model"),
        );
        harness.run_turn(&mut session, "one").await.unwrap();
        harness.run_turn(&mut session, "two").await.unwrap();
        assert_eq!(session.token_budget.used_input, 16);
        assert_eq!(session.token_budget.used_output, 10);
        assert_eq!(session.turn_count, 2);
    }

    #[tokio::test]
    async fn slash_command_parsing() {
        assert!(matches!(SlashCommand::parse("/help"), Some(SlashCommand::Help)));
        assert!(matches!(SlashCommand::parse("/clear"), Some(SlashCommand::Clear)));
        assert!(matches!(SlashCommand::parse("/model gpt-4"), Some(SlashCommand::Model(model)) if model == "gpt-4"));
        assert!(matches!(SlashCommand::parse("/provider mock"), Some(SlashCommand::Provider(provider)) if provider == "mock"));
        assert!(matches!(SlashCommand::parse("/status"), Some(SlashCommand::Status)));
        assert!(matches!(SlashCommand::parse("/compact"), Some(SlashCommand::Compact)));
        assert!(matches!(SlashCommand::parse("/exit"), Some(SlashCommand::Exit)));
        assert!(matches!(SlashCommand::parse("/quit"), Some(SlashCommand::Exit)));
        assert!(matches!(SlashCommand::parse("/wat"), Some(SlashCommand::Unknown(name)) if name == "wat"));
        assert!(SlashCommand::parse("hello").is_none());
    }

    #[tokio::test]
    async fn scanner_integration() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("src")).await.unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}")
            .await
            .unwrap();
        fs::write(dir.path().join("package.json"), r#"{"dependencies":{"react":"^18"}}"#)
            .await
            .unwrap();
        fs::write(dir.path().join("app.py"), "print('x')").await.unwrap();

        let scanner = ProjectScanner::new(dir.path(), 50_000);
        let ctx = scanner.scan().unwrap();
        assert!(ctx
            .languages
            .iter()
            .any(|l| l.name == "Rust" && l.file_count >= 1));
        assert!(ctx.languages.iter().any(|l| l.name == "Python"));
        assert!(ctx.frameworks.iter().any(|f| f.name == "React"));
        assert!(ctx.total_files >= 3);
    }

    #[tokio::test]
    async fn config_loader_defaults() {
        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::new(dir.path().join("nonexistent-caduceus.json"));
        let config = loader.load().unwrap();
        assert_eq!(config.default_provider.0, "anthropic");
    }

    #[tokio::test]
    async fn slash_command_parse() {
        assert!(matches!(SlashCommand::parse("/help"), Some(SlashCommand::Help)));
        assert!(matches!(SlashCommand::parse("/status"), Some(SlashCommand::Status)));
        assert!(matches!(SlashCommand::parse("/model gpt-4"), Some(SlashCommand::Model(_))));
        assert!(SlashCommand::parse("hello").is_none());
    }
}
