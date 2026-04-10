# Caduceus — Feature Matrix

> **Caduceus** synthesizes capabilities from **7 source repositories** into a unified Rust-based AI coding assistant, organized across **6 architectural layers**. This document catalogs every feature — implemented, stubbed, planned, and envisioned — providing a single source of truth for project scope and progress.

| Source Repo | Shorthand |
|---|---|
| Hermes IDE | Hermes |
| Hermes IDE Supplement | Hermes Supp |
| Claw Code (Claude CLI) | Claw |
| Claurst (Claude Code internals) | Claurst |
| Open Multi-Agent | Multi-Agent |
| E2B (Cloud Sandbox) | E2B |
| Tree-sitter + Qdrant + Zed CRDT | TS/Q/Zed |

**Status Legend:** ✅ Implemented · 🔧 Stubbed · 📋 Planned · 💡 Future  
**Priority:** P0 (Critical) · P1 (High) · P2 (Medium) · P3 (Nice-to-have)

---

## 1. Feature Matrix

### 1.1 Presentation Layer (22 features)

| # | Feature | Description | Source(s) | Status | Priority | Crate |
|---|---------|-------------|-----------|--------|----------|-------|
| 1 | TUI framework (ratatui) | Terminal UI shell with panels, input, scrollback | Claurst | ✅ | P0 | `caduceus-orchestrator` |
| 2 | Streaming token rendering | Real-time display of LLM output as tokens arrive | Claurst, Claw | ✅ | P0 | `caduceus-providers` |
| 3 | Permission dialogs (Y/N/A) | Interactive permission prompts with allow-once / allow-session / deny | Claurst | ✅ | P0 | `caduceus-permissions` |
| 4 | Slash command palette | Autocomplete-enabled command input with `/` prefix | Claw, Claurst | ✅ | P0 | `caduceus-orchestrator` |
| 5 | PTY output rendering | Faithful rendering of subprocess PTY output in the terminal | Hermes | ✅ | P0 | `caduceus-runtime` |
| 6 | Syntax highlighting (syntect) | Language-aware syntax coloring for code blocks and diffs | Claurst | 📋 | P1 | `caduceus-ui` |
| 7 | Diff viewer | Side-by-side / unified diff display for file edits | Hermes, Claurst | 📋 | P1 | `caduceus-ui` |
| 8 | Model picker (searchable) | Fuzzy-searchable model selector with provider grouping | Claurst, Hermes | 📋 | P1 | `caduceus-ui` |
| 9 | Session browser / resume | List, search, and resume past conversation sessions | Claurst, Claw | 📋 | P1 | `caduceus-ui` |
| 10 | Status line | Persistent footer showing model, token count, git branch, cost | Claurst | 📋 | P1 | `caduceus-ui` |
| 11 | Split terminal layout | Multiple terminal panes with drag-to-resize | Hermes | 📋 | P1 | `caduceus-app` |
| 12 | Headless mode (`--print`) | Non-interactive single-shot mode for scripting and CI | Claw, Claurst | 📋 | P1 | `caduceus-cli` |
| 13 | Output formats (text/json/stream) | Selectable output serialization for programmatic consumption | Claw, Claurst | 📋 | P1 | `caduceus-cli` |
| 14 | Tauri shell + IPC | Native desktop window with Rust↔JS IPC bridge | Hermes | 📋 | P1 | `caduceus-app` |
| 15 | Context visualizer (`/ctx_viz`) | Visual breakdown of context window usage by category | Claurst | 📋 | P2 | `caduceus-ui` |
| 16 | Theme picker | Switchable color themes with preview | Claurst, Hermes | 📋 | P2 | `caduceus-ui` |
| 17 | Vim mode (modal editing) | Modal key bindings for the input area | Claurst | 📋 | P2 | `caduceus-ui` |
| 18 | Desktop notifications | OS-native notifications on task completion / errors | Hermes | 📋 | P2 | `caduceus-app` |
| 19 | Keybinding configurator | User-customizable key mappings via config file | Claurst | 📋 | P2 | `caduceus-ui` |
| 20 | Image rendering (Sixel/Kitty) | Inline image display using terminal graphics protocols | Claurst | 💡 | P3 | `caduceus-ui` |
| 21 | Buddy / companion sprite | Animated ASCII/pixel companion that reflects agent state | Claurst | 💡 | P3 | `caduceus-companion` |
| 22 | Voice input (Deepgram STT) | Speech-to-text input via streaming microphone capture | Claurst | 💡 | P3 | `caduceus-ui` |

### 1.2 Orchestration Layer (32 features)

