# Claurst Black-Box Behavioral Specification

---

## Provenance

**Analysis type:** Black-box extraction from public documentation only.  
**Sources read:** `README.md`, `docs/*.md` (index, providers, commands, plugins, agents, advanced, tools, configuration, installation, auth, keybindings), `src-rust/Cargo.toml` and all crate `Cargo.toml` files, `LICENSE.md`.  
**Sources NOT read:** No `.rs` source files were examined. The `spec/` directory was not consulted. No internal implementation details were accessed.  
**License constraint:** claurst is GPL-3.0. This specification is a black-box behavioral description only, derived entirely from public-facing documentation, consistent with clean-room reverse engineering principles.  
**Version analyzed:** 0.0.8  

---

## 1. Multi-Provider Support

### Supported Providers

Claurst supports 18+ LLM providers through a unified provider abstraction:

| Provider | Default Model | Auth Method |
|---|---|---|
| **Anthropic** (default) | `claude-sonnet-4-6` | `ANTHROPIC_API_KEY` env var |
| **OpenAI** | `gpt-4o` | `OPENAI_API_KEY` |
| **Google Gemini** | `gemini-2.5-flash` | `GOOGLE_API_KEY` or `GOOGLE_APPLICATION_CREDENTIALS` |
| **Azure OpenAI** | `gpt-4o` | `AZURE_API_KEY` + `AZURE_RESOURCE_NAME` + `AZURE_API_VERSION` |
| **AWS Bedrock** | `anthropic.claude-sonnet-4-6-v1` | `AWS_BEARER_TOKEN_BEDROCK` or SigV4 credentials |
| **GitHub Copilot** | `gpt-4o` | `GITHUB_TOKEN` |
| **Cohere** | `command-r-plus` | `COHERE_API_KEY` |
| **Ollama** (local) | `llama3.2` | None (local) |
| **LM Studio** (local) | current loaded model | None (local) |
| **LLaMA.cpp** (local) | default | None (local) |
| **Groq** | `llama-3.3-70b-versatile` | `GROQ_API_KEY` |
| **DeepSeek** | `deepseek-chat` | `DEEPSEEK_API_KEY` |
| **Mistral AI** | `mistral-large-latest` | `MISTRAL_API_KEY` |
| **xAI (Grok)** | `grok-2` | `XAI_API_KEY` |
| **OpenRouter** | `anthropic/claude-sonnet-4` | `OPENROUTER_API_KEY` |
| **Together AI** | `meta-llama/Llama-3.3-70B-Instruct-Turbo` | `TOGETHER_API_KEY` |
| **Perplexity** | `sonar-pro` | `PERPLEXITY_API_KEY` |
| **DeepInfra** | `meta-llama/Llama-3.3-70B-Instruct` | `DEEPINFRA_API_KEY` |
| **Venice AI** | `llama-3.3-70b` | `VENICE_API_KEY` |
| **Cerebras** | `llama-3.3-70b` | `CEREBRAS_API_KEY` |

### How `/connect` Works

`/connect` is an interactive slash command for configuring provider endpoints at runtime without editing config files:

```
/connect                              # Interactive provider picker
/connect <provider-name>              # Connect to a named provider
/connect openai https://api.openai.com/v1   # Connect with explicit base URL
```

### Connection Flow

1. **CLI flags** (highest priority, session-only): `claurst --provider openai --model gpt-4o`
2. **`/connect` command** (interactive session, updates active provider)
3. **`~/.claurst/settings.json`** `provider` field (persistent)
4. **Default**: Anthropic

Per-provider configuration in `settings.json` under the `providers` key supports `api_key`, `api_base`, `enabled`, `models_whitelist`, `models_blacklist`, and provider-specific `options`.

### Model Registry

Claurst ships a bundled model snapshot for Anthropic, OpenAI, and Google. It optionally refreshes from `https://models.dev/api.json` (cached to `~/.claurst/models_cache.json`, refreshed at most every 5 minutes). Network failures fall back silently to the bundled snapshot. When no model is explicitly set, Claurst scores available models by known priority patterns to pick the best default.

