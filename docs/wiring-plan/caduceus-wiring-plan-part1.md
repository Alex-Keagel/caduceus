# Caduceus Wiring Plan — Parts 1–3

> **Generated from:** All 10 spec files (claurst-blackbox, claurst-full, claw-code, e2b, hermes-ide, hermes-ide-supplement, open-multi-agent, qdrant, tree-sitter, zed-crdt)

---

## Part 1: Architecture Overview

### 1.1 The Six Layers

```
╔══════════════════════════════════════════════════════════════════════════════╗
║  LAYER 1 — PRESENTATION (hermes-ide specs)                                 ║
║  Tauri 2 + React/TypeScript frontend                                       ║
║  xterm.js terminals • CodeMirror editor • split panes • plugin host        ║
║  Tauri IPC bridge (invoke/event) to Rust backend                           ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  LAYER 2 — ORCHESTRATION (claw-code + claurst specs)                       ║
║  Rust agent harness engine                                                 ║
║  Conversation loop • Tool registry & dispatch • Permission enforcement     ║
║  Session persistence (JSONL + SQLite) • System prompt assembly              ║
║  Multi-provider LLM adapter layer (Anthropic/OpenAI/Gemini/local)          ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  LAYER 3 — WORKERS (open-multi-agent spec) [POST-V1]                      ║
║  Coordinator-driven multi-agent runtime                                    ║
║  Task DAG with dependency resolution • Agent pool with concurrency control ║
║  Team message bus + shared memory • Loop detection • Scheduler             ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  LAYER 4 — SANDBOX (E2B spec)                                              ║
║  Secure micro-VM execution                                                 ║
║  Process/PTY via gRPC-web • Filesystem CRUD • Networking/port-forward      ║
║  Snapshots • Volumes • Templates • MCP gateway                             ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  LAYER 5 — OMNISCIENCE (tree-sitter + qdrant specs)                        ║
║  AST parsing (tree-sitter): incremental parse, query-driven chunk extract  ║
║  Vector search (qdrant-edge): embedded EdgeShard, semantic code retrieval   ║
║  Chunking pipeline: parse → extract → embed → upsert → search             ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  LAYER 6 — MULTIPLAYER (Zed CRDT spec)                                     ║
║  RGA-based CRDT text buffer with Lamport timestamps                        ║
║  Anchor system for stable positions • Fragment-based tombstone model        ║
║  Rope data structure (B+ tree) • Version vectors • Remote selections       ║
╚══════════════════════════════════════════════════════════════════════════════╝
```

### 1.2 End-to-End Data Flow

```
User types prompt in React terminal
         │
         ▼
┌─────────────────────────────┐
│ 1. PRESENTATION             │  React SessionProvider dispatches
│    React → Tauri IPC        │  write_to_session(session_id, base64_data)
│    (invoke: write_to_session)│  or creates new orchestrator session
└────────────┬────────────────┘
             │ IPC invoke / Tauri event
             ▼
┌─────────────────────────────┐
│ 2. ORCHESTRATION            │  Rust engine receives prompt
│    System prompt assembly   │  ├─ Assembles system prompt + memory + tools
│    Conversation loop        │  ├─ Sends to LLM provider (streaming SSE)
│    Tool dispatch            │  ├─ Model returns text + tool_use blocks
│    Permission enforcement   │  ├─ Permission check (allow/deny/ask)
│                             │  └─ Dispatches tool calls
└────────────┬────────────────┘
             │ Tool execution requests
             ▼
┌─────────────────────────────┐
│ 5. OMNISCIENCE              │  Code intelligence tool calls:
│    Tree-sitter parse        │  ├─ grep_search / glob_search → local FS
│    Qdrant vector search     │  ├─ LSP → language server
│                             │  ├─ Semantic search → qdrant-edge query()
│                             │  └─ AST query → tree-sitter chunk extraction
└────────────┬────────────────┘
             │ Search results returned as tool_result
             ▼
┌─────────────────────────────┐
│ 2. ORCHESTRATION (cont.)    │  Model receives search results
│    Next model turn          │  ├─ Plans file edits / shell commands
│    Tool dispatch: edit/bash │  └─ Emits edit_file / bash tool calls
└────────────┬────────────────┘
             │ Execution dispatch
             ├──────────────────────────────────────┐
             ▼                                      ▼
┌─────────────────────────┐          ┌─────────────────────────┐
│ LOCAL EXECUTION         │          │ 4. SANDBOX (E2B)        │
│ (workspace-constrained) │          │ Secure micro-VM         │
│ File read/write/edit    │          │ ├─ sandbox.commands.run()│
│ Local bash with sandbox │          │ ├─ sandbox.files.write() │
│                         │          │ └─ sandbox.pty.create()  │
└────────────┬────────────┘          └────────────┬────────────┘
             │                                     │
             ▼                                     ▼
┌─────────────────────────────────────────────────────────────┐
│ 6. MULTIPLAYER (CRDT)                                       │
│    buffer.edit() produces Operation (EditOperation)          │
│    ├─ Lamport timestamp assigned                            │
│    ├─ Fragment inserted/deleted in CRDT tree                │
│    ├─ Operation broadcast to all replicas                   │
│    └─ Subscription fires → Patch<Edit> to React             │
└────────────┬────────────────────────────────────────────────┘
             │ Tauri event: buffer-changed
             ▼
┌─────────────────────────────┐
│ 1. PRESENTATION             │
│    React receives patch     │  ├─ CodeMirror/xterm updates in-place
│    Terminal streams output   │  ├─ Diff viewer shows changes
│    CRDT cursors rendered    │  └─ Remote AI cursor visible in editor
└─────────────────────────────┘
```

### 1.3 Cross-Layer Communication Matrix

| From ↓ / To → | Presentation | Orchestration | Workers | Sandbox | Omniscience | Multiplayer |
|----------------|:---:|:---:|:---:|:---:|:---:|:---:|
| **Presentation** | — | IPC invoke | — | — | — | CRDT ops |
| **Orchestration** | Events/stream | — | Task dispatch | Tool exec | Tool exec | buffer.edit() |
| **Workers** | — | Results | Msg bus | Tool exec | Tool exec | buffer.edit() |
| **Sandbox** | — | Tool result | — | — | — | FS watch → edit |
| **Omniscience** | — | Tool result | — | — | — | — |
| **Multiplayer** | Subscription | — | — | — | Reparse trigger | — |

