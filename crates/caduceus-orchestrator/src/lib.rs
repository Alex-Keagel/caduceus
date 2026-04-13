pub mod automations;
pub mod background;
pub mod bugbot;
pub mod compaction;
pub mod context;
pub mod headless;
pub mod instructions;
pub mod kanban;
pub mod mentions;
pub mod modes;
pub mod workers;

pub use context::{AssembledContext, ContextSource};
pub use headless::{
    CompactOutputFilter, ReplAction, ReplMode, ReplState, SummaryCompressor, TypoSuggester,
};
pub use modes::{AgentPersona, PersonaRegistry};
pub use workers::{
    BusMessage, Complexity, ContextReference, DagTask, DagTaskStatus, DecomposedTask,
    JitContextLoader, MessageBus, MultiRepoWorkspace, NotificationChannel, NotificationRoute,
    NotificationRouter, NotificationSeverity, Plugin, PluginAgent, PluginCapability,
    PluginCapabilityManager, PluginCommand, PluginDefinedTool, PluginExtensions, PluginSkill,
    PluginSystem, PluginToolRegistry, RefType, RepoEntry, SchedulerStrategy, SharedMemory,
    SharedMemoryEntry, TaskDag, TaskDecomposer, TaskScheduler, TeamAgent, TeamOrchestrator,
};

use caduceus_core::{
    AgentEvent, CaduceusError, CancellationToken, ModelId, ProviderId, Result, SessionId,
    SessionPhase, SessionState, TokenUsage, ToolCallId, WarningLevel,
};
use caduceus_providers::{ChatRequest, ChatResponse, LlmAdapter, StopReason};
use caduceus_tools::ToolRegistry;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
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

// ── P1: Effort Levels ──────────────────────────────────────────────────────────

/// Controls the detail level of LLM interactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffortLevel {
    Min,
    Low,
    Medium,
    High,
    Max,
}

impl EffortLevel {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "min" | "minimum" => Some(Self::Min),
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "max" | "maximum" => Some(Self::Max),
            _ => None,
        }
    }

    /// System prompt detail level description.
    pub fn system_prompt_detail(&self) -> &'static str {
        match self {
            Self::Min => "Be extremely concise. One sentence max.",
            Self::Low => "Be brief. Short paragraphs only.",
            Self::Medium => "Provide balanced detail with examples when helpful.",
            Self::High => "Be thorough. Include examples, edge cases, and alternatives.",
            Self::Max => {
                "Be exhaustive. Cover every detail, edge case, alternative, and implication."
            }
        }
    }

    /// Suggested max_tokens for this effort level.
    pub fn max_tokens(&self) -> u32 {
        match self {
            Self::Min => 256,
            Self::Low => 1024,
            Self::Medium => 4096,
            Self::High => 8192,
            Self::Max => 16384,
        }
    }

    /// Suggested temperature for this effort level.
    pub fn temperature(&self) -> f32 {
        match self {
            Self::Min => 0.0,
            Self::Low => 0.2,
            Self::Medium => 0.5,
            Self::High => 0.7,
            Self::Max => 0.8,
        }
    }
}

// ── P1: Query Configuration ────────────────────────────────────────────────────

/// Per-query overrides for model parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryConfig {
    pub model: Option<ModelId>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl QueryConfig {
    /// Parse from `/config` command args like `model=gpt-4 temp=0.5 tokens=8192`.
    pub fn parse(args: &str) -> Self {
        let mut config = Self::default();
        for part in args.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                match key {
                    "model" => config.model = Some(ModelId::new(value)),
                    "temp" | "temperature" => config.temperature = value.parse().ok(),
                    "tokens" | "max_tokens" => config.max_tokens = value.parse().ok(),
                    _ => {}
                }
            }
        }
        config
    }
}

// ── P1: Loop Detection ─────────────────────────────────────────────────────────

/// Tracks tool call fingerprints to detect infinite loops.
pub struct LoopDetector {
    fingerprints: Vec<u64>,
    max_history: usize,
    consecutive_threshold: usize,
}

impl LoopDetector {
    pub fn new(max_history: usize, consecutive_threshold: usize) -> Self {
        Self {
            fingerprints: Vec::new(),
            max_history,
            consecutive_threshold,
        }
    }

    /// Record a tool call and return true if a loop is detected.
    pub fn record(&mut self, tool_name: &str, args: &serde_json::Value) -> bool {
        let fingerprint = Self::hash_call(tool_name, args);
        self.fingerprints.push(fingerprint);

        // Keep bounded history
        if self.fingerprints.len() > self.max_history {
            self.fingerprints
                .drain(..self.fingerprints.len() - self.max_history);
        }

        self.is_looping()
    }

    fn hash_call(tool_name: &str, args: &serde_json::Value) -> u64 {
        let mut hasher = DefaultHasher::new();
        tool_name.hash(&mut hasher);
        args.to_string().hash(&mut hasher);
        hasher.finish()
    }

    fn is_looping(&self) -> bool {
        if self.fingerprints.len() < self.consecutive_threshold {
            return false;
        }
        let tail = &self.fingerprints[self.fingerprints.len() - self.consecutive_threshold..];
        tail.iter().all(|&f| f == tail[0])
    }

    pub fn reset(&mut self) {
        self.fingerprints.clear();
    }
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new(20, 3)
    }
}