| # | Feature | Description | Source(s) | Status | Priority | Crate |
|---|---------|-------------|-----------|--------|----------|-------|
| 23 | Multi-turn conversation loop | Core agentic loop: prompt → LLM → tools → repeat | Claurst, Claw, Multi-Agent | ✅ | P0 | `caduceus-orchestrator` |
| 24 | System prompt assembly | Dynamic construction of system prompt from config, project context, and memory | Claurst | ✅ | P0 | `caduceus-orchestrator` |
| 25 | Tool dispatch pipeline | Route tool calls to handlers with schema validation and result normalization | Claurst, Claw | ✅ | P0 | `caduceus-tools` |
| 26 | Permission gating | Gate every tool invocation through the capability-token permission system | Claurst | ✅ | P0 | `caduceus-permissions` |
| 27 | Provider registry | Register and resolve LLM providers by name with health checks | Claurst | ✅ | P0 | `caduceus-providers` |
| 28 | Model registry (bundled + refresh) | Bundled model catalog with runtime refresh from provider APIs | Claurst | ✅ | P0 | `caduceus-providers` |
| 29 | Provider capability detection | Introspect provider support for vision, tools, streaming, thinking | Claurst | ✅ | P0 | `caduceus-providers` |
| 30 | Slash command registry | Extensible registry of `/commands` with argument parsing | Claw, Claurst | ✅ | P0 | `caduceus-orchestrator` |
| 31 | Configuration layering | Merge config from CLI flags → env vars → project → global defaults | Claurst | 🔧 | P0 | `caduceus-core` |
| 32 | Retry logic (exponential backoff) | Automatic retries with jitter for transient API failures | Claurst, Multi-Agent | 📋 | P0 | `caduceus-providers` |
| 33 | Cancellation token | Cooperative cancellation of in-flight LLM calls and tool executions | Claurst, Multi-Agent | 📋 | P0 | `caduceus-orchestrator` |
| 34 | Effort levels (Min → Max) | Adjustable reasoning depth that tunes system prompt and model params | Claurst | 📋 | P1 | `caduceus-orchestrator` |
| 35 | Query configuration | Per-query overrides for model, temperature, max_tokens | Claurst | 📋 | P1 | `caduceus-orchestrator` |
| 36 | Parallel tool execution | Execute independent tool calls concurrently with join semantics | Claurst, Multi-Agent | 📋 | P1 | `caduceus-tools` |
| 37 | Tool round limiting | Cap the number of tool-use rounds per turn to prevent runaway loops | Claurst | 📋 | P1 | `caduceus-orchestrator` |
| 38 | Max turns limit | Hard limit on total conversation turns for automated runs | Claurst, Multi-Agent | 📋 | P1 | `caduceus-orchestrator` |
| 39 | Extended thinking (`--thinking`) | Enable chain-of-thought / thinking mode for supported models | Claurst | 📋 | P1 | `caduceus-orchestrator` |
| 40 | Structured output validation + retry | Validate LLM JSON output against schema; retry on failure | Multi-Agent | 📋 | P1 | `caduceus-orchestrator` |
| 41 | Loop detection (fingerprint-based) | Detect repetitive tool-call patterns and break agentic loops | Multi-Agent | 📋 | P1 | `caduceus-orchestrator` |
| 42 | Hook system (~27 lifecycle events) | Pre/post hooks for tool calls, turns, sessions, errors | Claurst | 📋 | P1 | `caduceus-permissions` |
| 43 | Budget USD limit | Hard-stop when cumulative session cost exceeds a user-set dollar cap | Claurst | 📋 | P1 | `caduceus-telemetry` |
| 44 | Permission modes | Switchable modes: default (ask), plan (read-only), bypass (trusted) | Claurst | 🔧 | P1 | `caduceus-permissions` |
| 45 | Provider connection (`/connect`) | Interactive flow to add API keys for new providers | Claurst | 📋 | P1 | `caduceus-providers` |
| 46 | MCP client (tool discovery) | Discover and invoke tools from external MCP servers | Claurst | 📋 | P1 | `caduceus-mcp` |
| 47 | Model whitelisting / blacklisting | Admin-configurable allow/deny lists for model selection | Claurst | 📋 | P2 | `caduceus-providers` |
| 48 | Tool choice control | Force or suppress specific tool use via API tool_choice param | Claurst | 📋 | P2 | `caduceus-orchestrator` |
| 49 | Response format (JSON mode) | Request structured JSON responses from the model | Claurst | 📋 | P2 | `caduceus-orchestrator` |
| 50 | Feature flags | Runtime-togglable feature gates for gradual rollout | Claurst | 📋 | P2 | `caduceus-core` |
| 51 | Agent personas (build/plan/explore) | Pre-configured system prompt variants for different task modes | Claurst | 📋 | P2 | `caduceus-orchestrator` |
| 52 | Plugin system (TOML/JSON manifest) | Load third-party plugins with declared capabilities and tools | Claurst | 💡 | P2 | `caduceus-plugin` |
| 53 | Plugin commands / agents / skills | Plugins can register new commands, agent types, and skill handlers | Claurst | 💡 | P2 | `caduceus-plugin` |
| 54 | Plugin capability grants | Fine-grained permission grants scoped to each plugin | Claurst | 💡 | P2 | `caduceus-plugin` |

