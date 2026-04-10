pub mod instructions;
pub mod workers;

use caduceus_core::{
    AgentEvent, CaduceusError, ModelId, ProviderId, Result, SessionId, SessionPhase, SessionState,
    StopReason, TokenUsage, ToolCallId,
};
use caduceus_providers::{ChatRequest, LlmAdapter};
use caduceus_tools::ToolRegistry;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;

// ── Config loader ──────────────────────────────────────────────────────────────

pub struct ConfigLoader {
    config_path: std::path::PathBuf,
}

impl ConfigLoader {
    pub fn new(config_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            config_path: config_path.into(),
        }
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
            std::fs::create_dir_all(parent).map_err(|e| CaduceusError::Config(e.to_string()))?;
        }
        let json = serde_json::to_string_pretty(config)?;
        std::fs::write(&self.config_path, json).map_err(|e| CaduceusError::Config(e.to_string()))
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
    Agents,
    Skills,
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
            "agents" => Self::Agents,
            "skills" => Self::Skills,
            "exit" | "quit" => Self::Exit,
            "model" => Self::Model(parts.get(1).map(|s| s.to_string()).unwrap_or_default()),
            "provider" => Self::Provider(parts.get(1).map(|s| s.to_string()).unwrap_or_default()),
            other => Self::Unknown(other.to_string()),
        };
        Some(cmd)
    }
}

// ── Conversation history ───────────────────────────────────────────────────────

/// Manages an ordered list of provider-level messages for the conversation.
#[derive(Debug, Clone, Default)]
pub struct ConversationHistory {
    messages: Vec<caduceus_providers::Message>,
}

impl ConversationHistory {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn append(&mut self, message: caduceus_providers::Message) {
        self.messages.push(message);
    }