// ── Slash commands ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointCommand {
    Create,
    List,
    Restore(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KanbanCommand {
    Open,
    Add(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Help,
    Clear,
    Model(String),
    Provider(String),
    Status,
    Compact,
    Init,
    Marketplace,
    Install(String),
    Recommend,
    McpStatus,
    McpAdd(String),
    Agents,
    Skills,
    Effort(String),
    Config(String),
    Export(String),
    Mode(String),
    Checkpoint(CheckpointCommand),
    Kanban(KanbanCommand),
    Review,
    Fork,
    Telemetry,
    Context(context::ContextCommand),
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
        let args = parts.get(1).map(|value| value.trim()).unwrap_or_default();
        let cmd = match parts[0] {
            "help" => Self::Help,
            "clear" => Self::Clear,
            "status" => Self::Status,
            "compact" => Self::Compact,
            "init" => Self::Init,
            "marketplace" => Self::Marketplace,
            "install" => Self::Install(args.to_string()),
            "recommend" => Self::Recommend,
            "mcp" => {
                let subparts: Vec<&str> = args.splitn(2, ' ').collect();
                match subparts[0] {
                    "status" => Self::McpStatus,
                    "add" => {
                        Self::McpAdd(subparts.get(1).map(|s| s.to_string()).unwrap_or_default())
                    }
                    _ if args.is_empty() => Self::Unknown("mcp".to_string()),
                    _ => Self::Unknown(format!("mcp {args}")),
                }
            }
            "checkpoint" => {
                let subparts: Vec<&str> = args.splitn(3, ' ').collect();
                match subparts[0] {
                    "" => Self::Checkpoint(CheckpointCommand::Create),
                    "list" => Self::Checkpoint(CheckpointCommand::List),
                    "restore" => Self::Checkpoint(CheckpointCommand::Restore(
                        subparts
                            .get(1)
                            .map(|s| s.trim().to_string())
                            .unwrap_or_default(),
                    )),
                    _ => Self::Unknown(format!("checkpoint {args}")),
                }
            }
            "kanban" => {
                let subparts: Vec<&str> = args.splitn(2, ' ').collect();
                match subparts[0] {
                    "" => Self::Kanban(KanbanCommand::Open),
                    "add" => Self::Kanban(KanbanCommand::Add(
                        subparts
                            .get(1)
                            .map(|s| s.trim().to_string())
                            .unwrap_or_default(),
                    )),
                    _ => Self::Unknown(format!("kanban {args}")),
                }
            }
            "agents" => Self::Agents,
            "skills" => Self::Skills,
            "exit" | "quit" => Self::Exit,
            "review" => Self::Review,
            "telemetry" => Self::Telemetry,
            "context" => Self::Context(context::ContextCommand::parse(args)),
            "model" => Self::Model(args.to_string()),
            "provider" => Self::Provider(args.to_string()),
            "effort" => Self::Effort(args.to_string()),
            "config" => Self::Config(args.to_string()),
            "export" => Self::Export(args.to_string()),
            "mode" => Self::Mode(args.to_string()),
            "fork" => Self::Fork,
            other => Self::Unknown(other.to_string()),
        };
        Some(cmd)
    }

    pub fn description(&self) -> String {
        match self {
            Self::Help => "Show available commands".to_string(),
            Self::Clear => "Clear current session output".to_string(),
            Self::Model(model) if model.is_empty() => "Set active model".to_string(),
            Self::Model(model) => format!("Switch active model to {model}"),
            Self::Provider(provider) if provider.is_empty() => "Set active provider".to_string(),
            Self::Provider(provider) => format!("Switch active provider to {provider}"),
            Self::Status => "Show current session status".to_string(),
            Self::Compact => "Compact the current conversation".to_string(),
            Self::Init => "Initialize Caduceus in the current project".to_string(),
            Self::Marketplace => "Opens marketplace panel".to_string(),
            Self::Install(name) if name.is_empty() => {
                "Install a skill/agent/plugin by name".to_string()
            }
            Self::Install(name) => format!("Install a skill/agent/plugin by name: {name}"),
            Self::Recommend => "Get recommendations for current project".to_string(),
            Self::McpStatus => "Show connected MCP servers".to_string(),
            Self::McpAdd(name) if name.is_empty() => "Add MCP server from registry".to_string(),
            Self::McpAdd(name) => format!("Add MCP server from registry: {name}"),
            Self::Agents => "List available agents".to_string(),
            Self::Skills => "List available skills".to_string(),
            Self::Effort(level) if level.is_empty() => {
                "Set effort level (min/low/medium/high/max)".to_string()
            }
            Self::Effort(level) => format!("Set effort level to {level}"),
            Self::Config(args) if args.is_empty() => "Show current configuration".to_string(),
            Self::Config(args) if args.starts_with("set ") => {
                format!(
                    "Set configuration value: {}",
                    args.trim_start_matches("set ").trim()
                )
            }
            Self::Config(args) => format!("Inspect or update config: {args}"),
            Self::Export(args) if args.is_empty() => {
                "Export current session as JSON and Markdown".to_string()
            }
            Self::Export(args) => format!("Export current session: {args}"),
            Self::Mode(mode) if mode.is_empty() => {
                "Set agent mode (plan/act/research/autopilot/architect/debug/review)".to_string()
            }
            Self::Mode(mode) => format!("Switch agent mode to {mode}"),
            Self::Checkpoint(CheckpointCommand::Create) => {
                "Create a manual workspace checkpoint".to_string()
            }
            Self::Checkpoint(CheckpointCommand::List) => "List session checkpoints".to_string(),
            Self::Checkpoint(CheckpointCommand::Restore(id)) if id.is_empty() => {
                "Restore a checkpoint by id".to_string()
            }
            Self::Checkpoint(CheckpointCommand::Restore(id)) => {
                format!("Restore checkpoint {id}")
            }
            Self::Kanban(KanbanCommand::Open) => "Open the kanban board".to_string(),
            Self::Kanban(KanbanCommand::Add(title)) if title.is_empty() => {
                "Add a kanban card to backlog".to_string()
            }
            Self::Kanban(KanbanCommand::Add(title)) => {
                format!("Add kanban card to backlog: {title}")
            }
            Self::Exit => "Exit the current session".to_string(),
            Self::Review => "Run BugBot on current git diff".to_string(),
            Self::Fork => "Fork the current session into a new branch".to_string(),
            Self::Telemetry => "Show current session telemetry metrics".to_string(),
            Self::Context(ref cmd) => match cmd {
                context::ContextCommand::Overview => {
                    "Show context usage breakdown and zone".to_string()
                }
                context::ContextCommand::Breakdown => {
                    "Show detailed per-component token counts".to_string()
                }
                context::ContextCommand::Compact => {
                    "Compact conversation with default strategy".to_string()
                }
                context::ContextCommand::CompactWithStrategy(strategy) => {
                    format!("Compact conversation with {strategy} strategy")
                }
                context::ContextCommand::Pin { label, .. } => {
                    format!("Pin context item: {label}")
                }
                context::ContextCommand::Unpin { label } => {
                    format!("Unpin context item: {label}")
                }
                context::ContextCommand::Pins => "List pinned context items".to_string(),
                context::ContextCommand::Zone => {
                    "Show current performance zone with recommendation".to_string()
                }
                context::ContextCommand::Clear => {
                    "Clear all history, keep pins and system prompt".to_string()
                }
            },
            Self::Unknown(command) => format!("Unknown slash command: {command}"),
        }
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
        crate::context::estimate_tokens(text)
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
        let stop_reason = match stop_reason { StopReason::EndTurn => caduceus_core::StopReason::EndTurn, StopReason::MaxTokens => caduceus_core::StopReason::MaxTokens, StopReason::StopSequence => caduceus_core::StopReason::StopSequence, StopReason::ToolUse => caduceus_core::StopReason::ToolUse, };
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

    // ── New events for rich visualization ──────────────────────────────────────

    pub async fn emit_thinking_started(&self, iteration: u32) {
        self.emit(AgentEvent::ThinkingStarted { iteration }).await;
    }

    pub async fn emit_reasoning_delta(&self, content: impl Into<String>) {
        self.emit(AgentEvent::ReasoningDelta { content: content.into() }).await;
    }

    pub async fn emit_reasoning_complete(&self, content: impl Into<String>, duration_ms: u64) {
        self.emit(AgentEvent::ReasoningComplete { content: content.into(), duration_ms }).await;
    }

    pub async fn emit_context_warning(&self, level: impl Into<String>, used: u32, max: u32) {
        self.emit(AgentEvent::ContextWarning {
            level: level.into(), used_tokens: used, max_tokens: max,
        }).await;
    }

    pub async fn emit_context_compacted(&self, freed: u32, before: u32, after: u32) {
        self.emit(AgentEvent::ContextCompacted {
            freed_tokens: freed, before, after,
        }).await;
    }

    pub async fn emit_loop_detected(&self, tool_name: impl Into<String>, count: u32) {
        self.emit(AgentEvent::LoopDetected {
            tool_name: tool_name.into(), consecutive_count: count,
        }).await;
    }

    pub async fn emit_circuit_breaker(&self, failures: u32, last_tools: Vec<String>) {
        self.emit(AgentEvent::CircuitBreakerTriggered {
            consecutive_failures: failures, last_tools,
        }).await;
    }

    pub async fn emit_tree_node(&self, id: impl Into<String>, parent_id: Option<String>, label: impl Into<String>, status: impl Into<String>) {
        self.emit(AgentEvent::ExecutionTreeNode {
            id: id.into(), parent_id, label: label.into(), status: status.into(),
        }).await;
    }

    pub async fn emit_tree_update(&self, id: impl Into<String>, status: impl Into<String>, detail: Option<String>) {
        self.emit(AgentEvent::ExecutionTreeUpdate {
            id: id.into(), status: status.into(), detail,
        }).await;
    }

    pub async fn emit_message_part(&self, part: caduceus_core::MessagePartType) {
        self.emit(AgentEvent::MessagePart { part_type: part }).await;
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
    max_tool_rounds: usize,
    emitter: Option<AgentEventEmitter>,
    instruction_set: Option<instructions::InstructionSet>,
    cancellation_token: Option<CancellationToken>,
    effort_level: Option<EffortLevel>,
    query_config: Option<QueryConfig>,
    mode: Option<modes::AgentMode>,
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
            max_tool_rounds: 25,
            emitter: None,
            instruction_set: None,
            cancellation_token: None,
            effort_level: None,
            query_config: None,
            mode: None,
        }
    }

    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }

    pub fn with_max_tool_rounds(mut self, n: usize) -> Self {
        self.max_tool_rounds = n;
        self
    }

    pub fn with_emitter(mut self, emitter: AgentEventEmitter) -> Self {
        self.emitter = Some(emitter);
        self
    }

    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    pub fn with_effort_level(mut self, level: EffortLevel) -> Self {
        self.effort_level = Some(level);
        self
    }

    pub fn with_query_config(mut self, config: QueryConfig) -> Self {
        self.query_config = Some(config);
        self
    }

    pub fn with_mode(mut self, mode: modes::AgentMode) -> Self {
        self.mode = Some(mode);
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

    /// Check cancellation if a token is set.
    fn check_cancellation(&self) -> Result<()> {
        if let Some(ref token) = self.cancellation_token {
            token.check()?;
        }
        Ok(())
    }

    /// Build the effective system prompt incorporating effort level and mode.
    fn effective_system_prompt(&self) -> String {
        let mut prompt = self.system_prompt.clone();

        // Prepend mode-specific instructions
        if let Some(ref mode) = self.mode {
            let mode_config = mode.config();
            prompt = format!(
                "<agent_mode mode=\"{}\">\n{}\n</agent_mode>\n\n{}",
                mode.name(),
                mode_config.system_prompt_prefix,
                prompt
            );
        }

        if let Some(ref effort) = self.effort_level {
            prompt = format!(
                "{}\n\n<effort_level>\n{}\n</effort_level>",
                prompt,
                effort.system_prompt_detail()
            );
        }
        prompt
    }

    /// Resolve effective max_tokens: query_config > effort_level > default.
    fn effective_max_tokens(&self) -> u32 {
        if let Some(ref qc) = self.query_config {
            if let Some(tokens) = qc.max_tokens {
                return tokens;
            }
        }
        if let Some(ref effort) = self.effort_level {
            return effort.max_tokens();
        }
        4096
    }

    /// Resolve effective temperature: query_config > effort_level > None.
    fn effective_temperature(&self) -> Option<f32> {
        if let Some(ref qc) = self.query_config {
            if qc.temperature.is_some() {
                return qc.temperature;
            }
        }
        self.effort_level.map(|e| e.temperature())
    }

    /// Resolve effective model: query_config > session state.
    fn effective_model(&self, state: &SessionState) -> ModelId {
        if let Some(ref qc) = self.query_config {
            if let Some(ref model) = qc.model {
                return model.clone();
            }
        }
        state.model_id.clone()
    }

    /// Full agent conversation loop.
    ///
    /// 1. Append user message to conversation history
    /// 2. Assemble context within token budget
    /// 3. Send to LLM (non-streaming for tool calls, streaming for final text)
    /// 4. If stop_reason == ToolUse, execute each tool call, feed results back
    /// 5. Repeat until EndTurn / MaxTokens / max_turns exhausted
    /// 6. Return final assistant text
    pub async fn run(
        &self,
        state: &mut SessionState,
        history: &mut ConversationHistory,
        user_input: &str,
    ) -> Result<String> {
        self.check_cancellation()?;

        state.phase = SessionPhase::Running;
        if let Some(ref em) = self.emitter {
            em.emit_phase_changed(SessionPhase::Running).await;
        }

        history.append(caduceus_providers::Message::user(user_input));

        let system_prompt = self.effective_system_prompt();
        let assembler = ContextAssembler::new(self.max_context_tokens, &system_prompt);
        let tool_specs = self.tools.specs();

        // Token budget warning
        let warning = state.token_budget.warning_level();
        if warning != WarningLevel::None {
            if let Some(ref em) = self.emitter {
                let msg = match warning {
                    WarningLevel::Warning70 => "Warning: 70% of context budget used",
                    WarningLevel::Warning85 => "Warning: 85% of context budget used",
                    WarningLevel::Critical95 => "Critical: 95% of context budget used",
                    WarningLevel::None => unreachable!(),
                };
                em.emit_error(msg).await;
            }
        }

        let mut loop_detector = LoopDetector::new(20, 3);
        let mut consecutive_failures: u32 = 0;
        let mut final_text = String::new();
        let mut tool_sequence: Vec<String> = Vec::new();

        // ── Tool-calling loop ─────────────────────────────────────────────────
        for iteration in 0..self.max_tool_rounds {
            self.check_cancellation()?;

            // Circuit breaker: too many consecutive failures
            if consecutive_failures >= 5 {
                if let Some(ref em) = self.emitter {
                    em.emit_circuit_breaker(consecutive_failures, tool_sequence.iter().rev().take(5).cloned().collect()).await;
                    em.emit_error(&format!(
                        "Circuit breaker: {} consecutive tool failures. Last: {}",
                        consecutive_failures,
                        tool_sequence.last().unwrap_or(&"none".to_string())
                    )).await;
                }
                final_text = format!(
                    "🛑 Circuit breaker triggered after {} consecutive tool failures.\n\
                    Last tools: {}\nPlease simplify the request or check the working directory.",
                    consecutive_failures,
                    tool_sequence.iter().rev().take(5).cloned().collect::<Vec<_>>().join(", ")
                );
                break;
            }

            // Emit thinking event
            if let Some(ref em) = self.emitter {
                em.emit_thinking_started(iteration as u32).await;
                    em.emit_tree_node(format!("turn-{}", iteration), None, format!("Turn {} — Thinking", iteration + 1), "running").await;
            }

            // Assemble messages within budget
            let messages = assembler.assemble(history);

            let request = ChatRequest {
                model: self.effective_model(state),
                messages,
                system: Some(system_prompt.clone()),
                max_tokens: self.effective_max_tokens(),
                temperature: self.effective_temperature(),
                thinking_mode: false,
                tool_choice: None,
                tools: tool_specs.clone(),
                response_format: None,
            };

            // Call LLM (non-streaming to get tool_calls)
            let response = self.provider.chat(request).await?;

            // Update token budget
            state.token_budget.used_input += response.input_tokens;
            state.token_budget.used_output += response.output_tokens;
            state.turn_count += 1;

            // Emit text content if any
            if !response.content.is_empty() {
                if let Some(ref em) = self.emitter {
                    em.emit_text_delta(&response.content).await;
                }
            }

            // Check stop reason
            match response.stop_reason {
                StopReason::EndTurn | StopReason::MaxTokens | StopReason::StopSequence => {
                    // No tool calls — final response
                    history.append(caduceus_providers::Message::assistant(&response.content));
                    final_text = response.content;
                    if let Some(ref em) = self.emitter {
                        em.emit_turn_complete(response.stop_reason.clone(), TokenUsage {
                            input_tokens: response.input_tokens,
                            output_tokens: response.output_tokens,
                            cache_read_tokens: response.cache_read_tokens,
                            cache_write_tokens: response.cache_creation_tokens,
                        }).await;
                    }
                    break;
                }
                StopReason::ToolUse => {
                    if response.tool_calls.is_empty() {
                        // stop_reason says tool_use but no tool_calls — treat as end
                        history.append(caduceus_providers::Message::assistant(&response.content));
                        final_text = response.content;
                        break;
                    }

                    // Store assistant message with tool calls in history
                    let mut assistant_msg = caduceus_providers::Message::assistant(&response.content);
                    assistant_msg.tool_calls = response.tool_calls.clone();
                    history.append(assistant_msg);

                    // Execute each tool call
                    for tool_use in &response.tool_calls {
                        // Loop detection
                        if loop_detector.record(&tool_use.name, &tool_use.input) {
                            if let Some(ref em) = self.emitter {
                                em.emit_circuit_breaker(consecutive_failures, tool_sequence.iter().rev().take(5).cloned().collect()).await;
                    em.emit_error(&format!(
                                    "Loop detected: tool '{}' called repeatedly with same args",
                                    tool_use.name
                                )).await;
                            }
                            consecutive_failures += 1;
                        }

                        tool_sequence.push(tool_use.name.clone());

                        // Emit tool start event
                        if let Some(ref em) = self.emitter {
                            em.emit_tool_call_start(caduceus_core::ToolCallId(tool_use.id.clone()), &tool_use.name).await;
                        }

                        // Execute tool
                        let result = self.tools.execute(&tool_use.name, tool_use.input.clone()).await;

                        let (result_content, is_error) = match result {
                            Ok(r) => {
                                if r.is_error {
                                    consecutive_failures += 1;
                                } else {
                                    consecutive_failures = 0;
                                }
                                (r.content, r.is_error)
                            }
                            Err(e) => {
                                consecutive_failures += 1;
                                (e.to_string(), true)
                            }
                        };

                        // Emit tool complete event
                        if let Some(ref em) = self.emitter {
                            em.emit_tool_result_end(caduceus_core::ToolCallId(tool_use.id.clone()), &result_content, is_error).await;
                        }

                        // Add tool result to history
                        let mut tool_msg = caduceus_providers::Message {
                            role: "tool".into(),
                            content: result_content,
                            content_blocks: None,
                            tool_calls: vec![],
                            tool_result: Some(caduceus_core::ToolResult::success("").with_tool_use_id(&tool_use.id)),
                        };
                        tool_msg.content = tool_msg.content.clone();
                        history.append(tool_msg);
                    }
                }
            }
        }

        // Fallback if loop exhausted
        if final_text.is_empty() {
            final_text = format!(
                "⚠️ Agent used all {} tool iterations.\nTools: {}\nUse /compact or simplify.",
                self.max_tool_rounds,
                tool_sequence.join(", ")
            );
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

// ── #234: Agent Execution Tree Visualizer ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VizTreeNode {
    pub id: String,
    pub label: String,
    /// One of: "active", "succeeded", "failed", "pruned"
    pub status: String,
    pub parent: Option<String>,
    pub error: Option<String>,
    pub depth: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionTreeViz {
    pub nodes: Vec<VizTreeNode>,
}

impl ExecutionTreeViz {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, node: VizTreeNode) {
        self.nodes.push(node);
    }

    pub fn node_color(status: &str) -> &'static str {
        match status {
            "active" => "#f59e0b",    // amber / yellow
            "succeeded" => "#10b981", // green
            "failed" => "#ef4444",    // red
            "pruned" => "#6b7280",    // gray
            _ => "#6b7280",
        }
    }

    /// Emit React Flow nodes + edges JSON.
    pub fn to_react_flow_json(&self) -> serde_json::Value {
        let rf_nodes: Vec<serde_json::Value> = self
            .nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "type": "default",
                    "data": {
                        "label": n.label,
                        "status": n.status,
                        "error": n.error,
                    },
                    "style": {
                        "background": Self::node_color(&n.status),
                        "color": "#fff",
                        "borderRadius": "8px",
                    },
                    "position": {
                        "x": (n.depth as f64) * 200.0,
                        "y": 0.0,  // caller is responsible for layout
                    }
                })
            })
            .collect();

        let rf_edges: Vec<serde_json::Value> = self
            .nodes
            .iter()
            .filter_map(|n| {
                n.parent.as_ref().map(|p| {
                    serde_json::json!({
                        "id": format!("{}->{}", p, n.id),
                        "source": p,
                        "target": n.id,
                        "type": "smoothstep",
                    })
                })
            })
            .collect();

        serde_json::json!({ "nodes": rf_nodes, "edges": rf_edges })
    }

    /// Emit Mermaid `graph TD` flowchart syntax.
    pub fn to_mermaid(&self) -> String {
        let mut out = String::from("graph TD\n");
        for node in &self.nodes {
            let safe_label = node.label.replace('"', "'");
            out.push_str(&format!("    {}[\"{}\"]\n", node.id, safe_label));
            let color = match node.status.as_str() {
                "succeeded" => "fill:#10b981,color:#fff",
                "failed" => "fill:#ef4444,color:#fff",
                "active" => "fill:#f59e0b,color:#fff",
                _ => "fill:#6b7280,color:#fff",
            };
            out.push_str(&format!("    style {} {}\n", node.id, color));
            if let Some(parent) = &node.parent {
                out.push_str(&format!("    {} --> {}\n", parent, node.id));
            }
        }
        out
    }
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
            SlashCommand::parse("/init"),
            Some(SlashCommand::Init)
        ));
        assert!(matches!(
            SlashCommand::parse("/model gpt-4"),
            Some(SlashCommand::Model(_))
        ));
        assert!(matches!(
            SlashCommand::parse("/marketplace"),
            Some(SlashCommand::Marketplace)
        ));
        assert!(matches!(
            SlashCommand::parse("/install code-review"),
            Some(SlashCommand::Install(ref name)) if name == "code-review"
        ));
        assert!(matches!(
            SlashCommand::parse("/recommend"),
            Some(SlashCommand::Recommend)
        ));
        assert!(matches!(
            SlashCommand::parse("/mcp status"),
            Some(SlashCommand::McpStatus)
        ));
        assert!(matches!(
            SlashCommand::parse("/mcp add github"),
            Some(SlashCommand::McpAdd(ref name)) if name == "github"
        ));
        assert!(matches!(
            SlashCommand::parse("/checkpoint"),
            Some(SlashCommand::Checkpoint(CheckpointCommand::Create))
        ));
        assert!(matches!(
            SlashCommand::parse("/checkpoint list"),
            Some(SlashCommand::Checkpoint(CheckpointCommand::List))
        ));
        assert!(matches!(
            SlashCommand::parse("/checkpoint restore abc123"),
            Some(SlashCommand::Checkpoint(CheckpointCommand::Restore(ref id))) if id == "abc123"
        ));
        assert!(matches!(
            SlashCommand::parse("/kanban"),
            Some(SlashCommand::Kanban(KanbanCommand::Open))
        ));
        assert!(matches!(
            SlashCommand::parse("/kanban add Implement board"),
            Some(SlashCommand::Kanban(KanbanCommand::Add(ref title))) if title == "Implement board"
        ));
        assert!(matches!(
            SlashCommand::parse("/export markdown notes/session.md"),
            Some(SlashCommand::Export(ref args)) if args == "markdown notes/session.md"
        ));
        assert!(matches!(
            SlashCommand::parse("/fork"),
            Some(SlashCommand::Fork)
        ));
        assert!(SlashCommand::parse("hello").is_none());
    }

    #[test]
    fn slash_command_description_strings() {
        assert_eq!(
            SlashCommand::Marketplace.description(),
            "Opens marketplace panel"
        );
        assert_eq!(
            SlashCommand::Install("skill-name".to_string()).description(),
            "Install a skill/agent/plugin by name: skill-name"
        );
        assert_eq!(
            SlashCommand::Recommend.description(),
            "Get recommendations for current project"
        );
        assert_eq!(
            SlashCommand::McpStatus.description(),
            "Show connected MCP servers"
        );
        assert_eq!(
            SlashCommand::McpAdd("registry-name".to_string()).description(),
            "Add MCP server from registry: registry-name"
        );
        assert_eq!(SlashCommand::Skills.description(), "List available skills");
        assert_eq!(SlashCommand::Agents.description(), "List available agents");
        assert_eq!(
            SlashCommand::Init.description(),
            "Initialize Caduceus in the current project"
        );
        assert_eq!(
            SlashCommand::Checkpoint(CheckpointCommand::List).description(),
            "List session checkpoints"
        );
        assert_eq!(
            SlashCommand::Kanban(KanbanCommand::Add("Write tests".to_string())).description(),
            "Add kanban card to backlog: Write tests"
        );
        assert_eq!(
            SlashCommand::Fork.description(),
            "Fork the current session into a new branch"
        );
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
            cache_read_tokens: None,
            cache_creation_tokens: None,
        }]
    }

    fn make_session() -> caduceus_core::SessionState {
        caduceus_core::SessionState::new(
            ".",
            caduceus_core::ProviderId::new("mock"),
            caduceus_core::ModelId::new("mock-model"),
        )
    }

    fn make_chat_response(text: &str) -> ChatResponse {
        ChatResponse {
            content: text.to_string(),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
        }
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
            MockLlmAdapter::new(vec![make_chat_response("Please provide a message.")]),
        );
        let harness =
            AgentHarness::new(adapter, caduceus_tools::ToolRegistry::new(), 4096, "system");
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        let result = harness.run(&mut state, &mut history, "").await.unwrap();
        assert!(!result.is_empty());
    }

    /// 6. rate_limit_recovery — successive turns both succeed
    #[tokio::test]
    async fn rate_limit_recovery() {
        let adapter = Arc::new(MockLlmAdapter::new(vec![
            make_chat_response("first response"),
            make_chat_response("second response after recovery"),
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
            MockLlmAdapter::new(vec![make_chat_response("remembered")]),
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

    // ── P1: Effort level tests ─────────────────────────────────────────────────

    #[test]
    fn effort_level_from_str() {
        assert_eq!(EffortLevel::from_str_loose("min"), Some(EffortLevel::Min));
        assert_eq!(EffortLevel::from_str_loose("low"), Some(EffortLevel::Low));
        assert_eq!(
            EffortLevel::from_str_loose("medium"),
            Some(EffortLevel::Medium)
        );
        assert_eq!(
            EffortLevel::from_str_loose("med"),
            Some(EffortLevel::Medium)
        );
        assert_eq!(EffortLevel::from_str_loose("high"), Some(EffortLevel::High));
        assert_eq!(EffortLevel::from_str_loose("max"), Some(EffortLevel::Max));
        assert_eq!(EffortLevel::from_str_loose("MAX"), Some(EffortLevel::Max));
        assert_eq!(EffortLevel::from_str_loose("unknown"), None);
    }

    #[test]
    fn effort_level_max_tokens_monotonic() {
        let levels = [
            EffortLevel::Min,
            EffortLevel::Low,
            EffortLevel::Medium,
            EffortLevel::High,
            EffortLevel::Max,
        ];
        for w in levels.windows(2) {
            assert!(
                w[0].max_tokens() <= w[1].max_tokens(),
                "{:?} should have <= tokens than {:?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn effort_level_system_prompt_not_empty() {
        for level in [
            EffortLevel::Min,
            EffortLevel::Low,
            EffortLevel::Medium,
            EffortLevel::High,
            EffortLevel::Max,
        ] {
            assert!(!level.system_prompt_detail().is_empty());
        }
    }

    // ── P1: Query config tests ─────────────────────────────────────────────────

    #[test]
    fn query_config_parse_full() {
        let config = QueryConfig::parse("model=gpt-4 temp=0.5 tokens=8192");
        assert_eq!(config.model.as_ref().unwrap().0, "gpt-4");
        assert_eq!(config.temperature, Some(0.5));
        assert_eq!(config.max_tokens, Some(8192));
    }

    #[test]
    fn query_config_parse_partial() {
        let config = QueryConfig::parse("temp=0.2");
        assert!(config.model.is_none());
        assert_eq!(config.temperature, Some(0.2));
        assert!(config.max_tokens.is_none());
    }

    #[test]
    fn query_config_parse_empty() {
        let config = QueryConfig::parse("");
        assert!(config.model.is_none());
        assert!(config.temperature.is_none());
        assert!(config.max_tokens.is_none());
    }

    // ── P1: Loop detection tests ───────────────────────────────────────────────

    #[test]
    fn loop_detector_no_false_positive() {
        let mut detector = LoopDetector::new(20, 3);
        let args1 = serde_json::json!({"cmd": "ls"});
        let args2 = serde_json::json!({"cmd": "pwd"});
        assert!(!detector.record("bash", &args1));
        assert!(!detector.record("bash", &args2));
        assert!(!detector.record("bash", &args1));
    }

    #[test]
    fn loop_detector_detects_consecutive_duplicates() {
        let mut detector = LoopDetector::new(20, 3);
        let args = serde_json::json!({"cmd": "ls"});
        assert!(!detector.record("bash", &args));
        assert!(!detector.record("bash", &args));
        assert!(detector.record("bash", &args)); // 3rd consecutive
    }

    #[test]
    fn loop_detector_reset_clears() {
        let mut detector = LoopDetector::new(20, 3);
        let args = serde_json::json!({"cmd": "ls"});
        detector.record("bash", &args);
        detector.record("bash", &args);
        detector.reset();
        assert!(!detector.record("bash", &args)); // Reset, so starts fresh
    }

    // ── P1: Slash command effort/config ────────────────────────────────────────

    #[test]
    fn slash_command_effort() {
        assert!(matches!(
            SlashCommand::parse("/effort high"),
            Some(SlashCommand::Effort(ref level)) if level == "high"
        ));
        assert!(matches!(
            SlashCommand::parse("/effort"),
            Some(SlashCommand::Effort(ref level)) if level.is_empty()
        ));
    }

    #[test]
    fn slash_command_config() {
        assert!(matches!(
            SlashCommand::parse("/config model=gpt-4 temp=0.5"),
            Some(SlashCommand::Config(ref args)) if args == "model=gpt-4 temp=0.5"
        ));
        assert!(matches!(
            SlashCommand::parse("/config set default_model gpt-5.4"),
            Some(SlashCommand::Config(ref args)) if args == "set default_model gpt-5.4"
        ));
        assert!(matches!(
            SlashCommand::parse("/config"),
            Some(SlashCommand::Config(ref args)) if args.is_empty()
        ));
    }

    #[test]
    fn slash_command_export_default() {
        assert!(matches!(
            SlashCommand::parse("/export"),
            Some(SlashCommand::Export(ref args)) if args.is_empty()
        ));
    }

    // ── P0: Cancellation in harness ────────────────────────────────────────────

    #[tokio::test]
    async fn harness_cancellation_before_start() {
        let token = CancellationToken::new();
        token.cancel();

        let adapter = Arc::new(
            MockLlmAdapter::new(vec![make_chat_response("response")]),
        );
        let harness =
            AgentHarness::new(adapter, caduceus_tools::ToolRegistry::new(), 4096, "system")
                .with_cancellation_token(token);
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        let result = harness.run(&mut state, &mut history, "hello").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cancelled"));
    }

    // ── P1: Effort level affects harness ───────────────────────────────────────

    #[tokio::test]
    async fn harness_with_effort_level() {
        let adapter =
            Arc::new(MockLlmAdapter::new(vec![make_chat_response("ok")]));
        let harness = AgentHarness::new(
            adapter.clone(),
            caduceus_tools::ToolRegistry::new(),
            4096,
            "base system prompt",
        )
        .with_effort_level(EffortLevel::Max);

        let mut state = make_session();
        let mut history = ConversationHistory::new();
        harness
            .run(&mut state, &mut history, "hello")
            .await
            .unwrap();

        // Verify the request had high max_tokens from Max effort
        let requests = adapter.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].max_tokens, EffortLevel::Max.max_tokens());
    }

    // ── P1: Query config override ──────────────────────────────────────────────

    #[tokio::test]
    async fn harness_with_query_config() {
        let adapter =
            Arc::new(MockLlmAdapter::new(vec![make_chat_response("ok")]));
        let qc = QueryConfig {
            model: Some(ModelId::new("custom-model")),
            temperature: Some(0.9),
            max_tokens: Some(2048),
        };
        let harness = AgentHarness::new(
            adapter.clone(),
            caduceus_tools::ToolRegistry::new(),
            4096,
            "system",
        )
        .with_query_config(qc);

        let mut state = make_session();
        let mut history = ConversationHistory::new();
        harness
            .run(&mut state, &mut history, "hello")
            .await
            .unwrap();

        let requests = adapter.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model.0, "custom-model");
        assert_eq!(requests[0].temperature, Some(0.9));
        assert_eq!(requests[0].max_tokens, 2048);
    }

    // ── P1: Tool round limiting (infrastructure) ───────────────────────────────

    #[test]
    fn harness_default_max_tool_rounds() {
        let adapter: Arc<dyn LlmAdapter> = Arc::new(MockLlmAdapter::new(vec![]));
        let harness =
            AgentHarness::new(adapter, caduceus_tools::ToolRegistry::new(), 4096, "system");
        assert_eq!(harness.max_tool_rounds, 25);
    }

    #[test]
    fn harness_custom_max_tool_rounds() {
        let adapter: Arc<dyn LlmAdapter> = Arc::new(MockLlmAdapter::new(vec![]));
        let harness =
            AgentHarness::new(adapter, caduceus_tools::ToolRegistry::new(), 4096, "system")
                .with_max_tool_rounds(10);
        assert_eq!(harness.max_tool_rounds, 10);
    }

    // ── Mode slash command ─────────────────────────────────────────────────────

    #[test]
    fn slash_command_mode_parse() {
        assert!(matches!(
            SlashCommand::parse("/mode plan"),
            Some(SlashCommand::Mode(ref m)) if m == "plan"
        ));
        assert!(matches!(
            SlashCommand::parse("/mode autopilot"),
            Some(SlashCommand::Mode(ref m)) if m == "autopilot"
        ));
        assert!(matches!(
            SlashCommand::parse("/mode"),
            Some(SlashCommand::Mode(ref m)) if m.is_empty()
        ));
    }

    #[test]
    fn slash_command_mode_description() {
        assert!(SlashCommand::Mode("plan".into())
            .description()
            .contains("plan"));
        assert!(SlashCommand::Mode(String::new())
            .description()
            .contains("agent mode"));
    }

    // ── Mode integration with harness ──────────────────────────────────────────

    #[tokio::test]
    async fn harness_with_mode_prepends_prompt() {
        let adapter =
            Arc::new(MockLlmAdapter::new(vec![make_chat_response("ok")]));
        let harness = AgentHarness::new(
            adapter.clone(),
            caduceus_tools::ToolRegistry::new(),
            4096,
            "base prompt",
        )
        .with_mode(modes::AgentMode::Plan);

        let mut state = make_session();
        let mut history = ConversationHistory::new();
        harness
            .run(&mut state, &mut history, "hello")
            .await
            .unwrap();

        let requests = adapter.recorded_requests();
        assert_eq!(requests.len(), 1);
        // Mode prefix should appear in the system prompt
        let system = requests[0].system.as_ref().unwrap();
        assert!(system.contains("PLAN mode"));
        assert!(system.contains("base prompt"));
    }

    #[test]
    fn test_max_turns_effort_level() {
        // EffortLevel::Max should have the highest token budget
        assert!(EffortLevel::Max.max_tokens() > EffortLevel::Min.max_tokens());
        assert!(EffortLevel::High.max_tokens() > EffortLevel::Low.max_tokens());
        assert!(EffortLevel::Medium.max_tokens() > EffortLevel::Min.max_tokens());
    }

    #[test]
    fn test_kill_switch_stops_agent() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(
            token.is_cancelled(),
            "cancel() should set the token to cancelled"
        );
    }

    // ── #234: ExecutionTreeViz tests ──────────────────────────────────────────

    fn make_viz_node(id: &str, status: &str, parent: Option<&str>, depth: usize) -> VizTreeNode {
        VizTreeNode {
            id: id.to_string(),
            label: format!("Node {id}"),
            status: status.to_string(),
            parent: parent.map(str::to_string),
            error: None,
            depth,
        }
    }

    #[test]
    fn exec_tree_add_and_color() {
        let mut tree = ExecutionTreeViz::new();
        tree.add_node(make_viz_node("root", "succeeded", None, 0));
        tree.add_node(make_viz_node("child", "failed", Some("root"), 1));
        assert_eq!(tree.nodes.len(), 2);
        assert_eq!(ExecutionTreeViz::node_color("succeeded"), "#10b981");
        assert_eq!(ExecutionTreeViz::node_color("failed"), "#ef4444");
        assert_eq!(ExecutionTreeViz::node_color("active"), "#f59e0b");
        assert_eq!(ExecutionTreeViz::node_color("pruned"), "#6b7280");
        assert_eq!(ExecutionTreeViz::node_color("unknown"), "#6b7280");
    }

    #[test]
    fn exec_tree_react_flow_json() {
        let mut tree = ExecutionTreeViz::new();
        tree.add_node(make_viz_node("root", "succeeded", None, 0));
        tree.add_node(make_viz_node("child", "active", Some("root"), 1));
        let json = tree.to_react_flow_json();
        let nodes = json["nodes"].as_array().unwrap();
        let edges = json["edges"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["source"], "root");
        assert_eq!(edges[0]["target"], "child");
        assert_eq!(nodes[0]["data"]["status"], "succeeded");
        assert_eq!(nodes[1]["data"]["label"], "Node child");
    }

    #[test]
    fn exec_tree_mermaid_output() {
        let mut tree = ExecutionTreeViz::new();
        tree.add_node(make_viz_node("root", "succeeded", None, 0));
        tree.add_node(make_viz_node("a", "failed", Some("root"), 1));
        tree.add_node(make_viz_node("b", "pruned", Some("root"), 1));
        let mermaid = tree.to_mermaid();
        assert!(mermaid.starts_with("graph TD\n"));
        assert!(mermaid.contains("root --> a"));
        assert!(mermaid.contains("root --> b"));
        assert!(mermaid.contains("fill:#10b981")); // succeeded
        assert!(mermaid.contains("fill:#ef4444")); // failed
        assert!(mermaid.contains("fill:#6b7280")); // pruned
    }

    #[test]
    fn exec_tree_no_edges_for_roots() {
        let mut tree = ExecutionTreeViz::new();
        tree.add_node(make_viz_node("r1", "active", None, 0));
        tree.add_node(make_viz_node("r2", "active", None, 0));
        let json = tree.to_react_flow_json();
        assert_eq!(json["edges"].as_array().unwrap().len(), 0);
    }
    // ── Phase 1e: Tool loop + circuit breaker + event tests ───────────────────

    #[tokio::test]
    async fn tool_loop_executes_tool_and_returns_final() {
        // Script: first response has tool_call, second is final text
        let tool_response = ChatResponse {
            content: String::new(),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: caduceus_providers::StopReason::ToolUse,
            tool_calls: vec![caduceus_core::ToolUse {
                id: "tc1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "hello.txt"}),
            }],
        };
        let final_response = make_chat_response("Here is the file content.");

        let adapter = Arc::new(MockLlmAdapter::new(vec![tool_response, final_response]));

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello world").unwrap();
        let mut registry = caduceus_tools::ToolRegistry::new();
        registry.register(Arc::new(caduceus_tools::ReadFileTool::new(dir.path())));

        let harness = AgentHarness::new(adapter, registry, 4096, "system");
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        let result = harness.run(&mut state, &mut history, "read hello.txt").await.unwrap();

        assert_eq!(result, "Here is the file content.");
        assert!(state.turn_count >= 2, "should have at least 2 turns (tool + final)");
    }

    #[tokio::test]
    async fn circuit_breaker_stops_after_consecutive_failures() {
        // Script: 10 tool_call responses that will all fail (unknown tool)
        let bad_responses: Vec<ChatResponse> = (0..10).map(|i| ChatResponse {
            content: String::new(),
            input_tokens: 5,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: caduceus_providers::StopReason::ToolUse,
            tool_calls: vec![caduceus_core::ToolUse {
                id: format!("tc{i}"),
                name: "nonexistent_tool".into(),
                input: serde_json::json!({}),
            }],
        }).collect();

        let adapter = Arc::new(MockLlmAdapter::new(bad_responses));
        let harness = AgentHarness::new(adapter, caduceus_tools::ToolRegistry::new(), 4096, "system");
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        let result = harness.run(&mut state, &mut history, "do something").await.unwrap();

        assert!(result.contains("Circuit breaker"), "should trigger circuit breaker: {result}");
        // Should stop well before 10 iterations
        assert!(state.turn_count <= 6, "should stop early, got {} turns", state.turn_count);
    }

    #[tokio::test]
    async fn events_emitted_during_tool_loop() {
        let tool_response = ChatResponse {
            content: "Let me read that.".into(),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: caduceus_providers::StopReason::ToolUse,
            tool_calls: vec![caduceus_core::ToolUse {
                id: "tc1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "test.txt"}),
            }],
        };
        let final_response = make_chat_response("Done!");

        let adapter = Arc::new(MockLlmAdapter::new(vec![tool_response, final_response]));

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "content").unwrap();
        let mut registry = caduceus_tools::ToolRegistry::new();
        registry.register(Arc::new(caduceus_tools::ReadFileTool::new(dir.path())));

        let (emitter, mut rx) = AgentEventEmitter::channel(64);
        let harness = AgentHarness::new(adapter, registry, 4096, "system")
            .with_emitter(emitter);
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        let _result = harness.run(&mut state, &mut history, "read test.txt").await.unwrap();

        // Collect all events
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Should have: phase_changed, thinking, text_delta, tool_call_start, tool_result_end, turn_complete, phase_changed
        let event_types: Vec<String> = events.iter().map(|e| format!("{:?}", std::mem::discriminant(e))).collect();
        assert!(events.len() >= 5, "expected ≥5 events, got {}: {:?}", events.len(), event_types);

        // Check specific events exist
        let has_tool_start = events.iter().any(|e| matches!(e, AgentEvent::ToolCallStart { .. }));
        let has_tool_end = events.iter().any(|e| matches!(e, AgentEvent::ToolResultEnd { .. }));
        let has_turn_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { .. }));
        assert!(has_tool_start, "missing ToolCallStart event");
        assert!(has_tool_end, "missing ToolResultEnd event");
        assert!(has_turn_complete, "missing TurnComplete event");
    }

    #[tokio::test]
    async fn tool_specs_sent_in_request() {
        let adapter = Arc::new(MockLlmAdapter::new(vec![make_chat_response("hi")]));
        let dir = tempfile::tempdir().unwrap();
        let mut registry = caduceus_tools::ToolRegistry::new();
        registry.register(Arc::new(caduceus_tools::ReadFileTool::new(dir.path())));

        let harness = AgentHarness::new(adapter.clone(), registry, 4096, "system");
        let mut state = make_session();
        let mut history = ConversationHistory::new();
        let _ = harness.run(&mut state, &mut history, "test").await;

        let requests = adapter.recorded_requests();
        assert!(!requests.is_empty());
        assert!(!requests[0].tools.is_empty(), "tools should be sent in request");
        assert!(requests[0].tools.iter().any(|t| t.name == "read_file"), "read_file tool should be in request");
    }

}

