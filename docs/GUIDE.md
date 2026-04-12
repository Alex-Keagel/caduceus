# Caduceus User Guide

> The complete guide to using Caduceus — a terminal-first, local-first AI development environment built on Tauri 2, Rust, and React.

---

## Table of Contents

1. [Getting Started](#1-getting-started)
2. [How the Agent Understands Your Codebase](#2-how-the-agent-understands-your-codebase)
3. [Agent Modes](#3-agent-modes)
4. [Slash Commands Reference](#4-slash-commands-reference)
5. [@Mentions](#5-mentions)
6. [Context Management](#6-context-management)
7. [Best Practices for Context Engineering](#7-best-practices-for-context-engineering)

---

## 1. Getting Started

### Install

Download the latest release for your platform from the [releases page](https://github.com/your-org/caduceus/releases), or build from source:

```bash
# Build from source (requires Rust 1.78+, Node.js 18+)
git clone https://github.com/your-org/caduceus
cd caduceus
cargo build --release
npm install && npm run build
cargo tauri build
```

### Configure a Provider

Caduceus needs an LLM provider to function. The fastest path:

```bash
# Option A: GitHub Copilot (zero config if already authenticated)
gh auth login

# Option B: Anthropic Claude
export ANTHROPIC_API_KEY=sk-ant-...

# Option C: OpenAI
export OPENAI_API_KEY=sk-...
```

See [PROVIDERS.md](PROVIDERS.md) for all providers and per-operation model routing.

### Open a Project

```bash
# Launch the desktop app
caduceus

# Or point it at a specific directory
caduceus --project /path/to/your/repo
```

When you open a project for the first time, Caduceus runs `ProjectScanner` on the directory and builds an initial index. This takes a few seconds for most codebases and runs in the background — you can start chatting immediately.

### Initialize Project Config

```bash
/init
```

This scaffolds `.caduceus/` in your project root with a starter `CADUCEUS.md` and config files. Commit these files — they give the AI persistent context about your project's conventions and architecture.

---

## 2. How the Agent Understands Your Codebase

Caduceus's **Omniscience layer** (crate: `caduceus-omniscience`) continuously builds a rich, multi-dimensional model of your codebase. It is composed of four subsystems.

### ProjectScanner

**What it does:** On session start (and incrementally on file changes), ProjectScanner walks your project directory, detects programming languages, frameworks, and project structure, then builds a structured file tree with token-budget estimates for each file.

**How it works:**

1. Reads `.caduceusignore` (and `.gitignore`) to exclude irrelevant paths
2. Detects languages by file extension and content heuristics
3. Identifies framework fingerprints (e.g., `Cargo.toml` → Rust workspace, `package.json` → Node.js)
4. Produces a context map: file path → language, size, estimated tokens, importance score
5. Feeds the top-ranked files into the system prompt as the initial context window

**What you see:** When you open a project, the status bar shows a scan progress indicator. Once complete, `@file` and `@folder` completions become available.

### FederatedIndex

**What it does:** Cross-project symbol search. When you're working in a monorepo or have multiple related projects open, FederatedIndex lets the agent search for symbols, types, and functions across all indexed projects in a single query.

**How it works:**

- Each project maintains its own vector index (embedded `qdrant-edge` shards)
- A federation layer issues fan-out queries and merges ranked results
- Results are de-duplicated and returned with source-project provenance

**When it matters:** Most useful in monorepos with shared libraries, or when your project imports a sibling package you've also indexed.

```
# Example: finding all usages of a type across a monorepo
@mention UserPermission in any crate
```

### CodePropertyGraph

**What it does:** Maps the relationships between functions, classes, and modules using AST analysis powered by `tree-sitter`. The graph answers questions like "what calls this function?" and "what does this module import?"

**Nodes:** functions, classes, methods, modules, structs, enums, interfaces  
**Edges:** calls, imports, inherits, implements, instantiates

**What the agent uses it for:**

- Impact analysis ("if I change this function, what else might break?")
- Navigation ("show me all callers of `parse_config`")
- Refactoring suggestions that respect dependency direction
- Detecting circular dependencies

**How it's built:** On every file save (or agent turn), the scanner re-parses changed files with `tree-sitter`, diffs the AST, and updates the affected graph nodes incrementally.

### WikiEngine

**What it does:** Auto-maintains a living knowledge wiki in `.caduceus/wiki/` that documents your project's architecture, key APIs, and patterns in plain Markdown.

**Structure:**

```
.caduceus/
└── wiki/
    ├── index.md          # Master index with links to all pages
    ├── architecture.md   # Auto-generated architecture overview
    ├── api/              # One page per public API surface
    │   ├── config.md
    │   └── providers.md
    ├── patterns.md       # Detected design patterns
    └── glossary.md       # Project-specific terminology
```

**WikiWatcher — what triggers updates:**

- **File save:** Any change to a source file triggers re-analysis of that file's public API surface and updates the relevant wiki page
- **Agent turn:** At the end of every agent turn, the WikiEngine checks for staleness and regenerates any pages whose source files have changed since last write
- **Manual trigger:** Run `/wiki refresh` to force a full rebuild
- **`/init`:** Generates the initial wiki from scratch using a deep scan pass

**Reading the wiki:** The agent automatically injects relevant wiki pages into its context when answering questions. You can also read them directly:

```bash
cat .caduceus/wiki/architecture.md
```

**Committing the wiki:** The `.caduceus/wiki/` directory is worth committing — it gives future sessions instant architectural context and saves the scanning overhead on first open.

---

## 3. Agent Modes

Switch modes with `/mode <name>` or by selecting from the mode picker in the status bar.

| Mode | Keyword | What it does |
|------|---------|-------------|
| **Plan** | `plan` | Read-only analysis. The agent can read files, search code, and draft a plan — but makes **no** file edits or shell commands. Use this to review a proposed approach before execution. |
| **Act** | `act` | Executes the plan step by step. File edits, shell commands, and git operations are permitted (subject to your permission grants). |
| **Research** | `research` | Deep code exploration mode. The agent reads broadly, traces call graphs, and synthesizes findings into a structured report. No edits. |
| **Autopilot** | `autopilot` | Combines Plan → Act automatically. The agent plans, asks for your go/no-go, then executes. Best for well-defined tasks. |
| **Architect** | `architect` | High-level design mode. The agent reasons about structure, dependencies, and trade-offs. Produces architecture docs, diagrams (Mermaid), and migration plans. |
| **Debug** | `debug` | Focused on diagnosing failures. Reads logs, traces, test output, and source to pinpoint root causes. Suggests fixes but doesn't apply them without Act mode. |
| **Review** | `review` | Code review mode. Analyzes staged changes or a diff, checks against project standards, and produces structured feedback. |

### Mode Tips

- Start a complex task in **Plan** mode to sanity-check the approach before granting write access
- Use **Research** before **Architect** on an unfamiliar codebase
- **Autopilot** works best with a well-written `CADUCEUS.md` that defines your standards

---

## 4. Slash Commands Reference

Type `/` in the input to open the command palette with fuzzy autocomplete.

### Session Commands

| Command | Description |
|---------|-------------|
| `/help` | Show all available slash commands |
| `/model [name]` | Switch the active model. Without an argument, opens the searchable model picker. |
| `/mode [name]` | Switch agent mode (plan / act / research / autopilot / architect / debug / review) |
| `/compact` | Summarize and compress the current conversation to free context window space |
| `/export [filename]` | Export the full conversation to a Markdown file |
| `/clear` | Clear the current conversation (does not delete session history) |

### Project Commands

| Command | Description |
|---------|-------------|
| `/init` | Initialize `.caduceus/` in the current project (scaffolds `CADUCEUS.md`, config, wiki) |
| `/config [key] [value]` | Get or set a config value. Browse sections interactively without arguments. |
| `/connect [provider]` | Add API credentials for a new LLM provider |
| `/wiki [refresh\|show]` | Manage the auto-generated project wiki |
| `/scan` | Re-run ProjectScanner on the current directory |

### Context Commands

| Command | Description |
|---------|-------------|
| `/ctx` | Show a breakdown of current context window usage by category |
| `/ctx_viz` | Visual context window usage map |
| `/checkpoint [name]` | Save a named context checkpoint you can restore later |
| `/restore [name]` | Restore a saved checkpoint |
| `/zone add [zone]` | Add a context zone (see [Context Management](#6-context-management)) |

### Plugin Commands

| Command | Description |
|---------|-------------|
| `/plugins list` | List installed plugins |
| `/plugins install [name]` | Install a plugin |
| `/plugins enable [name]` | Enable a disabled plugin |
| `/plugins disable [name]` | Disable a plugin without uninstalling |

---

## 5. @Mentions

`@mentions` pull specific resources into the agent's context window on demand.

### @file

Include a specific file in context:

```
@file src/main.rs
@file src/config.rs tell me about the config schema
```

Caduceus reads the file content, token-estimates it, and injects it with a label. If the file is large, it is automatically chunked and the most relevant sections are prioritized.

### @folder

Include all files in a directory (up to a configurable token budget):

```
@folder src/providers/
@folder crates/caduceus-core/src/ explain the error types
```

Files within the folder are ranked by relevance to your query and included in descending priority order.

### @url

Fetch and inject the content of a URL:

```
@url https://docs.anthropic.com/en/api/messages summarize the streaming API
@url https://github.com/org/repo/blob/main/README.md
```

The page is fetched, converted to clean Markdown, and injected into context. Useful for including API docs, RFCs, or GitHub issues.

### @git

Reference Git objects:

```
@git HEAD~3       # Diff of last 3 commits
@git main..HEAD   # Diff between branches
@git abc1234      # Specific commit
```

---

## 6. Context Management

The context window is finite. Caduceus gives you tools to manage it deliberately.

### Context Zones

Zones divide the context window into named regions with priority levels. Files in high-priority zones are always included; lower-priority zones are evicted first under pressure.

```
/zone add core src/core.rs          # always-include zone
/zone add reference docs/API.md     # included when relevant
/zone add scratch notes.md          # lowest priority
```

### Compaction

When the context window approaches its limit, the status bar shows a warning. Run `/compact` to:

1. Ask the agent to summarize the conversation so far
2. Replace the full history with a compact summary (configurable line/character budget)
3. Preserve the most recent N turns verbatim

Compaction is non-destructive — the full history is saved to the session database and recoverable.

### Checkpoints

Save context snapshots before making large changes:

```
/checkpoint before-refactor
# ... do the refactor ...
/restore before-refactor   # if something goes wrong
```

### Token Budget Awareness

The status bar shows live token counts:
- **Used**: tokens currently in the context window
- **Budget**: model's maximum context length
- **Remaining**: how much headroom you have

Each `@mention` shows its token cost in the autocomplete dropdown before you commit to including it.

---

## 7. Best Practices for Context Engineering

### 1. Write a good CADUCEUS.md

The single highest-leverage thing you can do. The agent reads this file on every session start. Include:

- What the project does (2–3 sentences)
- Architecture overview (layers, key crates/modules)
- Coding conventions (naming, error handling, test requirements)
- What's off-limits (auto-generated files, vendor dirs)
- How to build and test

See [CUSTOMIZATION.md](CUSTOMIZATION.md) for examples.

### 2. Use Plan mode before Act mode

Before any large change, switch to Plan mode. Review the proposed approach. Only switch to Act once the plan looks correct. This prevents the agent from going in the wrong direction for many steps.

### 3. Be specific with @mentions

Instead of: *"Look at the providers code"*  
Prefer: `@folder crates/caduceus-providers/src/ how does the Anthropic adapter handle streaming?`

Explicit context is faster and cheaper than letting the agent search.

### 4. Compact early, compact often

Run `/compact` proactively when the context passes 50% full — not when it's at 95% and the model is struggling. Early compaction produces better summaries.

### 5. Use Research mode to front-load understanding

For unfamiliar codebases, start with a Research session to build a mental model before asking the agent to make changes. The wiki and checkpoint system let you preserve what was learned.

### 6. Commit your .caduceus directory

Committing `.caduceus/` (excluding any secrets) gives every teammate instant context on the next session open:

```gitignore
# .gitignore — exclude secrets, keep config and wiki
.caduceus/secrets
.caduceus/sessions/
```

### 7. Keep CADUCEUS.md under 500 lines

The agent reads this file on every turn. A long, exhaustive file dilutes the signal. Link to ARCHITECTURE.md for detail; keep CADUCEUS.md focused on what the agent needs to make day-to-day decisions.