### Model Whitelisting / Blacklisting

Per-provider `models_whitelist` and `models_blacklist` arrays allow restricting which models can be selected. Whitelist takes effect first; blacklist removes entries from the whitelist result.

---

## 2. Buddy System

### What It Is

Every Claurst user has a persistent companion character called a "Buddy" (also referred to as a companion). The companion appears as a small sprite in the terminal UI and occasionally comments on user activity. The mascot of the project is named "Rustle" (a crab, shown in the README).

### Deterministic Generation

The companion's **visual traits** — species, eyes, hat, rarity, shiny status, and five stats — are generated deterministically by hashing the user's ID with a seeded PRNG (Mulberry32). This means:
- The companion is always the same for a given user.
- It cannot be changed by editing config files, because traits are regenerated from the hash on every read.

### Species (18 available)
`duck`, `goose`, `blob`, `cat`, `dragon`, `octopus`, `owl`, `penguin`, `turtle`, `snail`, `ghost`, `axolotl`, `capybara`, `cactus`, `robot`, `rabbit`, `mushroom`, `chonk`

### Rarity Tiers

| Rarity | Weight | Display |
|---|---|---|
| common | 60% | ★ |
| uncommon | 25% | ★★ |
| rare | 10% | ★★★ |
| epic | 4% | ★★★★ |
| legendary | 1% | ★★★★★ |

Rarity affects stat floors: legendary companions have a minimum stat floor of 50; common companions start at 5.

### Stats

Each companion has five stats: **DEBUGGING**, **PATIENCE**, **CHAOS**, **WISDOM**, **SNARK**. One stat is the peak (higher rolls), one is the dump stat (lower rolls), the rest scatter around the rarity floor.

### "Hatching" / Persistence

On first encounter, the model names and assigns a personality to the companion ("hatching"). After hatching, only the **soul** (name, personality, hatchedAt timestamp) is persisted to `~/.claude.json` under the `companion` key:

```json
{
  "companion": {
    "name": "Vortox",
    "personality": "a chaotic little axolotl who celebrates every bug as a feature",
    "hatchedAt": 1712345678901
  }
}
```

### User Interaction

Users observe the companion sprite in the TUI. The companion occasionally comments on activity. There are no documented slash commands specifically for managing the companion; it is passive and always-present.

---

## 3. Chat Forking

### What It Does

`/fork` creates a new independent session that begins from the exact current conversation state. It is useful for exploring two different approaches to a problem without losing either branch. The fork preserves the full message history but generates fresh UUIDs for all messages, forming a new linked chain.

### How to Trigger

```
/fork                         # Fork with auto-generated session name
/fork <new-session-name>      # Fork with a specific name
```

The forked session is independent: changes made in the fork do not affect the original, and vice versa.

### Session Storage

Sessions are stored as JSONL files under `~/.claude/projects/<sanitized-cwd>/<session-id>.jsonl`. Each line is a JSON object with fields including `uuid`, `parentUuid`, `type`, `message`, and `timestamp`. `/fork` rewrites all UUIDs while preserving the `parentUuid` chain structure.

### Programmatic Access

The SDK exports a `forkSession` function for programmatic forking.

---

## 4. Memory Consolidation

Claurst has two distinct "memory" concepts:

### A. Context Window Compaction (`/compact`)

**What it does:** When the conversation history grows large, `/compact` summarizes the entire prior exchange using the model and replaces the raw messages with a dense summary. This reduces token usage while preserving semantic continuity.

```
/compact
/compact focus on the database schema changes    # with custom instructions
```

**Auto-compaction:** Claurst tracks token usage after every turn. When usage crosses the threshold (effective context window minus a 13,000-token buffer), it runs compaction automatically. The threshold fraction is configurable via `compact_threshold` (default 0.85) in `settings.json`.

**Control:**
- `DISABLE_AUTO_COMPACT=1` — disables auto-compaction but keeps `/compact` available
- `DISABLE_COMPACT=1` — disables all compaction
- `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=<n>` — sets threshold to n% of context window
- `autoCompactEnabled` in `~/.claude.json` — boolean toggle