// ── #236: PRD Parser ─────────────────────────────────────────────────────────

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct PrdTask {
    pub id: usize,
    pub title: String,
    pub description: String,
    pub parent_id: Option<usize>,
    pub priority: u8,
    pub complexity: u8,
    pub estimated_hours: f64,
    pub dependencies: Vec<usize>,
    pub tags: Vec<String>,
}

pub struct PrdParser;

impl PrdParser {
    /// Return (heading, content) pairs for every markdown section.
    pub fn extract_sections(text: &str) -> Vec<(String, String)> {
        let mut sections: Vec<(String, String)> = Vec::new();
        let mut current_title: Option<String> = None;
        let mut buf = String::new();

        for line in text.lines() {
            if line.starts_with('#') {
                if let Some(title) = current_title.take() {
                    sections.push((title, buf.trim().to_string()));
                    buf.clear();
                }
                let title = line.trim_start_matches('#').trim().to_string();
                if !title.is_empty() {
                    current_title = Some(title);
                }
            } else if current_title.is_some() {
                buf.push_str(line);
                buf.push('\n');
            }
        }
        if let Some(title) = current_title {
            sections.push((title, buf.trim().to_string()));
        }
        sections
    }

    /// Parse a markdown PRD document into a flat list of `PrdTask`s.
    pub fn parse(prd_text: &str) -> Vec<PrdTask> {
        // Collect (level, title, content) triples.
        let mut triples: Vec<(usize, String, String)> = Vec::new();
        let mut current: Option<(usize, String)> = None;
        let mut buf = String::new();

        for line in prd_text.lines() {
            if line.starts_with('#') {
                if let Some((lvl, title)) = current.take() {
                    triples.push((lvl, title, buf.trim().to_string()));
                    buf.clear();
                }
                let level = line.chars().take_while(|&c| c == '#').count();
                let title = line[level..].trim().to_string();
                if !title.is_empty() {
                    current = Some((level, title));
                }
            } else if current.is_some() {
                buf.push_str(line);
                buf.push('\n');
            }
        }
        if let Some((lvl, title)) = current {
            triples.push((lvl, title, buf.trim().to_string()));
        }

        // Build tasks with parent tracking via a stack of (task_id, heading_level).
        let mut tasks: Vec<PrdTask> = Vec::new();
        let mut parent_stack: Vec<(usize, usize)> = Vec::new();

        for (id, (level, title, content)) in triples.into_iter().enumerate() {
            while parent_stack.last().is_some_and(|&(_, l)| l >= level) {
                parent_stack.pop();
            }
            let parent_id = parent_stack.last().map(|&(pid, _)| pid);
            let priority = Self::extract_priority(&content);
            let complexity = Self::extract_complexity(&content);
            let estimated_hours = Self::extract_hours(&content);
            let tags = Self::extract_tags(&content);

            tasks.push(PrdTask {
                id,
                title,
                description: content,
                parent_id,
                priority,
                complexity,
                estimated_hours,
                dependencies: Vec::new(),
                tags,
            });
            parent_stack.push((id, level));
        }
        tasks
    }