### 1.3 Workers Layer (21 features)

| # | Feature | Description | Source(s) | Status | Priority | Crate |
|---|---------|-------------|-----------|--------|----------|-------|
| 55 | Anthropic provider adapter | Native Claude API integration with streaming, tools, vision | Claurst | ✅ | P0 | `caduceus-providers` |
| 56 | OpenAI-compatible adapter | Chat Completions API adapter (GPT-4o, o1, compatible endpoints) | Claurst | ✅ | P0 | `caduceus-providers` |
| 57 | Ollama adapter | Local model support via OpenAI-compatible interface | Claurst | ✅ | P0 | `caduceus-providers` |
| 58 | Streaming SSE normalization | Normalize Server-Sent Events across providers into unified stream | Claurst | ✅ | P0 | `caduceus-providers` |
| 59 | Bash / shell tool | Execute shell commands with timeout and output capture | Claw, Claurst | ✅ | P0 | `caduceus-tools` |
| 60 | File read tool (paginated) | Read file contents with line-range pagination | Claw, Claurst | ✅ | P0 | `caduceus-tools` |
| 61 | File write tool | Create or overwrite files with content | Claw, Claurst | ✅ | P0 | `caduceus-tools` |
| 62 | File edit tool (substring replace) | Surgical search-and-replace edits within files | Claw, Claurst | ✅ | P0 | `caduceus-tools` |
| 63 | Glob search tool | Find files by glob pattern | Claw, Claurst | ✅ | P0 | `caduceus-tools` |
| 64 | Grep / regex search tool | Search file contents with regex patterns | Claw, Claurst | ✅ | P0 | `caduceus-tools` |
| 65 | Azure OpenAI adapter | Azure-hosted OpenAI models with AAD auth and deployment routing | Claurst | 📋 | P1 | `caduceus-providers` |
| 66 | Google Gemini adapter | Native Gemini API integration with streaming and function calling | Claurst | 📋 | P1 | `caduceus-providers` |
| 67 | Web fetch tool | Retrieve and extract content from URLs | Claw | 📋 | P1 | `caduceus-tools` |
| 68 | Apply-patch tool | Apply unified diff patches to files | Claw | 📋 | P1 | `caduceus-tools` |
| 69 | Vertex AI adapter | Google Cloud Vertex AI with service account auth | Claurst | 📋 | P2 | `caduceus-providers` |
| 70 | AWS Bedrock adapter | Amazon Bedrock API with SigV4 auth | Claurst | 📋 | P2 | `caduceus-providers` |
| 71 | LSP bridge tool | Language Server Protocol client for goto-def, references, diagnostics | Claurst, Hermes | 📋 | P2 | `caduceus-codeintel` |
| 72 | Vision support (multi-provider) | Image input encoding for Claude, GPT-4o, Gemini | Claurst | 📋 | P2 | `caduceus-providers` |
| 73 | Tool fallback text extraction | Extract usable text from tool errors / partial results | Multi-Agent, Claurst | 📋 | P2 | `caduceus-providers` |
| 74 | Tool preset reduction | Named tool subsets (read-only, full, minimal) for constrained agents | Multi-Agent | 📋 | P2 | `caduceus-tools` |
| 75 | Notebook cell tool | Read/write/execute Jupyter notebook cells | Claurst | 💡 | P3 | `caduceus-tools` |

### 1.4 Sandbox Layer (20 features)

