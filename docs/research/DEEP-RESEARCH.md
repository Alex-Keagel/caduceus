# Caduceus — Deep Research Compendium

> **Compiled from:** 10 behavioral specifications, 2 wiring plans, 1 instruction-management study, 7 source repositories, and state-of-the-art literature across agent architecture, CRDTs, AST search, sandboxing, LLM providers, and desktop app engineering.
>
> **Source repos analyzed:** Hermes IDE, Hermes IDE Supplement, Claw Code (Claude CLI), Claurst (Claude Code internals), Open Multi-Agent, E2B (Cloud Sandbox), Tree-sitter, Qdrant, Zed CRDT.

---

## Table of Contents

1. [Agent Architecture — State of the Art (2026)](#1-agent-architecture--state-of-the-art-2026)
2. [Instruction Management — File Formats & Standards](#2-instruction-management--file-formats--standards)
3. [CRDT Collaborative Editing — Best Practices](#3-crdt-collaborative-editing--best-practices)
4. [AST-Based Code Search — Chunking & Embeddings](#4-ast-based-code-search--chunking--embeddings)
5. [Sandbox Execution — Secure Agent Code Running](#5-sandbox-execution--secure-agent-code-running)
6. [LLM Provider Landscape — Multi-Provider Architecture](#6-llm-provider-landscape--multi-provider-architecture)
7. [Desktop App Architecture — Tauri vs Electron](#7-desktop-app-architecture--tauri-vs-electron)
8. [References](#8-references)

---

## 1. Agent Architecture — State of the Art (2026)

### 1.1 Tool Use Patterns

Modern AI coding agents use **typed tool schemas** to give the LLM a structured interface to external capabilities. The dominant pattern is:

| Pattern | Description | Adoption |
|---------|-------------|----------|
| **JSON Schema tools** | Each tool declares a JSON Schema for its input; the model emits structured `tool_use` blocks | Anthropic, OpenAI, Google |
| **MCP (Model Context Protocol)** | Standardized tool/resource discovery over stdio/HTTP transports | Claude Code, VS Code, Cursor |
| **A2A (Agent-to-Agent)** | Google's agent interop protocol for cross-agent tool delegation | Early adoption (2025–2026) |
| **Decoupled execution** | Tool dispatch is asynchronous — the model issues a call, the runtime executes it, and the result is injected into the next turn | Universal |

**Key insight:** Tool schemas serve as both the model's API reference and runtime validation. The JSON Schema is sent to the model in the system prompt, then reused on the backend to validate the model's tool-call arguments before dispatch. This prevents malformed input from reaching tool handlers.

**Tool dispatch pipeline** (extracted from Claurst/Claw specs):
1. Model emits `tool_use` content blocks.
2. Runtime validates input against JSON Schema.
3. Permission enforcer checks capability tokens.
4. Pre-tool hook runs (can block).
5. Tool handler executes.
6. Post-tool hook runs.
7. Result normalized and injected as `tool_result`.

**Caduceus implementation:** `caduceus-tools` provides a `ToolRegistry` with builtin → runtime → plugin layers. Each tool has a JSON Schema input definition. `caduceus-permissions` enforces capability checks (6 tiers: `fs.read`, `fs.write`, `process.exec`, `network.http`, `git.mutate`, `fs.escape`). The dispatch pipeline is orchestrated by `caduceus-orchestrator::ConversationEngine`.

### 1.2 Planning Approaches

| Approach | How It Works | Strengths | Weaknesses |
|----------|-------------|-----------|------------|
| **ReAct** | Interleave reasoning ("Thought") with actions ("Act") and observations ("Obs") in a single loop | Simple, effective for linear tasks | Struggles with long-horizon planning |
| **Plan-and-Execute** | Separate planning phase produces a task list; execution phase runs each step | Better for complex multi-step tasks | Plan can be stale by execution time |
| **LangGraph graph-based** | Model the agent as a state machine / directed graph with conditional edges | Maximum control, supports cycles | Requires explicit graph definition |
| **Coordinator + DAG** | Coordinator agent decomposes goal into a dependency-aware task DAG, then dispatches to specialists | Best for multi-agent; handles parallelism | Higher latency from decomposition step |

**Caduceus implementation:** The core agent loop in `caduceus-orchestrator` uses a **ReAct-style iterative loop** (prompt → LLM → tool calls → repeat until `end_turn`). The post-v1 multi-agent layer (`caduceus-workers`, spec'd from Open Multi-Agent) uses the **Coordinator + DAG** pattern where a coordinator LLM call decomposes a goal into tasks with dependencies, and a worker pool executes them with bounded concurrency.

### 1.3 Self-Correction / Reflection

State-of-the-art approaches:

- **Multi-pass review:** After a coding task, re-run the conversation with the output included as context and ask the model to critique and fix.
- **Reviewer subagents:** Spawn a separate agent (possibly different model) to review the primary agent's work.
- **Structured output retry:** When the model outputs invalid JSON, inject a corrective message describing the validation error and retry once (from Open Multi-Agent spec).
- **Loop detection:** A sliding window of normalized turn fingerprints detects repetitive patterns. First occurrence: inject a warning. Second: terminate with `LoopDetected` error.

**Caduceus implementation:** Structured output validation + retry is spec'd in `caduceus-orchestrator`. Loop detection uses a fingerprint-based sliding window (from `caduceus-workers`). The multi-agent coordinator pattern allows a synthesis phase where the coordinator reviews completed task results before producing the final output.

### 1.4 Memory Systems

| Type | Storage | Lifetime | Use Case |
|------|---------|----------|----------|
| **Short-term scratchpad** | In-memory conversation transcript | Single session | Current task context |
| **Persistent vector/DB** | SQLite + qdrant-edge | Across sessions | Semantic search over code and conversation history |
| **Episodic memory** | Session transcripts (JSONL/SQLite) | Permanent | Resume sessions, search past interactions |
| **Semantic memory** | Vector embeddings of code chunks | Until re-index | Code intelligence, symbol lookup |
| **Instruction memory** | `.caduceus/memory.md` | Permanent (user-editable) | Learned conventions, project-specific rules |
| **Shared team memory** | Append-only SQLite log | Team session lifetime | Cross-agent context in multi-agent runs |

**Context compaction:** When the conversation transcript exceeds 85% of the model's token budget, the orchestrator compacts by summarizing older turns while preserving recent context and all tool results. Effort levels (Min → Max) control how aggressively compaction is applied.

**Caduceus implementation:** `caduceus-storage` provides SQLite persistence for sessions, messages, tool calls, costs, and memories (scoped: session / project / global). `caduceus-omniscience` provides vector search via embedded qdrant-edge. The `memory.md` pattern (from Claude Code's `MEMORY.md`) provides persistent learned context as append-only markdown.

### 1.5 Context Management

**RAG Pipeline (Retrieval-Augmented Generation):**

```
User query → Embedder → Vector search (qdrant-edge)
                               ↓
                    Top-K code chunks (with metadata)
                               ↓
                    Injected into system prompt as <context> blocks
                               ↓
                    LLM generates response grounded in retrieved code
```

**Token budget management:**
- Each model has a known context window (e.g., Claude 3.5 Sonnet: 200K tokens).
- Budget set to 85% of model max (Normal effort) to leave room for the response.
- System prompt + memory + project context + conversation history must fit within budget.
- Auto-compaction triggers when usage exceeds the threshold.

**Session/state IDs:** Every session has a UUID. Session state machine: `Creating → Initializing → ShellReady → LaunchingAgent → Idle ⇄ Busy → Closing → Destroyed`.

**Caduceus implementation:** `caduceus-orchestrator::ConversationEngine` assembles the system prompt with layered context (working dir, memory, instruction files, project scan). Token counting uses `tiktoken-rs` for OpenAI-compatible BPE. Context compaction is built into the `run_query_loop` algorithm. Session state is persisted in SQLite via `caduceus-storage`.

### 1.6 Multi-Agent Coordination

| Pattern | Description | When to Use |
|---------|-------------|-------------|
| **Single agent** | One model, one conversation, one tool registry | Simple tasks, v1 |
| **Peer-to-peer** | Agents communicate directly via message bus | Decentralized tasks |
| **State machine** | Agent transitions through explicit states with conditional routing | Complex workflows with branching |
| **Coordinator pattern** | One agent decomposes and assigns; workers execute | Complex multi-step tasks requiring specialization |

**Coordinator pattern details** (from Open Multi-Agent spec):
1. **Simple-goal short-circuit:** If the objective appears simple and a specialist matches, bypass the coordinator and route directly.
2. **Decomposition phase:** Coordinator generates a structured task list with titles, descriptions, assignees, and dependency references.
3. **Queue execution:** Dependency-aware task pipeline with bounded concurrency.
4. **Synthesis phase:** Coordinator aggregates results into a final response.

**Concurrency model** (three-layer semaphore):
1. Tool execution: bounded parallelism per agent turn.
2. Agent pool: global concurrent agent limit.
3. Per-agent mutex: prevents the same agent from running concurrently.

**Failure cascades:** Task failure propagates recursively to all dependent tasks in the DAG (status → `Failed`). Retry uses exponential backoff (configurable floor, ceiling, max attempts).

**Caduceus implementation:** v1 uses single-agent `caduceus-orchestrator`. Post-v1 `caduceus-workers` implements the full coordinator + DAG pattern. Shared context uses an append-only SQLite log (`SharedMemory`), not OS shared memory.

### 1.7 Observability & Guardrails

| Mechanism | Purpose | Implementation |
|-----------|---------|----------------|
| **Tracing** | Structured trace spans for model calls, tool calls, task execution, agent execution | Span categories with run ID, timing, actor identity |
| **Token counting** | Per-turn and cumulative token tracking with cost in USD | `caduceus-telemetry` with SQLite cost log |
| **Audit log** | Append-only record of every permission decision | `caduceus-permissions` → SQLite `audit_log` table |
| **Capability tokens** | Default-deny execution model; 6 capability tiers | `caduceus-permissions::PermissionEnforcer` |
| **Human-in-the-loop** | Permission dialogs for destructive operations; `PermissionRequest` events | `AgentEvent::PermissionRequest` → UI dialog → `approve_permission` IPC |
| **Budget limits** | Hard-stop when cumulative cost exceeds user-set USD cap | `caduceus-telemetry` |
| **Loop detection** | Fingerprint-based sliding window detects repetitive tool-call patterns | `caduceus-workers` |
| **Prompt-injection defense** | Untrusted content wrapped as data-only blocks with provenance tags | `caduceus-orchestrator` wraps tool outputs in `<tool_result trust="untrusted">` |

**Caduceus implementation:** `caduceus-telemetry` handles token counting, per-turn cost calculation, and SQLite cost logging (local only — no external telemetry). `caduceus-permissions` provides the append-only audit log and capability-token system. All permission grant decisions are recorded with timestamp, tool name, capability, sanitized arguments (secrets redacted), and user decision.

---

## 2. Instruction Management — File Formats & Standards

### 2.1 File Format Landscape

| Format | Tool | Location | Structure |
|--------|------|----------|-----------|
| `AGENTS.md` | OpenAI / Linux Foundation | Repo root | Markdown with sections for capabilities, tools, constraints |
| `CLAUDE.md` | Claude Code | Repo root + `.claude/` | Hierarchical markdown with user → project → path scoping |
| `.cursor/rules/*.mdc` | Cursor | Project dir | YAML frontmatter + markdown body with glob-based `applyTo` |
| `.windsurf/rules/*.md` | Windsurf | Project dir | Markdown with YAML frontmatter |
| `copilot-instructions.md` | GitHub Copilot | `.github/` | Plain markdown; agents in `.github/agents/*.agent.md` |
| `CADUCEUS.md` | Caduceus | Repo root | Project instructions (like CLAUDE.md) |

### 2.2 Format Comparison

| Criterion | YAML frontmatter + MD | JSON | Pure Markdown | TOML |
|-----------|----------------------|------|--------------|------|
| **Token efficiency** | ★★★★★ (30% better than JSON) | ★★★ | ★★★★ | ★★★★ |
| **Human readability** | ★★★★★ | ★★ | ★★★★★ | ★★★★ |
| **Machine parsability** | ★★★★ | ★★★★★ | ★★ | ★★★★ |
| **Industry adoption** | Dominant (Claude, Copilot, Cursor, Windsurf) | MCP configs only | Legacy | Rust ecosystem |
| **Supports structured metadata** | Yes (YAML header) | Yes | No | Yes |
| **Supports freeform instructions** | Yes (MD body) | Awkward | Yes | No |

**Winner: YAML frontmatter + Markdown body.** Research confirms YAML is ~30% more token-efficient than JSON for the same structured data, and this format is now the dominant pattern across all major AI coding tools.

### 2.3 Priority Hierarchy (Highest → Lowest)

```
1. User global        (~/.caduceus/instructions.md)
2. Project root       (CADUCEUS.md or AGENTS.md)
3. Path-specific      (.caduceus/instructions/*.md with applyTo globs)
4. Active agent       (.caduceus/agents/*.md when selected)
5. Active skill       (.caduceus/skills/*.md when triggered)
6. MCP-discovered     (dynamic from MCP servers)
7. Memory             (.caduceus/memory.md — lowest priority, auto-updated)
```

**Merge semantics:** More specific always wins. Path-specific instructions with `applyTo: "**/*.rs"` only activate when the agent is working on Rust files. Agent/skill instructions only activate when explicitly selected or trigger-matched.

### 2.4 Agent & Skill Definition Format

**Agent definition:**
```yaml
---
name: code-reviewer
description: Reviews code for bugs and security
tools: [read_file, grep_search]
applyTo: "**/*.rs"
triggers:
  - "review this"
  - "check for bugs"
---
You are a senior code reviewer...
```

**Skill definition:**
```yaml
---
name: release
description: Prepare and ship a release
triggers:
  - "create a release"
  - "ship it"
steps:
  - Run tests
  - Update version
  - Create tag
  - Push
---
## Release Procedure
1. Verify all tests pass: `cargo test --workspace`
2. Update version in Cargo.toml
...
```

### 2.5 MCP Server Configuration

Standard `mcpServers` format (compatible with VS Code, Cursor, Claude):

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."],
      "type": "stdio"
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" },
      "type": "stdio"
    }
  }
}
```

**Naming conventions:** kebab-case, descriptive, no spaces. Keys: `command`, `args`, `type` (stdio | http | sse), `env`.

### 2.6 How Caduceus Implements This

Caduceus uses the file structure:

| File | Location | Purpose |
|------|----------|---------|
| `CADUCEUS.md` | repo root | Project instructions (like CLAUDE.md) |
| `AGENTS.md` | repo root | Universal standard (read alongside CADUCEUS.md) |
| `.caduceus/agents/*.md` | project dir | Custom agent personas |
| `.caduceus/skills/*.md` | project dir | Reusable task modules |
| `.caduceus/instructions/*.md` | project dir | Path-specific rules with glob scoping |
| `.caduceus/mcp.json` | project dir | MCP server configurations |
| `.caduceus/memory.md` | project dir | Persistent learned context |
| `~/.caduceus/instructions.md` | user home | User-global defaults |

**Key design decisions:**
1. Support `AGENTS.md` as universal standard alongside `CADUCEUS.md`.
2. YAML frontmatter for all structured config (30% fewer tokens than JSON).
3. JSON only for MCP config (industry standard — tools expect it).
4. Glob-based path scoping from Cursor's pattern (most granular).
5. Trigger phrases for agent/skill discovery from Copilot's pattern.
6. Memory as append-only markdown from Claude's MEMORY.md pattern.
7. Hierarchical merge — more specific always wins.

---

## 3. CRDT Collaborative Editing — Best Practices

### 3.1 CRDT Framework Comparison

| Feature | Yjs | Automerge | Custom RGA (Zed-style) |
|---------|-----|-----------|----------------------|
| **Language** | JavaScript (WASM bindings) | Rust + JS | Pure Rust |
| **CRDT type** | YATA (Yet Another Transformation Approach) | RGA variant + JSON doc | RGA (Replicated Growable Array) |
| **Text performance** | ★★★★ | ★★★ | ★★★★★ |
| **Binary size** | ~60 KB WASM | ~150 KB WASM | Native (zero overhead) |
| **Tombstone model** | Hidden characters | Operation log | Fragment visibility flag |
| **Memory efficiency** | Moderate (YATA overhead) | Higher (full op history) | Best (B+ tree rope) |
| **Undo support** | Built-in | Built-in | Custom (undo counts per fragment) |
| **Incremental reparse** | Via external integration | Via external integration | Native tree-sitter integration |
| **Anchor system** | Relative positions | Relative positions | Native Anchor type with Bias |
| **Rust-native** | No (FFI required) | Yes, but heavy | Yes |

### 3.2 AI Agents as CRDT Peers

A critical 2026 insight: **AI agents should be first-class CRDT participants**, not external editors that overwrite files. This means:

- **Replica identity:** Each agent gets a unique `ReplicaId` (Zed uses `ReplicaId(2)` for the default AI agent, `≥ 8` for additional collaborators).
- **Presence & awareness:** The agent's cursor position and selection are tracked and can be rendered in the editor.
- **Anchor-based targeting:** When the agent targets a function for editing, it uses an Anchor (not a byte offset) that remains stable through concurrent human edits.
- **Causal ordering:** Lamport timestamps provide total ordering; version vectors track causal dependencies.

### 3.3 Merge Semantics for Code

| Scenario | Resolution |
|----------|-----------|
| Concurrent inserts at same position | Higher Lamport timestamp wins; tie-broken by higher `replica_id` |
| Concurrent delete + insert | Insert wins (delete is a tombstone, not physical removal) |
| Undo/redo | `UndoOperation` flips fragment visibility via undo count (odd = undone, even = active) |
| Conflicting renames | Last-writer-wins per Lamport ordering |

**Intention preservation:** The RGA model preserves insertion intent because each character has a unique identity (fragment ID + offset). Two users inserting at the "same" position will see their text interleaved correctly based on Lamport order, not overwritten.

### 3.4 Garbage Collection and Tombstone Compaction

- **Tombstone retention:** Deleted text is preserved in a separate `deleted_text` rope for undo/redo support.
- **Version vector gating:** Tombstones can only be truly removed when all replicas have observed the deletion (checked via `Global::observed_all`).
- **Fragment merging:** Adjacent fragments from the same replica with consecutive timestamps can be merged to reduce B+ tree node count.
- **Checkpoint snapshots:** Periodically serialize the full buffer state; old operation history before the checkpoint can be discarded if all replicas have synced past it.

### 3.5 Network Transport

| Transport | Latency | Reliability | Use Case |
|-----------|---------|-------------|----------|
| **WebSocket** | Low (~50ms) | Good (TCP-based) | Primary for LAN/internet collaboration |
| **WebRTC** | Lowest (~10ms peer-to-peer) | Moderate (NAT traversal issues) | Local network, P2P editing |
| **Durable Streams** | Low | Best (persistent, resumable) | Cloudflare-based, serverless |
| **In-process channel** | Zero | Perfect | AI agent on same machine (v1) |

### 3.6 Recommendation for Caduceus: Zed-Inspired Custom RGA

Caduceus implements a **custom RGA** in `caduceus-crdt`, closely following Zed's architecture. This was chosen over Yjs/Automerge for several reasons:

1. **Pure Rust, zero FFI:** No WASM compilation or JavaScript runtime required.
2. **Rope data structure:** B+ tree-backed text storage (128-byte chunks with bitmap metadata for char boundaries, newlines, tabs) gives O(log n) random access.
3. **Native Anchor system:** Anchors with `Bias` (Left/Right) provide stable buffer positions critical for AI edit targeting.
4. **Lamport + Version Vectors:** Simple, well-understood causality model. `Lamport` for total ordering, `Global` (version vector) for causal dependency tracking.
5. **Fragment-based tombstones:** Deleted text preserved in a separate rope for undo; fragments have a `visible` flag rather than physical removal.
6. **Incremental reparse integration:** When `Buffer::edit()` produces a `Patch<Edit>`, the subscription fires → `caduceus-omniscience` receives the `InputEdit` for tree-sitter incremental reparse.

**How it differs from Yjs:**
- Yjs uses YATA (a different CRDT algorithm optimized for string editing) with character-level tracking. Our RGA uses fragment-level tracking with Locator-based fractional positioning.
- Yjs requires a JavaScript/WASM runtime; our implementation is pure Rust.
- Yjs doesn't have a native Anchor system — position tracking requires external relative-position utilities.

**How it differs from Automerge:**
- Automerge stores the full operation history for conflict resolution, leading to higher memory usage. Our approach uses fragment visibility flags and version vectors.
- Automerge's Rust implementation is heavier (full JSON document CRDT); ours is purpose-built for text editing only.

### 3.7 Clock Model Details

```rust
// Lamport scalar clock — total ordering for inserts/deletes/undo
pub struct Lamport { pub value: u32, pub replica_id: ReplicaId }
// Ordering: first by value, then by replica_id

// Version vector — causal dependency tracking, reconnect diffing
pub struct Global(pub SmallVec<[u32; 4]>);  // indexed by replica slot
// Operations: observe(timestamp), join(other), meet(other), observed(timestamp)

// Anchor — stable position that survives concurrent edits
pub struct Anchor {
    pub timestamp_replica_id: ReplicaId,
    pub timestamp_value: u32,
    pub offset: u32,
    pub bias: Bias,        // Left | Right
    pub buffer_id: BufferId,
}
```

**Caduceus implementation:** `caduceus-crdt` provides `Buffer`, `Rope`, `Anchor`, `Lamport`, and `Global` types. In v1, AI edits use `ReplicaId(2)`. Post-v1 multi-agent mode allocates unique replica IDs per worker.

---

## 4. AST-Based Code Search — Chunking & Embeddings

### 4.1 Why AST Chunking Beats Line/Character Splitting

| Approach | Preserves Semantics | Handles Nested Code | Cross-Reference Quality | Token Efficiency |
|----------|:-------------------:|:-------------------:|:----------------------:|:----------------:|
| Fixed-size character split | ❌ | ❌ | ❌ | ★★ |
| Line-based split | ❌ | ❌ | ★ | ★★★ |
| Recursive text split | ★ | ❌ | ★★ | ★★★ |
| **AST-aware chunking** | ✅ | ✅ | ★★★★★ | ★★★★★ |

**The problem with naive splitting:** A 512-token character split can cut a function in half, break a class definition across two chunks, or separate a function signature from its body. This destroys semantic coherence and makes embedding-based retrieval unreliable.

**AST-aware chunking** uses the parse tree to identify natural semantic boundaries: function definitions, class declarations, module blocks, trait implementations. Each chunk represents a complete semantic unit, making embeddings far more meaningful.

### 4.2 Tree-sitter: The De Facto Parser

Tree-sitter is a **GLR-based incremental parser** that produces concrete syntax trees (CSTs) with byte + row/column spans. Key properties for code search:

- **Incremental reparsing:** Apply `InputEdit` to the old tree, reparse, and only the changed subtrees are rebuilt. `changed_ranges(old, new)` returns the minimal affected ranges.
- **Error tolerance:** Produces useful structure even under syntax errors (`ERROR`, `MISSING` nodes). Valid chunks within an otherwise-errored file are still extractable.
- **Query-based extraction:** S-expression pattern language with captures (`@name`) for structural matching. Language-specific `.scm` query files define chunk extraction patterns.
- **Language coverage:** 100+ language grammars available, community-maintained.

**Extraction pipeline:**
1. Parse file → `Tree`.
2. Find semantic nodes via language-specific queries (`tags.scm`, custom chunk queries).
3. Use node byte ranges to slice source text.
4. On edit: apply `InputEdit`, reparse incrementally with old tree, refresh only changed ranges.

### 4.3 Chunking Strategies

| Strategy | Description | Best For |
|----------|-------------|----------|
| **Structure-aware** | One chunk per top-level function/class/module | Well-structured code |
| **Recursive segmentation** | Split large nodes recursively until each chunk is within token budget | Large files with deep nesting |
| **Sibling merging** | Merge small adjacent siblings (e.g., short utility functions) into a single chunk | Files with many small functions |
| **Sliding window with overlap** | AST-aligned sliding window with partial overlap at boundaries | Dense code with cross-function references |

**Caduceus approach:** Structure-aware chunking with recursive segmentation for large nodes.

### 4.4 Chunk Size Guidance

| Metric | Recommended Range | Rationale |
|--------|-------------------|-----------|
| **Token count** | 40–300 tokens per chunk (target: ~150) | Small enough for precise retrieval, large enough for context |
| **Embedding model context** | Stay within 512 tokens for most models | Most code embedding models have 512-token context windows |
| **Metadata overhead** | ~50 tokens per chunk for file path, language, symbol kind, etc. | Budget for metadata in the embedding input |

**Caduceus uses 40–300 tokens per chunk** with metadata including file_path, language, symbol_kind, symbol_name, container (enclosing class/module), start_line, end_line, and byte_range.

### 4.5 Embedding Models

| Model | Dimensions | Context | Code-Optimized | Open Source |
|-------|-----------|---------|:--------------:|:-----------:|
| **text-embedding-3-small** (OpenAI) | 1536 (or 768 via Matryoshka) | 8191 tokens | Moderate | ❌ |
| **text-embedding-3-large** (OpenAI) | 3072 (or 768/1536 via Matryoshka) | 8191 tokens | Moderate | ❌ |
| **CodeBERT** (Microsoft) | 768 | 512 tokens | ✅ | ✅ |
| **StarCoder** embeddings | 768–2048 | 8192 tokens | ✅ | ✅ |
| **Voyage Code 3** | 1024 | 16K tokens | ✅ | ❌ |
| **Nomic Embed Code** | 768 | 8192 tokens | ✅ | ✅ |
| **Local ONNX models** | 384–768 | 512 tokens | Varies | ✅ |

**Caduceus default:** 768-dimensional embeddings (compatible with qdrant-edge configuration). The `Embedder` trait allows swappable models — local ONNX for offline use, API-based for higher quality.

### 4.6 Hybrid Search

Pure vector search misses exact matches (e.g., searching for a function named `parse_query` might not rank the literal function definition highest). **Hybrid search** combines:

1. **Vector search** (semantic similarity via qdrant-edge) — finds conceptually related code.
2. **Graph-based structural analysis** (AST parent/child/sibling relationships) — finds structurally related code.
3. **Keyword/grep search** (exact text matching) — finds literal matches.

The results are fused using reciprocal rank fusion (RRF) or a learned reranker.

### 4.7 Open-Source Tools

| Tool | Language | Approach |
|------|----------|----------|
| **tree-sitter** | C + Rust bindings | Incremental GLR parser, S-expression queries |
| **code-chunk** | Python | AST-aware chunking with tree-sitter |
| **astchunk** | Rust | Tree-sitter based code chunking |
| **qdrant-edge** | Rust | Embedded vector search (EdgeShard) |
| **fastembed** | Rust | Local ONNX embedding inference |

### 4.8 How Caduceus Implements This — caduceus-omniscience

`caduceus-omniscience` provides two engines:

**AstEngine** (tree-sitter):
```
File change → AstEngine::reparse(path, source, edit)
  ├─ Apply InputEdit to cached Tree (incremental reparse)
  ├─ Compute changed_ranges(old_tree, new_tree)
  └─ Extract only affected chunks via tree-sitter Query
       → Vec<CodeChunk> { file_path, language, symbol_kind, symbol_name,
                           container, start_line, end_line, byte_range, code }
```

Supported chunk types: `function`, `method`, `class`, `module`, `trait_impl`. Each language has a dedicated `.scm` query file.

**VectorIndex** (qdrant-edge):
```
Collection config (per workspace):
  vector name:  "text"
  dimension:    768
  distance:     Cosine
  storage:      Float32, in-memory
  HNSW:         m=16, ef_construct=100
  payload idx:  repo, workspace, file_path, language, symbol_kind
```

**CodeIntelligence** unified API:
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

**Incremental re-index:** On file save, `AstEngine::reparse()` computes `changed_ranges()`, only affected chunks are re-extracted and re-embedded, and `VectorIndex::delete_by_file()` + `upsert_chunks()` runs only for the delta. `optimize()` after bulk ingest; `flush()` on session checkpoints.

---

## 5. Sandbox Execution — Secure Agent Code Running

### 5.1 Sandbox Technology Comparison

| Technology | Isolation Level | Startup Time | Memory Overhead | Security |
|-----------|:--------------:|:-----------:|:--------------:|:--------:|
| **Firecracker MicroVMs** | ★★★★★ (hardware virtualization) | ~125ms | ~5 MB per VM | ★★★★★ |
| **gVisor** | ★★★★ (kernel syscall interception) | ~100ms | ~15 MB | ★★★★ |
| **Docker** (namespace isolation) | ★★★ (shared kernel) | ~500ms | ~10 MB | ★★★ |
| **WASM sandboxes** | ★★★★ (memory isolation) | ~10ms | ~1 MB | ★★★★ |
| **Process sandboxing** (seccomp/landlock) | ★★ | ~1ms | ~0 | ★★ |

**Security hierarchy:** MicroVM > gVisor > WASM > Docker > Process

### 5.2 Cloud Sandbox Provider Comparison

| Provider | Technology | Languages | Persistent Storage | MCP Support | Pricing Model |
|----------|-----------|-----------|:-----------------:|:-----------:|:-------------|
| **E2B** | Firecracker MicroVMs | Any (Linux) | Volumes + Snapshots | ✅ (gateway on port 50005) | Per-second billing |
| **Modal** | gVisor containers | Python-focused | Volumes | ❌ | Per-second GPU/CPU |
| **Daytona** | Docker-based | Any | Docker volumes | ❌ | Open source + cloud |
| **Northflank** | Kubernetes pods | Any | PVCs | ❌ | Per-resource |

### 5.3 E2B Architecture (Caduceus's Chosen Platform)

```
┌──────────────────────────────────────────┐
│              Client SDK                   │
└─────────────┬────────────────────────────┘
              │ HTTPS / gRPC-web
              ▼
┌──────────────────────────────────────────┐
│           E2B API Server                  │
│   https://api.e2b.app                     │
│   - Sandbox lifecycle (REST)              │
│   - Template management (REST)            │
│   - Volume management (REST)              │
└─────────────┬────────────────────────────┘
              │
              ▼
┌──────────────────────────────────────────┐
│         Sandbox VM (micro-VM)             │
│   ┌──────────────────────────────────┐   │
│   │    envd daemon (port 49983)       │   │
│   │  - Filesystem (gRPC-web)          │   │
│   │  - Process/PTY (gRPC-web)         │   │
│   │  - File upload/download (HTTP)    │   │
│   └──────────────────────────────────┘   │
│   ┌──────────────────────────────────┐   │
│   │  mcp-gateway (port 50005)         │   │
│   │  - MCP server proxy               │   │
│   └──────────────────────────────────┘   │
└──────────────────────────────────────────┘
```

**Key properties:**
- Default timeout: 300s (5 min), max: 86,400s (24h Pro)
- envd port: 49983, MCP port: 50005
- Keepalive ping: 50s interval
- Auth: `X-API-Key` header or `E2B_API_KEY` env var

### 5.4 Best Practices for Agent Sandboxing

1. **Ephemeral per-execution:** Create a fresh sandbox for each agent task; destroy on completion. Prevents state leakage between tasks.
2. **Network isolation by default:** Only open specific ports via template config. Block outbound internet unless explicitly needed.
3. **Filesystem confinement:** All file operations within the sandbox's filesystem. No host filesystem access.
4. **Credential isolation:** Sandbox credentials (`envd_access_token`) stored in session state, never logged.
5. **Timeout enforcement:** Always set timeouts. Use keepalive pings to detect stale sandboxes.
6. **Snapshot for reproducibility:** Create snapshots before destructive operations for rollback capability.
7. **Resource monitoring:** Track CPU, memory, disk usage. Kill runaway processes.

### 5.5 Sandbox Lifecycle

```
create() → Running → pause() → Paused → resume() → Running
                                               └─ kill() → Killed
                  └─ set_timeout() → auto-kill on expiry
                  └─ create_snapshot() → SnapshotInfo
```

### 5.6 How Caduceus Implements This — caduceus-runtime/sandbox

**v1 (local execution):** `caduceus-runtime` uses structured process spawning with:
- Explicit `argv` arrays (no shell-string eval)
- Working directory canonicalized and locked to project root
- Environment: default-empty with minimal allowlist
- Timeout: soft kill at 30s, hard kill at 300s with process-group termination
- Output: stdout/stderr capped at 1 MB each
- File boundary: canonical path validation + symlink resolution enforces workspace root confinement
- Command denylist for defense-in-depth

**Post-v1 (cloud sandbox):** `caduceus-sandbox` wraps the E2B micro-VM API:
```
SandboxClient::create(opts) → Sandbox { id, envd_access_token, domain }
  ├─ Sandbox::commands → CommandsApi (run, list, kill, send_stdin)
  ├─ Sandbox::files    → FilesApi    (read, write, list, remove, watch_dir)
  ├─ Sandbox::pty      → PtyApi      (create, send_data, resize, kill)
  └─ Sandbox::git      → GitApi      (clone, checkout, status, diff)
```

All sandbox operations require the `process.exec` capability from `caduceus-permissions`.

---

## 6. LLM Provider Landscape — Multi-Provider Architecture

### 6.1 Provider Comparison

| Provider | Default Model | Key Strengths | API Style | Auth |
|----------|--------------|---------------|-----------|------|
| **Anthropic** | `claude-sonnet-4-6` | Best tool use, long context (200K) | Native Messages API | `ANTHROPIC_API_KEY` |
| **OpenAI** | `gpt-4o` | Broadest ecosystem, JSON mode | Chat Completions | `OPENAI_API_KEY` |
| **Google Gemini** | `gemini-2.5-flash` | Multimodal, large context (1M) | Generative Language API | `GOOGLE_API_KEY` |
| **xAI (Grok)** | `grok-2` | Real-time knowledge | OpenAI-compatible | `XAI_API_KEY` |
| **DeepSeek** | `deepseek-chat` | Cost-efficient reasoning | OpenAI-compatible | `DEEPSEEK_API_KEY` |
| **Ollama** (local) | `llama3.2` | Offline, free, private | OpenAI-compatible | None |
| **LM Studio** (local) | Current loaded model | GUI + API, offline | OpenAI-compatible | None |
| **vLLM** (local) | User-configured | High-throughput serving | OpenAI-compatible | None |
| **Groq** | `llama-3.3-70b-versatile` | Fastest inference | OpenAI-compatible | `GROQ_API_KEY` |
| **OpenRouter** | `anthropic/claude-sonnet-4` | Provider aggregation | OpenAI-compatible | `OPENROUTER_API_KEY` |
| **AWS Bedrock** | `anthropic.claude-sonnet-4-6-v1` | Enterprise, VPC | SigV4 + Bearer | AWS credentials |
| **Azure OpenAI** | `gpt-4o` | Enterprise, compliance | OpenAI-compatible | `AZURE_API_KEY` + resource |

### 6.2 The OpenAI-Compatible API Pattern

The most important architectural insight: **most providers implement the OpenAI Chat Completions API format.** This means a single `OpenAICompatibleAdapter` can cover:

- OpenAI (native)
- Ollama, vLLM, LM Studio, LLaMA.cpp (local)
- Groq, DeepSeek, Mistral, xAI, OpenRouter, Together AI, Perplexity, DeepInfra (cloud)

The adapter needs only: `base_url`, `api_key`, `model_id`, and optional `headers`.

**Anthropic is the exception** — it uses its own Messages API format with content blocks (text, tool_use, tool_result, thinking, image) rather than OpenAI's role-based message format. This requires a dedicated `AnthropicAdapter`.

### 6.3 Streaming Protocols

| Protocol | Format | Provider Usage |
|----------|--------|---------------|
| **SSE (Server-Sent Events)** | `text/event-stream` with `data:` prefixed JSON lines | Anthropic, OpenAI, most cloud providers |
| **WebSocket** | Binary frames | Some local servers, Bridge mode |
| **gRPC-web** | Protobuf streams | E2B sandbox communication |

**SSE is the dominant pattern.** Each SSE event contains a delta (partial token, tool call fragment, or control event). The provider adapter parses the stream into typed `StreamEvent` variants.

### 6.4 Rate Limiting and Retry Strategies

| Strategy | Description |
|----------|-------------|
| **Exponential backoff** | Base delay × multiplier^attempt, clamped to ceiling |
| **Jitter** | Random ±20% on each delay to prevent thundering herd |
| **Retry-After header** | Respect provider's `Retry-After` if present |
| **429 detection** | Automatic backoff on HTTP 429 (Too Many Requests) |
| **5xx retry** | Retry server errors (500, 502, 503) up to max attempts |
| **Timeout** | Per-request timeout (default: 60s) with connection-level timeout |

**Caduceus config:** Configurable floor, ceiling, and max attempts per retry policy.

### 6.5 Token Counting and Cost Management

- **Token counting:** `tiktoken-rs` for OpenAI-compatible BPE tokenization. Per-turn input and output tokens tracked.
- **Cost calculation:** Provider-specific per-token pricing (input and output rates differ). Stored in `costs` table.
- **Daily rollup:** `cost_daily` table aggregates by provider + model for dashboards.
- **Budget enforcement:** User-set USD cap; hard-stop when cumulative session cost exceeds it.
- **Effort levels:** Adjustable reasoning depth (Min → Max) that tunes token budget allocation from 50% to 95% of model max.

### 6.6 Model Registry

Caduceus ships a **bundled model snapshot** for major providers (Anthropic, OpenAI, Google). At runtime, it optionally refreshes from a remote API (cached locally, refreshed at most every 5 minutes). Network failures fall back silently to the bundled snapshot.

Features:
- Per-provider `models_whitelist` and `models_blacklist` arrays.
- Auto-selection: when no model is explicitly set, score available models by priority patterns to pick the best default.
- Model capability detection: vision, tools, streaming, thinking support.

### 6.7 How Caduceus Implements This — caduceus-providers

`caduceus-providers` exposes a `LlmProvider` trait with two concrete adapters:

1. **`AnthropicAdapter`** — Native Anthropic Messages API with streaming SSE.
2. **`OpenAICompatibleAdapter`** — Covers OpenAI, Ollama, vLLM, LM Studio, Groq, DeepSeek, and 10+ other providers.

**Provider resolution order:**
1. CLI flags (highest priority, session-only)
2. `/connect` command (interactive session)
3. Project config
4. Global config (`~/.caduceus/settings.json`)
5. Default: Anthropic

**Streaming pipeline:**
```
[caduceus-providers] HTTP SSE → StreamEvent (TextDelta, ToolCallDelta, MessageStop, …)
    │ tokio::sync::mpsc (bounded, cap 1024)
    ▼
[caduceus-orchestrator] StreamAccumulator → AgentEvent
    │ tokio::sync::mpsc (bounded, cap 1024)
    ▼
[src-tauri] app.emit("agent:event", &agent_event)
    │ Tauri event bus (WebView → JS bridge)
    ▼
[React] listen("agent:event", callback) → useReducer dispatch
```

**Backpressure:** `TextDelta` events are droppable on overflow; control events (`ToolCallStart`, `PermissionRequest`, `TurnComplete`, `Error`) are never dropped.

---

## 7. Desktop App Architecture — Tauri vs Electron

### 7.1 Why Tauri 2 Over Electron

| Criterion | Tauri 2 | Electron |
|-----------|---------|----------|
| **Binary size** | ~5–10 MB | ~100–200 MB (bundles Chromium) |
| **Memory usage** | ~50–100 MB | ~200–500 MB |
| **Startup time** | ~200ms | ~1–3s |
| **Backend language** | Rust (native performance) | Node.js (JavaScript) |
| **WebView** | OS-native (WebKit/WebView2/WebKitGTK) | Bundled Chromium |
| **Security model** | Capability-based IPC with validation | Full Node.js access from renderer |
| **Auto-updater** | Built-in plugin | electron-updater |
| **Cross-platform** | macOS, Windows, Linux, iOS, Android | macOS, Windows, Linux |

**Key advantage for Caduceus:** The Rust backend is the same language as the agent runtime, CRDT engine, AST parser, and vector index. No FFI boundary between the app shell and the core logic.

### 7.2 IPC Patterns (Typed Commands, Event Streaming)

**Tauri IPC** uses two communication patterns:

1. **Commands (invoke):** Frontend calls a named Rust function with typed arguments, awaits a typed response.
2. **Events (listen/emit):** Rust backend emits named events that the frontend subscribes to. Used for streaming (LLM tokens, agent status changes).

**Command groups in Caduceus:**

| Group | Commands | Notes |
|-------|----------|-------|
| `session::*` | `session_create`, `session_list`, `session_delete` | CRUD for agent sessions |
| `agent::*` | `agent_turn`, `agent_abort` | Drive the conversation loop |
| `terminal::*` | `terminal_exec`, `create_pty`, `write_pty`, `resize_pty`, `close_pty` | PTY management |
| `project::*` | `project_scan` | Language/framework detection |
| `git::*` | `git_status`, `git_diff` | Read-only Git queries |
| `config::*` | `config_get` | Configuration retrieval |

**Type contract:** IPC contract types are defined in `caduceus-core` and mirrored as TypeScript types in `src/types/`. A code-generation step ensures Rust and TypeScript types never drift.

### 7.3 PTY Management for Terminal Emulation

From the Hermes IDE spec, the PTY session lifecycle:

```
Creating → Initializing → ShellReady → LaunchingAgent → Idle ⇄ Busy
                                                         ↕        ↕
                                                    NeedsInput   Error
                                                         ↓
                                                   Closing → Destroyed
```

**Key patterns:**
- **Module-level terminal pool:** PTY instances are managed by the Rust backend (not React-owned xterm instances).
- **PTY security:** OSC 52 clipboard exfiltration stripped; paste bracket mode enforced.
- **Terminal intelligence:** < 5ms per suggestion run; sources: history + static command index + project context.
- **Ghost text:** Inline command suggestion suffix rendered over the terminal.

### 7.4 WebView Security (CSP, Input Validation)

```
Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'
```

- No `eval()`, no inline scripts, no remote script loading.
- LLM-generated markdown sanitized before DOM insertion (strip `<script>`, `<iframe>`, `on*` handlers, `javascript:` URIs).
- All Tauri IPC inputs validated on the Rust side — backend treats every IPC message as untrusted.
- API keys never cross the IPC boundary — React frontend never sees raw API keys.

**macOS-specific:**
- Minimum: macOS 13.0
- Title bar: overlay style
- Window: 1200×800 default, 600×400 minimum

### 7.5 How Caduceus Implements This — src-tauri

**Application lifecycle** (from Hermes IDE supplement spec):
1. Register Tauri plugins (shell, notifications, dialog, updater, process, telemetry).
2. On setup: resolve app data dir, create directories, write `running.marker`, initialize SQLite DB, run migrations, clean stale worktrees, initialize `sysinfo::System`, start worktree watcher, store `AppState` in managed state, install native menu.
3. Wires close/destroy/exit events to a one-shot workspace save path.

**Shared app state:**
```rust
struct AppState {
    db: Mutex<Database>,
    pty_manager: Mutex<PtyManager>,
    sys: Mutex<sysinfo::System>,
    // ... startup marker, worktree watcher
}
```

**Frontend architecture:**
- React state/context for session management
- Flat component files
- Typed `src/api/` invoke layer
- Module-level terminal pool (not React-owned xterm instances)
- xterm.js for terminal rendering, CodeMirror for code editing

---

## 8. References

### Agent Architecture

- [OpenAI Function Calling](https://platform.openai.com/docs/guides/function-calling) — Tool use schema specification
- [Anthropic Tool Use](https://docs.anthropic.com/en/docs/build-with-claude/tool-use) — Content block tool_use/tool_result protocol
- [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) — Standardized tool/resource discovery
- [Google A2A Protocol](https://github.com/google/A2A) — Agent-to-Agent interop
- [LangGraph](https://github.com/langchain-ai/langgraph) — Graph-based agent orchestration
- [ReAct: Synergizing Reasoning and Acting](https://arxiv.org/abs/2210.03629) — Yao et al., 2022
- [Plan-and-Solve Prompting](https://arxiv.org/abs/2305.04091) — Wang et al., 2023

### Instruction Management

- [AGENTS.md Specification](https://openai.com/index/agents-md/) — OpenAI/Linux Foundation universal standard
- [Claude Code Instructions](https://docs.anthropic.com/en/docs/claude-code/settings#instructions-files) — CLAUDE.md hierarchical pattern
- [Cursor Rules](https://docs.cursor.com/context/rules-for-ai) — .cursor/rules/*.mdc with glob scoping
- [Windsurf Rules](https://docs.windsurf.com/windsurf/memories#rules) — .windsurf/rules/*.md
- [GitHub Copilot Instructions](https://docs.github.com/en/copilot/customizing-copilot) — copilot-instructions.md

### CRDT Collaborative Editing

- [Zed Editor Source](https://github.com/zed-industries/zed) — RGA-based CRDT text buffer
- [Yjs](https://github.com/yjs/yjs) — YATA CRDT framework
- [Automerge](https://github.com/automerge/automerge) — Rust CRDT library
- [CRDT Survey: Shapiro et al.](https://hal.inria.fr/inria-00555588) — Comprehensive CRDT taxonomy
- [RGA: Replicated Growable Array](https://pages.lip6.fr/Marc.Shapiro/papers/rgasplit-group2016-11.pdf) — Roh et al.
- [Martin Kleppmann — CRDTs: The Hard Parts](https://www.youtube.com/watch?v=x7drE24geUw) — Advanced CRDT challenges

### AST-Based Code Search

- [Tree-sitter](https://tree-sitter.github.io/tree-sitter/) — Incremental parser
- [Qdrant](https://qdrant.tech/) — Vector search engine
- [Qdrant Edge](https://github.com/qdrant/qdrant/tree/master/lib/edge) — Embedded Rust vector search
- [CodeBERT](https://github.com/microsoft/CodeBERT) — Code-optimized embeddings
- [StarCoder](https://huggingface.co/bigcode/starcoder) — Code LLM with embedding capability
- [Voyage Code 3](https://docs.voyageai.com/docs/embeddings) — Code-optimized embeddings

### Sandbox Execution

- [E2B](https://e2b.dev/) — Micro-VM sandboxes for AI agents
- [Firecracker](https://firecracker-microvm.github.io/) — Lightweight VMM
- [gVisor](https://gvisor.dev/) — Container runtime sandbox
- [Modal](https://modal.com/) — Serverless compute with GPU support
- [Daytona](https://daytona.io/) — Development environment management

### LLM Provider APIs

- [Anthropic Messages API](https://docs.anthropic.com/en/api/messages) — Native API reference
- [OpenAI Chat Completions](https://platform.openai.com/docs/api-reference/chat) — De facto standard
- [Google Gemini API](https://ai.google.dev/api) — Generative Language API
- [Ollama](https://ollama.ai/) — Local model running
- [vLLM](https://docs.vllm.ai/) — High-throughput serving
- [tiktoken](https://github.com/openai/tiktoken) — BPE tokenization

### Desktop App Architecture

- [Tauri 2](https://v2.tauri.app/) — Rust-based desktop app framework
- [Electron](https://www.electronjs.org/) — Chromium-based desktop apps
- [xterm.js](https://xtermjs.org/) — Terminal emulation for web
- [CodeMirror](https://codemirror.net/) — Browser-based code editor
- [ratatui](https://ratatui.rs/) — Rust TUI framework

---

## Appendix A: Source Specifications Inventory

| Spec File | Source Repo | Focus Area | Key Contributions |
|-----------|-------------|------------|-------------------|
| `spec-claurst-blackbox.md` | Claurst | Provider landscape | 18+ providers, model registry, buddy system |
| `spec-claurst-full.md` | Claurst | Complete behavioral spec | Query loop, tool system, permissions, memory, config, token budget |
| `spec-claw-code.md` | Claw Code | Agent harness | CLI modes, runtime, telemetry, plugins, tools |
| `spec-e2b.md` | E2B | Sandbox execution | Micro-VM lifecycle, filesystem, PTY, MCP gateway, volumes, snapshots |
| `spec-hermes-ide.md` | Hermes IDE | Desktop app | PTY management, SQLite persistence, project scanner, Git integration |
| `spec-hermes-ide-supplement.md` | Hermes Supp | Desktop app details | Tauri IPC surface, PTY internals, frontend contracts |
| `spec-open-multi-agent.md` | Open Multi-Agent | Multi-agent | Coordinator pattern, task DAG, agent pool, shared memory, tracing |
| `spec-qdrant.md` | Qdrant | Vector search | Embedded EdgeShard, search/query API, payload indexing |
| `spec-tree-sitter.md` | Tree-sitter | AST parsing | Incremental reparse, query system, error recovery |
| `spec-zed-crdt.md` | Zed | CRDT editing | RGA buffer, Lamport clocks, Rope, Anchor system, version vectors |

**Wiring Plans:**
- `caduceus-wiring-plan-part1.md` — Architecture overview, end-to-end data flow, crate boundaries
- `caduceus-wiring-plan-part2.md` — Data model (SQLite schema), security model, detailed wiring

---

## Appendix B: Instruction Management Research (Full)

*The following section is the complete content of `research-instruction-management.md`, preserved here for consolidated reference.*

---

# Caduceus Instruction Management — Research Findings

## Best Practices for Agent Instruction Files (2026)

### File Types & Naming Conventions

| File | Location | Format | Purpose | Standard |
|------|----------|--------|---------|----------|
| `AGENTS.md` | repo root | Markdown | Universal agent instructions (tool-agnostic) | OpenAI/Linux Foundation standard |
| `CADUCEUS.md` | repo root | Markdown | Caduceus-specific project instructions (like CLAUDE.md) | Our convention |
| `.caduceus/agents/*.md` | project dir | YAML frontmatter + Markdown | Custom agent personas | Copilot pattern |
| `.caduceus/skills/*.md` | project dir | YAML frontmatter + Markdown | Reusable task modules | Copilot pattern |
| `.caduceus/instructions/*.md` | project dir | YAML frontmatter + Markdown | Path-specific rules (glob patterns) | Cursor pattern |
| `.caduceus/mcp.json` | project dir | JSON | MCP server configurations | VS Code/Cursor standard |
| `.caduceus/memory.md` | project dir | Markdown | Persistent learned context | Claude pattern |
| `~/.caduceus/instructions.md` | user home | Markdown | User-global defaults | Claude pattern |

### Format Decision: YAML Frontmatter + Markdown

**Winner: YAML frontmatter + Markdown body**

Research shows:
- **YAML is ~30% more token-efficient than JSON** for the same structured data
- YAML frontmatter + Markdown is the dominant pattern across Claude Code, Copilot, Cursor, Windsurf, Codex
- Better readability for human editing
- Supported by all major agent frameworks

```markdown
---
name: code-reviewer
description: Reviews code for bugs and security
tools: [read_file, grep_search]
applyTo: "**/*.rs"
triggers:
  - "review this"
  - "check for bugs"
---
You are a senior code reviewer...
```

### Priority Hierarchy (highest to lowest)

1. **User global** (`~/.caduceus/instructions.md`)
2. **Project root** (`CADUCEUS.md` or `AGENTS.md`)
3. **Path-specific** (`.caduceus/instructions/*.md` with `applyTo` globs)
4. **Active agent** (`.caduceus/agents/*.md` when selected)
5. **Active skill** (`.caduceus/skills/*.md` when triggered)
6. **MCP-discovered** (dynamic from MCP servers)
7. **Memory** (`.caduceus/memory.md` — lowest priority, auto-updated)

### MCP Server Config Format

Standard `mcpServers` format (compatible with VS Code, Cursor, Claude):

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."],
      "type": "stdio"
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" },
      "type": "stdio"
    }
  }
}
```

Naming: kebab-case, descriptive, no spaces. Keys: `command`, `args`, `type` (stdio|http|sse), `env`.

### Agent Definition Format

```markdown
---
name: test-writer
description: Writes comprehensive tests for code
tools: [read_file, write_file, bash, grep_search]
model: claude-sonnet-4-6
triggers:
  - "write tests"
  - "add coverage"
---
You are a test engineer. For each function:
1. Write happy path test
2. Write edge case tests
3. Use project's test patterns
```

### Skill Definition Format

```markdown
---
name: release
description: Prepare and ship a release
triggers:
  - "create a release"
  - "ship it"
steps:
  - Run tests
  - Update version
  - Create tag
  - Push
---
## Release Procedure
1. Verify all tests pass: `cargo test --workspace`
2. Update version in Cargo.toml
3. Create git tag: `git tag v{version}`
4. Push: `git push origin main --tags`
```

### Key Design Decisions for Caduceus

1. **Support AGENTS.md as universal standard** — read it alongside CADUCEUS.md
2. **Use YAML frontmatter** for all structured config (30% fewer tokens than JSON)
3. **JSON only for MCP config** (industry standard, tools expect it)
4. **Glob-based path scoping** from Cursor's pattern (most granular)
5. **Trigger phrases** for agent/skill discovery (from Copilot pattern)
6. **Memory as append-only markdown** (from Claude's MEMORY.md pattern)
7. **Hierarchical merge** — more specific always wins