---

## Part 2: Crate Map

### 2.1 `caduceus-core`

| Field | Value |
|-------|-------|
| **Purpose** | Shared types, configuration, session state, feature flags, auth, persistence |
| **Source specs** | claurst-full §3, claw-code §7, hermes-ide §3 |
| **Dependencies** | (foundation — no Caduceus deps) |

**Public API Surface:**

```rust
// Identity & Configuration
pub struct SessionId(pub String);
pub struct ProviderId(pub String);
pub struct ModelId(pub String);
pub struct BufferId(pub u64);

// Session State
pub enum SessionPhase {
    Creating, Initializing, ShellReady, LaunchingAgent,
    Idle, Busy, NeedsInput, Error, Closing, Disconnected, Destroyed,
}

pub struct SessionState {
    pub id: SessionId,
    pub phase: SessionPhase,
    pub working_directory: PathBuf,
    pub workspace_paths: Vec<PathBuf>,
    pub provider: Option<ProviderId>,
    pub model: Option<ModelId>,
    pub metrics: SessionMetrics,
    pub version: clock::Global,  // from CRDT layer
}

// Transcript
pub enum TranscriptEntry {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
    System(SystemMessage),
    Summary(SummaryMessage),
}

// Token Budget
pub struct TokenBudget {
    pub tokens_used: usize,
    pub context_window: usize,
    pub tokens_remaining: usize,
    pub fill_fraction: f64,
    pub warning_level: WarningLevel,
}

// Configuration (merged from file hierarchy)
pub trait ConfigSource {
    fn merge(&mut self, other: &Self);
}
pub struct CaduceusConfig { /* hooks, plugins, mcp, permissions, providers, etc. */ }

// Persistence
pub trait SessionStorage {
    fn save_session(&self, session: &SessionState) -> Result<()>;
    fn load_session(&self, id: &SessionId) -> Result<SessionState>;
    fn list_sessions(&self) -> Result<Vec<SessionId>>;
    fn save_transcript(&self, id: &SessionId, entry: &TranscriptEntry) -> Result<()>;
}

// Auth
pub enum AuthMethod { ApiKey, Bearer, AwsCredentials, OAuth, None }
pub struct StoredCredential { /* provider, method, secret, expiry */ }
pub trait AuthStore {
    fn get_credentials(&self, provider: &ProviderId) -> Result<StoredCredential>;
    fn store_credentials(&mut self, provider: &ProviderId, cred: StoredCredential) -> Result<()>;
}

// Memory
pub enum MemoryScope { Session, Project, Global }
pub struct MemoryEntry {
    pub scope: MemoryScope,
    pub scope_id: String,
    pub key: String,
    pub value: String,
    pub source: String,
    pub confidence: f64,
}

// Error types
pub enum CaduceusError {
    Auth(String), RateLimit(String), ContextOverflow(String),
    ProviderError(String), ToolError(String), IoError(std::io::Error),
    /* ... */
}
```

**Behavioral Contracts:**
- Transcript format is append-only JSONL, max 50 MB per file
- Config merges: later files override scalars, objects merge recursively, arrays replace
- Token warning at 80%, critical at 95%, compact recommendation at 90%, collapse at 97%

---

### 2.2 `caduceus-api`

| Field | Value |
|-------|-------|
| **Purpose** | Multi-provider LLM abstraction — streaming, tool calling, model registry |
| **Source specs** | claurst-full §5, claw-code §5, claurst-blackbox §1 |
| **Dependencies** | `caduceus-core` |

**Public API Surface:**

```rust
// Provider trait — the central abstraction
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn create_message(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError>;
    fn create_message_stream(&self, req: ProviderRequest)
        -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
    async fn health_check(&self) -> Result<(), ProviderError>;
    fn capabilities(&self) -> ProviderCapabilities;
}

// Normalized request/response
pub struct ProviderRequest {
    pub model: ModelId,
    pub messages: Vec<Message>,
    pub system_prompt: Option<String>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: Option<u32>,
    pub stream: bool,
    pub temperature: Option<f32>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub stop_sequences: Option<Vec<String>>,
}

pub struct ProviderResponse {
    pub id: String,
    pub content: Vec<ContentBlock>,
    pub model: ModelId,
    pub stop_reason: StopReason,
    pub usage: TokenUsage,
}

// Streaming events (provider-agnostic)
pub enum StreamEvent {
    MessageStart,
    ContentBlockStart { index: usize, block_type: BlockType },
    TextDelta { index: usize, text: String },
    ThinkingDelta { index: usize, text: String },
    InputJsonDelta { index: usize, json: String },
    ContentBlockStop { index: usize },
    MessageDelta { stop_reason: StopReason, usage: TokenUsage },
    MessageStop,
    Error(ProviderError),
}

// Stream accumulator
pub struct StreamAccumulator { /* reconstructs final message from deltas */ }

// Model registry
pub struct ModelRegistry { /* bundled snapshot + optional remote refresh */ }
pub struct ModelInfo {
    pub provider: ProviderId,
    pub model: ModelId,
    pub display_name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
}

// Provider error taxonomy
pub enum ProviderError {
    ContextOverflow, RateLimited, AuthFailed, QuotaExceeded,
    ModelNotFound, ServerError, InvalidRequest, ContentFiltered,
    StreamError, Other(String),
}
impl ProviderError {
    pub fn is_retryable(&self) -> bool;
}

// Capabilities
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub tool_calling: bool,
    pub thinking: bool,
    pub multimodal_input: bool,
    pub caching: bool,
    pub structured_output: bool,
    pub system_prompt_style: SystemPromptStyle,
}
```

**Behavioral Contracts:**
- 29+ overflow message patterns recognized across providers
- Retry: exponential backoff with jitter, bounded caps
- Model registry refreshes from `models.dev` at most every 5 minutes; falls back silently to bundled snapshot

---

### 2.3 `caduceus-tools`

| Field | Value |
|-------|-------|
| **Purpose** | 40+ built-in tools, tool registry, permission-aware dispatch |
| **Source specs** | claw-code §3, claurst-full §2 |
| **Dependencies** | `caduceus-core`, `caduceus-api`, `caduceus-sandbox`, `caduceus-omniscience` |

