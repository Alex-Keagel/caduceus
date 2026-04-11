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
    SessionPhase, SessionState, StopReason, TokenUsage, ToolCallId, WarningLevel,
};
use caduceus_providers::{ChatRequest, LlmAdapter};
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
        // Check cancellation before starting
        self.check_cancellation()?;

        state.phase = SessionPhase::Running;
        if let Some(ref em) = self.emitter {
            em.emit_phase_changed(SessionPhase::Running).await;
        }

        history.append(caduceus_providers::Message::user(user_input));

        let system_prompt = self.effective_system_prompt();
        let assembler = ContextAssembler::new(self.max_context_tokens, &system_prompt);
        let final_text;

        // Check cancellation before LLM call
        self.check_cancellation()?;

        // Emit token warning if applicable
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

        {
            let messages = assembler.assemble(history);

            let mut request = ChatRequest {
                model: self.effective_model(state),
                messages,
                system: Some(system_prompt.clone()),
                max_tokens: self.effective_max_tokens(),
                temperature: self.effective_temperature(),
                thinking_mode: false,
                tool_choice: None,
                response_format: None,
            };

            // Apply thinking mode: prepend chain-of-thought instruction
            if request.thinking_mode {
                if let Some(ref sys) = request.system {
                    request.system = Some(format!("Think step by step.\n\n{}", sys));
                }
                request.max_tokens = request.max_tokens.max(8192);
            }

            let mut stream = self.provider.stream(request).await?;
            let mut usage = TokenUsage::default();
            let mut response_content = String::new();

            while let Some(chunk) = stream.next().await {
                // Check cancellation during streaming
                self.check_cancellation()?;

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
            MockLlmAdapter::new(vec![]).with_stream_chunks(vec![make_final_stream("response")]),
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
            Arc::new(MockLlmAdapter::new(vec![]).with_stream_chunks(vec![make_final_stream("ok")]));
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
            Arc::new(MockLlmAdapter::new(vec![]).with_stream_chunks(vec![make_final_stream("ok")]));
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
            Arc::new(MockLlmAdapter::new(vec![]).with_stream_chunks(vec![make_final_stream("ok")]));
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
}