| # | Feature | Description | Source(s) | Status | Priority | Crate |
|---|---------|-------------|-----------|--------|----------|-------|
| 76 | Process spawning (structured argv) | Spawn child processes with structured argument vectors and env control | Hermes, Claw | ✅ | P0 | `caduceus-runtime` |
| 77 | File CRUD (workspace-confined) | Create / read / update / delete files within workspace boundaries | Hermes | ✅ | P0 | `caduceus-runtime` |
| 78 | PTY management | Create, send input, resize, and kill pseudo-terminal sessions | Hermes | ✅ | P0 | `caduceus-runtime` |
| 79 | Timeout enforcement | Kill long-running processes after configurable timeout | Hermes | ✅ | P0 | `caduceus-runtime` |
| 80 | Symlink resolution & confinement | Resolve symlinks and enforce workspace jail on all file ops | Hermes | ✅ | P0 | `caduceus-runtime` |
| 81 | SQLite persistence (WAL) | WAL-mode SQLite with migrations for all structured data | Hermes | ✅ | P0 | `caduceus-storage` |
| 82 | Session storage (CRUD) | Full lifecycle management for conversation sessions | Claurst, Claw | ✅ | P0 | `caduceus-storage` |
| 83 | Auth store (keychain) | Secure credential storage via OS keychain integration | Claurst | ✅ | P0 | `caduceus-permissions` |
| 84 | Directory conventions (`~/.caduceus/`) | Standardized paths for config, data, cache, logs | Claurst | 🔧 | P0 | `caduceus-core` |
| 85 | File watching | Watch workspace files for changes and trigger reindex | Hermes, E2B | 📋 | P1 | `caduceus-runtime` |
| 86 | JSONL transcript export | Export full conversation transcripts as JSONL for auditing | Claurst | 📋 | P1 | `caduceus-storage` |
| 87 | Session resumption | Resume conversations from persisted state with context reload | Claurst, Claw | 📋 | P1 | `caduceus-storage` |
| 88 | E2B sandbox lifecycle | Create, connect, pause, resume, and kill cloud sandboxes | E2B | 📋 | P1 | `caduceus-runtime` |
| 89 | Crash recovery / session restore | Recover in-flight sessions after unexpected process termination | Hermes | 📋 | P1 | `caduceus-storage` |
| 90 | E2B template management | Create, list, and instantiate sandbox templates | E2B | 📋 | P2 | `caduceus-runtime` |
| 91 | E2B volume management | Attach, detach, and manage persistent storage volumes | E2B | 📋 | P2 | `caduceus-runtime` |
| 92 | E2B network controls | Port access rules, CIDR allowlists, DNS configuration | E2B | 📋 | P2 | `caduceus-runtime` |
| 93 | Worktree isolation | Use git worktrees for parallel, isolated task branches | Claurst | 📋 | P2 | `caduceus-git` |
| 94 | Session forking / sidechains | Fork a session mid-conversation to explore alternative paths | Claurst | 📋 | P2 | `caduceus-storage` |
| 95 | E2B snapshot / restore | Capture and restore full sandbox state | E2B | 💡 | P3 | `caduceus-runtime` |

### 1.5 Omniscience Layer (22 features)

| # | Feature | Description | Source(s) | Status | Priority | Crate |
|---|---------|-------------|-----------|--------|----------|-------|
| 96 | Token budget tracking | Track context window usage against model-specific limits | Claurst | ✅ | P0 | `caduceus-orchestrator` |
| 97 | Token counting | Accurate token counting per message via tiktoken / provider APIs | Claurst | ✅ | P0 | `caduceus-telemetry` |
| 98 | Auto-compaction (>85% threshold) | Automatically summarize conversation when context exceeds 85% | Claurst | ✅ | P0 | `caduceus-orchestrator` |
| 99 | Manual compaction (`/compact`) | User-triggered conversation summarization to reclaim context | Claurst, Claw | ✅ | P0 | `caduceus-orchestrator` |
| 100 | Cost estimation & tracking | Per-turn and cumulative cost tracking with SQLite cost log | Claurst | ✅ | P0 | `caduceus-telemetry` |
| 101 | Instruction memory (CLAUDE.md) | Load project-level instructions from convention files | Claurst | ✅ | P0 | `caduceus-orchestrator` |
| 102 | Tree-sitter incremental parsing | Parse source files incrementally; reparse only changed ranges | TS/Q/Zed | ✅ | P0 | `caduceus-omniscience` |
| 103 | AST query / capture support | Run tree-sitter queries to extract structural code elements | TS/Q/Zed | ✅ | P0 | `caduceus-omniscience` |
| 104 | Semantic chunk extraction | Extract function, class, and block-level chunks for indexing | TS/Q/Zed | ✅ | P0 | `caduceus-omniscience` |
| 105 | Qdrant vector indexing (EdgeShard) | In-process vector index with HNSW for fast ANN search | TS/Q/Zed | ✅ | P0 | `caduceus-omniscience` |
| 106 | Semantic search (query/rank/filter) | Natural language search over indexed codebase chunks | TS/Q/Zed | ✅ | P0 | `caduceus-omniscience` |
| 107 | Incremental reindex | Reindex only files/ranges that changed since last index | TS/Q/Zed | ✅ | P0 | `caduceus-omniscience` |
| 108 | Payload indexes / metadata filters | Filter search results by language, file path, symbol type | TS/Q/Zed | ✅ | P0 | `caduceus-omniscience` |
| 109 | Project context (languages/frameworks) | Detect languages, frameworks, and build systems in workspace | Hermes | ✅ | P0 | `caduceus-scanner` |
| 110 | Cache control / prompt caching | Leverage provider-side prompt caching for repeated prefixes | Claurst | 📋 | P1 | `caduceus-providers` |
| 111 | Parser-error-aware down-ranking | Reduce relevance score for chunks containing parse errors | TS/Q/Zed | 📋 | P1 | `caduceus-omniscience` |
| 112 | Token warning levels | Progressive warnings at 70%, 85%, 95% context utilization | Claurst | 📋 | P1 | `caduceus-orchestrator` |
| 113 | Context assembly / attunement | Intelligent selection and ordering of context for each prompt | Claurst | 🔧 | P1 | `caduceus-orchestrator` |
| 114 | Memory store (project/session) | Persistent key-value memory across sessions and projects | Claurst | 📋 | P1 | `caduceus-storage` |
| 115 | Embedding model selection | Configurable embedding models for vector indexing | TS/Q/Zed | 📋 | P2 | `caduceus-omniscience` |
| 116 | Durable session tracer | OpenTelemetry-compatible trace export for session analytics | Claurst | 📋 | P2 | `caduceus-telemetry` |
| 117 | Cross-project index federation | Search across multiple project indexes simultaneously | TS/Q/Zed | 💡 | P3 | `caduceus-omniscience` |