**Public API Surface:**

```rust
// Tool definition
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub permission_level: PermissionLevel,
}

pub enum PermissionLevel { ReadOnly, WorkspaceWrite, DangerFullAccess }

// Tool registry (three layers)
pub struct ToolRegistry {
    builtins: HashMap<String, Box<dyn Tool>>,
    runtime: HashMap<String, Box<dyn Tool>>,
    plugins: HashMap<String, Box<dyn Tool>>,
}
impl ToolRegistry {
    pub fn register_builtin(&mut self, tool: impl Tool);
    pub fn register_runtime(&mut self, tool: impl Tool);
    pub fn register_plugin(&mut self, tool: impl Tool) -> Result<()>; // rejects shadows
    pub fn get(&self, name: &str) -> Option<&dyn Tool>;
    pub fn list_definitions(&self) -> Vec<ToolDefinition>;
}

// Tool trait
#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: serde_json::Value, ctx: ToolContext) -> Result<ToolResult>;
}

pub struct ToolContext {
    pub session_id: SessionId,
    pub working_directory: PathBuf,
    pub permission_mode: PermissionMode,
    pub agent_id: Option<String>,
    pub abort_signal: CancellationToken,
}

pub struct ToolResult {
    pub output: String,      // JSON-stringified
    pub is_error: bool,
}

// Permission enforcement pipeline
pub enum PermissionDecision { Allow(Option<serde_json::Value>), Ask(String), Deny(String), Passthrough }
pub trait PermissionEnforcer {
    fn check(&self, tool: &str, input: &serde_json::Value, mode: PermissionMode) -> PermissionDecision;
}
```

**Built-in Tool Catalog:**
- **File:** `read_file`, `write_file`, `edit_file`, `NotebookEdit`
- **Shell:** `bash`, `PowerShell`, `REPL`
- **Search:** `glob_search`, `grep_search`, `ToolSearch`, `LSP`
- **Web:** `WebFetch`, `WebSearch`
- **Agentic:** `Agent`, `TaskCreate/Get/List/Stop/Update/Output`
- **Planning:** `EnterPlanMode`, `ExitPlanMode`
- **MCP:** `MCP`, `McpAuth`, `ListMcpResources`, `ReadMcpResource`
- **Worker:** `WorkerCreate/Get/Observe/ResolveTrust/AwaitReady/SendPrompt/Restart/Terminate`

**Behavioral Contracts:**
- Validate input → permission check → execute → render result/error
- Name collisions: plugin cannot shadow built-in; runtime cannot shadow built-in or plugin
- File reads reject >10 MiB and likely-binary files; writes reject >10 MiB
- Read-before-write enforcement via tracked `readFileState`

---

### 2.4 `caduceus-runtime`

| Field | Value |
|-------|-------|
| **Purpose** | Conversation engine, session management, system prompt assembly, compaction |
| **Source specs** | claw-code §4, claurst-full §§1–4, hermes-ide §2 |
| **Dependencies** | `caduceus-core`, `caduceus-api`, `caduceus-tools`, `caduceus-plugins` |

**Public API Surface:**

```rust
// Conversation engine
pub struct ConversationEngine {
    session: SessionState,
    transcript: Vec<TranscriptEntry>,
    tool_registry: ToolRegistry,
    provider: Box<dyn LlmProvider>,
    permission_enforcer: Box<dyn PermissionEnforcer>,
    hooks: HookRunner,
}

impl ConversationEngine {
    pub async fn run_turn(&mut self, user_input: String) -> Result<TurnResult>;
    // Inner loop:
    // 1. Build system prompt + history + tools + token budget
    // 2. Stream model response
    // 3. If tool_use blocks → dispatch tools → append results → loop
    // 4. Until no more tool calls or iteration limit
    
    pub async fn compact(&mut self, instructions: Option<String>) -> Result<()>;
    pub fn export_transcript(&self) -> String; // Markdown
}

pub struct TurnResult {
    pub assistant_text: String,
    pub tool_calls: Vec<ToolCallRecord>,
    pub usage: TokenUsage,
    pub stop_reason: StopReason,
}

// System prompt builder
pub struct SystemPromptBuilder;
impl SystemPromptBuilder {
    pub fn build(opts: SystemPromptOptions) -> String;
}
pub struct SystemPromptOptions {
    pub working_directory: PathBuf,
    pub memory_content: Option<String>,
    pub instruction_files: Vec<String>,
    pub output_style: Option<String>,
    pub coordinator_mode: bool,
    // ...
}

// Session persistence (JSONL)
pub struct JsonlSessionStore { /* path, workspace fingerprint */ }

// Hook system
pub struct HookRunner {
    pub pre_tool_hooks: Vec<HookCommand>,
    pub post_tool_hooks: Vec<HookCommand>,
}
// Hooks receive JSON on stdin, exit 0=allow, 2=deny
```

**Behavioral Contracts:**
- Multi-iteration loop until model stops requesting tools or iteration limit reached
- Auto-compaction at 85% of context window (configurable)
- Compaction preserves tool-use/tool-result boundaries
- Sessions persist as JSONL under `~/.caduceus/projects/{base64(cwd)}/{session_id}.jsonl`

---

### 2.5 `caduceus-sandbox`

| Field | Value |
|-------|-------|
| **Purpose** | E2B micro-VM integration for secure code execution |
| **Source specs** | e2b §§1–21 |
| **Dependencies** | `caduceus-core` |

**Public API Surface:**