    /// Infer dependency edges from keyword references between task descriptions.
    /// Returns pairs `(dependent_id, dependency_id)`.
    pub fn infer_dependencies(tasks: &[PrdTask]) -> Vec<(usize, usize)> {
        let mut deps = Vec::new();
        for task in tasks {
            for other in tasks {
                if other.id == task.id {
                    continue;
                }
                if task
                    .description
                    .to_lowercase()
                    .contains(&other.title.to_lowercase())
                {
                    deps.push((task.id, other.id));
                }
            }
        }
        deps
    }

    fn extract_priority(text: &str) -> u8 {
        let lower = text.to_lowercase();
        if lower.contains("priority: high") || lower.contains("priority:high") {
            8
        } else if lower.contains("priority: low") || lower.contains("priority:low") {
            2
        } else {
            5
        }
    }

    fn extract_complexity(text: &str) -> u8 {
        let lower = text.to_lowercase();
        if lower.contains("complexity: high") || lower.contains("complexity:high") {
            8
        } else if lower.contains("complexity: low") || lower.contains("complexity:low") {
            2
        } else {
            5
        }
    }

    fn extract_hours(text: &str) -> f64 {
        for word in text.split_whitespace() {
            let stripped = word.trim_end_matches('h');
            if let Ok(h) = stripped.parse::<f64>() {
                if h > 0.0 && h < 1000.0 {
                    return h;
                }
            }
        }
        1.0
    }