### 1.6 Multiplayer Layer (15 features)

| # | Feature | Description | Source(s) | Status | Priority | Crate |
|---|---------|-------------|-----------|--------|----------|-------|
| 118 | CRDT text buffer (RGA) | Replicated Growable Array for conflict-free concurrent edits | TS/Q/Zed | ✅ | P0 | `caduceus-crdt` |
| 119 | Lamport clocks + version vectors | Logical clocks for causal ordering of distributed operations | TS/Q/Zed | ✅ | P0 | `caduceus-crdt` |
| 120 | Rope storage (B+ tree) | Efficient large-text storage with O(log n) edits via B+ tree rope | TS/Q/Zed | ✅ | P0 | `caduceus-crdt` |
| 121 | Stable anchors (cursor positions) | Position markers that survive concurrent insertions and deletions | TS/Q/Zed | ✅ | P0 | `caduceus-crdt` |
| 122 | Session forking | Branch a conversation into parallel exploratory threads | Claurst | 📋 | P2 | `caduceus-storage` |
| 123 | Task DAG execution | Execute interdependent tasks as a directed acyclic graph | Multi-Agent | 💡 | P2 | `caduceus-orchestrator` |
| 124 | Team auto-orchestration | Automatically decompose work across specialized agent personas | Multi-Agent | 💡 | P2 | `caduceus-orchestrator` |
| 125 | Team message bus | Pub/sub message bus for inter-agent communication | Multi-Agent | 💡 | P2 | `caduceus-orchestrator` |
| 126 | Team shared memory | Shared context store accessible by all agents in a team | Multi-Agent | 💡 | P2 | `caduceus-orchestrator` |
| 127 | Scheduler strategies | Round-robin, least-busy, and capability-match agent scheduling | Multi-Agent | 💡 | P3 | `caduceus-orchestrator` |
| 128 | Bridge / remote control (WebSocket) | External control plane via WebSocket JSON-RPC | Claurst | 💡 | P3 | `caduceus-sync` |
| 129 | SSH sessions | Remote session management over SSH tunnels | Claurst, Hermes | 💡 | P3 | `caduceus-remote` |
| 130 | ACP protocol (JSON-RPC 2.0) | Agent Communication Protocol for standardized agent interop | Claurst | 💡 | P3 | `caduceus-remote` |
| 131 | Collaboration sync (deferred-op replay) | Replay buffered operations for eventual consistency across peers | TS/Q/Zed | 💡 | P3 | `caduceus-sync` |
| 132 | Remote selections / AI cursors | Display remote collaborator and AI agent cursor positions | TS/Q/Zed | 💡 | P3 | `caduceus-presence` |

### Summary

| Layer | Features | ✅ | 🔧 | 📋 | 💡 |
|-------|----------|---|---|---|---|
| Presentation | 22 | 5 | 0 | 12 | 5 |
| Orchestration | 32 | 8 | 2 | 16 | 6 |
| Workers | 21 | 10 | 0 | 8 | 3 |
| Sandbox | 20 | 8 | 1 | 8 | 3 |
| Omniscience | 22 | 14 | 1 | 5 | 2 |
| Multiplayer | 15 | 4 | 0 | 1 | 10 |
| **Total** | **132** | **49** | **4** | **50** | **29** |