```rust
// Sandbox client
pub struct SandboxClient {
    api_key: String,
    api_url: String,
    domain: String,
}

impl SandboxClient {
    pub async fn create(&self, opts: SandboxOpts) -> Result<Sandbox>;
    pub async fn connect(&self, id: &str) -> Result<Sandbox>;
    pub async fn list(&self, query: SandboxQuery) -> Result<Vec<SandboxInfo>>;
}

pub struct Sandbox {
    pub id: String,
    pub template_id: String,
    pub envd_access_token: String,
    pub domain: String,
    pub commands: CommandsApi,
    pub files: FilesApi,
    pub pty: PtyApi,
    pub git: GitApi,
}

// Process execution
impl CommandsApi {
    pub async fn run(&self, cmd: &str, opts: CommandOpts) -> Result<CommandResult>;
    pub async fn list(&self) -> Result<Vec<ProcessInfo>>;
    pub async fn kill(&self, pid: u32) -> Result<bool>;
    pub async fn send_stdin(&self, pid: u32, data: &[u8]) -> Result<()>;
}

pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
}

// Filesystem
impl FilesApi {
    pub async fn read(&self, path: &str) -> Result<Vec<u8>>;
    pub async fn write(&self, path: &str, content: &[u8]) -> Result<WriteInfo>;
    pub async fn list(&self, path: &str, depth: u32) -> Result<Vec<EntryInfo>>;
    pub async fn remove(&self, path: &str) -> Result<()>;
    pub async fn watch_dir(&self, path: &str, callback: impl Fn(FsEvent)) -> Result<WatchHandle>;
}

// Lifecycle
impl Sandbox {
    pub async fn kill(&self) -> Result<()>;
    pub async fn pause(&self) -> Result<bool>;
    pub async fn set_timeout(&self, timeout_ms: u64) -> Result<()>;
    pub async fn create_snapshot(&self) -> Result<SnapshotInfo>;
    pub fn get_host(&self, port: u16) -> String; // "{port}-{id}.{domain}"
}
```

**Owned Data Types:** `SandboxOpts`, `SandboxInfo`, `SandboxState` (Running/Paused/Killed), `CommandResult`, `CommandHandle`, `EntryInfo`, `WriteInfo`, `FsEvent`, `SnapshotInfo`, `SandboxMetrics`

**Behavioral Contracts:**
- Default sandbox timeout: 300s (5 min), max 24h (Pro)
- Commands run inside `/bin/bash -l -c <cmd>`
- envd daemon on port 49983, MCP gateway on port 50005
- gRPC-web (Connect protocol) over HTTPS to envd
- Keepalive ping every 50 seconds

---

### 2.6 `caduceus-omniscience`

| Field | Value |
|-------|-------|
| **Purpose** | AST parsing (tree-sitter) + vector search (qdrant-edge) for code intelligence |
| **Source specs** | tree-sitter §§1–14, qdrant §§1–14 |
| **Dependencies** | `caduceus-core` |

**Public API Surface:**

```rust
// === Tree-sitter integration ===

pub struct AstEngine {
    parsers: HashMap<String, Parser>,      // per-language
    queries: HashMap<String, Query>,       // chunk queries per-language
    trees: HashMap<PathBuf, Tree>,         // cached trees per-file
}

impl AstEngine {
    pub fn parse(&mut self, path: &Path, source: &[u8], language: &str) -> Result<&Tree>;
    pub fn reparse(&mut self, path: &Path, source: &[u8], edit: &InputEdit) -> Result<Vec<Range>>;
    pub fn extract_chunks(&self, path: &Path, source: &[u8]) -> Result<Vec<CodeChunk>>;
    pub fn changed_ranges(&self, path: &Path, old_tree: &Tree, new_tree: &Tree) -> Vec<Range>;
}

pub struct CodeChunk {
    pub file_path: PathBuf,
    pub language: String,
    pub symbol_kind: String,       // function, method, class, module, trait_impl
    pub symbol_name: Option<String>,
    pub container: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub byte_range: std::ops::Range<usize>,
    pub code: String,
    pub has_error: bool,
}

// === Qdrant-edge vector search ===

pub struct VectorIndex {
    shard: EdgeShard,
    embedding_dim: usize,
}

impl VectorIndex {
    pub fn open(path: &Path, dim: usize) -> Result<Self>;
    pub fn upsert_chunks(&self, chunks: Vec<IndexedChunk>) -> Result<()>;
    pub fn search(&self, query_embedding: Vec<f32>, filter: SearchFilter, limit: usize) -> Result<Vec<SearchHit>>;
    pub fn delete_by_file(&self, file_path: &str) -> Result<()>;
    pub fn optimize(&self) -> Result<()>;
    pub fn flush(&self) -> Result<()>;
}

pub struct IndexedChunk {
    pub id: u64,
    pub embedding: Vec<f32>,
    pub metadata: ChunkMetadata,
}

pub struct ChunkMetadata {
    pub repo: String,
    pub file_path: String,
    pub language: String,
    pub symbol_kind: String,
    pub symbol_name: Option<String>,
    pub container: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub hash: String,
    pub code: String,
}

pub struct SearchFilter {
    pub repo: Option<String>,
    pub language: Option<String>,
    pub file_path: Option<String>,
    pub symbol_kind: Option<String>,
}

pub struct SearchHit {
    pub score: f32,
    pub metadata: ChunkMetadata,
}

// === Combined code intelligence API ===

pub struct CodeIntelligence {
    ast: AstEngine,
    index: VectorIndex,
    embedder: Box<dyn Embedder>,
}

impl CodeIntelligence {
    pub async fn index_file(&mut self, path: &Path) -> Result<usize>; // returns chunk count
    pub async fn index_directory(&mut self, dir: &Path) -> Result<usize>;
    pub async fn semantic_search(&self, query: &str, filter: SearchFilter, limit: usize) -> Result<Vec<SearchHit>>;
    pub fn reindex_changed(&mut self, path: &Path, edit: &InputEdit) -> Result<()>;
}

#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
}
```

**Behavioral Contracts:**
- Parse with tree-sitter, extract chunks via query-defined semantic units (`@chunk.function`, etc.)
- Chunks target 40–300 tokens; one chunk per top-level symbol
- Qdrant-edge: Cosine distance, Float32, payload indexes on `repo`, `file_path`, `language`, `symbol_kind`
- Incremental re-index: `tree.edit()` → reparse with old tree → `changed_ranges()` → re-extract only affected chunks
- Optimize after bulk ingest; flush on checkpoints
- Error-tolerant: index valid chunks even when file has syntax errors (mark `has_error`)

---

### 2.7 `caduceus-crdt`

| Field | Value |
|-------|-------|
| **Purpose** | CRDT text buffer for real-time collaborative editing (human + AI) |
| **Source specs** | zed-crdt §§1–9 |
| **Dependencies** | (foundation — no Caduceus deps; may depend on `caduceus-core` for IDs) |