**Hook integration:** The `PreCompact` hook fires before compaction (exit code 2 blocks it); `PostCompact` fires after.

**Context limits:**
- Warning threshold: 20,000 tokens before the effective window limit
- Blocking limit: 3,000 tokens before the effective window — further input is blocked until compaction

### B. AGENTS.md Hierarchical Instruction Memory

**What it does:** Claurst reads instruction files from the filesystem before every session and whenever relevant files change, building a persistent memory of user and project-level instructions.

**Lookup order (lowest to highest priority):**
1. `/etc/claude-code/CLAUDE.md` (system-wide, admin-controlled)
2. `~/.claude/CLAUDE.md` and `~/.claude/rules/*.md` (personal global)
3. `CLAUDE.md`, `.claude/CLAUDE.md`, `.claude/rules/*.md` in each directory from filesystem root to CWD

`AGENTS.md` files are treated equivalently to `CLAUDE.md`.

**`/memory` command:** Opens a management UI for viewing, editing, and organizing these instruction files.

```
/memory
/memory list
/memory add <note>
/memory delete <id>
/memory clear
```

**Session memory notes:** Short notes that persist across sessions, readable by the model at session start for continuity.

---

## 5. TUI Features

### Framework

Built with **ratatui** (v0.29) and **crossterm** (v0.28), rendering an AMOLED-style terminal UI with real-time streaming output.

### UI Elements Documented

- **Conversation view** — Scrollable chat history with streaming assistant responses
- **Input field** — Multi-line input with Shift+Enter for newlines
- **Syntax-highlighted code blocks** — Powered by **syntect** (with default-syntaxes and default-themes)
- **Diff viewer** — For showing file changes
- **Permission dialogs** — Interactive prompts for tool approvals (Y/N/A/Enter/Escape)
- **Slash command palette** — Autocomplete overlay triggered by `/` or `Ctrl+K`
- **Interactive model picker** — Searchable list overlay (`/model` or `Ctrl+A`)
- **Session browser** — For `/resume` session selection
- **Status line** — Configurable bottom bar showing model name, token count, session name, git branch (toggleable with `/statusline`)
- **Buddy/companion sprite** — Small persistent companion character
- **Context visualizer** — `/ctx_viz` opens token usage breakdown
- **Memory management UI** — `/memory` renders an editing interface
- **Keybinding configurator** — `/keybindings` shows an interactive rebinding panel
- **Settings panel** — `/config` renders interactive settings
- **Theme picker** — `/theme` allows previewing and selecting color themes
- **Image rendering** — Sixel image protocol support (via `icy_sixel`) and PNG/JPEG display

### Color Themes

`default`, `dark`, `light`, `solarized`, `deuteranopia` (built-in). Custom themes are supported.

### Keybinding System

Context-aware keybinding system with these contexts: `global`, `chat`, `confirmation`, `modelPicker`, `commandPalette`, `search`, `vim.normal`, `vim.insert`, `vim.visual`.

**Key defaults:**
- `Enter` — submit message
- `Shift+Enter` — newline without submit
- `Ctrl+K` — open command palette
- `Ctrl+A` — open model picker
- `Ctrl+R` — history search
- `Ctrl+B` — create git branch
- `Escape` (during streaming) — interrupt
- `Ctrl+L` — redraw screen
- `Ctrl+F` / `Ctrl+Shift+F` — inline / global search

Custom keybindings are stored in `~/.claude/keybindings.json`. Chord bindings are supported.

### Vim Mode

`/vim` or `--vim` enables vim-style modal input (normal/insert/visual modes with standard motions). Persisted to user settings.

---

## 6. Speech Modes

The README notes these as **[EXPERIMENTAL]** features accessible via slash commands:

### `/Rocky`
A speech mode that changes how the model responds. The README teases "Try /Rocky and /Caveman to hear the difference!" — the exact behavioral change is marked experimental and not further documented in the public docs. Based on the name, this likely produces a "Rocky Balboa"-style terse or gritty speech pattern.

