# Caduceus Engine Development Rules

## Architecture

Caduceus is a 14-crate Rust engine powering the Caduceus IDE (a Zed fork).

The engine is **IDE-agnostic** — it has no dependency on Zed/gpui. The AgentTools
are Zed-specific adapters that live in the IDE repo, implementing Zed's `AgentTool`
trait and delegating to the engine via the bridge.

| Layer | Location | Purpose |
|-------|----------|---------|
| **Engine** | `~/Dev/caduceus/crates/` | 14 crates — core logic, tools, orchestration (IDE-agnostic) |
| **Bridge** | `~/Dev/zed/crates/caduceus_bridge/` | Wires engine to IDE via direct Rust calls |
| **AgentTools** | `~/Dev/zed/crates/agent/src/tools/caduceus_*.rs` | Zed-specific adapters (implement `AgentTool` trait) |
| **UI** | `~/Dev/zed/crates/agent_ui/src/` | User-facing panels, buttons, indicators |

## Engine ↔ IDE Parity (MANDATORY)

Every engine capability that is developed or changed MUST also be developed and wired in the IDE:

1. **Engine crate change → Bridge method → AgentTool → UI exposure**. No feature ships without all 4 layers.
2. When adding a public method to any crate, add the corresponding bridge method in `~/Dev/zed/crates/caduceus_bridge/src/`.
3. If the method is user-facing, create or update the AgentTool in `~/Dev/zed/crates/agent/src/tools/caduceus_*.rs`.
4. If it has visual feedback, wire it into `~/Dev/zed/crates/agent_ui/src/`.

## Pre-Commit Checklist (MANDATORY)

Before every commit, run tests in BOTH projects:

```bash
# Engine tests (this repo)
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace -- -D warnings

# Bridge + IDE tests (Zed fork)
cd ~/Dev/zed && cargo test -p caduceus_bridge
cd ~/Dev/zed && cargo check -p agent -p agent_ui

# Release build (verify linking)
cd ~/Dev/zed && cargo build --release -p zed --features gpui_platform/runtime_shaders
```

Do NOT commit if any of these fail.

## Testing Requirements

Every complex feature MUST include:
- **Unit tests** in the crate where the feature is implemented
- **Integration tests** in `~/Dev/zed/crates/caduceus_bridge/tests/` validating the bridge
- **Hardcore review** via the `iterate-hardcore` skill before merging significant changes

For non-trivial features (multi-file, architectural, new tools):
1. Write tests FIRST (TDD when possible)
2. Run the `iterate-hardcore` skill for scored review
3. All dimensions must reach 8+ before shipping

## Crate Structure

| Crate | Purpose |
|-------|---------|
| `caduceus-core` | Base types, config, hooks, keybindings |
| `caduceus-orchestrator` | Agent harness, modes, kanban, PRD, progress, time tracking |
| `caduceus-tools` | 36+ tools, registry, validation, scaffolding |
| `caduceus-omniscience` | Semantic search, code graph, RAG, embeddings |
| `caduceus-storage` | SQLite persistence, sessions, memory, wiki, trajectories |
| `caduceus-telemetry` | Token counting, cost tracking, SLOs, budget, drift |
| `caduceus-git` | Git operations (diff, status, commit, worktrees) |
| `caduceus-permissions` | Capability tokens, secret scanning, audit, trust scoring |
| `caduceus-providers` | Multi-provider LLM API, circuit breaker |
| `caduceus-mcp` | MCP client/server, security scanning |
| `caduceus-marketplace` | Skill registry, evolution, versioning |
| `caduceus-scanner` | Language/framework detection |
| `caduceus-runtime` | Session management, file ops, snapshots |
| `caduceus-crdt` | Collaborative editing (replica IDs, Lamport clocks) |

## Safety Invariants

- Read-only modes (Plan, Research, Architect, Review) MUST block write tools
- Kill switch MUST cancel all running sessions immediately
- Checkpoints MUST be created before destructive operations
- Auto-compact MUST trigger before context explosion (threshold: 40 messages)
- All `.unwrap()` calls are forbidden — use `?` or `.map_err()`

## Project-local `private/` files — read open, write requires grant + path

This repo's top-level `private/` directory holds audits, reviews, scratch notes, and reviewer/critique artifacts. The convention is **profile-agnostic** (same rules in plan, research, act, autopilot, and any custom mode) and is also enforced as an **envelope-level invariant**: all four presets in `caduceus-permissions/src/envelope.rs` allow reading `private/**` (locked in by the `all_presets_allow_reading_private_directory` test).

**Read access — always permitted.** Any agent in any profile may read freely from `private/**` to ground its work. No prompt, no grant flow.

**Write access — never silent.** Before writing, creating, or modifying any file under `private/`, the agent MUST do BOTH of the following in a single `ask_user` interaction:

1. **Ask permission to write** — explicitly state intent (kind of artifact, which workflow produced it, why `private/` vs. session workspace).
2. **Ask where to write** — propose a target subpath (e.g. `private/audits/<slug>-<date>.md`, `private/reviews/<pr>-<date>.md`) AND offer the user the chance to override the slug, subdirectory, or filename.

This applies to every write tool — `create`, `edit`, `bash` redirects (`> private/foo`, `tee private/foo`), `git add` of newly authored `private/**` files, and any agent-generated artifact a workflow would otherwise auto-place there. **Exception:** an explicit user request to update a specific `private/` file (e.g. *"update `private/audits/foo.md` with X"*) does not need re-prompting for that same file in that same turn.

See `~/Dev/.github/copilot-instructions.md` for the cross-repo source of truth.

## Conventions

- Crate names: `caduceus-{name}` (kebab-case)
- Public API: all public methods need doc comments
- Error types: use `CaduceusError` from core, not `anyhow` directly
- Tests: `#[test]` for unit, `tests/` dir for integration
- No `mod.rs` files — use `src/{crate_name}.rs` as lib root