    fn extract_tags(text: &str) -> Vec<String> {
        text.split_whitespace()
            .filter(|w| w.starts_with('#'))
            .map(|w| w.trim_start_matches('#').to_string())
            .collect()
    }
}

// ── #237: Smart Task Recommender ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TaskRecommendation {
    pub task_id: usize,
    pub score: f64,
    pub reason: String,
}

pub struct TaskRecommender;

impl TaskRecommender {
    /// Rank incomplete tasks by readiness, priority, and inverse complexity.
    pub fn recommend_next(tasks: &[PrdTask], completed: &[usize]) -> Vec<TaskRecommendation> {
        let mut recs: Vec<TaskRecommendation> = tasks
            .iter()
            .filter(|t| !completed.contains(&t.id))
            .map(|t| {
                let dep_s = Self::dependency_score(t, completed);
                let pri_s = Self::priority_score(t);
                let cmp_s = Self::complexity_score(t);
                let score = 0.4 * dep_s + 0.35 * pri_s + 0.25 * cmp_s;
                let reason =
                    format!("dep_ready={dep_s:.2} priority={pri_s:.2} complexity_inv={cmp_s:.2}");
                TaskRecommendation {
                    task_id: t.id,
                    score,
                    reason,
                }
            })
            .collect();

        recs.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        recs
    }

    fn dependency_score(task: &PrdTask, completed: &[usize]) -> f64 {
        if task.dependencies.is_empty() || task.dependencies.iter().all(|d| completed.contains(d)) {
            1.0
        } else {
            0.0
        }
    }

    fn priority_score(task: &PrdTask) -> f64 {
        f64::from(task.priority) / 10.0
    }

    fn complexity_score(task: &PrdTask) -> f64 {
        if task.complexity == 0 {
            1.0
        } else {
            1.0 / f64::from(task.complexity)
        }
    }
}

// ── #239: Unlimited Task Hierarchy ───────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HierarchicalTask {
    pub id: usize,
    pub title: String,
    pub parent_id: Option<usize>,
    pub status: String,
    pub priority: u8,
    pub complexity: u8,
    pub estimated_hours: f64,
    pub actual_hours: f64,
    pub tags: Vec<String>,
    pub level: usize,
}

pub struct TaskTree {
    tasks: HashMap<usize, HierarchicalTask>,
    next_id: usize,
}

impl TaskTree {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            next_id: 0,
        }
    }

    pub fn add_task(&mut self, title: &str, parent_id: Option<usize>) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let level = parent_id.map_or(0, |p| self.depth(p) + 1);
        self.tasks.insert(
            id,
            HierarchicalTask {
                id,
                title: title.to_string(),
                parent_id,
                status: "pending".to_string(),
                priority: 5,
                complexity: 5,
                estimated_hours: 1.0,
                actual_hours: 0.0,
                tags: Vec::new(),
                level,
            },
        );
        id
    }

    pub fn get_task(&self, id: usize) -> Option<&HierarchicalTask> {
        self.tasks.get(&id)
    }

    pub fn children(&self, id: usize) -> Vec<&HierarchicalTask> {
        let mut ch: Vec<&HierarchicalTask> = self
            .tasks
            .values()
            .filter(|t| t.parent_id == Some(id))
            .collect();
        ch.sort_by_key(|t| t.id);
        ch
    }

    /// All descendants of `id`, depth-first.
    pub fn subtree(&self, id: usize) -> Vec<&HierarchicalTask> {
        let mut result = Vec::new();
        for child in self.children(id) {
            result.push(child);
            result.extend(self.subtree(child.id));
        }
        result
    }

    /// Number of ancestors (root = 0).
    pub fn depth(&self, id: usize) -> usize {
        let mut depth = 0;
        let mut current = id;
        while let Some(parent) = self.tasks.get(&current).and_then(|t| t.parent_id) {
            depth += 1;
            current = parent;
        }
        depth
    }

    /// Percentage of immediate children with status `"done"`.
    /// Leaf tasks with `status == "done"` return 100.0, otherwise 0.0.
    pub fn progress(&self, id: usize) -> f64 {
        let ch = self.children(id);
        if ch.is_empty() {
            return if self.tasks.get(&id).is_some_and(|t| t.status == "done") {
                100.0
            } else {
                0.0
            };
        }
        let done = ch.iter().filter(|c| c.status == "done").count();
        done as f64 / ch.len() as f64 * 100.0
    }

    /// Visual tree with indentation.
    pub fn to_tree_string(&self) -> String {
        let mut output = String::new();
        let mut roots: Vec<&HierarchicalTask> = self
            .tasks
            .values()
            .filter(|t| t.parent_id.is_none())
            .collect();
        roots.sort_by_key(|t| t.id);
        for root in roots {
            self.write_node(&mut output, root, 0);
        }
        output
    }

    fn write_node(&self, output: &mut String, task: &HierarchicalTask, depth: usize) {
        let indent = "  ".repeat(depth);
        output.push_str(&format!("{indent}- [{}] {}\n", task.status, task.title));
        for child in self.children(task.id) {
            self.write_node(output, child, depth + 1);
        }
    }
}

impl Default for TaskTree {
    fn default() -> Self {
        Self::new()
    }
}

// ── #240: Time Tracking ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TimeEntry {
    pub task_id: usize,
    pub estimated_hours: f64,
    pub actual_hours: f64,
    pub started_at: u64,
    pub completed_at: Option<u64>,
}

#[derive(Default)]
pub struct TimeTracker {
    entries: Vec<TimeEntry>,
}

impl TimeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_task(&mut self, task_id: usize, estimated: f64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.entries.push(TimeEntry {
            task_id,
            estimated_hours: estimated,
            actual_hours: 0.0,
            started_at: now,
            completed_at: None,
        });
    }

    pub fn complete_task(&mut self, task_id: usize, actual: f64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Some(e) = self
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.task_id == task_id && e.completed_at.is_none())
        {
            e.actual_hours = actual;
            e.completed_at = Some(now);
        }
    }

    /// Ratio of total estimated to total actual for completed tasks.
    pub fn velocity(&self) -> f64 {
        let completed: Vec<&TimeEntry> = self
            .entries
            .iter()
            .filter(|e| e.completed_at.is_some() && e.actual_hours > 0.0)
            .collect();
        if completed.is_empty() {
            return 1.0;
        }
        let est: f64 = completed.iter().map(|e| e.estimated_hours).sum();
        let act: f64 = completed.iter().map(|e| e.actual_hours).sum();
        if act == 0.0 {
            1.0
        } else {
            est / act
        }
    }

    pub fn total_estimated(&self) -> f64 {
        self.entries.iter().map(|e| e.estimated_hours).sum()
    }

    pub fn total_actual(&self) -> f64 {
        self.entries.iter().map(|e| e.actual_hours).sum()
    }

    /// Tasks that are still running and have exceeded their estimate.
    pub fn overdue_tasks(&self) -> Vec<usize> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.entries
            .iter()
            .filter(|e| {
                e.completed_at.is_none()
                    && (now.saturating_sub(e.started_at)) as f64 / 3600.0 > e.estimated_hours
            })
            .map(|e| e.task_id)
            .collect()
    }
}

// ── #245: SRE Agent Mode ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SreAlert {
    pub id: String,
    pub severity: String,
    pub source: String,
    pub message: String,
    pub timestamp: u64,
    pub acknowledged: bool,
}

#[derive(Debug, Clone)]
pub struct Runbook {
    pub name: String,
    pub trigger_pattern: String,
    pub steps: Vec<String>,
}

#[derive(Default)]
pub struct SreAgent {
    alerts: Vec<SreAlert>,
    runbooks: Vec<Runbook>,
}

impl SreAgent {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest_alert(&mut self, alert: SreAlert) {
        self.alerts.push(alert);
    }

    /// Find the first runbook whose trigger pattern appears in the alert.
    pub fn match_runbook(&self, alert: &SreAlert) -> Option<&Runbook> {
        let msg = alert.message.to_lowercase();
        let src = alert.source.to_lowercase();
        self.runbooks.iter().find(|rb| {
            let p = rb.trigger_pattern.to_lowercase();
            msg.contains(&p) || src.contains(&p)
        })
    }

    pub fn pending_alerts(&self) -> Vec<&SreAlert> {
        self.alerts.iter().filter(|a| !a.acknowledged).collect()
    }

    pub fn acknowledge(&mut self, alert_id: &str) {
        if let Some(a) = self.alerts.iter_mut().find(|a| a.id == alert_id) {
            a.acknowledged = true;
        }
    }

    pub fn add_runbook(&mut self, runbook: Runbook) {
        self.runbooks.push(runbook);
    }
}

// ── #246: Progress Inference ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct InferredProgress {
    pub task_id: usize,
    pub percentage: f64,
    pub confidence: f64,
    pub evidence: Vec<String>,
}

pub struct ProgressInferrer;

impl ProgressInferrer {
    /// Estimate progress from git commit messages referencing a task title.
    pub fn infer_from_commits(task_title: &str, commit_messages: &[String]) -> InferredProgress {
        if commit_messages.is_empty() {
            return InferredProgress {
                task_id: 0,
                percentage: 0.0,
                confidence: 0.0,
                evidence: Vec::new(),
            };
        }
        let title_lower = task_title.to_lowercase();
        let title_words: Vec<&str> = title_lower.split_whitespace().collect();
        let done_kws = [
            "done",
            "complete",
            "finish",
            "implement",
            "close",
            "resolve",
        ];

        let mut evidence = Vec::new();
        let mut matching = 0usize;
        let mut completion_hints = 0usize;

        for msg in commit_messages {
            let lower = msg.to_lowercase();
            let relevant = title_words.iter().any(|w| lower.contains(*w));
            if relevant {
                matching += 1;
                evidence.push(msg.clone());
                if done_kws.iter().any(|kw| lower.contains(kw)) {
                    completion_hints += 1;
                }
            }
        }

        let confidence = matching as f64 / commit_messages.len() as f64;
        let percentage = if matching == 0 {
            0.0
        } else {
            completion_hints as f64 / matching as f64 * 100.0
        };

        InferredProgress {
            task_id: 0,
            percentage,
            confidence,
            evidence,
        }
    }

    /// Progress from test suite pass rate (0–100).
    pub fn infer_from_tests(total: usize, passing: usize) -> f64 {
        if total == 0 {
            return 0.0;
        }
        (passing as f64 / total as f64 * 100.0).min(100.0)
    }