### `/Caveman`
Another experimental speech mode. Implied to produce simplified, primitive-style language output based on the name and the "hear the difference" invitation in the README.

### `/Normal`
Resets the speech mode back to standard output. Used to return from `/Rocky` or `/Caveman` modes.

**Notes:** 
- These are slash commands registered in the commands system
- They appear to modify the model's output style or system prompt for the session
- No further behavioral details are documented publicly beyond the README mention
- All three are marked `[EXPERIMENTAL]`

---

## 7. Plugin System

### Discovery

Plugins are loaded from `~/.claurst/plugins/`. Each subdirectory with a valid `plugin.toml` or `plugin.json` manifest is recognized as a plugin.

### Manifest Formats

Both TOML (`plugin.toml`) and JSON (`plugin.json`) are supported. The loader normalizes camelCase and snake_case field names. Required field: `name` (kebab-case, no spaces).

### What Plugins Can Provide

| Extension Type | Description |
|---|---|
| **Slash commands** | `.md` files in `commands/` directory define new `/commands` |
| **Agents** | `.md` agent definition files in `agents/` directory |
| **Skills** | Subdirectories in `skills/` each containing a `SKILL.md` |
| **MCP servers** | Inline `[[mcp_servers]]` definitions in the manifest |
| **LSP servers** | Inline `[[lsp_servers]]` for language-aware editing |
| **Output styles** | `.md` or `.json` style definitions in `output-styles/` |
| **Hooks** | `hooks.json` or inline hook configuration |
| **User config** | Declared user-configurable options surfaced by `/plugin info` |

### Hook System (Plugin Hooks)

Plugins hook into ~27 lifecycle events. Each hook is a shell command that receives a JSON payload on stdin.

**Events include:** `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionDenied`, `PermissionRequest`, `Notification`, `UserPromptSubmit`, `SessionStart`, `SessionEnd`, `Stop`, `StopFailure`, `SubagentStart`, `SubagentStop`, `PreCompact`, `PostCompact`, `Setup`, `TeammateIdle`, `TaskCreated`, `TaskCompleted`, `Elicitation`, `ElicitationResult`, `ConfigChange`, `WorktreeCreate`, `WorktreeRemove`, `InstructionsLoaded`, `CwdChanged`, `FileChanged`

**Blocking hooks:** Setting `blocking: true` on a hook means a non-zero exit code blocks the triggering operation. Non-blocking hooks (default) only log warnings on failure.

**Hook environment variables:** `CLAUDE_PLUGIN_ROOT`, `CLAUDE_PLUGIN_NAME`, `CLAUDE_TOOL_NAME`, `CLAUDE_TOOL_INPUT`, `CLAUDE_TOOL_RESULT`

**Matcher patterns:** Hook entries support `matcher` with `*` wildcard for tool-name filtering (e.g. `"File*"`, `"*Tool"`).

### Capability Grants

Plugins declare capability categories to restrict their access: `"read_files"`, `"write_files"`, `"network"`, `"shell"`, `"browser"`, `"mcp"`. Omitting the `capabilities` field grants all capabilities.

### Managing Plugins

```
/plugin                      — list all installed plugins
/plugin list                 — list with status
/plugin info <name>          — detailed info and user config
/plugin enable <name>        — enable (persisted)
/plugin disable <name>       — disable (persisted)
/plugin install <path>       — install from local directory
/plugin install author/name  — install from marketplace
/plugin reload               — rescan and reload all plugins
/reload-plugins              — alias for reload
```

### Plugin Marketplace

Plugins published to the Claurst marketplace have a `marketplace_id` field (format: `"author/plugin-name"`). The marketplace supports browsing, installing by ID, and updating.

### User-Configurable Options

Plugins declare `user_config` options (string, number, boolean, directory, file types) that appear in `/plugin info`. Options can be marked `required` and `sensitive` (masked in UI).

---

## 8. CLI Interface

### Basic Usage

