# Caduceus Engine

Pure Rust AI agent engine library. No UI — designed to be embedded in IDEs, CLIs, or any host application.

For the IDE, see [caduceus-ide](https://github.com/Alex-Keagel/caduceus-ide).

## What is it

A workspace of 14 Rust crates that provide everything needed to build an AI coding agent:

| Crate | Purpose |
|---|---|
| caduceus-core | Session state, IDs, events, tool types, token tracking |
| caduceus-orchestrator | AgentHarness (tool loop), modes, context assembly, scaffolders |
| caduceus-providers | LLM adapters (Anthropic, OpenAI, Copilot, Gemini, Ollama) |
| caduceus-tools | 28 tools (bash, read/write/edit file, grep, git, web, think, etc.) |
| caduceus-storage | SQLite persistence, wiki engine, memory store |
| caduceus-runtime | Sandbox execution, bash validator, file ops |
| caduceus-permissions | Capability system, audit log, hooks |
| caduceus-marketplace | 80+ skills, 30+ agents, catalog, installer |
| caduceus-omniscience | Code intelligence, semantic search, symbol parsing |
| caduceus-telemetry | Token counting, cost calculation |
| caduceus-mcp | Model Context Protocol client |
| caduceus-git | Git operations, checkpoints |
| caduceus-crdt | Collaborative editing (Lamport clocks, fragments) |
| caduceus-scanner | Project language/framework detection |

## Quick Start

```bash
git clone https://github.com/Alex-Keagel/caduceus.git
cd caduceus
cargo test --workspace
```

## Agent Loop

```
User message -> AgentHarness::run()
  - Assemble context (budget-aware)
  - Call LLM with tools
  - stop_reason?
    - EndTurn -> return final text
    - ToolUse -> execute tools -> feed results -> loop
      - LoopDetector (same args 3x -> alert)
      - Circuit breaker (5 failures -> stop)
      - CancellationToken check
  - Emit events: ThinkingStarted, ToolCallStart, ToolResultEnd, TurnComplete
```

## Events

The engine emits structured AgentEvent variants for visualization:

- ThinkingStarted, ReasoningDelta, ReasoningComplete
- ToolCallStart, ToolResultEnd
- ContextWarning, ContextCompacted
- LoopDetected, CircuitBreakerTriggered
- ExecutionTreeNode, ExecutionTreeUpdate
- MessagePart (Text, Reasoning, ToolInvocation, CodeArtifact, Source, Suggestion)

## Tests

```bash
cargo test --workspace  # 1,083 tests
```

## License

MIT