    /// Progress from file creation ratio (0–100).
    pub fn infer_from_files(files_planned: usize, files_created: usize) -> f64 {
        if files_planned == 0 {
            return 0.0;
        }
        (files_created as f64 / files_planned as f64 * 100.0).min(100.0)
    }

    /// Weighted average: 40% commits, 40% tests, 20% files.
    pub fn combined(commits: f64, tests: f64, files: f64) -> f64 {
        (0.4 * commits + 0.4 * tests + 0.2 * files).min(100.0)
    }
}

// ── Tests for #236–#237, #239–#240, #245–#246 ────────────────────────────────

#[cfg(test)]
mod feature_tests_236_246 {
    use super::*;

    // ── #236 PrdParser ────────────────────────────────────────────────────────

    #[test]
    fn prd_extract_sections_basic() {
        let md = "# Auth\nBuild login.\n## OAuth\nUse OAuth2.";
        let sections = PrdParser::extract_sections(md);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "Auth");
        assert!(sections[0].1.contains("Build login"));
        assert_eq!(sections[1].0, "OAuth");
    }

    #[test]
    fn prd_parse_sets_parent_id() {
        let md = "# Feature\nTop level.\n## Sub-feature\nChild task.";
        let tasks = PrdParser::parse(md);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].parent_id, None);
        assert_eq!(tasks[1].parent_id, Some(0));
    }

    #[test]
    fn prd_parse_extracts_priority() {
        let md = "# Task\npriority: high\nDo something.";
        let tasks = PrdParser::parse(md);
        assert_eq!(tasks[0].priority, 8);
    }

    #[test]
    fn prd_infer_dependencies_finds_reference() {
        let tasks = vec![
            PrdTask {
                id: 0,
                title: "Database setup".to_string(),
                description: "Set up the database.".to_string(),
                parent_id: None,
                priority: 5,
                complexity: 5,
                estimated_hours: 1.0,
                dependencies: vec![],
                tags: vec![],
            },
            PrdTask {
                id: 1,
                title: "API layer".to_string(),
                description: "Implement API after Database setup is complete.".to_string(),
                parent_id: None,
                priority: 5,
                complexity: 5,
                estimated_hours: 1.0,
                dependencies: vec![],
                tags: vec![],
            },
        ];
        let deps = PrdParser::infer_dependencies(&tasks);
        assert!(deps.contains(&(1, 0)));
    }

    // ── #237 TaskRecommender ──────────────────────────────────────────────────

    fn make_task(id: usize, priority: u8, complexity: u8, deps: Vec<usize>) -> PrdTask {
        PrdTask {
            id,
            title: format!("Task {id}"),
            description: String::new(),
            parent_id: None,
            priority,
            complexity,
            estimated_hours: 1.0,
            dependencies: deps,
            tags: vec![],
        }
    }

    #[test]
    fn recommender_excludes_completed() {
        let tasks = vec![make_task(0, 9, 1, vec![]), make_task(1, 5, 5, vec![])];
        let recs = TaskRecommender::recommend_next(&tasks, &[0]);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].task_id, 1);
    }

    #[test]
    fn recommender_dep_not_ready_scores_zero_component() {
        let tasks = vec![
            make_task(0, 8, 1, vec![99]), // dep 99 not completed
            make_task(1, 5, 5, vec![]),
        ];
        let recs = TaskRecommender::recommend_next(&tasks, &[]);
        // Task 1 should score higher because task 0's dep is not satisfied
        let id1 = recs.iter().find(|r| r.task_id == 1).unwrap();
        let id0 = recs.iter().find(|r| r.task_id == 0).unwrap();
        assert!(id1.score > id0.score);
    }

    #[test]
    fn recommender_score_formula() {
        // Single task: dep_ready=1 (no deps), priority=10 -> 1.0, complexity=1 -> 1.0
        let tasks = vec![make_task(0, 10, 1, vec![])];
        let recs = TaskRecommender::recommend_next(&tasks, &[]);
        let expected = 0.4 * 1.0 + 0.35 * 1.0 + 0.25 * 1.0;
        assert!((recs[0].score - expected).abs() < 1e-9);
    }

    // ── #239 TaskTree ─────────────────────────────────────────────────────────

    #[test]
    fn task_tree_add_and_get() {
        let mut tree = TaskTree::new();
        let root = tree.add_task("Root", None);
        let child = tree.add_task("Child", Some(root));
        assert_eq!(tree.get_task(root).unwrap().title, "Root");
        assert_eq!(tree.get_task(child).unwrap().parent_id, Some(root));
    }

    #[test]
    fn task_tree_depth() {
        let mut tree = TaskTree::new();
        let a = tree.add_task("A", None);
        let b = tree.add_task("B", Some(a));
        let c = tree.add_task("C", Some(b));
        assert_eq!(tree.depth(a), 0);
        assert_eq!(tree.depth(b), 1);
        assert_eq!(tree.depth(c), 2);
    }

    #[test]
    fn task_tree_children_and_subtree() {
        let mut tree = TaskTree::new();
        let root = tree.add_task("Root", None);
        let c1 = tree.add_task("C1", Some(root));
        let c2 = tree.add_task("C2", Some(root));
        let gc = tree.add_task("GC", Some(c1));
        assert_eq!(tree.children(root).len(), 2);
        let sub = tree.subtree(root);
        assert_eq!(sub.len(), 3);
        assert!(sub.iter().any(|t| t.id == gc));
    }

    #[test]
    fn task_tree_progress() {
        let mut tree = TaskTree::new();
        let root = tree.add_task("Root", None);
        let c1 = tree.add_task("C1", Some(root));
        let c2 = tree.add_task("C2", Some(root));
        tree.tasks.get_mut(&c1).unwrap().status = "done".to_string();
        let _ = c2;
        assert!((tree.progress(root) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn task_tree_to_tree_string() {
        let mut tree = TaskTree::new();
        let root = tree.add_task("Root", None);
        tree.add_task("Child", Some(root));
        let s = tree.to_tree_string();
        assert!(s.contains("Root"));
        assert!(s.contains("Child"));
        assert!(s.contains("  -")); // indented child
    }

    // ── #240 TimeTracker ──────────────────────────────────────────────────────

    #[test]
    fn time_tracker_start_complete_velocity() {
        let mut tracker = TimeTracker::new();
        tracker.start_task(1, 4.0);
        tracker.complete_task(1, 2.0);
        // velocity = 4.0 / 2.0 = 2.0
        assert!((tracker.velocity() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn time_tracker_totals() {
        let mut tracker = TimeTracker::new();
        tracker.start_task(1, 3.0);
        tracker.complete_task(1, 2.0);
        tracker.start_task(2, 5.0);
        tracker.complete_task(2, 6.0);
        assert!((tracker.total_estimated() - 8.0).abs() < 1e-9);
        assert!((tracker.total_actual() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn time_tracker_no_completed_velocity_one() {
        let tracker = TimeTracker::new();
        assert!((tracker.velocity() - 1.0).abs() < 1e-9);
    }

    // ── #245 SreAgent ─────────────────────────────────────────────────────────

    fn make_alert(id: &str, msg: &str) -> SreAlert {
        SreAlert {
            id: id.to_string(),
            severity: "critical".to_string(),
            source: "prometheus".to_string(),
            message: msg.to_string(),
            timestamp: 0,
            acknowledged: false,
        }
    }

    #[test]
    fn sre_agent_ingest_and_pending() {
        let mut agent = SreAgent::new();
        agent.ingest_alert(make_alert("a1", "disk full"));
        agent.ingest_alert(make_alert("a2", "cpu spike"));
        assert_eq!(agent.pending_alerts().len(), 2);
    }

    #[test]
    fn sre_agent_acknowledge() {
        let mut agent = SreAgent::new();
        agent.ingest_alert(make_alert("a1", "disk full"));
        agent.acknowledge("a1");
        assert_eq!(agent.pending_alerts().len(), 0);
    }

    #[test]
    fn sre_agent_match_runbook() {
        let mut agent = SreAgent::new();
        agent.add_runbook(Runbook {
            name: "disk-runbook".to_string(),
            trigger_pattern: "disk full".to_string(),
            steps: vec!["Check disk".to_string(), "Clean up".to_string()],
        });
        let alert = make_alert("a1", "disk full on /var");
        let rb = agent.match_runbook(&alert);
        assert!(rb.is_some());
        assert_eq!(rb.unwrap().name, "disk-runbook");
    }

    #[test]
    fn sre_agent_no_runbook_match() {
        let mut agent = SreAgent::new();
        agent.add_runbook(Runbook {
            name: "disk-runbook".to_string(),
            trigger_pattern: "disk full".to_string(),
            steps: vec![],
        });
        let alert = make_alert("a1", "network timeout");
        assert!(agent.match_runbook(&alert).is_none());
    }

    // ── #246 ProgressInferrer ─────────────────────────────────────────────────

    #[test]
    fn progress_infer_from_commits_matching() {
        let msgs = vec![
            "implement auth login".to_string(),
            "fix auth token bug".to_string(),
            "unrelated commit".to_string(),
        ];
        let p = ProgressInferrer::infer_from_commits("auth", &msgs);
        assert!(p.confidence > 0.0);
        assert_eq!(p.evidence.len(), 2);
    }

    #[test]
    fn progress_infer_from_commits_empty() {
        let p = ProgressInferrer::infer_from_commits("auth", &[]);
        assert_eq!(p.percentage, 0.0);
        assert_eq!(p.confidence, 0.0);
    }

    #[test]
    fn progress_infer_from_tests() {
        assert!((ProgressInferrer::infer_from_tests(10, 8) - 80.0).abs() < 1e-9);
        assert_eq!(ProgressInferrer::infer_from_tests(0, 0), 0.0);
    }

    #[test]
    fn progress_infer_from_files() {
        assert!((ProgressInferrer::infer_from_files(4, 2) - 50.0).abs() < 1e-9);
        assert!((ProgressInferrer::infer_from_files(4, 5) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn progress_combined() {
        let c = ProgressInferrer::combined(100.0, 80.0, 60.0);
        let expected = 0.4 * 100.0 + 0.4 * 80.0 + 0.2 * 60.0;
        assert!((c - expected).abs() < 1e-9);
    }
}

// ── #259: AgentScaffolder ─────────────────────────────────────────────────────

pub struct AgentScaffoldConfig {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub trigger_phrases: Vec<String>,
    pub persona: String,
    pub instructions: Vec<String>,
}

pub struct AgentScaffolder;

impl AgentScaffolder {
    const TOOL_SETS: &'static [(&'static str, &'static [&'static str])] = &[
        ("read-only", &["read", "search"]),
        ("standard", &["shell", "read", "edit", "search"]),
        (
            "full",
            &["shell", "read", "edit", "search", "browser", "mcp"],
        ),
    ];

    pub fn available_tool_sets() -> Vec<(&'static str, &'static [&'static str])> {
        Self::TOOL_SETS.to_vec()
    }

    pub fn suggest_triggers(description: &str) -> Vec<String> {
        let lower = description.to_lowercase();
        let mut triggers = Vec::new();

        let keyword_map: &[(&str, &[&str])] = &[
            ("review", &["review this", "check this", "look at this"]),
            ("create", &["create a", "generate a", "build a", "make a"]),
            ("analyze", &["analyze this", "examine this", "inspect"]),
            (
                "debug",
                &["debug this", "fix this bug", "why is this failing"],
            ),
            (
                "refactor",
                &["refactor this", "clean up this", "improve this"],
            ),
            ("test", &["write tests for", "add tests to", "test this"]),
            ("document", &["document this", "add docs to", "write docs"]),
            ("deploy", &["deploy this", "ship this", "release this"]),
            ("migrate", &["migrate this", "upgrade this", "convert this"]),
            (
                "optimize",
                &["optimize this", "make this faster", "improve performance"],
            ),
        ];

        for (keyword, phrases) in keyword_map {
            if lower.contains(keyword) {
                for phrase in *phrases {
                    triggers.push(phrase.to_string());
                }
            }
        }

        if triggers.is_empty() {
            triggers.push(format!("help me with {}", description.to_lowercase()));
        }

        triggers
    }

    pub fn generate(config: &AgentScaffoldConfig) -> String {
        let tools_str = config
            .tools
            .iter()
            .map(|t| format!("'{t}'"))
            .collect::<Vec<_>>()
            .join(", ");

        let triggers_block = config
            .trigger_phrases
            .iter()
            .map(|t| format!("- '{t}'"))
            .collect::<Vec<_>>()
            .join("\\n");

        let description_yaml = format!(
            "\"When to invoke {}\\n\\nTrigger phrases:\\n{}\\n\\nExamples:\\n- User says '{}' → invoke this agent\"",
            config.description,
            triggers_block,
            config.trigger_phrases.first().cloned().unwrap_or_default()
        );

        let model_line = match &config.model {
            Some(m) => format!("\nmodel: {m}"),
            None => String::new(),
        };

        let title = to_title_case(&config.name);

        let instructions_md = config
            .instructions
            .iter()
            .enumerate()
            .map(|(i, step)| format!("{}. {step}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "---\ndescription: {description_yaml}\nname: {name}\ntools: [{tools_str}]{model_line}\n---\n\n\
# {title}\n\n\
{persona}\n\n\
## When Invoked\n{instructions_md}\n\n\
## Quality Checklist\n\
- [ ] Understood the full context before acting\n\
- [ ] Solution addresses the root cause, not just symptoms\n\
- [ ] Changes are minimal and targeted\n",
            name = config.name,
            persona = config.persona,
        )
    }

    pub fn quick_generate(name: &str, description: &str) -> String {
        let triggers = Self::suggest_triggers(description);
        let config = AgentScaffoldConfig {
            name: name.to_string(),
            description: description.to_string(),
            tools: vec![
                "shell".into(),
                "read".into(),
                "edit".into(),
                "search".into(),
            ],
            model: None,
            trigger_phrases: triggers,
            persona: format!("You are a senior engineer with expertise in {description}."),
            instructions: vec![
                "First, understand the full context of the request.".to_string(),
                "Then, plan the approach before executing.".to_string(),
                "Finally, verify your output meets the requirements.".to_string(),
            ],
        };
        Self::generate(&config)
    }
}

// ── #260: SkillScaffolder ─────────────────────────────────────────────────────

pub struct SkillScaffoldConfig {
    pub name: String,
    pub description: String,
    pub trigger_phrases: Vec<String>,
    pub steps: Vec<String>,
    pub examples: Vec<(String, String)>,
    pub tools_needed: Vec<String>,
}

pub struct SkillScaffolder;

impl SkillScaffolder {
    pub fn generate(config: &SkillScaffoldConfig) -> String {
        let triggers_inline = config.trigger_phrases.join("', '");
        let description_yaml = format!(
            "\"{}. Trigger on: '{triggers_inline}'.\"",
            config.description
        );

        let title = to_title_case(&config.name);

        let triggers_md = config
            .trigger_phrases
            .iter()
            .map(|t| format!("- {t}"))
            .collect::<Vec<_>>()
            .join("\n");

        let steps_md = config
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {s}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        let tools_md = if config.tools_needed.is_empty() {
            String::new()
        } else {
            let list = config
                .tools_needed
                .iter()
                .map(|t| format!("- {t}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!("\n## Tools Required\n{list}\n")
        };

        let examples_md = config
            .examples
            .iter()
            .enumerate()
            .map(|(i, (inp, out))| {
                format!("### Example {}\n**Input:** {inp}\n**Output:** {out}", i + 1)
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let examples_section = if examples_md.is_empty() {
            String::new()
        } else {
            format!("\n## Examples\n{examples_md}\n")
        };

        format!(
            "---\nname: {name}\ndescription: {description_yaml}\n---\n\n\
# {title}\n\n\
## When to Use\n{triggers_md}\n\n\
## Steps\n{steps_md}\n\
{tools_md}\
{examples_section}",
            name = config.name,
        )
    }

    pub fn quick_generate(name: &str, description: &str) -> String {
        let config = SkillScaffoldConfig {
            name: name.to_string(),
            description: description.to_string(),
            trigger_phrases: vec![format!("'{name}'"), format!("help with {name}")],
            steps: vec![
                "Gather context and understand the request.".to_string(),
                "Execute the core task.".to_string(),
                "Verify and summarize the result.".to_string(),
            ],
            examples: Vec::new(),
            tools_needed: Vec::new(),
        };
        Self::generate(&config)
    }

    /// Extract a skill definition from a chat history by distilling key steps.
    pub fn from_conversation(messages: &[String]) -> String {
        let mut steps: Vec<String> = Vec::new();

        for msg in messages {
            let lower = msg.to_lowercase();
            // Heuristic: lines starting with action verbs are likely steps
            for line in msg.lines() {
                let trimmed = line.trim();
                let l = trimmed.to_lowercase();
                if l.starts_with("first")
                    || l.starts_with("then")
                    || l.starts_with("next")
                    || l.starts_with("finally")
                    || l.starts_with("step")
                    || (l.len() > 5
                        && (l.starts_with("run ")
                            || l.starts_with("create ")
                            || l.starts_with("add ")
                            || l.starts_with("update ")
                            || l.starts_with("check ")))
                {
                    steps.push(trimmed.to_string());
                }
                let _ = lower.len(); // suppress unused binding warning
            }
        }

        if steps.is_empty() {
            steps.push("Review the conversation context.".to_string());
            steps.push("Execute the identified task.".to_string());
        }

        let config = SkillScaffoldConfig {
            name: "extracted-skill".to_string(),
            description: "Skill extracted from conversation history.".to_string(),
            trigger_phrases: vec!["extracted skill".to_string()],
            steps,
            examples: Vec::new(),
            tools_needed: Vec::new(),
        };
        Self::generate(&config)
    }
}

// ── #261: InstructionsScaffolder ─────────────────────────────────────────────

pub struct InstructionsConfig {
    pub project_name: String,
    pub project_type: String,
    pub languages: Vec<String>,
    pub build_command: String,
    pub test_command: String,
    pub lint_command: String,
    pub architecture_notes: Vec<String>,
    pub coding_standards: Vec<String>,
    pub important_files: Vec<String>,
    pub custom_rules: Vec<String>,
}

pub struct InstructionsScaffolder;

impl InstructionsScaffolder {
    pub fn generate(config: &InstructionsConfig) -> String {
        let langs = config.languages.join(", ");

        let arch_md = if config.architecture_notes.is_empty() {
            "- No architecture notes provided.".to_string()
        } else {
            config
                .architecture_notes
                .iter()
                .map(|n| format!("- {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let standards_md = if config.coding_standards.is_empty() {
            "- Follow language idioms and best practices.".to_string()
        } else {
            config
                .coding_standards
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let files_md = if config.important_files.is_empty() {
            "- No important files specified.".to_string()
        } else {
            config
                .important_files
                .iter()
                .map(|f| format!("- `{f}`"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let rules_md = if config.custom_rules.is_empty() {
            "- Always run tests before committing.".to_string()
        } else {
            config
                .custom_rules
                .iter()
                .map(|r| format!("- {r}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        format!(
            "# Project Instructions\n\n\
## Project Overview\n\
- Name: {name}\n\
- Type: {project_type}\n\
- Languages: {langs}\n\n\
## Build & Test\n\
- Build: `{build}`\n\
- Test: `{test}`\n\
- Lint: `{lint}`\n\n\
## Architecture\n{arch_md}\n\n\
## Coding Standards\n{standards_md}\n\n\
## Important Files\n{files_md}\n\n\
## Rules\n{rules_md}\n",
            name = config.project_name,
            project_type = config.project_type,
            build = config.build_command,
            test = config.test_command,
            lint = config.lint_command,
        )
    }

    pub fn auto_detect(
        project_root: &str,
        languages: &[String],
        file_count: usize,
    ) -> InstructionsConfig {
        let is_rust = languages.iter().any(|l| l.eq_ignore_ascii_case("rust"));
        let is_python = languages.iter().any(|l| l.eq_ignore_ascii_case("python"));
        let is_ts = languages
            .iter()
            .any(|l| l.eq_ignore_ascii_case("typescript") || l.eq_ignore_ascii_case("ts"));

        let project_type = if is_rust && is_ts {
            "Rust + TypeScript".to_string()
        } else if is_rust {
            "Rust".to_string()
        } else if is_python {
            "Python".to_string()
        } else if is_ts {
            "TypeScript".to_string()
        } else {
            "Unknown".to_string()
        };

        let (build, test, lint) = if is_rust {
            (
                "cargo build".into(),
                "cargo test --workspace".into(),
                "cargo clippy -- -D warnings".into(),
            )
        } else if is_python {
            (
                "pip install -e .".into(),
                "pytest".into(),
                "ruff check .".into(),
            )
        } else if is_ts {
            ("npm run build".into(), "npm test".into(), "eslint .".into())
        } else {
            ("make build".into(), "make test".into(), "make lint".into())
        };

        let arch_notes = vec![
            format!("Project root: {project_root}"),
            format!("Approximate file count: {file_count}"),
        ];

        InstructionsConfig {
            project_name: project_root
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("project")
                .to_string(),
            project_type,
            languages: languages.to_vec(),
            build_command: build,
            test_command: test,
            lint_command: lint,
            architecture_notes: arch_notes,
            coding_standards: Vec::new(),
            important_files: Vec::new(),
            custom_rules: Vec::new(),
        }
    }

    pub fn template_for(project_type: &str) -> String {
        let config = match project_type.to_lowercase().as_str() {
            "rust" => InstructionsConfig {
                project_name: "my-rust-project".into(),
                project_type: "Rust".into(),
                languages: vec!["Rust".into()],
                build_command: "cargo build".into(),
                test_command: "cargo test --workspace".into(),
                lint_command: "cargo clippy -- -D warnings && cargo fmt --check".into(),
                architecture_notes: vec![
                    "Organized as a Cargo workspace.".into(),
                    "Each crate has a single responsibility.".into(),
                ],
                coding_standards: vec![
                    "Use rustfmt for formatting.".into(),
                    "No clippy warnings allowed (enforced in CI).".into(),
                    "Prefer owned types in public APIs.".into(),
                ],
                important_files: vec![
                    "Cargo.toml — Workspace manifest".into(),
                    "src/main.rs — Entry point".into(),
                ],
                custom_rules: vec![
                    "Always run `cargo fmt --all` before committing.".into(),
                    "All public APIs must have doc comments.".into(),
                ],
            },
            "python" => InstructionsConfig {
                project_name: "my-python-project".into(),
                project_type: "Python".into(),
                languages: vec!["Python".into()],
                build_command: "pip install -e '.[dev]'".into(),
                test_command: "pytest".into(),
                lint_command: "ruff check . && mypy .".into(),
                architecture_notes: vec![
                    "Uses a src-layout for packaging.".into(),
                    "Type annotations required on all public functions.".into(),
                ],
                coding_standards: vec![
                    "Ruff enforces PEP 8 and import ordering.".into(),
                    "mypy runs in strict mode.".into(),
                    "Use virtual environments (venv).".into(),
                ],
                important_files: vec![
                    "pyproject.toml — Project config and dependencies".into(),
                    "src/ — Main package source".into(),
                ],
                custom_rules: vec![
                    "Never commit secrets — use environment variables.".into(),
                    "All tests go in the tests/ directory.".into(),
                ],
            },
            "typescript" => InstructionsConfig {
                project_name: "my-ts-project".into(),
                project_type: "TypeScript".into(),
                languages: vec!["TypeScript".into()],
                build_command: "npm run build".into(),
                test_command: "npx vitest".into(),
                lint_command: "eslint . && prettier --check .".into(),
                architecture_notes: vec![
                    "Strict TypeScript mode enabled.".into(),
                    "ES modules throughout.".into(),
                ],
                coding_standards: vec![
                    "No `any` types.".into(),
                    "Prettier enforces formatting.".into(),
                    "ESLint enforces style rules.".into(),
                ],
                important_files: vec![
                    "tsconfig.json — TypeScript configuration".into(),
                    "package.json — Dependencies".into(),
                ],
                custom_rules: vec![
                    "Run `npm run lint` before committing.".into(),
                    "Prefer named exports over default exports.".into(),
                ],
            },
            "react" => InstructionsConfig {
                project_name: "my-react-app".into(),
                project_type: "React + TypeScript".into(),
                languages: vec!["TypeScript".into(), "CSS".into()],
                build_command: "npm run build".into(),
                test_command: "npx vitest".into(),
                lint_command: "eslint . && prettier --check .".into(),
                architecture_notes: vec![
                    "Component-based architecture.".into(),
                    "State managed via React hooks.".into(),
                    "No class components — functional only.".into(),
                ],
                coding_standards: vec![
                    "Each component in its own file.".into(),
                    "Use React Testing Library for UI tests.".into(),
                    "CSS Modules for scoped styles.".into(),
                ],
                important_files: vec![
                    "src/App.tsx — Root component".into(),
                    "src/components/ — Reusable components".into(),
                ],
                custom_rules: vec![
                    "Never mutate state directly.".into(),
                    "Always handle loading and error states in UI.".into(),
                ],
            },
            "fullstack" => InstructionsConfig {
                project_name: "my-fullstack-app".into(),
                project_type: "Fullstack (Backend + Frontend)".into(),
                languages: vec!["TypeScript".into(), "Rust".into()],
                build_command: "cargo build && npm run build".into(),
                test_command: "cargo test --workspace && npx vitest".into(),
                lint_command: "cargo clippy -- -D warnings && eslint .".into(),
                architecture_notes: vec![
                    "Backend: Rust API server.".into(),
                    "Frontend: TypeScript/React SPA.".into(),
                    "API contract defined with OpenAPI spec.".into(),
                ],
                coding_standards: vec![
                    "Backend follows Rust workspace conventions.".into(),
                    "Frontend follows React + TypeScript conventions.".into(),
                    "All API endpoints must have integration tests.".into(),
                ],
                important_files: vec![
                    "backend/src/main.rs — API entry point".into(),
                    "frontend/src/App.tsx — Frontend root".into(),
                    "api/openapi.yaml — API contract".into(),
                ],
                custom_rules: vec![
                    "Always validate API schema changes don't break the frontend.".into(),
                    "Run both backend and frontend tests in CI.".into(),
                ],
            },
            _ => InstructionsConfig {
                project_name: "my-project".into(),
                project_type: project_type.to_string(),
                languages: vec![project_type.to_string()],
                build_command: "make build".into(),
                test_command: "make test".into(),
                lint_command: "make lint".into(),
                architecture_notes: vec!["Add architecture notes here.".into()],
                coding_standards: vec!["Follow language idioms and best practices.".into()],
                important_files: vec!["README.md — Project documentation".into()],
                custom_rules: vec!["Always run tests before committing.".into()],
            },
        };
        Self::generate(&config)
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

fn to_title_case(s: &str) -> String {
    s.split(['-', '_', ' '])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ── Tests for #259–#261 ───────────────────────────────────────────────────────

#[cfg(test)]
mod feature_tests_259_261 {
    use super::*;

    // ── #259 AgentScaffolder ──────────────────────────────────────────────────

    #[test]
    fn agent_available_tool_sets_returns_three() {
        let sets = AgentScaffolder::available_tool_sets();
        assert_eq!(sets.len(), 3);
        let names: Vec<&str> = sets.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"read-only"));
        assert!(names.contains(&"standard"));
        assert!(names.contains(&"full"));
    }

    #[test]
    fn agent_tool_set_contents() {
        let sets = AgentScaffolder::available_tool_sets();
        let standard = sets.iter().find(|(n, _)| *n == "standard").unwrap();
        assert!(standard.1.contains(&"shell"));
        assert!(standard.1.contains(&"edit"));
        let full = sets.iter().find(|(n, _)| *n == "full").unwrap();
        assert!(full.1.contains(&"browser"));
        assert!(full.1.contains(&"mcp"));
    }

    #[test]
    fn agent_suggest_triggers_review() {
        let triggers = AgentScaffolder::suggest_triggers("code review tool");
        assert!(!triggers.is_empty());
        assert!(triggers.iter().any(|t| t.contains("review")));
    }

    #[test]
    fn agent_suggest_triggers_fallback() {
        let triggers = AgentScaffolder::suggest_triggers("xyzzy obscure thing");
        assert!(!triggers.is_empty());
    }

    #[test]
    fn agent_generate_contains_required_sections() {
        let config = AgentScaffoldConfig {
            name: "test-agent".to_string(),
            description: "A test agent".to_string(),
            tools: vec!["read".into(), "search".into()],
            model: None,
            trigger_phrases: vec!["test this".to_string()],
            persona: "You are a senior tester.".to_string(),
            instructions: vec!["Step one.".to_string(), "Step two.".to_string()],
        };
        let out = AgentScaffolder::generate(&config);
        assert!(out.contains("---"));
        assert!(out.contains("name: test-agent"));
        assert!(out.contains("tools: ['read', 'search']"));
        assert!(out.contains("# Test Agent"));
        assert!(out.contains("You are a senior tester."));
        assert!(out.contains("## When Invoked"));
        assert!(out.contains("1. Step one."));
        assert!(out.contains("2. Step two."));
        assert!(out.contains("## Quality Checklist"));
    }

    #[test]
    fn agent_generate_with_model() {
        let config = AgentScaffoldConfig {
            name: "my-agent".to_string(),
            description: "desc".to_string(),
            tools: vec!["shell".into()],
            model: Some("claude-opus-4".to_string()),
            trigger_phrases: vec![],
            persona: "You are an expert.".to_string(),
            instructions: vec![],
        };
        let out = AgentScaffolder::generate(&config);
        assert!(out.contains("model: claude-opus-4"));
    }

    #[test]
    fn agent_generate_no_model_omits_model_line() {
        let config = AgentScaffoldConfig {
            name: "my-agent".to_string(),
            description: "desc".to_string(),
            tools: vec![],
            model: None,
            trigger_phrases: vec![],
            persona: "Expert.".to_string(),
            instructions: vec![],
        };
        let out = AgentScaffolder::generate(&config);
        assert!(!out.contains("model:"));
    }

    #[test]
    fn agent_quick_generate_valid_output() {
        let out = AgentScaffolder::quick_generate("my-agent", "reviews pull requests");
        assert!(out.contains("name: my-agent"));
        assert!(out.contains("# My Agent"));
        assert!(out.contains("reviews pull requests"));
        assert!(out.contains("## When Invoked"));
    }

    #[test]
    fn agent_title_case_kebab() {
        assert_eq!(to_title_case("my-agent-name"), "My Agent Name");
        assert_eq!(to_title_case("single"), "Single");
        assert_eq!(to_title_case("snake_case"), "Snake Case");
    }

    // ── #260 SkillScaffolder ──────────────────────────────────────────────────

    #[test]
    fn skill_generate_contains_required_sections() {
        let config = SkillScaffoldConfig {
            name: "my-skill".to_string(),
            description: "Does something useful".to_string(),
            trigger_phrases: vec!["do the thing".to_string(), "help me".to_string()],
            steps: vec!["First step.".to_string(), "Second step.".to_string()],
            examples: vec![("input text".to_string(), "output text".to_string())],
            tools_needed: vec!["bash".to_string()],
        };
        let out = SkillScaffolder::generate(&config);
        assert!(out.contains("name: my-skill"));
        assert!(out.contains("# My Skill"));
        assert!(out.contains("## When to Use"));
        assert!(out.contains("- do the thing"));
        assert!(out.contains("## Steps"));
        assert!(out.contains("1. First step."));
        assert!(out.contains("2. Second step."));
        assert!(out.contains("## Tools Required"));
        assert!(out.contains("- bash"));
        assert!(out.contains("## Examples"));
        assert!(out.contains("**Input:** input text"));
        assert!(out.contains("**Output:** output text"));
    }

    #[test]
    fn skill_generate_no_tools_no_tools_section() {
        let config = SkillScaffoldConfig {
            name: "minimal-skill".to_string(),
            description: "Minimal".to_string(),
            trigger_phrases: vec!["trigger".to_string()],
            steps: vec!["Do it.".to_string()],
            examples: vec![],
            tools_needed: vec![],
        };
        let out = SkillScaffolder::generate(&config);
        assert!(!out.contains("## Tools Required"));
        assert!(!out.contains("## Examples"));
    }

    #[test]
    fn skill_generate_description_has_triggers_inline() {
        let config = SkillScaffoldConfig {
            name: "s".to_string(),
            description: "My skill".to_string(),
            trigger_phrases: vec!["phrase a".to_string(), "phrase b".to_string()],
            steps: vec![],
            examples: vec![],
            tools_needed: vec![],
        };
        let out = SkillScaffolder::generate(&config);
        assert!(out.contains("phrase a"));
        assert!(out.contains("phrase b"));
    }

    #[test]
    fn skill_quick_generate_valid() {
        let out = SkillScaffolder::quick_generate("pdf-reader", "reads PDF files");
        assert!(out.contains("name: pdf-reader"));
        assert!(out.contains("# Pdf Reader"));
        assert!(out.contains("## Steps"));
    }

    #[test]
    fn skill_from_conversation_extracts_steps() {
        let msgs = vec![
            "First, read the file.".to_string(),
            "Then, parse the content.".to_string(),
            "Finally, return the result.".to_string(),
        ];
        let out = SkillScaffolder::from_conversation(&msgs);
        assert!(out.contains("## Steps"));
        assert!(out.contains("First, read the file."));
        assert!(out.contains("Then, parse the content."));
        assert!(out.contains("Finally, return the result."));
    }

    #[test]
    fn skill_from_conversation_empty_fallback() {
        let out = SkillScaffolder::from_conversation(&[]);
        assert!(out.contains("## Steps"));
        assert!(out.contains("Review the conversation context."));
    }

    // ── #261 InstructionsScaffolder ───────────────────────────────────────────

    #[test]
    fn instructions_generate_contains_all_sections() {
        let config = InstructionsConfig {
            project_name: "my-project".to_string(),
            project_type: "Rust".to_string(),
            languages: vec!["Rust".to_string()],
            build_command: "cargo build".to_string(),
            test_command: "cargo test".to_string(),
            lint_command: "cargo clippy".to_string(),
            architecture_notes: vec!["Single crate.".to_string()],
            coding_standards: vec!["Use rustfmt.".to_string()],
            important_files: vec!["src/lib.rs — Library root".to_string()],
            custom_rules: vec!["No unsafe code.".to_string()],
        };
        let out = InstructionsScaffolder::generate(&config);
        assert!(out.contains("# Project Instructions"));
        assert!(out.contains("- Name: my-project"));
        assert!(out.contains("- Type: Rust"));
        assert!(out.contains("- Languages: Rust"));
        assert!(out.contains("- Build: `cargo build`"));
        assert!(out.contains("- Test: `cargo test`"));
        assert!(out.contains("- Lint: `cargo clippy`"));
        assert!(out.contains("## Architecture"));
        assert!(out.contains("- Single crate."));
        assert!(out.contains("## Coding Standards"));
        assert!(out.contains("- Use rustfmt."));
        assert!(out.contains("## Important Files"));
        assert!(out.contains("`src/lib.rs — Library root`"));
        assert!(out.contains("## Rules"));
        assert!(out.contains("- No unsafe code."));
    }

    #[test]
    fn instructions_generate_defaults_when_empty() {
        let config = InstructionsConfig {
            project_name: "p".to_string(),
            project_type: "".to_string(),
            languages: vec![],
            build_command: "build".to_string(),
            test_command: "test".to_string(),
            lint_command: "lint".to_string(),
            architecture_notes: vec![],
            coding_standards: vec![],
            important_files: vec![],
            custom_rules: vec![],
        };
        let out = InstructionsScaffolder::generate(&config);
        assert!(out.contains("No architecture notes provided."));
        assert!(out.contains("Follow language idioms"));
        assert!(out.contains("No important files specified."));
        assert!(out.contains("Always run tests before committing."));
    }

    #[test]
    fn instructions_auto_detect_rust() {
        let cfg =
            InstructionsScaffolder::auto_detect("/home/user/myapp", &["Rust".to_string()], 42);
        assert_eq!(cfg.project_name, "myapp");
        assert_eq!(cfg.project_type, "Rust");
        assert!(cfg.build_command.contains("cargo"));
        assert!(cfg.test_command.contains("cargo test"));
        assert!(cfg.architecture_notes.iter().any(|n| n.contains("42")));
    }

    #[test]
    fn instructions_auto_detect_python() {
        let cfg = InstructionsScaffolder::auto_detect("/proj", &["Python".to_string()], 10);
        assert_eq!(cfg.project_type, "Python");
        assert!(cfg.test_command.contains("pytest"));
    }

    #[test]
    fn instructions_auto_detect_typescript() {
        let cfg = InstructionsScaffolder::auto_detect("/proj", &["TypeScript".to_string()], 5);
        assert_eq!(cfg.project_type, "TypeScript");
        assert!(cfg.build_command.contains("npm"));
    }

    #[test]
    fn instructions_auto_detect_rust_and_ts() {
        let cfg = InstructionsScaffolder::auto_detect(
            "/proj",
            &["Rust".to_string(), "TypeScript".to_string()],
            100,
        );
        assert_eq!(cfg.project_type, "Rust + TypeScript");
        assert!(cfg.build_command.contains("cargo"));
    }

    #[test]
    fn instructions_template_rust() {
        let out = InstructionsScaffolder::template_for("rust");
        assert!(out.contains("cargo build"));
        assert!(out.contains("cargo clippy"));
        assert!(out.contains("rustfmt"));
    }

    #[test]
    fn instructions_template_python() {
        let out = InstructionsScaffolder::template_for("python");
        assert!(out.contains("pytest"));
        assert!(out.contains("ruff"));
        assert!(out.contains("mypy"));
    }

    #[test]
    fn instructions_template_typescript() {
        let out = InstructionsScaffolder::template_for("typescript");
        assert!(out.contains("vitest"));
        assert!(out.contains("eslint"));
        assert!(out.contains("prettier"));
    }

    #[test]
    fn instructions_template_react() {
        let out = InstructionsScaffolder::template_for("react");
        assert!(out.contains("React"));
        assert!(out.contains("Testing Library"));
    }

    #[test]
    fn instructions_template_fullstack() {
        let out = InstructionsScaffolder::template_for("fullstack");
        assert!(out.contains("cargo"));
        assert!(out.contains("npm"));
        assert!(out.contains("openapi"));
    }

    #[test]
    fn instructions_template_unknown_fallback() {
        let out = InstructionsScaffolder::template_for("cobol");
        assert!(out.contains("cobol"));
        assert!(out.contains("make build"));

}
}