```bash
claurst                              # Interactive TUI
claurst "task description"           # Prompt as positional argument
claurst -p "explain this codebase"   # Headless print mode (short flag)
claurst --print "task"               # Headless print mode (long flag)
```

### Documented CLI Flags

| Flag | Description |
|---|---|
| `--print` / `-p` | Headless mode: process prompt and print response, then exit |
| `--provider <name>` | Set the active provider (e.g. `anthropic`, `openai`, `ollama`) |
| `--model <id>` | Set the model for this session |
| `--agent <name>` | Activate a named agent persona (`build`, `plan`, `explore`, or custom) |
| `--api-key <key>` | Set API key for this session only (not persisted) |
| `--output-format <fmt>` | Output format for headless mode: `text` (default), `json`, `stream-json` |
| `--verbose` | Enable debug-level log output |
| `--thinking <tokens>` | Set token budget for extended thinking |
| `--effort <level>` | Set thinking effort level: `low`, `medium`, `high`, `max` |
| `--permission-mode <mode>` | Permission mode: `default`, `plan`, `acceptEdits`, `bypassPermissions` |
| `--vim` | Enable vim keybindings for this session (not persisted) |
| `--add-dir <path>` | Grant read/write access to an additional directory (repeatable) |
| `--max-budget-usd <amount>` | Stop after spending this much (e.g. `2.00`) |
| `--max-turns <n>` | Stop after n model turns |
| `--max-tokens <n>` | Stop after n output tokens |
| `--ssh` | Enable a remote-accessible session |
| `--version` | Show version |
| `--help` | Show help |

### Output Formats (Headless)

| Format | Description |
|---|---|
| (default `text`) | Plain text of the final assistant message |
| `json` | Full message array as JSON (requires `--verbose`) |
| `stream-json` | Newline-delimited JSON stream of messages as they arrive (requires `--verbose`) |

### Stdin Support

In headless mode, the prompt can be piped via stdin:
```bash
cat prompt.txt | claurst --print
echo "explain this code" | claurst -p
```

### Environment Variable Overrides

| Variable | Effect |
|---|---|
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `OPENAI_API_KEY` | OpenAI API key |
| `GOOGLE_API_KEY` | Google API key |
| `GITHUB_TOKEN` | GitHub Copilot token |
| `GROQ_API_KEY` | Groq API key |
| `DEEPSEEK_API_KEY` | DeepSeek API key |
| `MISTRAL_API_KEY` | Mistral API key |
| `OLLAMA_HOST` | Ollama base URL (default: `http://localhost:11434`) |
| `LM_STUDIO_HOST` | LM Studio base URL (default: `http://localhost:1234`) |
| `LLAMA_CPP_HOST` | LLaMA.cpp base URL (default: `http://localhost:8080`) |
| `ANTHROPIC_BASE_URL` | Override Anthropic endpoint (for proxies) |
| `CLAUDE_CODE_EFFORT_LEVEL` | Override persisted effort level for this process |
| `CLAUDE_CODE_ENABLE_VOICE` | Pre-enable voice mode (`1`) |
| `DISABLE_AUTO_COMPACT` | Disable auto-compaction (`1`) |
| `DISABLE_COMPACT` | Disable all compaction (`1`) |
| `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` | Set compaction threshold as percentage |
| `CLAURST_COORDINATOR_MODE` | Enable coordinator/multi-agent mode (`1`) |
| `CLAURST_SIMPLE` | Restrict workers to `["Bash", "Read", "Edit"]` (`1`) |
| `USER_TYPE` | Set to `ant` to unlock Anthropic-internal commands |

---

## 9. Crate Architecture (from Cargo.toml only)

### Workspace

Located at `src-rust/`. Workspace resolver version 2. Workspace version: 0.0.8, edition 2021, license GPL-3.0.

### Crates