**Public API Surface:**

```rust
// Re-export clock primitives
pub use clock::{Lamport, ReplicaId, Global as VersionVector};

// Rope (B+ tree text storage)
pub struct Rope { /* SumTree<Chunk> */ }
impl Rope {
    pub fn new() -> Self;
    pub fn from(text: &str) -> Self;
    pub fn push(&mut self, text: &str);
    pub fn replace(&mut self, range: Range<usize>, text: &str);
    pub fn slice(&self, range: Range<usize>) -> Rope;
    pub fn len(&self) -> usize;
    pub fn offset_to_point(&self, offset: usize) -> Point;
    pub fn point_to_offset(&self, point: Point) -> usize;
    pub fn chunks_in_range(&self, range: Range<usize>) -> impl Iterator<Item = &str>;
}

// CRDT Buffer
pub struct Buffer {
    pub lamport_clock: Lamport,
    snapshot: BufferSnapshot,
    history: History,
    deferred_ops: OperationQueue<Operation>,
    subscriptions: Vec<Subscription>,
}

impl Buffer {
    pub fn new(replica_id: ReplicaId, text: &str) -> Self;
    
    // Local edit (returns operation for broadcast)
    pub fn edit<R: IntoIterator<Item = (Range<usize>, &str)>>(&mut self, edits: R) -> Operation;
    
    // Remote operation application
    pub fn apply_ops(&mut self, ops: impl IntoIterator<Item = Operation>);
    
    // Undo/redo
    pub fn undo(&mut self) -> Option<Operation>;
    pub fn redo(&mut self) -> Option<Operation>;
    
    // Transactions
    pub fn start_transaction(&mut self);
    pub fn end_transaction(&mut self);
    
    // Snapshot (immutable, thread-safe clone)
    pub fn snapshot(&self) -> BufferSnapshot;
    
    // Subscription
    pub fn subscribe(&mut self) -> Subscription;
    
    // Queries
    pub fn text(&self) -> String;
    pub fn text_for_range(&self, range: Range<usize>) -> String;
    pub fn len(&self) -> usize;
    pub fn version(&self) -> &VersionVector;
    
    // Anchors
    pub fn anchor_before(&self, offset: usize) -> Anchor;
    pub fn anchor_after(&self, offset: usize) -> Anchor;
    pub fn offset_for_anchor(&self, anchor: &Anchor) -> usize;
    
    // Edits since version
    pub fn edits_since(&self, version: &VersionVector) -> Vec<Edit<usize>>;
}

// Operations (serializable for network)
pub enum Operation {
    Edit(EditOperation),
    Undo(UndoOperation),
}

pub struct EditOperation {
    pub timestamp: Lamport,
    pub version: VersionVector,
    pub ranges: Vec<Range<FullOffset>>,
    pub new_text: Vec<Arc<str>>,
}

// Anchor — stable position across concurrent edits
pub struct Anchor {
    pub timestamp: Lamport,
    pub offset: u32,
    pub bias: Bias,
    pub buffer_id: BufferId,
}

pub struct Edit<D> {
    pub old: Range<D>,
    pub new: Range<D>,
}
```

**Behavioral Contracts:**
- RGA-based CRDT: higher Lamport timestamp wins position priority for concurrent insertions
- Deleted text preserved in `deleted_text` rope (tombstone model) for undo
- Version vector tracks observed operations per replica
- Deferred ops: if causality dependencies not met, buffer queues and retries
- Transaction grouping interval: 300ms default
- `BufferSnapshot` is cheap to clone (Arc-based), immutable, thread-safe
- `ReplicaId::AGENT = 2` for AI agent edits

---

### 2.8 `caduceus-workers` [POST-V1]

| Field | Value |
|-------|-------|
| **Purpose** | Multi-agent coordination runtime |
| **Source specs** | open-multi-agent §§1–15 |
| **Dependencies** | `caduceus-core`, `caduceus-api`, `caduceus-tools` |

**Public API Surface:**

```rust
// Orchestrator modes
pub struct Orchestrator { /* agent pool, tool registry */ }

impl Orchestrator {
    pub async fn run_agent(&self, config: AgentConfig, goal: &str) -> Result<AgentResult>;
    pub async fn run_tasks(&self, team: Team, tasks: Vec<TaskDef>) -> Result<TeamResult>;
    pub async fn run_team(&self, team: Team, objective: &str) -> Result<TeamResult>;
}

// Task DAG
pub struct TaskQueue { /* dependency-aware priority queue */ }
pub enum TaskStatus { Pending, Blocked, Running, Completed, Failed, Skipped }

// Team
pub struct Team {
    pub roster: Vec<AgentConfig>,
    pub message_bus: MessageBus,
    pub shared_memory: SharedMemory,
}

// Agent
pub struct AgentConfig {
    pub name: String,
    pub model: ModelId,
    pub provider: ProviderId,
    pub system_prompt: String,
    pub tools: Vec<String>,       // allowed tool names
    pub max_turns: Option<usize>,
    pub max_tokens: Option<usize>,
}

pub struct AgentResult {
    pub success: bool,
    pub output: String,
    pub messages: Vec<TranscriptEntry>,
    pub usage: TokenUsage,
    pub tool_calls: Vec<ToolCallRecord>,
    pub structured_output: Option<serde_json::Value>,
}

// Concurrency: 3-layer semaphore
// 1. Tool execution: bounded parallel per agent turn
// 2. Agent pool: global concurrent agent limit
// 3. Per-agent mutex: prevents concurrent reuse of same agent
```

**Behavioral Contracts:**
- Coordinator fallback: if decomposition parse fails, synthesize one task per agent
- Simple-goal bypass: text-length + complexity-pattern heuristic skips coordinator
- Task failure cascades recursively to dependents
- Retry: exponential backoff with floor/ceiling, attempts = maxRetries + 1
- Loop detection: sliding window of normalized fingerprints; warn-inject then terminate

---

### 2.9 `caduceus-plugins`

| Field | Value |
|-------|-------|
| **Purpose** | Plugin runtime, manifest loading, marketplace, lifecycle |
| **Source specs** | claurst-blackbox §7, claw-code §6, hermes-ide §9, hermes-ide-supplement §8 |
| **Dependencies** | `caduceus-core` |

