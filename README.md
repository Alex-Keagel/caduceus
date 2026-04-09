# Caduceus

> An AI coding agent with a terminal-first UX — powered by Tauri 2, React, and Rust.

## Architecture

```
caduceus/
├── crates/                      # Rust workspace
│   ├── caduceus-core/           # Shared types, traits, errors
│   ├── caduceus-storage/        # SQLite persistence (WAL mode)
│   ├── caduceus-scanner/        # Project language/framework detection
│   ├── caduceus-git/            # Git operations via git2
│   ├── caduceus-providers/      # LLM provider adapters (Anthropic, OpenAI-compat)
│   ├── caduceus-permissions/    # Capability enforcement + audit log
│   ├── caduceus-runtime/        # Sandboxed bash + file ops
│   ├── caduceus-tools/          # 10 built-in agent tools with JSON Schema
│   ├── caduceus-orchestrator/   # Agent harness, session manager, slash commands
│   ├── caduceus-telemetry/      # Token counting, cost logging, trace spans
│   ├── caduceus-omniscience/    # AST chunking + semantic vector search (stub)
│   └── caduceus-crdt/           # CRDT text buffer a la Zed (stub)
├── src-tauri/                   # Tauri 2 backend + IPC commands
├── src/                         # React + TypeScript frontend
│   ├── api/tauri.ts             # Typed IPC wrappers
│   ├── components/              # Terminal, Chat, GitPanel, CommandPalette, StatusBar
│   └── types/                   # TypeScript mirror of Rust core types
├── .github/workflows/ci.yml     # cargo fmt/clippy/test + tsc + vite build
└── index.html
```

## Quick Start

```bash
# Rust workspace
cd crates && cargo build --workspace

# Frontend
npm install
npm run dev

# Tauri full app (requires Tauri CLI)
npm run tauri dev
```

## Crate Overview

| Crate | Purpose |
|---|---|
| `caduceus-core` | SessionId, SessionState, TranscriptEntry, TokenBudget, traits |
| `caduceus-storage` | SQLite via rusqlite, WAL mode, SessionStorage impl |
| `caduceus-scanner` | Detect languages & frameworks from project files |
| `caduceus-git` | git status, diff, stage, commit via git2 |
| `caduceus-providers` | LlmAdapter trait, AnthropicAdapter, OpenAiCompatibleAdapter |
| `caduceus-permissions` | Capability enum, PermissionEnforcer, AuditLog |
| `caduceus-runtime` | BashSandbox, FileOps with workspace boundary enforcement |
| `caduceus-tools` | ToolRegistry, ToolSpec, 10 built-in tools with JSON Schema |
| `caduceus-orchestrator` | AgentHarness, SessionManager, ContextAssembler, slash commands |
| `caduceus-telemetry` | TokenCounter, CostLogger, TraceSpan |
| `caduceus-omniscience` | CodeChunker, SemanticIndex (Qdrant stub) |
| `caduceus-crdt` | Buffer, Fragment, Anchor, Lamport clock |

## License

MIT

