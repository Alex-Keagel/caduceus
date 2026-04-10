# Caduceus — Architecture Guide

This document describes Caduceus's internal design: crate boundaries, data flows, IPC protocol, security model, and the design decisions behind each major subsystem.

---

## Table of Contents

1. [Six-Layer Architecture](#1-six-layer-architecture)
2. [Crate Map and Dependency Graph](#2-crate-map-and-dependency-graph)
3. [Data Flow: Prompt → LLM → Tools → Response](#3-data-flow-prompt--llm--tools--response)
4. [Streaming Pipeline](#4-streaming-pipeline)
5. [Tauri IPC Protocol](#5-tauri-ipc-protocol)
6. [Security Model](#6-security-model)
7. [Layer 5 — Omniscience: AST + Vector Search](#7-layer-5--omniscience-ast--vector-search)
8. [Layer 6 — Multiplayer: CRDT Design](#8-layer-6--multiplayer-crdt-design)
9. [Layer 4 — Sandbox: E2B Integration](#9-layer-4--sandbox-e2b-integration)
10. [Layer 3 — Workers: Multi-Agent Runtime (post-v1)](#10-layer-3--workers-multi-agent-runtime-post-v1)
11. [Persistence: SQLite Schema](#11-persistence-sqlite-schema)
12. [Design Decisions](#12-design-decisions)

---

## 1. Six-Layer Architecture

Caduceus decomposes its concerns into six layers. Each layer has a well-defined responsibility and communicates with adjacent layers through typed interfaces only — never by sharing mutable state across a layer boundary.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  LAYER 1 — PRESENTATION                                                [v1]  │
│  Crates: src-tauri (IPC host), src/ (React + TypeScript)                    │
│  Responsibilities: render terminal, chat, Git panel, project context view   │
│  Talks to: Layer 2 via Tauri IPC invoke/event                               │
├──────────────────────────────────────────────────────────────────────────────┤
│  LAYER 2 — ORCHESTRATION                                               [v1]  │
│  Crates: caduceus-orchestrator, caduceus-runtime, caduceus-tools,           │
│          caduceus-providers, caduceus-permissions, caduceus-telemetry,      │
│          caduceus-storage, caduceus-scanner, caduceus-git, caduceus-core    │
│  Responsibilities: agent conversation loop, tool dispatch, permission gating│
│  Talks to: Layer 1 (events), Layer 4 (tool exec), Layer 5 (code intel)     │
├──────────────────────────────────────────────────────────────────────────────┤
│  LAYER 3 — WORKERS                                                [post-v1]  │
│  Crates: caduceus-workers (planned)                                          │
│  Responsibilities: task DAG execution, multi-agent coordination              │
│  Talks to: Layer 2 (agent spawning), Layer 4, Layer 5                       │
├──────────────────────────────────────────────────────────────────────────────┤
│  LAYER 4 — SANDBOX                                                [post-v1]  │
│  Crates: caduceus-sandbox (planned)                                          │
│  Responsibilities: E2B micro-VM lifecycle, file I/O, PTY, MCP gateway       │
│  Talks to: Layer 2 / Layer 3 (tool results)                                 │
├──────────────────────────────────────────────────────────────────────────────┤
│  LAYER 5 — OMNISCIENCE                                                 [v1]  │
│  Crates: caduceus-omniscience                                                │
│  Responsibilities: AST parse, semantic chunk extract, vector index, search  │
│  Talks to: Layer 2 (returns tool results), Layer 6 (reparse on buffer edit) │
├──────────────────────────────────────────────────────────────────────────────┤
│  LAYER 6 — MULTIPLAYER                                            [post-v1]  │
│  Crates: caduceus-crdt                                                       │
│  Responsibilities: CRDT text buffer, Lamport clocks, operation broadcast    │
│  Talks to: Layer 1 (buffer-changed events), Layer 5 (incremental reparse)  │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Cross-Layer Communication Matrix

| From ↓ / To → | Presentation | Orchestration | Workers | Sandbox | Omniscience | Multiplayer |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| **Presentation** | — | IPC invoke | — | — | — | CRDT ops |
| **Orchestration** | events / stream | — | task dispatch | tool exec | tool exec | `buffer.edit()` |
| **Workers** | — | results | msg bus | tool exec | tool exec | `buffer.edit()` |
| **Sandbox** | — | tool result | — | — | — | FS watch → edit |
| **Omniscience** | — | tool result | — | — | — | — |
| **Multiplayer** | subscription | — | — | — | reparse trigger | — |

---

## 2. Crate Map and Dependency Graph

### Dependency Rules

- Lower crates **never** import from upper crates.
- All cross-layer relationships are expressed through traits defined in `caduceus-core`.
- No concrete type leaks across a layer boundary in a direction that would create a cycle.

```
caduceus-core
│   Shared types, error taxonomy, IPC contract types, config schema.
│   Foundation — zero Caduceus dependencies.
│
├── caduceus-storage
│       SQLite persistence via rusqlite. WAL mode. Versioned migrations.
│       Implements: SessionStorage, MessageStorage, AuditLogStorage
│
├── caduceus-scanner
│       Static project analysis: language detection, framework fingerprints,
│       context maps with token-budget estimates.
│
├── caduceus-git
│       Git status, diff, stage, commit, branch ops via the git2 crate.
│
├── caduceus-providers
│       LlmProvider trait + two concrete adapters:
│         • AnthropicAdapter  — native Messages API + streaming SSE
│         • OpenAICompatibleAdapter — covers OpenAI, Ollama, vLLM, LM Studio
│       Model registry with bundled snapshot + optional remote refresh.
│
├── caduceus-permissions
│       Capability token system (6 tiers). PermissionEnforcer trait.
│       Append-only audit log. Secrets bridge to OS keychain.
│
├── caduceus-runtime
│       Structured process spawning (explicit argv, no shell-string eval).
│       File ops with workspace-root confinement and symlink resolution.
│       Timeout enforcement, output capture, per-path file leases.
│
├── caduceus-tools
│       ToolRegistry (builtin → runtime → plugin layers).
│       ~10 built-in tools with JSON Schema input validation.
│       Permission-aware dispatch: validate → permission check → execute.
│
├── caduceus-orchestrator
│       ConversationEngine: session state machine, multi-turn agent loop,
│       system prompt assembly, context compaction, slash commands.
│       Depends on: providers, tools, permissions, storage, scanner, git.
│
├── caduceus-telemetry
│       Token counting, per-turn cost calculation, SQLite cost log.
│       Local-only — no external telemetry.
│
├── caduceus-omniscience
│       AstEngine: tree-sitter parsers + chunk-extraction queries per language.
│       VectorIndex: qdrant-edge embedded EdgeShard.
│       CodeIntelligence: unified semantic_search() + reindex_changed() API.
│
└── caduceus-crdt
        Buffer: RGA-based CRDT text buffer with Lamport timestamps.
        Rope: B+ tree backed text storage.
        Anchor: stable positions across concurrent edits.
        Foundation — no Caduceus deps (may use caduceus-core for IDs).
```

---

## 3. Data Flow: Prompt → LLM → Tools → Response

This is the complete path of a single agent turn in v1:

```
[React] user submits prompt
   │
   │  invoke("agent_turn", { session_id, user_input })
   ▼
[src-tauri/main.rs] IPC handler validates input
   │
   │  calls into caduceus-orchestrator
   ▼
[caduceus-orchestrator] ConversationEngine::run_turn()
   │
   ├─ 1. Build system prompt (working dir, memory, instruction files, project ctx)
   ├─ 2. Append user message to transcript
   ├─ 3. Check token budget; compact if > 85 % full
   ├─ 4. Send ProviderRequest to LlmProvider::create_message_stream()
   │
   │  [caduceus-providers] AnthropicAdapter or OpenAICompatibleAdapter
   │  streams SSE → yields StreamEvent variants
   │
   ├─ 5. Accumulate StreamEvents into AssistantMessage
   ├─ 6. Emit AgentEvent::TextDelta for each text chunk → Tauri event
   │
   │  ┌── TOOL DISPATCH LOOP ──────────────────────────────────────────┐
   │  │  While stop_reason == ToolUse:                                 │
   │  │  7. Collect ToolUse blocks from response                       │
   │  │  8. For each tool call:                                        │
   │  │     a. Emit AgentEvent::ToolCallStart                          │
   │  │     b. PermissionEnforcer::check() → Allow / Ask / Deny       │
   │  │        • Ask: emit AgentEvent::PermissionRequest               │
   │  │          await AgentEvent::PermissionResponse from UI          │
   │  │     c. ToolRegistry::get(name)?.execute(input, ctx)            │
   │  │        → dispatches to caduceus-runtime (bash/file ops)        │
   │  │           or caduceus-omniscience (search)                     │
   │  │           or caduceus-git (git ops)                            │
   │  │     d. Append ToolResult to transcript                         │
   │  │     e. Emit AgentEvent::ToolResultEnd                          │
   │  │  9. Send next model turn with tool results                     │
   │  └────────────────────────────────────────────────────────────────┘
   │
   ├─ 10. Emit AgentEvent::TurnComplete { stop_reason, usage }
   ├─ 11. Persist turn to SQLite via caduceus-storage
   └─ 12. Update telemetry via caduceus-telemetry

[React] event listener receives AgentEvent stream
   ├─ TextDelta → appended to chat bubble
   ├─ ToolCallStart / ToolCallEnd → activity indicators
   ├─ PermissionRequest → permission dialog
   └─ TurnComplete → token count / cost display updated
```

---

## 4. Streaming Pipeline

LLM responses stream from provider to frontend through four stages, each decoupled by typed channel boundaries:

```
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 1 — caduceus-providers                                            │
│   HTTP SSE connection to LLM API                                        │
│   Yields:  StreamEvent  (TextDelta, ToolCallDelta, MessageStop, …)     │
└────────────────────────────┬────────────────────────────────────────────┘
                              │ tokio::sync::mpsc  (bounded, cap 1024)
                              ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 2 — caduceus-orchestrator                                         │
│   StreamAccumulator reconstructs full message from deltas               │
│   Emits:   AgentEvent  (TextDelta, ToolCallStart/End, TurnComplete, …) │
└────────────────────────────┬────────────────────────────────────────────┘
                              │ tokio::sync::mpsc  (bounded, cap 1024)
                              ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 3 — src-tauri (caduceus-shell)                                    │
│   app.emit("agent:event", &agent_event)                                 │
│   Serializes AgentEvent to JSON for Tauri event bus                     │
└────────────────────────────┬────────────────────────────────────────────┘
                              │ Tauri event bus (WebView → JS bridge)
                              ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 4 — React frontend                                                │
│   listen("agent:event", callback)                                       │
│   useReducer dispatches action → chat state updated → re-render        │
└─────────────────────────────────────────────────────────────────────────┘
```

**Backpressure policy:**
- Channel capacity 1 024 events per stage boundary.
- `TextDelta` events are droppable on overflow.
- `ToolCallStart`, `ToolCallEnd`, `PermissionRequest`, `TurnComplete`, and `Error` events are never dropped.

**`AgentEvent` type (defined in `caduceus-core`):**

```rust
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
```

---

## 5. Tauri IPC Protocol

All IPC commands return `Result<T, String>` serialized as JSON. IPC contract types are defined in `caduceus-core` and mirrored as TypeScript types in `src/types/`.

### Command Groups

| Group | Commands | Notes |
|---|---|---|
| `session::*` | `session_create`, `session_list`, `session_delete` | CRUD for agent sessions |
| `agent::*` | `agent_turn`, `agent_abort` | Drive the conversation loop |
| `terminal::*` | `terminal_exec`, `create_pty`, `write_pty`, `resize_pty`, `close_pty` | PTY management |
| `project::*` | `project_scan` | Language/framework detection |
| `git::*` | `git_status`, `git_diff` | Read-only Git queries (mutations via agent tools) |
| `config::*` | `config_get` | Configuration retrieval |

### Command Signatures (v1 implemented)

```typescript
// Create a new agent session
invoke<SessionInfo>("session_create", {
  request: { project_root: string, provider_id: string, model_id: string }
})

// Run one agent turn (response streamed via "agent:event" events)
invoke<AgentTurnResponse>("agent_turn", {
  request: { session_id: string, user_input: string }
})

// Abort in-flight agent turn
invoke<void>("agent_abort", { session_id: string })

// Scan project for language/framework context
invoke<ProjectScanResponse>("project_scan", { path: string })
```

### Streaming Events (frontend listens via `listen()`)

```typescript
listen<AgentEvent>("agent:event", (event) => {
  dispatch({ type: "AGENT_EVENT", payload: event.payload })
})
```

### Permission Approval Flow

When a tool requires a capability the agent doesn't yet have, the orchestrator emits `AgentEvent::PermissionRequest` and pauses the turn. The React UI renders an approval dialog. The user's decision is sent back via:

```typescript
invoke("approve_permission", { request_id: string, approved: boolean })
```

This resumes the blocked tool call or rejects it with a `PermissionDenied` error message injected into the conversation.

---

## 6. Security Model

### Threat Model

Caduceus runs shell commands and writes files on behalf of an LLM. The threat model assumes:
- Untrusted repository content (malicious files, CI configs)
- Untrusted LLM output (prompt-injection attacks in tool results)
- User mistakes (accidentally approving destructive operations)

### Secrets Management

API keys follow a strict no-leak policy:

| Location | Behaviour |
|---|---|
| OS keychain | Primary store: macOS Keychain, Linux Secret Service, Windows Credential Manager |
| Env vars | `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc. — checked first, take precedence |
| Fallback | Encrypted file `~/.caduceus/credentials.enc` (AES-256-GCM, argon2id key derivation) |
| SQLite / logs | **Never written** |
| Tauri IPC | **Never crosses the IPC boundary** — React frontend never sees raw API keys |

### Capability-Based Permissions

Default-deny execution model. Six capability tiers:

| Capability | Scope | Default |
|---|---|---|
| `fs.read` | Read files within workspace root | **Auto-granted** |
| `fs.write` | Write / edit / delete within workspace | Prompt per session |
| `process.exec` | Execute shell commands | Prompt per command |
| `network.http` | HTTP requests (web fetch, web search) | Prompt per session |
| `git.mutate` | Stage, commit, push, pull | Prompt per action |
| `fs.escape` | Access paths outside workspace root | **Always prompt**; never auto-grant |

All grant decisions are written to an append-only audit log: timestamp, tool name, capability, sanitized arguments (secrets redacted), user decision.

### Execution Sandboxing

Shell commands use **structured process spawning** — `std::process::Command` with explicit `argv` arrays. Shell-string evaluation is never used by default:

- **Working directory:** canonicalized, locked to project root.
- **Environment:** default-empty; a minimal allowlist is populated explicitly.
- **Timeout:** soft kill at 30 s, hard kill at 300 s with process-group termination.
- **Output capture:** stdout/stderr capped at 1 MB each; truncation surfaced to user.
- **File boundary:** canonical path validation + symlink resolution enforces workspace root confinement. `fs.escape` capability required for any path outside.
- **Command denylist:** pattern matching on common destructive commands (defense-in-depth only — workspace confinement is the actual security control).

### Prompt-Injection Defense

All untrusted content is wrapped as **data-only blocks** with provenance tags before being included in LLM context:

```
<tool_result tool="bash" timestamp="2025-…" trust="untrusted">
  … output here …
</tool_result>
```

Additional mitigations:
- Tool outputs cannot modify permissions, policies, or system prompts.
- High-risk capabilities (`fs.write`, `process.exec`, `git.mutate`, `network.http`) require an explicit capability check independent of model intent.
- PTY security: OSC 52 clipboard exfiltration stripped; paste bracket mode enforced.

### Webview Hardening

```
Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'
```

- No `eval()`, no inline scripts, no remote script loading.
- LLM-generated markdown is sanitized before DOM insertion (strip `<script>`, `<iframe>`, `on*` handlers, `javascript:` URIs).
- All Tauri IPC inputs are validated on the Rust side — the backend treats every IPC message as untrusted regardless of origin.

---

## 7. Layer 5 — Omniscience: AST + Vector Search

`caduceus-omniscience` provides two engines that the orchestrator uses for code-intelligence tool calls: an AST engine backed by tree-sitter and a vector index backed by qdrant-edge.

### Tree-sitter AST Engine

```
File change detected
       │
       ▼
AstEngine::reparse(path, source, edit)
       │
       ├─ Apply InputEdit to cached Tree (incremental reparse)
       ├─ Compute changed_ranges(old_tree, new_tree)
       └─ Extract only affected chunks via tree-sitter Query
              │
              ▼
       Vec<CodeChunk>
         ┌─ file_path, language, symbol_kind, symbol_name
         ├─ container (enclosing class/module)
         ├─ start_line / end_line / byte_range
         └─ code text (40–300 tokens per chunk)
```

Supported chunk types: `function`, `method`, `class`, `module`, `trait_impl`. Each language has a dedicated `.scm` query file using tree-sitter's pattern syntax (e.g., `@chunk.function`). Files with parse errors are indexed with `has_error: true`; valid chunks within an otherwise-errored file are still indexed.

### Qdrant-edge Vector Index

Deployment: **embedded `EdgeShard`** — no separate process, no network dependency.

```
Collection config (per workspace):
  vector name:  "text"
  dimension:    768        (embedding model dependent)
  distance:     Cosine
  storage:      Float32, in-memory
  HNSW:         m=16, ef_construct=100
  payload idx:  repo, workspace, file_path, language, symbol_kind
```

Indexing pipeline:

```
Vec<CodeChunk>
       │
       ▼
Embedder::embed(chunk.code)  →  Vec<f32>  (dimension 768)
       │
       ▼
VectorIndex::upsert_chunks(Vec<IndexedChunk>)
  ├─ id = hash(file_path + symbol_name + start_line)
  ├─ payload = ChunkMetadata { repo, file_path, language, … }
  └─ vector = embedding
```

Query path:

```
semantic_search(query: &str, filter: SearchFilter, limit: usize)
       │
       ├─ Embedder::embed([query]) → query_vector
       ├─ VectorIndex::search(query_vector, filter, limit)
       └─ Vec<SearchHit> { score: f32, metadata: ChunkMetadata }
```

Incremental re-index strategy: on file save, `AstEngine::reparse()` computes `changed_ranges()`, only the affected chunks are re-extracted and re-embedded, and `VectorIndex::delete_by_file()` + `upsert_chunks()` is called only for the delta. `optimize()` is called after bulk ingest; `flush()` on session checkpoints.

### Combined `CodeIntelligence` API

```rust
pub struct CodeIntelligence {
    ast: AstEngine,
    index: VectorIndex,
    embedder: Box<dyn Embedder>,
}

impl CodeIntelligence {
    pub async fn index_file(&mut self, path: &Path) -> Result<usize>;
    pub async fn index_directory(&mut self, dir: &Path) -> Result<usize>;
    pub async fn semantic_search(&self, query: &str, filter: SearchFilter, limit: usize) -> Result<Vec<SearchHit>>;
    pub fn reindex_changed(&mut self, path: &Path, edit: &InputEdit) -> Result<()>;
}
```

---

## 8. Layer 6 — Multiplayer: CRDT Design

`caduceus-crdt` implements an RGA (Replicated Growable Array) based CRDT text buffer, closely following Zed's architecture. It provides conflict-free merging of concurrent edits from humans, AI agents, and remote replicas.

### Replica Identity

| Replica ID | Meaning |
|---|---|
| `0` | `LOCAL` — primary human editor |
| `1` | `REMOTE_SERVER` — remote SSH / bridge authority |
| `2` | `AGENT` — default AI editor identity |
| `3` | `LOCAL_BRANCH` — branch/worktree shadow buffer |
| `≥ 8` | `FIRST_COLLAB_ID` — additional humans or named AI workers |

In v1 (single-agent), all AI edits use `ReplicaId(2)`. Post-v1 multi-agent mode allocates a unique replica ID per worker agent so selections, undo ownership, and causality tracking remain attributable.

### Clock Model

```rust
pub struct ReplicaId(pub u16);

// Lamport scalar clock — total ordering for inserts/deletes/undo
pub struct Lamport { pub value: u32, pub replica_id: ReplicaId }

// Version vector — causal dependency tracking, reconnect diffing
pub struct Global(pub SmallVec<[u32; 4]>);  // indexed by replica slot
```

Local edit increments the Lamport clock and updates the local slot in `Global`. Incoming remote operations are applied only when their `version` dependency is satisfied; otherwise they are queued in `deferred_ops`.

### Fragment and Rope Model

```
visible_text: Rope  ─────────────────────────────────  displayed text
deleted_text: Rope  ─────────────────────────────────  tombstoned characters (undo support)
fragments:    SumTree<Fragment>  ──────────────────────  ordered fragment tree

Fragment {
    id:               Locator       // fractional position ID
    timestamp:        Lamport       // determines insert ordering
    insertion_offset: u32
    len:              u32
    visible:          bool          // false = tombstoned
    deletions:        SmallVec<[Lamport; 2]>
    max_undos:        Global
}
```

`Locator::between(lhs, rhs)` generates a stable fractional position that sorts between its neighbours without renumbering. The B+ tree rope gives O(log n) random access, slicing, and iteration.

### Conflict Resolution

- **Concurrent inserts at the same position:** higher Lamport timestamp wins; tie-broken by higher `replica_id`.
- **Concurrent delete vs. insert:** insert wins (delete is a tombstone, not a physical removal).
- **Undo:** `UndoOperation` flips fragment visibility by incrementing undo counts. Odd undo count = undone, even = active.

### Anchor System

Anchors provide stable buffer positions that survive concurrent edits from any replica:

```rust
pub struct Anchor {
    pub timestamp_replica_id: ReplicaId,
    pub timestamp_value:      u32,
    pub offset:               u32,
    pub bias:                 Bias,     // Left | Right
    pub buffer_id:            BufferId,
}
```

Anchors are used for:
- AI edit targets (the agent specifies a named symbol; the anchor tracks it through human edits)
- Cursor/selection positions rendered in CodeMirror
- Diagnostic ranges from language server responses
- Cross-buffer references from semantic search results

### CRDT + AI Integration

When the orchestrator tool `edit_file` runs:

1. `ConversationEngine` calls `buffer.edit([(range, new_text)])`.
2. `Buffer::edit()` produces an `EditOperation` with a fresh Lamport timestamp (`ReplicaId(2)`).
3. The operation is appended to the `History` and written to `visible_text`.
4. A `Subscription` fires, yielding a `Patch<Edit>` to any subscriber.
5. `caduceus-shell` emits `app.emit("buffer-changed", patch)` → React re-renders CodeMirror.
6. `caduceus-omniscience` receives the `InputEdit` and triggers incremental reparse.

Any concurrent human edit follows the same path with `ReplicaId(0)`. The CRDT guarantees both replicas converge to the same final text regardless of order.

---

## 9. Layer 4 — Sandbox: E2B Integration

`caduceus-sandbox` (post-v1) wraps the E2B micro-VM API. Each sandbox is an isolated Linux container with a full filesystem, network, and persistent PTY, accessible over gRPC-web (Connect protocol).

### Architecture

```
caduceus-orchestrator
       │
       │  bash tool (sandboxed variant)
       ▼
SandboxClient::create(opts)  →  Sandbox { id, envd_access_token, domain }
       │
       ├─ Sandbox::commands  →  CommandsApi  (run, list, kill, send_stdin)
       ├─ Sandbox::files     →  FilesApi     (read, write, list, remove, watch_dir)
       ├─ Sandbox::pty       →  PtyApi       (create, send_data, resize, kill)
       └─ Sandbox::git       →  GitApi       (clone, checkout, status, diff)
```

### Sandbox Lifecycle

```
create() → Running → pause() → Paused → resume() → Running
                                               └─ kill() → Killed
                  └─ set_timeout() → auto-kill on expiry
                  └─ create_snapshot() → SnapshotInfo
```

Default timeout: 300 s. Max: 24 h (Pro tier). The envd daemon listens on port 49983; the MCP gateway on port 50005. Port forwarding: `sandbox.get_host(port)` → `"{port}-{id}.{domain}"`.

### Security Properties

- Process tree isolated in container namespaces — no host filesystem access.
- Network isolated by default; specific ports opened via volume/template config.
- Sandbox credentials (`envd_access_token`) stored in session state, never logged.
- All sandbox operations require the `process.exec` capability.

---

## 10. Layer 3 — Workers: Multi-Agent Runtime (post-v1)

`caduceus-workers` adds a coordinator-driven multi-agent execution layer. The coordinator is itself an LLM call that decomposes a goal into a task DAG and assigns tasks to a named agent roster.

### Execution Model

```
Orchestrator::run_team(team, objective)
       │
       ├─ coordinator_turn(objective, roster) → Vec<TaskDef>
       │      (JSON: task_id, description, dependencies, assignee, priority)
       │
       ├─ TaskQueue::push_all(tasks)         (dependency-aware priority queue)
       │
       └─ Worker pool (bounded concurrency):
              ├─ pop ready task (no unmet dependencies)
              ├─ spawn AgentConfig for assignee
              ├─ AgentHarness::run_turn(task.description)
              └─ on completion: mark done, unlock dependents
```

### Concurrency Model

Three-layer semaphore:
1. **Tool execution:** bounded parallelism per agent turn.
2. **Agent pool:** global concurrent agent limit.
3. **Per-agent mutex:** prevents the same agent config from running concurrently.

### Loop Detection

A sliding window of normalized turn fingerprints detects repetitive loops:
1. Warn: inject a loop-detection message into the agent's context.
2. Terminate: if the window fills again after warning, abort the agent with a `LoopDetected` error.

### Failure Cascades

Task failure propagates recursively to all dependent tasks in the DAG (status → `Failed`). Retry uses exponential backoff (configurable floor, ceiling, and max attempts).

### Shared Context

The team `SharedMemory` is an append-only session log visible to all workers in the same session. It uses SQLite as the backing store — entries are immutable records with timestamps and author IDs. **Not** OS shared memory; no concurrent raw-pointer access.

---

## 11. Persistence: SQLite Schema

Caduceus uses SQLite in WAL mode as the authoritative runtime store. All tables use idempotent migrations (`CREATE TABLE IF NOT EXISTS`; additive `ALTER TABLE` only).

### Core Tables

| Table | Purpose |
|---|---|
| `sessions` | Session lifecycle state, working dir, provider/model, phase |
| `messages` | Conversation transcript (role, content blocks as JSON, token counts) |
| `tool_calls` | Per-call record: tool name, input/output JSON, status, duration |
| `costs` | Per-turn token usage and USD cost (local only, never sent externally) |
| `cost_daily` | Daily rollup: session count, tokens, cost by provider+model |
| `audit_log` | Append-only permission decision log |
| `projects` | Scanned projects: languages, frameworks, conventions |
| `memories` | Scoped key-value memory (session / project / global) |
| `contexts` | Assembled prompt-context artifacts with token budget |
| `errors` | Deduplicated error fingerprints with optional resolutions |
| `plugins` | Installed plugin manifests |

Key schema decisions:
- `messages.content_json` is the canonical content-block envelope supporting text, tool_use, tool_result, thinking, image, and summary blocks.
- `costs` is the event table; `cost_daily` and `token_snapshots` are pre-computed rollups for dashboards.
- All foreign keys use `ON DELETE CASCADE` so deleting a session cleans up all child records automatically.
- `PRAGMA journal_mode = WAL` + single-writer queue eliminates most locking contention.

---

## 12. Design Decisions

### Why Rust for the backend?

The agent loop runs shell commands, writes files, and manages PTYs — system-level operations where Rust's ownership model prevents entire classes of bugs (use-after-free, data races, double-free). The async Tokio runtime handles the bursty, I/O-bound workload (streaming LLM responses, concurrent tool calls) efficiently.

### Why Tauri over Electron?

Tauri uses the OS's native webview (WebKit on macOS, WebView2 on Windows, WebKitGTK on Linux) instead of bundling Chromium. This results in ~10× smaller binary sizes and lower memory footprint — important for a dev tool that shares system resources with the user's editor, compiler, and language server.

### Why SQLite over a flat-file store?

The session store needs concurrent reads, indexed queries (e.g., "all tool calls for session X in the last hour"), and transactional writes (append transcript + update cost in one atomic operation). SQLite in WAL mode satisfies all three while remaining a single embedded file with no daemon.

### Why a CRDT buffer even in v1?

The CRDT crate ships in v1 as a foundation, even though real-time collaboration isn't a v1 feature. The reason: the CRDT's anchor system provides stable buffer positions that survive any edit, which is critical for AI tool calls that target named symbols. Without anchors, a concurrent human edit can invalidate the byte offset the agent computed, causing misapplied edits.

### Why qdrant-edge (embedded) instead of a qdrant server?

A desktop app cannot require the user to run a separate vector DB process. `qdrant-edge`'s `EdgeShard` provides the same cosine HNSW index as the full server but as an in-process library, with no network dependency and sub-millisecond query latency on laptop hardware.

### Why a clean-room implementation?

Caduceus draws on behavioral specifications extracted from several AI coding tools. Using a clean-room process (Phase A: spec extraction in isolation; Phase B: implementation from specs only) allows us to ship under MIT without inheriting the GPL-3.0 or BSL 1.1 terms of the reference projects. See `spec/` for the provenance-tagged spec artifacts.

### IPC type contract in `caduceus-core`

IPC contract types live in `caduceus-core` (not in `src-tauri`) so that both the Rust backend and the TypeScript frontend share a single source of truth. A code-generation step mirrors the Rust types to TypeScript via `serde_json` schemas, eliminating the drift that would otherwise accumulate between hand-written TS types and the Rust structs.