| Crate | Package Name | Role (from description/deps) |
|---|---|---|
| `crates/cli` | `claurst` (the binary) | Entry point; aggregates all crates; defines the `claurst` binary |
| `crates/core` | `claurst-core` | Core types, configuration, session state, feature flags, PRNG, DB (SQLite), voice feature gate |
| `crates/api` | `claurst-api` | LLM provider abstraction layer; HTTP streaming; HMAC auth; caching |
| `crates/tools` | `claurst-tools` | 40+ built-in tools; PTY bash; file ops; computer-use (optional feature) |
| `crates/query` | `claurst-query` | Agentic query loop; orchestrates tool calls and model turns |
| `crates/tui` | `claurst-tui` | ratatui-based terminal UI; sixel image rendering; voice PTT; all UI features |
| `crates/commands` | `claurst-commands` | Slash command implementations; QR code; WebSocket; clipboard |
| `crates/mcp` | `claurst-mcp` | Model Context Protocol client; server registry |
| `crates/bridge` | `claurst-bridge` | Remote control WebSocket bridge; hostname detection |
| `crates/buddy` | `claurst-buddy` | Companion/buddy system; deterministic generation from user ID hash |
| `crates/plugins` | `claurst-plugins` | Plugin runtime; manifest loading; marketplace downloads; zip extraction |
| `crates/acp` | `claurst-acp` | Agent Client Protocol server; JSON-RPC 2.0 over stdio |

### Key External Dependencies (from workspace Cargo.toml)

| Category | Libraries |
|---|---|
| Async runtime | `tokio` (full), `tokio-stream`, `futures`, `async-trait`, `async-stream` |
| HTTP | `reqwest` (json, stream, native-tls, multipart), `reqwest-eventsource` |
| Serialization | `serde`, `serde_json`, `toml` |
| CLI parsing | `clap` (derive, env, string) |
| TUI | `ratatui`, `crossterm` |
| Database | `rusqlite` (bundled SQLite) |
| Image | `icy_sixel`, `image` (png, jpeg) |
| Text processing | `similar` (diffs), `syntect` (syntax highlighting), `unicode-width`, `unicode-segmentation` |
| WebSocket | `tokio-tungstenite` |
| Process | `nix` (Unix signals/process), `portable-pty` (PTY) |
| JSON Schema | `schemars` |
| Desktop automation | `enigo` (optional), `xcap` (optional) |
| QR codes | `qrcode` |
| Crypto | `sha2`, `hex`, `base64`, `hmac` |
| Concurrency | `parking_lot`, `dashmap` |

### Feature Flags (claurst-core)

36 feature flags grouped into categories (all gated in `dev_full`):

**Interaction & UI:** `ultraplan`, `ultrathink`, `history_picker`, `token_budget`, `message_actions`, `quick_search`, `away_summary`, `hook_prompts`, `kairos_brief`, `kairos_channels`, `lodestone`

**Agents & Memory:** `agent_triggers`, `agent_triggers_remote`, `extract_memories`, `verification_agent`, `builtin_explore_plan_agents`, `cached_microcompact`, `compaction_reminders`, `agent_memory_snapshot`, `teammem`

**Tools & Infrastructure:** `bash_classifier`, `bridge_mode`, `mcp_rich_output`, `connector_text`, `unattended_retry`, `new_init`, `powershell_auto_mode`, `shot_stats`, `tree_sitter_bash`, `tree_sitter_bash_shadow`, `prompt_cache_break_detection`, `native_clipboard_image`, `ccr_auto_connect`, `ccr_mirror`, `ccr_remote_setup`

**Hardware:** `voice` (enabled by default, gates `cpal` microphone capture)

### Crate Dependency Graph (simplified)

```
claurst (cli)
├── claurst-core          (foundation)
├── claurst-api           (→ claurst-core)
├── claurst-tools         (→ core, api, mcp)
├── claurst-query         (→ core, api, tools, plugins)
├── claurst-tui           (→ core, api, tools, query, mcp)
├── claurst-commands      (→ core, api, tools, query, mcp, tui, plugins, bridge)
├── claurst-mcp           (→ core)
├── claurst-bridge        (→ core, api, query)
├── claurst-buddy         (→ core)
├── claurst-plugins       (→ core)
└── claurst-acp           (→ core, api)
```

---

## 10. Unique Features