**Public API Surface:**

```rust
pub struct PluginManager {
    plugins_dir: PathBuf,
    installed: HashMap<String, PluginManifest>,
    enabled: HashSet<String>,
}

impl PluginManager {
    pub fn discover(&mut self) -> Result<Vec<PluginManifest>>;
    pub fn install(&mut self, data: &[u8]) -> Result<String>;  // returns plugin_id
    pub fn uninstall(&mut self, id: &str) -> Result<()>;
    pub fn enable(&mut self, id: &str) -> Result<()>;
    pub fn disable(&mut self, id: &str) -> Result<()>;
    pub fn get_hooks(&self, event: HookEvent) -> Vec<HookCommand>;
    pub fn get_tools(&self) -> Vec<Box<dyn Tool>>;
}

pub struct PluginManifest {
    pub id: String,               // kebab-case, no path separators
    pub name: String,
    pub version: String,
    pub permissions: Vec<PluginPermission>,
    pub tools: Vec<PluginToolDef>,
    pub hooks: Vec<HookDef>,
    pub commands: Vec<PluginCommand>,
}

pub enum PluginPermission {
    ReadFiles, WriteFiles, Network, Shell, Browser, Mcp, Storage,
    ClipboardRead, ClipboardWrite, TerminalRead, TerminalWrite,
    SessionsRead, Notifications,
}
```

---

### 2.10 `caduceus-tui` (Tauri backend)

| Field | Value |
|-------|-------|
| **Purpose** | Tauri IPC command handlers, PTY management, app lifecycle |
| **Source specs** | hermes-ide §§1–11, hermes-ide-supplement §§2–4 |
| **Dependencies** | `caduceus-core`, `caduceus-runtime`, `caduceus-tools`, `caduceus-omniscience`, `caduceus-crdt`, `caduceus-plugins` |

**Public API Surface:**

```rust
// Tauri app state
pub struct AppState {
    pub db: Mutex<Database>,
    pub pty_manager: Mutex<PtyManager>,
    pub sys: Mutex<sysinfo::System>,
    pub conversation_engines: Mutex<HashMap<SessionId, ConversationEngine>>,
    pub crdt_buffers: Mutex<HashMap<BufferId, Buffer>>,
}

// ~150 IPC command handlers registered via #[tauri::command]
// Grouped into: pty, project, attunement, db, git, plugins, process, workspace, menu, clipboard, transcript
```

---

### 2.11 `caduceus-mcp`

| Field | Value |
|-------|-------|
| **Purpose** | Model Context Protocol client — JSON-RPC 2.0 over stdio/HTTP |
| **Source specs** | claurst-full §8 |
| **Dependencies** | `caduceus-core` |

**Key Traits:** `McpClient { connect, list_tools, call_tool, list_resources, read_resource }`

---

### 2.12 `caduceus-bridge`

| Field | Value |
|-------|-------|
| **Purpose** | Remote control / cloud session bridge (WebSocket long-poll) |
| **Source specs** | claurst-full §10 |
| **Dependencies** | `caduceus-core`, `caduceus-api`, `caduceus-runtime` |

---

## Part 3: Integration Wiring

### 3.1 React ↔ Tauri IPC

| Aspect | Detail |
|--------|--------|
| **Interface** | Tauri `invoke()` (request/response) + Tauri `listen()` (events) |
| **Data format** | JSON — serde-serialized Rust structs ↔ TypeScript interfaces via `@tauri-apps/api/core` |
| **Error propagation** | All commands return `Result<T, String>`; errors stringified at boundary |
| **Streaming** | PTY output: base64-encoded chunks emitted as `session-output-{id}` events; frontend decodes and writes to xterm.js. CRDT patches: `buffer-changed-{id}` events carrying `Vec<Edit<usize>>` |

**Wiring Details:**
```
Frontend                           Backend
─────────                          ───────
invoke("create_session", opts) ──► create_session() → SessionUpdate
invoke("write_to_session", {      
  session_id, data: base64     ──► write_to_session() → ()
})                                 
                                   PTY reader thread emits:
listen("session-updated") ◄──────  app.emit("session-updated", SessionUpdate)
listen("cwd-changed-{id}") ◄────  app.emit("cwd-changed-{id}", path)
listen("pty-exit-{id}") ◄───────  app.emit("pty-exit-{id}", exit_status)
```

**Key Patterns:**
- Frontend pre-creates terminal (xterm.js) before calling `create_session` to avoid losing early output
- `SESSION_UPDATED` reducer aggressively deduplicates: skips re-render if phase, metrics, and key fields unchanged
- DB lock is dropped before acquiring PTY manager lock (deadlock avoidance)

---

### 3.2 Tauri Shell ↔ Orchestrator

| Aspect | Detail |
|--------|--------|
| **Interface** | Direct Rust function calls within Tauri command handlers |
| **Data format** | Native Rust structs (no serialization boundary) |
| **Error propagation** | `Result<T, anyhow::Error>` → stringified at IPC boundary |
| **Streaming** | `ConversationEngine::run_turn()` yields `StreamEvent` via `tokio::mpsc::channel`; Tauri handler forwards as events |

**Wiring Details:**
```rust
// In Tauri IPC handler:
#[tauri::command]
async fn run_agent_turn(state: State<'_, AppState>, session_id: String, prompt: String) -> Result<TurnResult, String> {
    let mut engines = state.conversation_engines.lock().unwrap();
    let engine = engines.get_mut(&SessionId(session_id)).ok_or("not found")?;
    engine.run_turn(prompt).await.map_err(|e| e.to_string())
}
```

**Key Flow:**
1. IPC handler acquires `ConversationEngine` for session
2. `run_turn()` builds system prompt, streams to LLM, dispatches tools
3. Tool results fed back to model; loop continues
4. Final `TurnResult` returned to frontend
5. Session state and transcript persisted

---

### 3.3 Orchestrator ↔ Tool Registry

