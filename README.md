# Caduceus

> A terminal-first AI coding agent for your desktop — built on Tauri 2, React, and Rust.

[![Rust](https://img.shields.io/badge/rust-1.78%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/tauri-2.x-blue?logo=tauri)](https://tauri.app/)
[![React](https://img.shields.io/badge/react-18.x-61DAFB?logo=react)](https://react.dev/)
[![TypeScript](https://img.shields.io/badge/typescript-5.x-3178C6?logo=typescript)](https://www.typescriptlang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-green)](LICENSE)

---

Caduceus is a local-first AI development environment that runs an autonomous coding agent directly on your machine. It pairs a terminal-first React/TypeScript UI with a multi-crate Rust backend, giving an LLM (Anthropic Claude, OpenAI-compatible, or any local model via Ollama/vLLM) the ability to read your codebase, run shell commands, edit files, query Git, and reason over semantically-indexed code — all within a strict capability permission model that keeps you in control. The v1 release ships a full single-agent loop; post-v1 work adds a coordinator-driven multi-agent runtime, E2B sandbox micro-VMs, and real-time CRDT collaborative editing.

---

## Architecture Overview

Caduceus is organized into six conceptual layers, each implemented as one or more Rust crates:

```
╔══════════════════════════════════════════════════════════════════════════════╗
║  LAYER 1 — PRESENTATION                                                    ║
║  Tauri 2 + React/TypeScript frontend                                       ║
║  xterm.js terminal • CodeMirror editor • split panes • command palette     ║
║  Typed Tauri IPC bridge (invoke/event) to Rust backend                     ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  LAYER 2 — ORCHESTRATION                                             [v1]  ║
║  Rust agent harness engine                                                 ║
║  Conversation loop • Tool registry & dispatch • Permission enforcement     ║
║  Session persistence (SQLite WAL) • System prompt assembly                 ║
║  Multi-provider LLM adapter layer (Anthropic / OpenAI-compatible)          ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  LAYER 3 — WORKERS (multi-agent)                               [post-v1]  ║
║  Coordinator-driven multi-agent runtime                                    ║
║  Task DAG with dependency resolution • Agent pool with concurrency control ║
║  Team message bus + shared memory • Loop detection • Scheduler             ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  LAYER 4 — SANDBOX (E2B micro-VM)                              [post-v1]  ║
║  Secure sandboxed code execution                                           ║
║  Process/PTY via gRPC-web • Filesystem CRUD • Port forwarding              ║
║  Snapshots • Volumes • Templates • MCP gateway                             ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  LAYER 5 — OMNISCIENCE                                               [v1]  ║
║  AST parsing (tree-sitter): incremental parse, query-driven chunk extract  ║
║  Vector search (qdrant-edge): embedded EdgeShard, semantic code retrieval  ║
║  Chunking pipeline: parse → extract → embed → upsert → search             ║
╠══════════════════════════════════════════════════════════════════════════════╣
║  LAYER 6 — MULTIPLAYER (CRDT)                                  [post-v1]  ║
║  RGA-based CRDT text buffer with Lamport timestamps                        ║
║  Anchor system for stable positions • Fragment-based tombstone model       ║
║  Rope data structure (B+ tree) • Version vectors • Remote AI cursors       ║
╚══════════════════════════════════════════════════════════════════════════════╝
```

---

## End-to-End Workflow

```
User types prompt in React chat panel
         │
         ▼
┌─────────────────────────────┐
│ 1. PRESENTATION             │  React dispatches agent_turn(session_id, input)
│    React → Tauri IPC        │  via typed invoke() wrapper
└────────────┬────────────────┘
             │ Tauri IPC invoke
             ▼
┌─────────────────────────────┐
│ 2. ORCHESTRATION            │  Rust engine assembles system prompt + memory
│    System prompt assembly   │  + tool definitions + token budget, then
│    Conversation loop        │  streams request to LLM provider (SSE)
│    Tool dispatch            │  Model returns text + tool_use blocks →
│    Permission enforcement   │  permission check → tool dispatch
└────────────┬────────────────┘
             │ Tool execution
             ├──────────────────────────────────────┐
             ▼                                      ▼
┌─────────────────────────────┐    ┌─────────────────────────────┐
│ 5. OMNISCIENCE              │    │ LOCAL EXECUTION             │
│    Tree-sitter AST parse    │    │ Workspace-constrained       │
│    Qdrant semantic search   │    │ File read / write / edit    │
│    grep / glob / LSP        │    │ Bash with structured argv   │
└────────────┬────────────────┘    └────────────┬────────────────┘
             │ tool_result                       │ tool_result
             └──────────────────┬────────────────┘
                                 ▼
┌─────────────────────────────────────────────────────────────────┐
│ 6. MULTIPLAYER (post-v1)                                        │
│    buffer.edit() → Lamport-stamped Operation → broadcast        │
│    Subscription fires → Patch<Edit> to React via Tauri event    │
└────────────┬────────────────────────────────────────────────────┘
             │ Tauri event: agent:event (AgentEvent enum)
             ▼
┌─────────────────────────────┐
│ 1. PRESENTATION             │  React reducer appends TextDelta to chat;
│    React event listener     │  xterm streams terminal output;
│    CodeMirror / xterm       │  diff viewer shows file changes
└─────────────────────────────┘
```

Streaming backpressure: events are buffered in a bounded channel (capacity 1 024). On overflow, oldest `TextDelta` events are dropped; `ToolCallStart`/`ToolCallEnd` events are never dropped.

---

## Features

### Layer 1 — Presentation (v1)
- xterm.js WebGL terminal renderer with PTY integration
- AI chat panel with streaming text deltas
- Git panel: status, diff viewer, stage, commit
- Command palette
- Agent status bar (phase, token usage, cost)
- Project context view (languages, frameworks, file count)

### Layer 2 — Orchestration (v1)
- Single-agent conversation loop with multi-turn tool use
- Multi-provider LLM support: Anthropic Claude, OpenAI-compatible (OpenAI, Ollama, vLLM, LM Studio)
- Capability-based permission model (6 capability tiers, default-deny)
- Append-only audit log for all permission decisions
- Session persistence in SQLite (WAL mode) with JSONL transcript export
- System prompt assembly with project context injection and token budgeting
- Auto-compaction at 85 % context fill
- Pre/post-tool hook system (stdin/stdout JSON protocol)
- Slash command support

### Layer 3 — Workers (post-v1)
- Coordinator meta-agent that decomposes goals into task DAGs
- Parallel agent execution with dependency resolution
- Loop detection with sliding-window fingerprint matching
- Retry with exponential backoff

### Layer 4 — Sandbox (post-v1)
- E2B micro-VM integration for secure, isolated code execution
- gRPC-web (Connect protocol) to envd daemon
- Sandbox snapshots, volumes, and custom templates
- MCP gateway on port 50005

### Layer 5 — Omniscience (v1 stub → full)
- Tree-sitter incremental AST parsing for 10+ languages
- Query-driven semantic chunk extraction (functions, classes, trait impls)
- Qdrant-edge embedded vector index (cosine, Float32, HNSW)
- Incremental re-indexing: edit → reparse → `changed_ranges()` → upsert only deltas
- Combined `CodeIntelligence` API unifying AST + vector search

### Layer 6 — Multiplayer / CRDT (post-v1)
- RGA-based CRDT text buffer (inspired by Zed's architecture)
- Lamport-timestamped operations; higher timestamp wins on concurrent insert
- Tombstone-model deletes with full undo support
- Anchor system: stable cursor positions that survive concurrent AI edits
- `ReplicaId::AGENT = 2` for attributable AI edits; per-worker IDs in multi-agent mode

---

## Prerequisites

| Tool | Version | Install |
|------|---------|---------|
| Rust + Cargo | 1.78+ | [rustup.rs](https://rustup.rs) |
| Node.js | 18+ | [nodejs.org](https://nodejs.org) |
| Tauri CLI | 2.x | `cargo install tauri-cli` |

> **macOS only (for now):** Linux and Windows Tauri builds are configured in `.github/workflows/release.yml` but have not been tested end-to-end yet.

---

## Quick Start

```bash
git clone https://github.com/alexkeagel/caduceus.git
cd caduceus

# Install Node dependencies
npm install

# Build all Rust crates
cargo build --workspace

# Run in dev mode (hot-reload frontend + Rust backend)
npm run tauri dev
```

On first launch, Caduceus will prompt you to select a workspace folder and configure an LLM provider API key. Keys are stored in your OS keychain (macOS Keychain / Linux Secret Service / Windows Credential Manager) and never written to disk or logs.

---

## Build from Source

```bash
# Debug build
cargo build --workspace
npm run build
npm run tauri dev

# Release build (all platforms)
npm run tauri build

# Cross-platform release (via GitHub Actions)
# See .github/workflows/release.yml
```

Artifacts are emitted to `src-tauri/target/release/bundle/`.

---

## Project Structure

```
caduceus/
├── crates/                         # Cargo workspace
│   ├── caduceus-core/              # Shared types, errors, IPC contracts, config schema
│   ├── caduceus-storage/           # SQLite (WAL), migrations, session/message persistence
│   ├── caduceus-scanner/           # Project scanner: languages, frameworks → context map
│   ├── caduceus-git/               # Git status, diff, stage, commit via git2
│   ├── caduceus-providers/         # LLM adapter trait + Anthropic + OpenAI-compat adapters
│   ├── caduceus-permissions/       # Capability system, audit log, secrets bridge (OS keychain)
│   ├── caduceus-runtime/           # Structured process execution, file ops, workspace boundary
│   ├── caduceus-tools/             # Tool registry, ~10 built-in tools with JSON Schema
│   ├── caduceus-orchestrator/      # Session state, agent harness, context assembly, slash cmds
│   ├── caduceus-telemetry/         # Token counting, cost logging to SQLite (local only)
│   ├── caduceus-omniscience/       # AST chunking (tree-sitter) + vector search (qdrant-edge)
│   └── caduceus-crdt/              # CRDT text buffer: Rope, Fragment, Anchor, Lamport clock
├── src-tauri/                      # Tauri 2 application shell
│   ├── src/
│   │   ├── main.rs                 # Tauri builder + IPC command registration
│   │   ├── lib.rs                  # IPC command handlers
│   │   ├── pty.rs                  # PTY session management
│   │   └── platform.rs            # OS-specific behaviour
│   └── tauri.conf.json
├── src/                            # React + TypeScript frontend
│   ├── api/                        # Typed Tauri IPC wrappers (mirrors caduceus-core types)
│   ├── components/
│   │   ├── terminal/               # xterm.js terminal renderer
│   │   ├── chat/                   # AI chat panel
│   │   ├── git/                    # Git panel + diff viewer
│   │   ├── palette/                # Command palette
│   │   └── status/                 # Agent status + project context
│   ├── hooks/
│   ├── state/                      # React Context + useReducer
│   └── types/                      # TypeScript mirror of Rust core types
├── .github/
│   ├── workflows/
│   │   ├── ci.yml                  # cargo fmt + clippy + test + tsc + vite build
│   │   └── release.yml             # Cross-platform Tauri release builds
│   └── agents/                     # Copilot agent definitions
└── spec/                           # Clean-room behavioral specifications (Phase A output)
```

### Crate Dependency Order

```
caduceus-core           ← foundation; depended on by all crates
  ├── caduceus-storage
  ├── caduceus-scanner
  ├── caduceus-git
  ├── caduceus-providers
  │     └── caduceus-permissions
  │     └── caduceus-runtime
  │     └── caduceus-tools
  │           └── caduceus-orchestrator
  │                 └── caduceus-telemetry
  ├── caduceus-omniscience
  └── caduceus-crdt
```

Lower layers never import from upper layers. Cross-layer boundaries are trait-based only.

---

## Contributing

1. Fork the repository and create a feature branch.
2. Run `cargo fmt --all && cargo clippy --workspace -- -D warnings` before committing.
3. Add or update tests for any behavioural change.
4. Open a pull request; CI must pass before review.

See [ARCHITECTURE.md](ARCHITECTURE.md) for a deep dive into design decisions and the crate API contracts.

> **Legal note:** Caduceus is a clean-room implementation guided by abstract behavioral specifications. No source code from referenced projects was copied. If you contribute, do not introduce code derived from GPL-3.0 or BSL 1.1 licensed projects.

---

## License

MIT — see [LICENSE](LICENSE).