Based on public documentation, claurst differentiates from other coding agents in these ways:

### 1. True Multi-Provider Architecture
20+ providers behind a single unified interface, including local-only providers (Ollama, LM Studio, LLaMA.cpp). Switching providers requires only a flag or one-time config change — no workflow changes.

### 2. The Buddy Companion System
A unique gamified companion with deterministic generation from the user's ID hash, rarity tiers, and five RPG-style stats. Designed to be persistent, personal, and unfakeable. No other documented coding agent has this.

### 3. Speech Modes (Experimental)
`/Rocky` and `/Caveman` speech mode modifiers — novelty/personality output filters not seen in other coding agents.

### 4. Session Forking
`/fork` creates independent parallel conversation branches from any point, enabling non-destructive exploration of alternative approaches within the same codebase context.

### 5. Agent Client Protocol (ACP)
The `claurst-acp` crate implements a JSON-RPC 2.0 over stdio protocol for programmatic control of the agent from external processes — not just the Anthropic SDK stream format.

### 6. Coordinator / Multi-Agent Orchestration
`CLAURST_COORDINATOR_MODE=1` enables a top-level coordinator that spawns and manages parallel worker agents with distinct tool sets, a shared task registry, and structured research → synthesis → implementation → verification workflows.

### 7. Plugin Marketplace with Capability Grants
A structured plugin ecosystem with TOML/JSON manifests, 27 hook events, marketplace distribution, and fine-grained capability grants (`read_files`, `write_files`, `network`, `shell`, `browser`, `mcp`).

### 8. Voice Input (Experimental)
Deepgram streaming STT integration for microphone-based prompt submission, with push-to-talk behavior. The `voice` feature is on by default (gates the `cpal` audio crate).

### 9. Remote Bridge + SSH Sessions
WebSocket bridge to claude.ai for remote control (in-process or daemon topology), plus `--ssh` for SSH-accessible sessions that can be connected to from another machine.

### 10. Worktree Isolation for Sub-Agents
Sub-agents can operate in isolated git worktrees via `EnterWorktreeTool`/`ExitWorktreeTool`. Custom worktree backends (Docker containers, VMs) are supported via `WorktreeCreate`/`WorktreeRemove` hooks.

### 11. No Telemetry
Explicitly stated in the README: "there's no tracking or telemetry."

### 12. GPL-3.0 Open Source with Clean-Room Provenance
The project documents its clean-room methodology (two-phase: specification agent → implementation agent, never seeing the original TypeScript source). This gives users legal clarity for studying and modifying the codebase.

---

## Slash Commands Reference (Categorized)

For caduceus implementation reference, the 70+ slash commands fall into these categories:

### Session & Navigation
`/help` (`h`, `?`), `/clear` (`reset`, `new`), `/exit` (`quit`), `/resume` (`continue`), `/session` (`remote`), `/fork`, `/rename`, `/rewind` (`checkpoint`), `/compact`

### Model & Provider
`/model`, `/providers`, `/connect`, `/thinking`, `/effort`

### Configuration & Settings
`/config` (`settings`), `/keybindings`, `/permissions` (`allowed-tools`), `/hooks`

### Code & Git
(git-related commands referenced but not individually enumerated in scanned sections)

### Search & Files
`/context`, `/ctx_viz`

### Memory & Context
`/memory`, `/usage`, `/cost`, `/stats`, `/status`

### Agents & Tasks
`/agents`, `/tasks`

### Planning & Review
`/plan`

### MCP & Integrations
`/mcp`

### Authentication
`/login`, `auth login`

### Display & Terminal
`/output-style`, `/theme`, `/statusline`, `/vim`, `/voice`

### Diagnostics & Info
`/doctor`, `/version`

### Export & Sharing
`/export`, `/share`

### Plugin Management
`/plugin`, `/reload-plugins`

### Advanced & Internal (some Anthropic-internal only)
`/thinking`, `/connect`, `/fork`, `/effort`, `/summary`, `/brief`, `/context`

---

*End of black-box specification. All information derived solely from public documentation.*