| Aspect | Detail |
|--------|--------|
| **Interface** | `ToolRegistry::get(name) -> &dyn Tool` + `Tool::execute(input, ctx)` |
| **Data format** | `serde_json::Value` for tool input; `ToolResult { output: String, is_error: bool }` for output |
| **Error propagation** | Tool failures become `ToolResult { is_error: true, output: error_message }` — never thrown; fed back into conversation as tool_result error |
| **Streaming** | Tools are async but non-streaming; results returned atomically. Shell tool supports `run_in_background` returning process ID for later polling |

**Dispatch Pipeline:**
```
Model emits tool_use block
    │
    ▼
1. Registry lookup by name ──── not found → error tool_result
    │
    ▼
2. Deserialize input JSON ──── parse error → error tool_result
    │
    ▼
3. Permission check ─────────── deny → error tool_result
    │                            ask → prompt user (IPC event)
    ▼
4. Hook: PreToolUse ─────────── exit 2 → deny
    │
    ▼
5. Tool::execute(input, ctx) ── exception → error tool_result
    │
    ▼
6. Hook: PostToolUse
    │
    ▼
7. Return ToolResult to conversation loop
```

---

### 3.4 Orchestrator ↔ LLM Providers

| Aspect | Detail |
|--------|--------|
| **Interface** | `LlmProvider::create_message_stream(req) -> Stream<StreamEvent>` |
| **Data format** | `ProviderRequest` (normalized) → provider-specific wire format (Anthropic JSON, OpenAI chat completions, etc.) → `StreamEvent` (normalized) |
| **Error propagation** | `ProviderError` typed union; `is_retryable()` drives retry policy; `ContextOverflow` triggers compaction |
| **Streaming** | SSE stream parsed into `StreamEvent` enum; `StreamAccumulator` reconstructs final `ProviderResponse` from deltas |

**Provider Adapter Pattern:**
```
ProviderRequest ──► AnthropicAdapter.normalize(req) ──► POST /v1/messages (SSE)
                                                              │
                    ◄── StreamEvent::TextDelta ◄──────────── SSE: content_block_delta
                    ◄── StreamEvent::InputJsonDelta ◄─────── SSE: tool input chunks
                    ◄── StreamEvent::MessageStop ◄────────── SSE: message_stop
```

**Retry Strategy:**
- Exponential backoff: base 1s, multiplier 2x, max 60s, jitter ±25%
- Retryable: `RateLimited`, `ServerError`, `StreamError`
- Non-retryable: `AuthFailed`, `ContextOverflow`, `ContentFiltered`, `InvalidRequest`

---

### 3.5 Tool Execution ↔ E2B Sandbox

| Aspect | Detail |
|--------|--------|
| **Interface** | `SandboxClient` async methods wrapped as tool implementations |
| **Data format** | Tool input JSON → E2B REST/gRPC requests → `CommandResult`/`EntryInfo` → JSON-stringified tool output |
| **Error propagation** | E2B errors (`SandboxError`, `TimeoutError`, `CommandExitError`) mapped to `ToolResult { is_error: true }` |
| **Streaming** | `CommandHandle` supports streaming stdout/stderr via callbacks; sandbox tool can optionally stream output to UI via Tauri events |

**Integration Pattern:**
```rust
// bash tool with E2B sandbox backend:
struct SandboxBashTool { sandbox: Arc<Sandbox> }

#[async_trait]
impl Tool for SandboxBashTool {
    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolResult> {
        let cmd = input["command"].as_str().unwrap();
        let result = self.sandbox.commands.run(cmd, CommandOpts {
            cwd: Some(input["cwd"].as_str().unwrap_or("/home/user")),
            timeout_ms: input["timeout"].as_u64().unwrap_or(60_000),
            ..Default::default()
        }).await?;
        
        Ok(ToolResult {
            output: serde_json::to_string(&json!({
                "stdout": result.stdout,
                "stderr": result.stderr,
                "exit_code": result.exit_code,
            }))?,
            is_error: result.exit_code != 0,
        })
    }
}
```

**Lifecycle:**
- Sandbox created per-session or per-task (configurable)
- Timeout extended via keep-alive for long-running sessions
- Filesystem watched for AI-generated file changes → fed back as context
- Snapshot/resume for expensive environment setup

---

### 3.6 Orchestrator ↔ Code Intelligence (tree-sitter + qdrant)

| Aspect | Detail |
|--------|--------|
| **Interface** | `CodeIntelligence` exposed as tools: `semantic_search` tool + enhanced `grep_search` with AST awareness |
| **Data format** | Query string + `SearchFilter` → `Vec<SearchHit>` with code snippets and metadata → JSON tool result |
| **Error propagation** | Parse failures are non-fatal (return partial results); index errors logged but don't block search |
| **Streaming** | Non-streaming; results returned atomically. Background indexing runs in a separate tokio task |

**Wiring:**
```
Orchestrator dispatches "semantic_search" tool
    │
    ▼
CodeIntelligence::semantic_search(query, filter, limit)
    │
    ├─► Embedder::embed([query]) → query_vector
    │
    └─► VectorIndex::search(query_vector, filter, limit)
         │
         └─► EdgeShard::query(SearchRequest {
                query: QueryEnum::Nearest(query_vector),
                filter: Filter { must: [repo, language, ...] },
                limit: 20,
                with_payload: true,
             }) → Vec<ScoredPoint>
                    │
                    └─► Map to Vec<SearchHit> with code, file_path, line range
```

**Index Maintenance:**
```
File change detected (FS watcher or git status)
    │
    ▼
AstEngine::reparse(path, source, edit)
    │
    ├─► tree.edit(&input_edit)
    ├─► parser.parse(source, Some(&old_tree))
    └─► changed_ranges = old_tree.changed_ranges(&new_tree)
         │
         ▼
    Extract new chunks from changed ranges only
         │
         ▼
    Embedder::embed(chunk_texts) → vectors
         │
         ▼
    VectorIndex::upsert_chunks(new_chunks)
    VectorIndex::delete_stale(old_chunk_hashes)
```

---

### 3.7 CRDT Buffer ↔ Tool Execution

| Aspect | Detail |
|--------|--------|
| **Interface** | `Buffer::edit()` called by file-write/edit tools; returns `Operation` for broadcast |
| **Data format** | Tool produces `(Range<usize>, &str)` edit pairs → `Buffer::edit()` → `Operation` (serializable) |
| **Error propagation** | Edit failures (e.g., offset out of bounds) return `Result::Err`; tool wraps as error result |
| **Streaming** | Each edit produces an `Operation` immediately broadcast to all replicas; subscriptions fire synchronously |