---

## 2. Layer Details

### 2.1 Presentation Layer

**What exists in source repos:**

- **Claurst** provides a full TUI framework built on `ratatui` with panels for input, output, status, and tool results. It includes streaming markdown rendering, permission dialogs, theme support, vim keybindings, a context visualizer, image rendering via Sixel, and a companion sprite system.
- **Claw Code** contributes the slash command palette, headless `--print` mode, and output format selection (text, JSON, streaming JSON).
- **Hermes IDE** provides split terminal layouts, desktop notifications, PTY output rendering, and the Tauri-based desktop shell with IPC.
- **Hermes Supplement** extends with theme switching, notification toasts, and the diff viewer.

**What's implemented in Caduceus today:**

The core TUI loop runs on ratatui with streaming token rendering from providers. Permission dialogs gate tool use interactively. The slash command palette supports all registered commands. PTY output from subprocesses renders faithfully.

**What's planned next:**

Syntax highlighting via `syntect`, the diff viewer, status line, headless CLI mode, and output format selection are P1 priorities. The Tauri desktop shell and model picker follow closely.

**Integration points:**

- Receives streamed tokens from **Workers** via the provider adapter layer
- Queries **Omniscience** for context visualization data
- Delegates permission checks to **Orchestration** and **Sandbox** layers
- Sends user commands to the **Orchestration** loop

---

### 2.2 Orchestration Layer

**What exists in source repos:**

- **Claurst** is the primary source: multi-turn conversation loop, system prompt assembly, configuration layering, tool dispatch, permission enforcement, hook system with ~27 lifecycle events, effort levels, extended thinking, slash commands, budget limits, provider/model registries, agent personas, feature flags, and the full plugin system.
- **Claw Code** contributes the REPL loop, one-shot mode, and command parsing.
- **Multi-Agent** contributes loop detection (fingerprint-based), structured output validation with retry, and the foundation for parallel tool execution and max-turn limiting.

**What's implemented in Caduceus today:**

The multi-turn conversation loop drives all interactions. System prompts are assembled dynamically from config, project context, and instruction memory. Tool dispatch validates schemas and routes calls through the permission system. The provider and model registries support Anthropic and OpenAI-compatible endpoints. Six slash commands are registered. Configuration layering is stubbed with CLI and env support.

**What's planned next:**

Retry logic with exponential backoff and cancellation tokens are P0 gaps. Extended thinking, loop detection, structured output validation, and the hook system are high-priority P1 items. MCP client integration enables external tool discovery.

**Integration points:**

- Drives the **Workers** layer for LLM inference and tool execution
- Enforces **Sandbox** confinement on all file/process operations
- Consults **Omniscience** for context assembly and token budget management
- Feeds **Multiplayer** with task decomposition for multi-agent execution

---

### 2.3 Workers Layer

**What exists in source repos:**

- **Claurst** provides adapters for Anthropic, OpenAI, Azure, Gemini, Vertex, Bedrock, and Ollama. It normalizes streaming SSE, handles vision input, and manages tool result formatting.
- **Claw Code** implements the core tool set: bash, file read/write/edit, glob, grep, web fetch, and apply-patch.
- **Multi-Agent** contributes tool preset reduction (limiting available tools per agent) and fallback text extraction from failed tool calls.

**What's implemented in Caduceus today:**

Anthropic and OpenAI-compatible adapters are fully functional with streaming. Ollama works via the OpenAI-compatible interface. Ten builtin tools are registered: bash, file read, file write, file edit, glob, grep, plus git and scanner tools. SSE normalization produces a unified token stream.

**What's planned next:**

Azure OpenAI and Google Gemini adapters are P1. Web fetch and apply-patch tools round out the core toolset. Vision support and the LSP bridge are P2 priorities.

**Integration points:**

- Called by **Orchestration** for every LLM inference and tool invocation
- Tool execution is sandboxed by the **Sandbox** layer
- Search tools can leverage **Omniscience** for semantic results
- Provider adapters feed streaming data to **Presentation**

---

### 2.4 Sandbox Layer

**What exists in source repos:**

- **Hermes IDE** provides process spawning with structured argv, workspace-confined file CRUD, PTY session management, timeout enforcement, and symlink resolution. It also contributes file watching.
- **E2B** provides the full cloud sandbox lifecycle: create, connect, pause, resume, kill; plus template management, persistent volumes, network controls (ports, CIDR), and snapshot/restore.
- **Claurst** contributes session storage, JSONL export, session resumption, worktree isolation, session forking, and directory conventions.
- **Claw Code** contributes the session CRUD interface.