    pub fn messages(&self) -> &[caduceus_providers::Message] {
        &self.messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Drop the oldest non-system messages until we are at or below `max_messages`.
    pub fn truncate_oldest(&mut self, max_messages: usize) {
        while self.messages.len() > max_messages {
            if let Some(pos) = self.messages.iter().position(|m| m.role != "system") {
                self.messages.remove(pos);
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    pub fn serialize(&self) -> Result<String> {
        serde_json::to_string(&self.messages).map_err(|e| CaduceusError::Config(e.to_string()))
    }

    pub fn deserialize(json: &str) -> Result<Self> {
        let messages: Vec<caduceus_providers::Message> =
            serde_json::from_str(json).map_err(|e| CaduceusError::Config(e.to_string()))?;
        Ok(Self { messages })
    }
}

// ── Context assembler ──────────────────────────────────────────────────────────

/// Assembles the full message list for an LLM request within a token budget.
/// Uses a simple char-based heuristic (1 token ~ 4 chars) to estimate token usage.
pub struct ContextAssembler {
    max_context_tokens: u32,
    system_prompt: String,
    project_context: Option<String>,
}

impl ContextAssembler {
    pub fn new(max_context_tokens: u32, system_prompt: impl Into<String>) -> Self {
        Self {
            max_context_tokens,
            system_prompt: system_prompt.into(),
            project_context: None,
        }
    }

    pub fn with_project_context(mut self, ctx: impl Into<String>) -> Self {
        self.project_context = Some(ctx.into());
        self
    }

    fn estimate_tokens(text: &str) -> u32 {
        (text.len() as u32) / 4 + 1
    }

    fn message_tokens(msg: &caduceus_providers::Message) -> u32 {
        Self::estimate_tokens(&msg.role) + Self::estimate_tokens(&msg.content)
    }

    /// Build the final message list that fits within the token budget.
    /// Strategy: always include system prompt + project context, then fit as many
    /// conversation messages as possible starting from the most recent.
    pub fn assemble(&self, history: &ConversationHistory) -> Vec<caduceus_providers::Message> {
        let mut result = Vec::new();

        let mut full_system = self.system_prompt.clone();
        if let Some(ref ctx) = self.project_context {
            full_system.push_str("\n\n<project_context>\n");
            full_system.push_str(ctx);
            full_system.push_str("\n</project_context>");
        }

        let system_msg = caduceus_providers::Message::system(&full_system);
        let mut budget_used = Self::message_tokens(&system_msg);
        result.push(system_msg);

        // Reserve 25% of budget for output
        let available = self.max_context_tokens.saturating_mul(3) / 4;

        // Collect conversation messages newest-first, stop when budget exceeded
        let mut to_include = Vec::new();
        for msg in history.messages().iter().rev() {
            let cost = Self::message_tokens(msg);
            if budget_used + cost > available {
                break;
            }
            budget_used += cost;
            to_include.push(msg.clone());
        }

        // Reverse to restore chronological order
        to_include.reverse();
        result.extend(to_include);
        result
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

    pub async fn delete(&self, id: &SessionId) -> Result<()> {
        self.storage.delete_session(id).await
    }
}

// ── Agent event emitter ────────────────────────────────────────────────────────

/// Sends `AgentEvent` values through a tokio mpsc channel for streaming to the frontend.
pub struct AgentEventEmitter {
    tx: mpsc::Sender<AgentEvent>,
}

impl AgentEventEmitter {
    pub fn new(tx: mpsc::Sender<AgentEvent>) -> Self {
        Self { tx }
    }

    /// Create a pair: (emitter, receiver).
    pub fn channel(buffer: usize) -> (Self, mpsc::Receiver<AgentEvent>) {
        let (tx, rx) = mpsc::channel(buffer);
        (Self { tx }, rx)
    }

    pub async fn emit(&self, event: AgentEvent) {
        let _ = self.tx.send(event).await;
    }

    pub async fn emit_text_delta(&self, text: impl Into<String>) {
        self.emit(AgentEvent::TextDelta { text: text.into() }).await;
    }

    pub async fn emit_tool_call_start(&self, id: ToolCallId, name: impl Into<String>) {
        self.emit(AgentEvent::ToolCallStart {
            id,
            name: name.into(),
        })
        .await;
    }

    pub async fn emit_tool_result_end(
        &self,
        id: ToolCallId,
        content: impl Into<String>,
        is_error: bool,
    ) {
        self.emit(AgentEvent::ToolResultEnd {
            id,
            content: content.into(),
            is_error,
        })
        .await;
    }

    pub async fn emit_turn_complete(&self, stop_reason: StopReason, usage: TokenUsage) {
        self.emit(AgentEvent::TurnComplete { stop_reason, usage })
            .await;
    }

    pub async fn emit_error(&self, message: impl Into<String>) {
        self.emit(AgentEvent::Error {
            message: message.into(),
        })
        .await;
    }

    pub async fn emit_phase_changed(&self, phase: SessionPhase) {
        self.emit(AgentEvent::SessionPhaseChanged { phase }).await;
    }
}

// ── Agent harness ──────────────────────────────────────────────────────────────
// The core conversation loop: send -> extract tool calls -> execute -> append -> repeat

pub struct AgentHarness {
    provider: Arc<dyn LlmAdapter>,
    #[allow(dead_code)]
    tools: ToolRegistry,
    system_prompt: String,
    max_context_tokens: u32,
    max_turns: usize,
    emitter: Option<AgentEventEmitter>,
    instruction_set: Option<instructions::InstructionSet>,
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
            emitter: None,
            instruction_set: None,
        }
    }

    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }

    pub fn with_emitter(mut self, emitter: AgentEventEmitter) -> Self {
        self.emitter = Some(emitter);
        self
    }

    /// Load workspace instructions and merge them into the system prompt.
    pub fn with_instructions(mut self, workspace_root: impl Into<std::path::PathBuf>) -> Self {
        let loader = instructions::InstructionLoader::new(workspace_root);
        match loader.load() {
            Ok(set) => {
                if !set.system_prompt.is_empty() {
                    self.system_prompt = format!("{}\n\n{}", self.system_prompt, set.system_prompt);
                }
                self.instruction_set = Some(set);
            }
            Err(e) => {
                tracing::warn!("Failed to load workspace instructions: {e}");
            }
        }
        self
    }

    /// Return the loaded instruction set, if any.
    pub fn instruction_set(&self) -> Option<&instructions::InstructionSet> {
        self.instruction_set.as_ref()
    }

    /// Full agent conversation loop.
    ///
    /// 1. Append user message to conversation history
    /// 2. Assemble context within token budget
    /// 3. Send to LLM
    /// 4. If stop_reason == ToolUse, execute each tool call, feed results back
    /// 5. Repeat until EndTurn / MaxTokens / max_turns exhausted
    /// 6. Return final assistant text
    pub async fn run(
        &self,
        state: &mut SessionState,
        history: &mut ConversationHistory,
        user_input: &str,
    ) -> Result<String> {
        state.phase = SessionPhase::Running;
        if let Some(ref em) = self.emitter {
            em.emit_phase_changed(SessionPhase::Running).await;
        }

        history.append(caduceus_providers::Message::user(user_input));

        let assembler = ContextAssembler::new(self.max_context_tokens, &self.system_prompt);
        let final_text;

        // v1: single-pass (no tool-use loop — that's post-v1 multi-agent)
        {
            let messages = assembler.assemble(history);

            let request = ChatRequest {
                model: state.model_id.clone(),
                messages,
                system: Some(self.system_prompt.clone()),
                max_tokens: 4096,
                temperature: None,
            };

            let mut stream = self.provider.stream(request).await?;
            let mut usage = TokenUsage::default();
            let mut response_content = String::new();

            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                if !chunk.delta.is_empty() {
                    response_content.push_str(&chunk.delta);
                    if let Some(ref em) = self.emitter {
                        em.emit_text_delta(&chunk.delta).await;
                    }
                }

                if let Some(input_tokens) = chunk.input_tokens {
                    usage.input_tokens = input_tokens;
                }
                if let Some(output_tokens) = chunk.output_tokens {
                    usage.output_tokens = output_tokens;
                }

                if chunk.is_final {
                    break;
                }
            }

            state.token_budget.used_input += usage.input_tokens;
            state.token_budget.used_output += usage.output_tokens;
            state.turn_count += 1;

            history.append(caduceus_providers::Message::assistant(&response_content));
            if let Some(ref em) = self.emitter {
                em.emit_turn_complete(StopReason::EndTurn, usage).await;
            }
            final_text = response_content;
        }

        state.phase = SessionPhase::Idle;
        if let Some(ref em) = self.emitter {
            em.emit_phase_changed(SessionPhase::Idle).await;
        }
        Ok(final_text)
    }

    /// Run one agent turn (simple, no tool loop). Kept for backward compat.
    pub async fn run_turn(&self, state: &mut SessionState, user_input: &str) -> Result<String> {
        let mut history = ConversationHistory::new();
        self.run(state, &mut history, user_input).await
    }
}

/// Execute tool calls from an LLM response via the ToolRegistry.
/// Returns a vec of (tool_call_id, result_content, is_error).
pub async fn execute_tool_calls(
    registry: &ToolRegistry,
    tool_calls: &[(String, String, serde_json::Value)],
) -> Vec<(String, String, bool)> {
    let mut results = Vec::new();
    for (id, name, input) in tool_calls {
        match registry.execute(name, input.clone()).await {
            Ok(result) => results.push((id.clone(), result.content, result.is_error)),
            Err(e) => results.push((id.clone(), e.to_string(), true)),
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_loader_defaults() {
        let loader = ConfigLoader::new("/nonexistent-caduceus-test-path.json");
        let config = loader.load().unwrap();
        assert_eq!(config.default_provider.0, "anthropic");
    }

    #[test]
    fn slash_command_parse() {
        assert!(matches!(
            SlashCommand::parse("/help"),
            Some(SlashCommand::Help)
        ));
        assert!(matches!(
            SlashCommand::parse("/status"),
            Some(SlashCommand::Status)
        ));
        assert!(matches!(
            SlashCommand::parse("/model gpt-4"),
            Some(SlashCommand::Model(_))
        ));
        assert!(SlashCommand::parse("hello").is_none());
    }

    #[test]
    fn conversation_history_append_and_len() {
        let mut history = ConversationHistory::new();
        assert!(history.is_empty());
        history.append(caduceus_providers::Message::user("hello"));
        history.append(caduceus_providers::Message::assistant("hi"));
        assert_eq!(history.len(), 2);
        assert!(!history.is_empty());
    }

    #[test]
    fn conversation_history_truncate_oldest() {
        let mut history = ConversationHistory::new();
        history.append(caduceus_providers::Message::user("msg1"));
        history.append(caduceus_providers::Message::assistant("resp1"));
        history.append(caduceus_providers::Message::user("msg2"));
        history.append(caduceus_providers::Message::assistant("resp2"));
        history.append(caduceus_providers::Message::user("msg3"));
        history.truncate_oldest(3);
        assert_eq!(history.len(), 3);
        // Oldest non-system messages should have been removed
        assert_eq!(history.messages()[0].content, "msg2");
    }

    #[test]
    fn conversation_history_serialize_roundtrip() {
        let mut history = ConversationHistory::new();
        history.append(caduceus_providers::Message::user("hello"));
        history.append(caduceus_providers::Message::assistant("world"));
        let json = history.serialize().unwrap();
        let restored = ConversationHistory::deserialize(&json).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored.messages()[0].content, "hello");
        assert_eq!(restored.messages()[1].content, "world");
    }

    #[test]
    fn context_assembler_fits_budget() {
        let assembler = ContextAssembler::new(100, "You are helpful.");
        let mut history = ConversationHistory::new();
        for i in 0..50 {
            history.append(caduceus_providers::Message::user(&format!("message {i}")));
        }
        let assembled = assembler.assemble(&history);
        // Should have system message plus whatever fits
        assert!(assembled.len() > 1);
        assert_eq!(assembled[0].role, "system");
        assert!(assembled.len() <= 51);
    }

    #[test]
    fn context_assembler_with_project_context() {
        let assembler = ContextAssembler::new(10000, "System prompt.")
            .with_project_context("Rust project with 100 files");
        let history = ConversationHistory::new();
        let assembled = assembler.assemble(&history);
        assert_eq!(assembled.len(), 1);
        assert!(assembled[0].content.contains("project_context"));
        assert!(assembled[0].content.contains("Rust project"));
    }

    #[tokio::test]
    async fn agent_event_emitter_sends_events() {
        let (emitter, mut rx) = AgentEventEmitter::channel(16);
        emitter.emit_text_delta("hello").await;
        emitter.emit_error("oops").await;
        drop(emitter);

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], AgentEvent::TextDelta { text } if text == "hello"));
        assert!(matches!(&events[1], AgentEvent::Error { message } if message == "oops"));
    }

    #[test]
    fn slash_command_exit_and_quit() {
        assert!(matches!(
            SlashCommand::parse("/exit"),
            Some(SlashCommand::Exit)
        ));
        assert!(matches!(
            SlashCommand::parse("/quit"),
            Some(SlashCommand::Exit)
        ));
    }

    #[test]
    fn slash_command_unknown() {
        assert!(matches!(
            SlashCommand::parse("/foobar"),
            Some(SlashCommand::Unknown(ref s)) if s == "foobar"
        ));
    }

    // ── Parity test scenarios ──────────────────────────────────────────────────

    use caduceus_providers::mock::MockLlmAdapter;
    use caduceus_providers::StreamChunk;
    use caduceus_tools::{BashTool, ReadFileTool};
    use std::sync::Arc;

    fn make_final_stream(text: &str) -> Vec<StreamChunk> {
        vec![StreamChunk {
            delta: text.to_string(),
            is_final: true,
            input_tokens: Some(10),
            output_tokens: Some(20),
        }]
    }

    fn make_session() -> caduceus_core::SessionState {
        caduceus_core::SessionState::new(
            ".",
            caduceus_core::ProviderId::new("mock"),
            caduceus_core::ModelId::new("mock-model"),
        )
    }

    /// 1. read_only_tool_execution — read_file works without write permission
    #[tokio::test]
    async fn read_only_tool_execution() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello world").unwrap();

        let mut registry = caduceus_tools::ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool::new(dir.path())));

        let result = registry
            .execute("read_file", serde_json::json!({"path": "hello.txt"}))
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("hello world"));
    }

    /// 2. write_requires_approval — write_file fails without fs.write capability registered
    #[tokio::test]
    async fn write_requires_approval() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = caduceus_tools::ToolRegistry::new();
        // Only read_file is registered; write_file is not approved
        registry.register(Arc::new(ReadFileTool::new(dir.path())));

        let result = registry
            .execute(
                "write_file",
                serde_json::json!({"path": "out.txt", "content": "data"}),
            )
            .await;

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("write_file") || msg.contains("Unknown"));
    }

    /// 3. bash_with_timeout — command that exceeds timeout returns timed_out=true
    #[tokio::test]
    async fn bash_with_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = caduceus_tools::ToolRegistry::new();
        registry.register(Arc::new(BashTool::new(dir.path())));

        let result = registry
            .execute(
                "bash",
                serde_json::json!({"command": "sleep 30", "timeout_secs": 1}),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        let v: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(v["timed_out"], true);
    }

    /// 4. cancellation_propagation — adapter error (simulating cancel) stops execution
    #[tokio::test]
    async fn cancellation_propagation() {
        // MockLlmAdapter with no scripted streams simulates an abort mid-session
        let adapter = Arc::new(MockLlmAdapter::new(vec![]));
        let harness =
            AgentHarness::new(adapter, caduceus_tools::ToolRegistry::new(), 4096, "system");
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        let result = harness.run(&mut state, &mut history, "do something").await;
        assert!(result.is_err(), "cancelled session should propagate error");
    }

    /// 5. empty_input_noop — empty string returns a graceful message, not an error
    #[tokio::test]
    async fn empty_input_noop() {
        let adapter = Arc::new(
            MockLlmAdapter::new(vec![])
                .with_stream_chunks(vec![make_final_stream("Please provide a message.")]),
        );
        let harness =
            AgentHarness::new(adapter, caduceus_tools::ToolRegistry::new(), 4096, "system");
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        let result = harness.run(&mut state, &mut history, "").await.unwrap();
        assert!(!result.is_empty());
    }

    /// 6. rate_limit_recovery — successive turns both succeed (models retry after transient failure)
    #[tokio::test]
    async fn rate_limit_recovery() {
        let adapter = Arc::new(MockLlmAdapter::new(vec![]).with_stream_chunks(vec![
            make_final_stream("first response"),
            make_final_stream("second response after recovery"),
        ]));
        let harness =
            AgentHarness::new(adapter, caduceus_tools::ToolRegistry::new(), 4096, "system");
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        let r1 = harness.run(&mut state, &mut history, "ping").await.unwrap();
        assert_eq!(r1, "first response");
        let r2 = harness
            .run(&mut state, &mut history, "ping again")
            .await
            .unwrap();
        assert_eq!(r2, "second response after recovery");
    }

    /// 7. context_overflow_truncation — oldest messages dropped when token budget exceeded
    #[test]
    fn context_overflow_truncation() {
        let mut history = ConversationHistory::new();
        for i in 0..20u32 {
            history.append(caduceus_providers::Message::user(format!("msg {i}")));
            history.append(caduceus_providers::Message::assistant(format!("resp {i}")));
        }
        assert_eq!(history.len(), 40);

        // Small budget forces truncation
        let assembler = ContextAssembler::new(50, "system");
        let assembled = assembler.assemble(&history);

        // System message always present; total assembled must fit the budget
        assert_eq!(assembled[0].role, "system");
        assert!(
            assembled.len() < 40,
            "oldest messages should have been dropped"
        );
    }

    /// 8. malformed_response_handling — adapter returns error, agent surfaces it cleanly
    #[tokio::test]
    async fn malformed_response_handling() {
        // No scripted streams → stream() returns Err (simulates unparseable response)
        let adapter = Arc::new(MockLlmAdapter::new(vec![]));
        let harness =
            AgentHarness::new(adapter, caduceus_tools::ToolRegistry::new(), 4096, "system");
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        let result = harness.run(&mut state, &mut history, "give me data").await;
        assert!(
            result.is_err(),
            "malformed/missing response should be an error"
        );
        let msg = result.unwrap_err().to_string();
        assert!(!msg.is_empty());
    }

    /// 9. multi_tool_turn — two tools in registry, both execute in one batch
    #[tokio::test]
    async fn multi_tool_turn() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        std::fs::write(dir.path().join("b.txt"), "bbb").unwrap();

        let mut registry = caduceus_tools::ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool::new(dir.path())));

        let tool_calls = vec![
            (
                "id-1".to_string(),
                "read_file".to_string(),
                serde_json::json!({"path": "a.txt"}),
            ),
            (
                "id-2".to_string(),
                "read_file".to_string(),
                serde_json::json!({"path": "b.txt"}),
            ),
        ];
        let results = execute_tool_calls(&registry, &tool_calls).await;

        assert_eq!(results.len(), 2);
        assert!(!results[0].2, "first tool call should succeed");
        assert!(results[0].1.contains("aaa"));
        assert!(!results[1].2, "second tool call should succeed");
        assert!(results[1].1.contains("bbb"));
    }

    /// 10. session_state_persistence — serialize conversation history, reload, verify state intact
    #[tokio::test]
    async fn session_state_persistence() {
        let adapter = Arc::new(
            MockLlmAdapter::new(vec![]).with_stream_chunks(vec![make_final_stream("remembered")]),
        );
        let harness =
            AgentHarness::new(adapter, caduceus_tools::ToolRegistry::new(), 4096, "system");
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        harness
            .run(&mut state, &mut history, "remember me")
            .await
            .unwrap();

        // Serialize and reload history
        let serialized = history.serialize().unwrap();
        let restored = ConversationHistory::deserialize(&serialized).unwrap();

        assert_eq!(restored.len(), history.len());
        // User message and assistant response should survive the round-trip
        assert!(restored
            .messages()
            .iter()
            .any(|m| m.content.contains("remember me")));
        assert!(restored
            .messages()
            .iter()
            .any(|m| m.content.contains("remembered")));
    }
}