**Wiring Pattern:**
```rust
// In edit_file tool:
async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolResult> {
    let path = input["path"].as_str().unwrap();
    let old_str = input["old_string"].as_str().unwrap();
    let new_str = input["new_string"].as_str().unwrap();
    
    // Get CRDT buffer for this file
    let buffer = self.buffers.get_mut(path)?;
    
    // Find old_str in visible text
    let offset = buffer.text().find(old_str).ok_or("old_string not found")?;
    let range = offset..offset + old_str.len();
    
    // CRDT edit — returns Operation for broadcast
    let op = buffer.edit([(range, new_str)]);
    
    // Broadcast to all connected replicas (human editors, other agents)
    self.broadcast_op(path, op).await;
    
    // Trigger re-parse for code intelligence
    let edit = InputEdit { /* from range + old/new lengths */ };
    self.omniscience.reindex_changed(Path::new(path), &edit)?;
    
    Ok(ToolResult { output: "edit applied".to_string(), is_error: false })
}
```

**AI Agent as Replica:**
- AI uses `ReplicaId::AGENT` (value 2)
- Each AI edit gets a unique Lamport timestamp
- Concurrent human + AI edits resolved by timestamp ordering (higher wins position)
- Both texts preserved — no data loss

---

### 3.8 CRDT Buffer ↔ React Frontend

| Aspect | Detail |
|--------|--------|
| **Interface** | Tauri events: `buffer-changed-{buffer_id}` carrying serialized `Vec<Edit<usize>>` |
| **Data format** | `Edit<usize> { old: Range<usize>, new: Range<usize> }` + text content; frontend applies to CodeMirror |
| **Error propagation** | CRDT guarantees convergence — no application-level errors possible |
| **Streaming** | Each operation produces a patch immediately emitted as a Tauri event; frontend applies incrementally |

**Wiring:**
```
Backend (Rust)                          Frontend (React)
────────────                            ────────────────
Buffer::subscribe() creates Subscription
    │
    ▼ (on each edit/apply_ops)
Subscription receives Patch<Edit<usize>>
    │
    ▼
app.emit("buffer-changed-{id}", {
    edits: [{old: {start, end}, new: {start, end}, text: "..."}],
    remote_selections: {
        2: [{anchor: ..., head: ...}]  // AI agent cursor
    }
})
    │                                   │
    │                                   ▼
    │                           listen("buffer-changed-{id}")
    │                                   │
    │                                   ▼
    │                           CodeMirror transaction:
    │                             editor.dispatch({
    │                               changes: edits.map(e => ({
    │                                 from: e.new.start,
    │                                 to: e.new.end,
    │                                 insert: e.text
    │                               }))
    │                             })
    │                                   │
    │                                   ▼
    │                           Render remote cursors:
    │                             AI agent cursor shown as colored marker
    │                             with ReplicaId label
```

**Remote Selection Display:**
- AI agent selections stored as `TreeMap<ReplicaId, Vec<Selection<Anchor>>>`
- Frontend renders colored cursor/selection decorations per replica
- Selections use Anchors — survive concurrent edits automatically

---

### 3.9 Workers ↔ Orchestrator [POST-V1]

| Aspect | Detail |
|--------|--------|
| **Interface** | `Orchestrator` wraps `ConversationEngine` instances; `AgentPool` manages concurrency |
| **Data format** | `TaskDef` → `AgentConfig` + prompt → `AgentResult`; team communication via `MessageBus` (in-memory pub-sub) |
| **Error propagation** | Task failures cascade to dependents via `TaskQueue::fail()` which recursively skips downstream tasks |
| **Streaming** | Progress events emitted per task: `TaskStarted`, `TaskCompleted`, `TaskFailed`, `BudgetExceeded` |

**Three-Layer Concurrency Model:**
```
┌─────────────────────────────────────────────────────────────┐
│ Layer 1: Tool Execution Semaphore                           │
│   Bounds parallel tool calls per agent turn (e.g., 5)       │
│   ┌─────────────────────────────────────────────────────┐   │
│   │ Layer 2: Agent Pool Semaphore                       │   │
│   │   Global cap on concurrent agent runs (e.g., 4)     │   │
│   │   ┌─────────────────────────────────────────────┐   │   │
│   │   │ Layer 3: Per-Agent Mutex                    │   │   │
│   │   │   Prevents concurrent reuse of same agent   │   │   │
│   │   │   Lock ordering: agent mutex before pool    │   │   │
│   │   └─────────────────────────────────────────────┘   │   │
│   └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

**Task Queue Protocol:**
```
Orchestrator::run_team(objective)
    │
    ├─ 1. Simple-goal check: if simple → bypass to best specialist
    │
    ├─ 2. Decomposition: coordinator agent → structured task list
    │      (fallback: one task per agent if parse fails)
    │
    ├─ 3. Queue execution loop:
    │      while runnable_tasks > 0 && budget_remaining:
    │        ├─ auto-assign pending tasks (scheduler strategy)
    │        ├─ batch dispatch to agent pool
    │        ├─ per-task: build prompt with shared_memory + messages
    │        ├─ per-task: run agent with retry/backoff
    │        ├─ on complete: persist output to shared_memory
    │        └─ on fail: cascade failure to dependents
    │
    └─ 4. Synthesis: coordinator aggregates results into final response
```

---

## Appendix: Crate Dependency Graph

```
caduceus-tui (Tauri backend binary)
├── caduceus-core          (foundation)
├── caduceus-api           (→ core)
├── caduceus-tools         (→ core, api, sandbox, omniscience)
├── caduceus-runtime       (→ core, api, tools, plugins)
├── caduceus-sandbox       (→ core)
├── caduceus-omniscience   (→ core)
├── caduceus-crdt          (→ core)
├── caduceus-plugins       (→ core)
├── caduceus-mcp           (→ core)
├── caduceus-bridge        (→ core, api, runtime)
└── caduceus-workers       (→ core, api, tools) [POST-V1]
```

---

*End of Parts 1–3*