**What's implemented in Caduceus today:**

Process spawning, file CRUD, PTY management, timeout enforcement, and symlink confinement are all operational. SQLite storage runs in WAL mode with migrations. Session CRUD supports full lifecycle. The auth store integrates with OS keychain. Directory conventions are stubbed.

**What's planned next:**

File watching, session resumption, JSONL export, and crash recovery are P1. E2B sandbox lifecycle integration is the gateway to cloud execution. Worktree isolation enables parallel task branches.

**Integration points:**

- All file and process operations from **Workers** are routed through Sandbox
- **Orchestration** persists session state via Sandbox storage
- **Omniscience** watches for file changes to trigger reindexing
- **Multiplayer** uses session forking for parallel exploration

---

### 2.5 Omniscience Layer

**What exists in source repos:**

- **Claurst** provides token budget tracking, auto-compaction at 85% capacity, manual `/compact`, cost estimation, instruction memory (`CLAUDE.md`), cache control, context assembly/attunement, and memory stores.
- **Tree-sitter** provides incremental parsing with edit-range tracking, AST queries via S-expression patterns, and language grammar support for 20+ languages.
- **Qdrant** provides the EdgeShard in-process vector index with HNSW, payload indexes for metadata filtering, and semantic search with cosine similarity scoring.
- **Hermes IDE** contributes the project scanner for language/framework detection.

**What's implemented in Caduceus today:**

This is the most complete layer. Token budget tracking, auto-compaction, manual compaction, cost tracking, and instruction memory are all operational. Tree-sitter incrementally parses source files, extracts semantic chunks (functions, classes, blocks), and feeds them to the Qdrant EdgeShard. Semantic search supports query, rank, and filter operations. Payload indexes enable metadata-based filtering by language and file path. Incremental reindexing only processes changed ranges. The project scanner detects languages and frameworks.

**What's planned next:**

Prompt caching (provider-side) is a key P1 optimization. Parser-error-aware down-ranking improves search quality. Token warning levels provide progressive alerts. The memory store enables persistent context across sessions.

**Integration points:**

- **Orchestration** queries Omniscience for context assembly before every prompt
- **Sandbox** file watchers trigger incremental reindexing
- **Workers** search tools can delegate to semantic search
- **Presentation** visualizes context usage via `/ctx_viz`

---

### 2.6 Multiplayer Layer

**What exists in source repos:**

- **Zed CRDT** provides the RGA text buffer, Lamport clocks, version vectors, rope storage via B+ tree, stable anchors for cursor positions, deferred-operation replay for eventual consistency, and remote selection rendering.
- **Multi-Agent** provides task DAG execution, team auto-orchestration with role assignment, a pub/sub message bus, shared memory stores, and scheduler strategies (round-robin, least-busy, capability-match).
- **Claurst** contributes the bridge/remote control concept via WebSocket, SSH sessions, and the ACP (Agent Communication Protocol) based on JSON-RPC 2.0.

**What's implemented in Caduceus today:**

The CRDT foundations are solid. The RGA text buffer supports conflict-free concurrent edits. Lamport clocks and version vectors provide causal ordering. Rope storage uses a B+ tree for efficient large-document operations. Stable anchors track cursor positions through edits.

**What's planned next:**

Session forking is the first concrete multiplayer feature (P2). Multi-agent capabilities — task DAGs, team orchestration, message bus, shared memory — are future priorities that unlock parallel AI workflows. The bridge, SSH, and ACP protocol enable remote and distributed operation.

**Integration points:**

- **Orchestration** decomposes tasks into the DAG for multi-agent execution
- **Sandbox** provides session forking and isolated worktrees per agent
- **Omniscience** provides the shared index that all agents search
- **Presentation** renders remote cursors and agent activity

---

## 3. Capability Sources

| Source Repo | Key Contributions | Layers Affected | # Features |
|---|---|---|---|
| **Hermes IDE** | Desktop shell, PTY management, process spawning, file CRUD, workspace confinement, split terminals, file watching, crash recovery | Presentation, Sandbox | 16 |
| **Hermes IDE Supplement** | Theme picker, notifications, diff viewer, split terminal enhancements | Presentation | 4 |
| **Claw Code (Claude CLI)** | REPL loop, one-shot mode, slash commands, output formats, core tools (bash, file, glob, grep, web fetch, apply-patch) | Presentation, Orchestration, Workers | 14 |
| **Claurst (Claude Code internals)** | Multi-provider LLM API, 36+ tools, orchestration loop, plugin/hook system, TUI framework, permission system, compaction, token budget, configuration, extended thinking, agent personas, MCP | All 6 layers | 78 |
| **Open Multi-Agent** | Task DAG, team orchestration, message bus, shared memory, schedulers, loop detection, structured output validation, parallel execution | Orchestration, Workers, Multiplayer | 14 |
| **E2B (Cloud Sandbox)** | Sandbox lifecycle, templates, volumes, network controls, snapshots | Sandbox | 6 |
| **Tree-sitter + Qdrant + Zed CRDT** | Incremental parsing, semantic chunking, vector search, CRDT buffers, Lamport clocks, rope storage, anchors, collaboration sync | Omniscience, Multiplayer | 16 |

> **Note:** Some features draw from multiple sources; the count reflects primary attribution. Claurst is the dominant contributor as the most feature-rich source repo.

---

## 4. Roadmap

### v0.1 — Foundation (Current)

> *Core loop works end-to-end. Ship the minimum viable AI coding assistant.*

- ✅ Multi-turn conversation loop with system prompt assembly
- ✅ Anthropic + OpenAI-compatible providers with streaming
- ✅ 10 builtin tools with schema validation and permission gating
- ✅ 6-tier capability token permission system with audit log
- ✅ SQLite persistence (WAL) with session and message storage
- ✅ Workspace scanner with language/framework detection
- ✅ Tree-sitter parsing + Qdrant semantic search (EdgeShard)
- ✅ CRDT text buffer with Lamport clocks and rope storage
- ✅ Token counting, cost tracking, auto-compaction
- ✅ Git operations (status, diff, stage, commit, branch)
- ✅ Slash command palette and instruction memory

**Status:** 49 features implemented, 4 stubbed

---

### v0.2 — Reliability & CLI (Next)

> *Harden the core, add headless mode, expand provider coverage.*

- 📋 Retry logic with exponential backoff and jitter
- 📋 Cancellation token for cooperative abort
- 📋 Loop detection (fingerprint-based)
- 📋 Structured output validation + retry
- 📋 Session resumption and crash recovery
- 📋 Headless mode (`--print`) and output formats
- 📋 Azure OpenAI and Google Gemini adapters
- 📋 Web fetch and apply-patch tools
- 📋 Hook system (~27 lifecycle events)
- 📋 JSONL transcript export
- 📋 Configuration layering (CLI → env → project → global)
- 📋 File watching with incremental reindex triggers
- 📋 E2B sandbox lifecycle (create, connect, kill)
- 📋 Budget USD limit enforcement
- 📋 MCP client for external tool discovery

---

### v0.3 — Experience & Extensions

> *Full TUI, plugin system, multi-agent groundwork.*

- 📋 Syntax highlighting, diff viewer, status line
- 📋 Model picker and session browser
- 📋 Extended thinking mode
- 📋 Effort levels (Min → Max)
- 📋 Parallel tool execution
- 📋 Agent personas (build / plan / explore)
- 📋 E2B template and volume management
- 📋 Plugin system with TOML/JSON manifest
- 📋 Context assembly / attunement
- 📋 Memory store (project / session scoped)
- 📋 Prompt caching (provider-side)
- 📋 Worktree isolation for parallel branches

---

### v1.0 — Full Vision

> *Desktop app, all providers, complete toolset, plugin ecosystem.*

- 📋 Tauri desktop shell with IPC
- 📋 All 6 provider adapters (Anthropic, OpenAI, Azure, Gemini, Vertex, Bedrock)
- 📋 Full tool suite including LSP bridge and notebook cells
- 📋 Plugin commands, agents, and skills
- 📋 Feature flags and model whitelisting
- 📋 Theme picker and keybinding configurator
- 📋 E2B network controls and snapshot/restore
- 📋 Task DAG execution and team auto-orchestration
- 📋 Durable session tracer (OpenTelemetry)

---

### Future

> *Real-time collaboration, remote access, novel interaction modes.*

- 💡 Team message bus and shared memory
- 💡 Scheduler strategies (round-robin, least-busy, capability-match)
- 💡 Bridge / remote control (WebSocket)
- 💡 SSH sessions and ACP protocol (JSON-RPC 2.0)
- 💡 Collaboration sync (deferred-op replay)
- 💡 Remote selections / AI cursors
- 💡 Voice input (Deepgram STT)
- 💡 Image rendering (Sixel / Kitty protocol)
- 💡 Buddy / companion sprite system
- 💡 Cross-project index federation
- 💡 Notebook cell tool

---

*Generated for Caduceus — 132 features across 6 layers, synthesized from 7 source repositories.*
