# Caduceus Behavioral Specification

## Provenance

- **Source repository:** hermes-hq/hermes-ide
- **Commit SHA:** 0ef3380cf0d797a1fbdb34dd93b5b26f3ce9406b
- **Analysis date:** 2025-07-17
- **Statement:** This document describes only observable behaviors, data flows, state machines, and API contracts. No copyrightable expression (source code, variable names, internal identifiers, comments, or error message strings) has been carried forward from the source repository.

---

## Table of Contents

1. [System Overview](#1-system-overview)
2. [PTY Session Management](#2-pty-session-management)
3. [SQLite Persistence](#3-sqlite-persistence)
4. [Project Scanner](#4-project-scanner)
5. [Git Integration](#5-git-integration)
6. [Tauri IPC Commands](#6-tauri-ipc-commands)
7. [Frontend Architecture](#7-frontend-architecture)
8. [AI Integration](#8-ai-integration)
9. [Plugin System](#9-plugin-system)
10. [Platform Handling](#10-platform-handling)
11. [Workspace Detection](#11-workspace-detection)

---

## 1. System Overview

The system is an AI-native terminal IDE built as a desktop application using a Rust backend with a React/TypeScript frontend connected via an IPC bridge. The application manages multiple terminal sessions simultaneously, each backed by a pseudo-terminal (PTY) process. It provides deep integration with AI coding agents (multiple providers supported), Git version control, a project intelligence system, and an extensible plugin architecture.

### High-Level Architecture

```
┌──────────────────────────────────────────────────────┐
│                   React Frontend                      │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌─────────┐ │
│  │ Terminal  │ │   Git    │ │ Context  │ │ Plugin  │ │
│  │  Panes   │ │  Panel   │ │  Panel   │ │ Manager │ │
│  └────┬─────┘ └────┬─────┘ └────┬─────┘ └────┬────┘ │
│       │             │            │             │      │
│  ┌────┴─────────────┴────────────┴─────────────┴────┐│
│  │           Session State (React Context)           ││
│  └──────────────────────┬────────────────────────────┘│
│                         │ IPC Bridge                   │
├─────────────────────────┼──────────────────────────────┤
│                         │ Rust Backend                 │
│  ┌──────────┐ ┌────────┴──┐ ┌──────────┐ ┌─────────┐│
│  │   PTY    │ │  Database  │ │   Git    │ │ Project │││
│  │ Manager  │ │  (SQLite)  │ │  Engine  │ │ Scanner │││
│  └──────────┘ └───────────┘ └──────────┘ └─────────┘││
└──────────────────────────────────────────────────────┘
```

### Application Window

- Default size: 1200×800 pixels, minimum 600×400
- Title bar style: overlay (macOS), standard (others)
- Hidden title text, drag-drop enabled
- macOS minimum: 13.0
- Content Security Policy restricts connections to: self, IPC, localhost, major AI provider APIs (Anthropic, OpenAI, Google), GitHub, and a telemetry endpoint

---

## 2. PTY Session Management

### 2.1 Session Lifecycle State Machine

Sessions progress through a well-defined state machine:

```
Creating → Initializing → ShellReady → LaunchingAgent → Idle ⇄ Busy
                                                          ↕        ↕
                                                     NeedsInput   Error
                                                          ↓
                                                    Closing → Disconnected (SSH)
                                                           → Destroyed (local)
```

**State Descriptions:**

| State | Description | Accepts Input? |
|-------|-------------|----------------|
| Creating | Session object allocated, PTY not yet spawned | No |
| Initializing | PTY spawned, shell loading | Yes |
| ShellReady | Shell prompt first detected | Yes |
| LaunchingAgent | AI agent command being started | Yes |
| Idle | AI agent waiting at prompt | Yes |
| Busy | AI agent actively working | Yes |
| NeedsInput | Agent waiting for user confirmation/permission | Yes |
| Error | Something went wrong | No |
| Closing | Teardown in progress | No |
| Disconnected | SSH connection lost (can reconnect) | No |
| Destroyed | PTY process exited | No |

### 2.2 Session Creation Flow

**Inputs required for session creation:**
- Session identifier (auto-generated if not provided)
- Display label (auto-incrementing counter)
- Working directory (defaults to home directory)
- Session color (visual identification)
- AI provider selection (claude, aider, codex, gemini, copilot, or none)
- Auto-approval flag and permission mode
- Custom command suffix (optional)
- Channel list (optional, for plugin integration)
- Terminal dimensions (rows, columns)
- SSH connection info (optional): host, port, user, tmux session, identity file

**Creation sequence:**
1. Frontend pre-generates a terminal identifier
2. Terminal event listener is created
3. Terminal dimensions are estimated from window size and font settings
4. Backend creates session entity with metrics structure
5. Working directory is resolved (worktree if linked, provided path, or home)
6. Default shell is fetched from database or auto-detected
7. PTY is opened with requested dimensions
8. PTY is explicitly resized on macOS (workaround for openpty quirk)
9. Shell integration scripts are injected (per shell type)
10. Command is built (local shell or SSH)
11. Child process is spawned
12. Background reader thread is started

### 2.3 Shell Integration

At PTY startup, the system injects lightweight shell-specific configuration to:
- Disable autosuggestion plugins that would interfere with AI agent input
- Set an environment variable indicating the process is running inside the IDE
- Enable ignoring space-prefixed commands in history
- Send a terminal resize signal after startup to fix dimension race conditions

**Per-shell behavior:**

| Shell | Integration Method | Key Behaviors |
|-------|-------------------|---------------|
| Zsh | Temporary ZDOTDIR with wrapper init files | Sources user config then disables autosuggestions, autocomplete; restores ZDOTDIR in final init file |
| Bash | Custom rcfile replacing login mode | Sources system profile and user profiles manually, disables ble.sh |
| Fish | Init command flag | Disables built-in autosuggestion feature |

Integration files are cleaned up when sessions close. Stale files from crashed sessions are cleaned on startup.

### 2.4 PTY I/O Flow

**Output processing pipeline:**
1. Reader thread reads PTY output continuously in background
2. ANSI escape codes are stripped for analysis purposes
3. Each line is analyzed by provider-specific adapters
4. Provider adapter extracts: token usage, tool calls, actions, phase hints, memory facts
5. State transitions are queued based on phase hints
6. Metrics are accumulated (tokens, files, actions, latency)
7. Output is base64-encoded and emitted to frontend
8. Frontend decodes and writes to terminal emulator buffer
9. User scroll position is preserved during output streaming

**Output analysis extracts:**
- Current working directory (via OSC 7 protocol)
- AI agent identification (provider, model, confidence)
- Token usage per provider (input, output, cache, cost)
- Tool calls (name, arguments, timestamp)
- Action events (commands, labels, suggestions)
- Phase hints (prompt detected, work started, input needed)
- Memory facts (key-value pairs with source and confidence)
- File paths mentioned or modified
- Port numbers in output

### 2.5 Silence Detection

A fallback mechanism checks for PTY silence every 2 seconds:
- If shell never became ready (no prompt detected), marks shell ready and triggers AI agent auto-launch
- This ensures the agent starts even with exotic prompt themes that don't match standard patterns
- If agent is active and output has stopped, transitions to appropriate state based on last output content

### 2.6 Session Metrics

Each session accumulates comprehensive metrics:

| Metric Category | Data Tracked |
|-----------------|-------------|
| Output | Line count, error count |
| Tokens | Per-provider input/output counts with cost estimates, history for sparkline visualization (30 samples) |
| Tools | Call counts by type, recent calls queue |
| Files | Set of files mentioned/modified, ordered queue of 50 most recent |
| Actions | Recent AI actions, available command templates |
| Memory | Deduped key-value facts about the project |
| Latency | Response time samples, p50/p95 percentiles |
| Execution | Command-output pairs with timing, input, summary, exit code (20 recent nodes) |

### 2.7 Terminal Resize

- Frontend uses ResizeObserver with debounced double-requestAnimationFrame to handle CSS layout settling
- Fallback window resize listener for edge cases (e.g., window restore from minimized)
- Resize signals sent to PTY on dimension changes
- Shell integration sends SIGWINCH after startup to fix race conditions

### 2.8 Multi-Session Management

- Session pool maintains collection of active sessions by identifier
- Auto-incrementing counter for label generation
- Sessions are independent PTY processes
- Layout system supports arbitrary split-pane arrangements
- Sessions can be switched via sidebar, keyboard shortcuts (Mod+1-9), or drag-drop

### 2.9 Session Termination

**Local sessions:** PTY process is killed with SIGHUP (grace period), then SIGKILL if needed. State transitions to Destroyed.

**SSH sessions:** State transitions to Disconnected. SSH control socket allows potential reconnection.

### 2.10 macOS Process Spawning

On macOS, standard fork/exec is unsafe in multi-threaded processes. The system uses POSIX spawn which atomically creates the child process. A helper binary acts as a trampoline to set the controlling terminal (required for sudo, ssh, gpg) before executing the real shell command.

### 2.11 Events Emitted to Frontend

| Event | Payload | Trigger |
|-------|---------|---------|
| session-updated | Full session state (phase, metrics, agent info) | Phase change, metrics update (~500ms) |
| cwd-changed-{id} | New working directory path | OSC 7 protocol detected |
| pty-exit-{id} | Exit status | PTY process terminates |
| command-prediction-{id} | Frequency-weighted suggestions | Command analysis |

---

## 3. SQLite Persistence

### 3.1 Database Configuration

- WAL (Write-Ahead Logging) mode for concurrent read/write
- Foreign key constraints enforced
- Located in application data directory

### 3.2 Schema Overview

**Sessions table:** Stores terminal session metadata — identifier, display label, color, group association, lifecycle phase, working directory, shell type, workspace paths (JSON array), creation/update timestamps, SSH configuration details, description.

**Token Usage table:** Per-session API token consumption records — provider name, model name, input/output token counts, estimated USD cost, timestamp.

**Token Snapshots table:** Historical snapshots of token usage for cost analytics over time.

**Cost Daily table:** Aggregated daily cost rollups by provider and model for dashboard display.

**Memory table:** Scoped persistent key-value entries — session/project/global scope, confidence scores, source attribution, access counter tracking.

**Execution Log table:** Records command executions — type classification, content, exit codes, working directory context.

**Execution Nodes table:** Detailed timeline of actions — commands, tool calls, operations with duration metrics and metadata. Capped at 20 recent entries per session.

**Settings table:** Application preferences as key-value pairs. Allowlist-validated on write. Machine-specific settings excluded from export.

**Projects (Realms) table:** Project definitions — path, name, detected languages (array), frameworks (array), architectural analysis (pattern, layers, entry points), conventions, scan status (surface/deep/full), timestamps.

**Error Patterns table:** Fingerprints of error messages — occurrence counts, resolution history, verification status.

**Command Patterns table:** Frequently-used command sequences — frequency statistics for prediction engine.

**Context Pins table:** Semantic references to files, memory entries, or text snippets — priority ordering, session/project scope.

**Context Snapshots table:** Versioned snapshots of analysis context for session undo/redo.

**Session-Projects junction table:** Many-to-many mapping with attachment role.

**Session Worktrees table:** Git worktree associations per session-project pair — branch tracking, activity timestamps.

**Hermes Config cache table:** Cached project-level configuration files with hash for change detection.

**Conventions table:** Code style rules and architectural conventions per project.

**SSH Saved Hosts table:** Reusable connection profiles — host, port, user, identity file, jump host.

**Plugins table:** Installed plugin metadata — version, permission grants.

**Plugin Storage table:** Plugin-private persistent key-value storage, isolated per plugin.

### 3.3 Migration Strategy

- Progressive schema creation via idempotent statements (tables created only if not existing)
- Schema evolution via idempotent column additions
- Table recreation for constraint changes (SQLite limitation for dropping constraints)
- Name migration: older database names automatically renamed on startup
- Explicit transactions with rollback on error for multi-statement operations

### 3.4 Common Query Patterns

- **Session CRUD:** Create, read by ID/filter, update individual fields (phase, label, description, color), full upsert on save
- **Memory queries:** Fetch by scope (session/project/global), merged lookups across multiple projects, access counter increment
- **Token aggregation:** Sum by provider/model, daily rollups, history for visualization
- **Project queries:** Ordered by existence on disk and usage score, filtered by session
- **Context assembly:** Join pins + projects + memory + conventions for a session
- **Worktree tracking:** Insert/update activity timestamps, query by session or project, delete on session close

### 3.5 Data Validation & Safety

- Settings keys validated against allowlist before insertion
- Path operations use canonicalization to prevent directory traversal
- File size limits on imports (1 MB for settings)
- JSON structure validation on imports
- Machine-specific settings excluded from export for portability

---

## 4. Project Scanner

### 4.1 Project Discovery

The scanner recursively walks directories (configurable depth, default 3) looking for Git repositories. It identifies project roots by the presence of a `.git` directory.

**Exclusions:**
- Security-sensitive directories (SSH keys, cloud credentials, encryption keys, Kubernetes configs)
- Vendor/dependency directories (node_modules, vendor, virtual environments, build targets)
- IDE worktree directories (ephemeral branching contexts managed by the system)

### 4.2 Language Detection

Language detection works in three progressive scan depths:

**Surface Scan (~2 seconds):**
- Walks depth 2 to find manifest files
- Counts file extensions at depth 2
- Requires >2 files of a given extension to classify
- Detects 35+ language/framework combinations

**Deep Scan (~30 seconds):**
- Includes everything from surface scan
- Detects architecture patterns
- Extracts conventions from config files
- Counts extensions at depth 3

**Full Scan (minutes):**
- Includes everything from deep scan
- Detects entry points
- Samples up to 200 source files across depth 5
- Extracts import patterns from first 50 lines of each file
- Generates conventions about frequently-imported modules

### 4.3 Framework Detection

Detection works by parsing manifest file contents for known dependency names.

**Languages and their manifest files:**

| Language | Manifest File | Example Frameworks Detected |
|----------|--------------|---------------------------|
| JavaScript/TypeScript | package.json | React, Next.js, Vue, Nuxt, Svelte, Angular, Express, Fastify, Nest, Remix, Astro, Tauri, Electron, Solid, Qwik |
| Rust | Cargo.toml | Actix-web, Axum, Rocket, Tauri, Tokio, Hyper, Diesel, SQLx, Leptos, Yew, Bevy |
| Python | requirements.txt/setup.py | Django, Flask, FastAPI, Tornado, Streamlit, TensorFlow, PyTorch, Pandas |
| Go | go.mod | Gin, Echo, Fiber, Chi, Hugo, gRPC, GORM, Cobra |
| Ruby | Gemfile | Rails, Sinatra, Jekyll, RSpec, Sidekiq |
| Java | pom.xml | Spring Boot, Quarkus, Micronaut, Hibernate, JUnit |
| PHP | composer.json | Laravel, Symfony, WordPress, Drupal |
| Elixir, Scala, Swift, C++, C# | Respective build files | Various framework detection |

### 4.4 Architecture Pattern Detection

| Pattern | Detection Criteria |
|---------|--------------------|
| Monorepo | Presence of packages/, apps/, or workspace config files |
| MVC | Presence of controllers/ + models/ directories |
| Next.js App Router | app/ directory with page/layout files |
| Next.js Pages Router | pages/ directory with index file |
| Tauri App | src-tauri/ directory |
| Rust Binary/Library/Mixed | Presence of main.rs vs lib.rs |
| Standard src-layout | src/ with common subdirectories (api, services, models, utils, components, hooks) |

### 4.5 Convention Extraction

The scanner extracts coding conventions from configuration files:

| Config File | Conventions Extracted |
|-------------|---------------------|
| .prettierrc / .editorconfig | Indent style (tabs/spaces), size, semicolons, quote style |
| tsconfig.json | Strict mode, path aliases |
| ESLint config | Linting presence |
| Cargo.toml | Rust edition, custom lints |
| package.json | Test framework (vitest/jest/mocha), build/lint scripts |
| Dockerfile / docker-compose | Docker usage |
| CI/CD configs | Platform detection (GitHub Actions, GitLab CI) |

### 4.6 Context Map Building

The context assembly mechanism builds an AI-consumable document for each session:

**Assembly process:**
1. Fetch all projects attached to the session from database
2. Collect context pins, memory entries, error resolutions, and conventions
3. Estimate token count (~4 characters ≈ 1 token)
4. Apply token budget trimming if necessary
5. Format as markdown document
6. Write atomically via temp file + rename pattern
7. Increment context version

**Token Budget System:**
- Default budget: 4,000 tokens (overridable per-project via configuration)
- Per-element estimates: project names ~1-5 tokens, conventions ~1 token each, pin file content up to ~2,000 tokens per file (8KB limit), memory entries ~3 tokens each
- If budget exceeded with multiple projects: removes conventions from secondary projects first (keeping minimum 2)
- Budget and estimated count returned with context for UI display

**Context file format:** Markdown with sections for projects, pinned context, memory, known error resolutions, and summary. Project-scoped pins distinguished from session-scoped pins. Large files truncated with notification.

### 4.7 Project Configuration File

Projects can include a `.hermes/context.json` configuration file that provides:
- Custom pins (files to always include in context)
- Memory entries (persistent facts about the project)
- Coding conventions (higher priority than auto-detected)
- Token budget override
- Change detection via content hash

---

## 5. Git Integration

### 5.1 Supported Operations

| Category | Operations |
|----------|-----------|
| **Status** | Current branch, remote tracking, ahead/behind counts, staged/unstaged/untracked files, conflict detection |
| **Staging** | Add individual files, add all, unstage individual files |
| **Committing** | Create commits with optional author override, commit message |
| **Remote** | Push to origin, pull (fetch + merge/fast-forward), fetch remote branches |
| **Branching** | List branches (local + remote), create, checkout, delete, check ahead/behind |
| **Diff** | Unified diff for staged or unstaged files, truncated at 2MB for binary safety |
| **Stash** | Save (including untracked), apply, pop, drop, clear, list with messages |
| **Merge** | Detect merge state, resolve conflicts (ours/theirs/manual), abort merge, complete merge |
| **Log** | Paginated commit history (50 per page), commit detail with file changes |
| **File Ops** | Browse directories with Git status annotations, read file content with language detection |
| **Worktree** | Create, remove, list, check branch availability, get info, has-changes check |

### 5.2 Authentication

Authentication uses a hierarchical credential chain:

1. **SSH Agent** — Attempts once per operation
2. **SSH Key Files** — Checks standard locations (~/.ssh/id_ed25519, ~/.ssh/id_rsa)
3. **Git Credential Helper** — Supports Git Credential Manager (browser OAuth), GitHub CLI integration
4. **Environment Variables** — Checks for platform tokens as plaintext fallback
5. **Failure** — Returns detailed help text with remediation steps

### 5.3 Worktree Management

**Location:** Worktrees are stored outside the project root in `{app_data_dir}/hermes-worktrees/{repo_hash}/{session_prefix}_{branch}/`

**Creation modes:**
- New branch from current HEAD
- Existing local branch
- From remote branch (auto-creates tracking branch)
- Reuse detection: if branch already checked out elsewhere, returns existing path with shared flag

**Safety guards for removal:**
1. Path must exist under the worktrees directory
2. Path must not equal repository root (canonical comparison)
3. Path must not be ancestor of repository root
4. Removal sequence: git worktree remove → prune → filesystem cleanup → stale ref cleanup

**Worktree file watcher:**
- Monitors the worktrees directory for deletions
- Per-path debounce (500ms minimum between events)
- Emits `worktree-path-deleted` event with session and project context
- Never deletes database records directly — only notifies frontend

**Journal system:**
- Tab-separated log file per repository
- Records action start and completion for crash recovery
- On startup, scans for incomplete operations and replays them
- Actions tracked: creation, deletion, branch changes

### 5.4 Git Status Reporting

- Respects .gitignore (excludes ignored files)
- Handles detached HEAD, bare repos, merge-in-progress states
- Circuit breaker: stops at 10,000 files with warning about .gitignore misconfiguration
- Merges index (staged) and working tree (unstaged) changes into unified list
- File statuses: added, modified, deleted, renamed, copied, untracked, conflicted

### 5.5 Data Flowing Between Git and UI

**Frontend → Backend:** Operation requests with project ID, session ID, file paths, commit messages, branch names

**Backend → Frontend:** Status objects with branch info, file lists, diff content, operation results (success/failure with messages), worktree metadata

**Polling:** Frontend polls git status at configurable intervals (default 3 seconds) with a shared cache to deduplicate across multiple components

---

## 6. Tauri IPC Commands

### 6.1 Session Management

| Command | Parameters | Returns | Description |
|---------|-----------|---------|-------------|
| create_session | id?, label, cwd, color, provider, auto_approve, permission_mode, suffix, channels, ssh_info, dimensions | Session | Creates new PTY session |
| close_session | session_id | void | Terminates session |
| get_session | session_id | Session | Fetches session state |
| get_recent_sessions | — | SessionEntry[] | Recent session history |
| get_snapshot | session_id | string | Terminal scrollback content |
| resize_session | session_id, rows, cols | void | Resizes PTY |
| update_label | session_id, label | void | Renames session |
| update_description | session_id, description | void | Updates description |
| update_group | session_id, group | void | Changes group |
| update_color | session_id, color | void | Changes color |
| write_to_session | session_id, data | void | Sends input to PTY |
| save_snapshots | sessions[] | void | Batch save scrollback |
| is_shell_foreground | session_id | boolean | Checks if shell (not child) owns foreground |
| add_workspace_path | session_id, path | void | Adds extra workspace path |
| remove_workspace_path | session_id, path | void | Removes workspace path |

### 6.2 Git Operations

| Command | Parameters | Returns | Description |
|---------|-----------|---------|-------------|
| git_status | session_id | GitSessionStatus | Full status for all projects |
| git_stage | session_id, project_id, file_path | GitOperationResult | Stage file |
| git_unstage | session_id, project_id, file_path | GitOperationResult | Unstage file |
| git_discard | session_id, project_id, file_path | GitOperationResult | Discard changes |
| git_commit | session_id, project_id, message, author? | GitOperationResult | Create commit |
| git_push | session_id, project_id | GitOperationResult | Push to remote |
| git_pull | session_id, project_id | GitOperationResult | Pull from remote |
| git_diff | session_id, project_id, file_path, staged | GitDiff | Get unified diff |
| git_list_branches | session_id, project_id | GitBranch[] | List all branches |
| git_create_branch | session_id, project_id, name, base? | GitOperationResult | Create branch |
| git_checkout_branch | session_id, project_id, name | GitOperationResult | Switch branch |
| git_delete_branch | session_id, project_id, name, force? | GitOperationResult | Delete branch |
| git_fetch_remote | session_id, project_id | GitOperationResult | Fetch from remote |
| git_branches_ahead_behind | session_id, project_id | AheadBehind[] | Tracking info |
| git_log | session_id, project_id, page | Commit[] | Paginated history |
| git_commit_detail | session_id, project_id, sha | CommitDetail | Full commit info |
| git_stash_save | session_id, project_id | GitOperationResult | Stash changes |
| git_stash_apply | session_id, project_id, index | GitOperationResult | Apply stash |
| git_stash_pop | session_id, project_id, index | GitOperationResult | Pop stash |
| git_stash_drop | session_id, project_id, index | GitOperationResult | Drop stash |
| git_stash_clear | session_id, project_id | GitOperationResult | Clear all stashes |
| git_merge_status | session_id, project_id | MergeStatus | Check merge state |
| git_resolve_conflict | session_id, project_id, file, strategy | GitOperationResult | Resolve conflict |
| git_abort_merge | session_id, project_id | GitOperationResult | Abort merge |
| git_continue_merge | session_id, project_id, message | GitOperationResult | Complete merge |
| worktree_create | session_id, project_id, branch, from_remote? | WorktreeResult | Create worktree |
| worktree_remove | session_id, project_id, path | GitOperationResult | Remove worktree |
| worktree_list | project_id | WorktreeInfo[] | List worktrees |
| worktree_check_available | project_id, branch, exclude? | boolean | Check branch availability |
| worktree_get_info | path | WorktreeInfo | Get worktree details |
| worktree_has_changes | path | boolean | Check for uncommitted changes |
| worktree_stash | session_id, project_id | GitOperationResult | Stash worktree changes |
| worktree_detect_orphans | — | OrphanInfo[] | Find orphaned worktrees |
| worktree_disk_usage | path | number | Calculate size |
| worktree_cleanup | paths[] | CleanupResult[] | Batch remove orphans |

### 6.3 SSH / Remote Operations

| Command | Parameters | Returns | Description |
|---------|-----------|---------|-------------|
| ssh_list_directory | session_id, path | FileEntry[] | List remote dir |
| ssh_read_file | session_id, path | FileContent | Read remote file |
| ssh_write_file | session_id, path, content | void | Write remote file |
| ssh_list_tmux_sessions | session_id | TmuxSession[] | List tmux sessions |
| ssh_list_tmux_windows | session_id, session_name | TmuxWindow[] | List tmux windows |
| ssh_tmux_select_window | session_id, session_name, index | void | Switch tmux window |
| ssh_tmux_new_window | session_id, session_name | void | Create tmux window |
| ssh_tmux_rename_window | session_id, session_name, index, name | void | Rename tmux window |
| ssh_add_port_forward | session_id, local_port, remote_host, remote_port, label? | void | Add port forward |
| ssh_remove_port_forward | session_id, local_port | void | Remove port forward |
| ssh_list_port_forwards | session_id | PortForward[] | List forwards |
| list_ssh_hosts | — | SshHost[] | Saved SSH hosts |
| upsert_ssh_host | host_data | void | Save SSH host |
| delete_ssh_host | host_id | void | Delete SSH host |

### 6.4 Project & Context

| Command | Parameters | Returns | Description |
|---------|-----------|---------|-------------|
| create_project | path, name | Project | Register project |
| get_registered_projects | — | Project[] | All projects |
| get_projects_ordered | — | ProjectExtended[] | Projects sorted by usage |
| get_project | project_id | Project | Single project |
| delete_project | project_id | void | Remove project |
| attach_session_project | session_id, project_id, role? | void | Attach project to session |
| detach_session_project | session_id, project_id | void | Detach project |
| get_session_projects | session_id | Project[] | Projects for session |
| scan_project | project_id, depth | void | Trigger scan |
| assemble_session_context | session_id, budget? | ContextInfo | Build context object |
| apply_context | session_id, execution_mode | ApplyResult | Write & send context |
| fork_session_context | source_id, target_id | void | Copy pins for new session |
| get_context_pins | session_id | Pin[] | List pins |
| add_context_pin | session_id, pin | void | Add pin |
| remove_context_pin | pin_id | void | Remove pin |
| load_hermes_config | project_id | HermesConfig | Load project config |
| detect_project | path | Project? | Find project root |
| scan_directory | path, max_depth? | Project[] | Discover projects in dir |
| nudge_project_context | session_id | void | Signal context update to AI |

### 6.5 Memory & Intelligence

| Command | Parameters | Returns | Description |
|---------|-----------|---------|-------------|
| save_memory | key, value, scope, source, confidence | void | Store fact |
| get_all_memory | scope? | MemoryEntry[] | Retrieve facts |
| delete_memory | key, scope | void | Remove fact |
| detect_shell_environment | session_id | ShellEnv | Detect shell config |
| read_shell_history | session_id | string[] | Read history file |
| get_session_commands | session_id | Command[] | Session command log |
| get_project_context | session_id | ProjectContext | Project metadata |

### 6.6 Process Management

| Command | Parameters | Returns | Description |
|---------|-----------|---------|-------------|
| list_processes | — | ProcessSnapshot | All processes |
| get_process_detail | pid | ProcessInfo | Single process |
| kill_process | pid, signal | void | Send signal |
| kill_process_tree | pid | void | Kill process and descendants |
| reveal_in_finder | path | void | Open in file manager |

### 6.7 Settings & Configuration

| Command | Parameters | Returns | Description |
|---------|-----------|---------|-------------|
| get_all_settings | — | Record | All settings |
| set_setting | key, value | void | Update setting |
| export_settings | path | void | Export to file |
| import_settings | path | void | Import from file |
| check_ai_providers | — | Record | Provider availability |

### 6.8 Costs

| Command | Parameters | Returns | Description |
|---------|-----------|---------|-------------|
| get_cost_history | days | CostEntry[] | Daily cost data |
| get_cost_by_project | days | ProjectCost[] | Per-project costs |

### 6.9 Other

| Command | Parameters | Returns | Description |
|---------|-----------|---------|-------------|
| show_context_menu | items[] | void | Show native context menu |
| update_menu_state | item_id, state | void | Update menu checkbox/enabled |
| copy_image_to_clipboard | path | void | Copy image file |
| export_prompt_bundle | data, path | void | Export prompt template |
| import_prompt_bundle | path | PromptBundle | Import prompt template |
| start_transcript_watcher | session_id | watcher_id | Monitor AI transcript |
| stop_transcript_watcher | watcher_id | void | Stop monitoring |
| plugin_fetch_url | url, headers, plugin_id | Response | Plugin HTTP GET |
| plugin_post_json | url, body, headers, plugin_id | Response | Plugin HTTP POST |
| plugin_exec_command | command, args, plugin_id | ExecResult | Plugin shell exec |

---

## 7. Frontend Architecture

### 7.1 Component Hierarchy

```
<App>
  ├── <Topbar> (window title, traffic light spacer on macOS)
  ├── <Sidebar>
  │   ├── <SessionList> (sessions with git info, colors, drag source)
  │   ├── <ActivityBar> (vertical tab strip, reorderable)
  │   └── Panel content (switched by active tab):
  │       ├── <GitPanel> (changes/worktrees views)
  │       ├── <FileExplorerPanel> (file tree, SSH remote)
  │       ├── <SearchPanel> (search with regex/case toggles)
  │       ├── <PluginManager> (installed/browse tabs)
  │       └── Plugin panels (custom left-side panels)
  ├── <SplitLayout> (recursive binary tree)
  │   ├── <SplitPane> (terminal wrapper)
  │   │   ├── Header (label, phase, close, session tabs)
  │   │   ├── <ScopeBar> (attached projects, branches)
  │   │   ├── <ProviderActionsBar> (compose, quick actions)
  │   │   ├── <TerminalPane> (xterm.js viewport)
  │   │   │   ├── <SuggestionOverlay> (command suggestions)
  │   │   │   └── <BranchMismatchAlert> (worktree collision)
  │   │   └── Drop zones (for drag-drop split)
  │   └── <SplitDivider> (resize handle)
  ├── <ContextPanel> (right side, collapsible)
  │   ├── Projects section
  │   ├── Workspace section
  │   ├── Files section (with pin buttons)
  │   ├── Memories section (key-value with CRUD)
  │   ├── Agent timeline (tool execution dots)
  │   ├── Agent stats (histogram bars, cost sparkline)
  │   ├── <ContextStatusBar> (sync state, token budget meter)
  │   └── <ContextPreview> (formatted markdown preview)
  ├── <StatusBar> (bottom)
  │   ├── Session count, execution mode toggle
  │   ├── Detected AI agent and model
  │   ├── Token count, cost, elapsed time
  │   ├── Working directory, theme picker
  │   └── Update indicator, bug report, shortcuts
  ├── Plugin bottom panels (resizable)
  └── Overlays:
      ├── <Settings> (multi-tab)
      ├── <CommandPalette> (search + actions)
      ├── <PromptComposer> (multi-part prompt builder)
      ├── <SessionCreator> (multi-step wizard)
      ├── <OnboardingWizard> (first-launch)
      ├── <ProjectPicker> (project attachment)
      ├── <WorkspacePanel> (project discovery)
      ├── <CostDashboard> (cost analytics)
      ├── <ShortcutsPanel> (keyboard reference)
      ├── <WhatsNewDialog> (changelog on update)
      ├── <UpdateDialog> (download/install)
      ├── <CloseSessionDialog> (confirmation)
      ├── <DirtyWorktreeDialog> (stash/discard)
      └── <BranchConflictDialog> (branch collision)
```

### 7.2 State Management

The application uses React Context with a reducer pattern as its primary state management approach.

**Session State structure:**
- `sessions`: Map of session ID → session data
- `activeSessionId`: Currently focused session
- `recentSessions`: History entries for workspace restore
- `defaultMode`: Default execution mode (manual/assisted/autonomous)
- `executionModes`: Per-session mode overrides
- `autonomousSettings`: Command frequency threshold, cancel delay
- `autoApplyEnabled`: Auto-apply context on changes
- `injectionLocks`: Per-session context injection guards
- `layout`: Binary tree of split panes with focused pane tracking
- `pendingCloseSessionId`: Session awaiting close confirmation
- `ui`: Panel visibility flags (context, sidebar, palette, process, git, file explorer, search, composer, flow mode, active left tab, file preview)

**Reducer action categories:**
- Session: update, remove, set active, set recent
- UI toggles: context, sidebar, palette, process panel, git panel, file explorer, search panel, left tab, subview
- Layout: init pane, split, close, focus, resize, set session, restore
- Execution: set mode, set default mode, toggle flow mode, auto-toast, auto-apply, autonomous settings, injection locks
- Close flow: request close, cancel close, skip confirm

**Derived hooks (memoized selectors):**
- Active session, session list, sidebar-ordered sessions (grouped, destroyed last)
- Total cost, total tokens (aggregated across sessions)
- Per-session execution mode, autonomous settings

### 7.3 Terminal Rendering

- **xterm.js** with WebGL renderer addon for hardware-accelerated rendering
- Base64-encoded byte transfer from backend to frontend
- User scroll position preserved during streaming output
- Scrollback restoration from saved snapshots using styled text
- Dead key composition fix for macOS WKWebView (blocks duplicate composition events)

**Key event interception:**
- Ctrl+C sent as raw byte (0x03) to bypass WebView menu consumption
- Shift+Enter generates CSI u sequence for AI tool distinction
- Cmd+Left/Right → Home/End mappings on macOS

**Copy operation:** Joins xterm soft-wrapped lines and trims trailing whitespace per real line.

### 7.4 Split Pane System

The layout is modeled as a binary tree:

- **Leaf nodes**: Terminal panes displaying a session
- **Split nodes**: Containers with direction (horizontal/vertical) and split ratio (clamped 0.15–0.85)
- **Counter-based ID generation** for unique node identification

**Tree operations (pure/immutable):**
- Replace node by ID
- Remove pane and promote sibling
- Collect panes in visual order
- Find pane by session or by ID
- Update split ratios
- Update pane's displayed session
- Remove all panes displaying a given session

**Layout rendering:** Recursive flex layout — first child gets fixed ratio, second child gets remaining space. 3px dividers between children.

**Drag-drop:** Sessions can be dragged from sidebar onto pane drop zones (left/right/top/bottom/center). Drop zone computed by mouse quadrant position (25% edges).

### 7.5 Command Palette

Modal overlay with search-as-you-type and keyboard navigation:

**Command categories:** View, Session, Settings, Navigation
**Features:**
- Hierarchical commands grouped by category
- Keyboard shortcuts displayed next to each command
- Real-time filtering
- Arrow key navigation, Enter to execute, Escape to close
- Session switching via Mod+1 through Mod+9
- Hidden commands appear only when searched (e.g., settings tab shortcuts)
- Plugin commands integrated

### 7.6 Prompt Composer

Multi-part prompt building interface:

**Composition fields:** Task, scope, constraints, roles (multi-select), styles (multi-select with intensity levels 1-5)

**Template system:**
- 100+ built-in templates across 18 categories (debugging, refactoring, performance, security, testing, architecture, documentation, git review, DevOps, etc.)
- User-saved templates with version migration
- Template groups (user-created categories)
- Pinning for quick access
- Import/export as JSON bundles

**Role system:** 23 built-in specialist roles (backend engineer, frontend engineer, debugger, security auditor, etc.) with system instructions. Role merging produces combined instructions.

**Style system:** 14 built-in style modifiers (concise, detailed, code-heavy, step-by-step, diff-format, formal, casual, etc.), each with 5 intensity levels.

**Compilation:** Roles and styles are merged into structured markdown before sending to the terminal.

### 7.7 Suggestion Engine

The terminal intelligence system provides command suggestions:

**Three-stage pipeline:**
1. **History matches** — Prefix-based lookup from session command history and shell history file
2. **Static index matches** — Multi-token or single-token prefix matching against a pre-built catalog of 1200+ commands (git, npm, cargo, docker, kubernetes, terraform, etc.)
3. **Score, deduplicate, rank** — Cap at 15 results

**Scoring algorithm:**
- History matches: base score +200 (session) or +300 (shell)
- Frequency boost: up to +200 based on occurrence count
- Recency boost: up to +100 based on position in history
- Static index matches: base +100
- Context relevance bonus: +150 if command category matches detected project type
- Exact prefix bonus: +100
- Length penalty for very long commands
- Multi-source bonus: +50 when same command appears in multiple sources

**Triggering conditions:**
- 50ms debounce after keystroke
- Session phase must be idle or shell_ready
- Shell must own foreground (OS-level check, cached at 300ms)
- Not in alternate screen buffer (vim, etc.)
- User must not have scrolled up

**Display:**
- Ghost text: faded inline completion at cursor position (opacity 0.4)
- Suggestion overlay: positioned list below cursor, flips above if near bottom
- First match highlighted with description
- Right arrow accepts ghost text, Tab accepts overlay selection, Enter executes

**Intent commands (colon-prefixed):**
- `:test` → detects and runs project test runner
- `:status` → git status
- `:log` → git log (recent 15 commits)
- Partial matching on `:` prefix filters suggestions

### 7.8 File Editor

Full-featured code editor built on CodeMirror 6:
- Syntax highlighting for 100+ file extensions
- Line numbers, bracket matching, code folding
- Search/replace with regex and case-sensitivity
- Go-to-line, undo/redo, line operations
- Comment toggling, bracket navigation
- Configurable indentation (tabs or spaces)
- Optional minimap (monochrome bars + syntax-colored tokens + viewport indicator)
- Auto-save with configurable delay
- Both local and SSH file editing
- Dynamic theme from CSS custom properties

---

## 8. AI Integration

### 8.1 Supported AI Providers

| Provider | Launch Command Behavior | Permission Modes |
|----------|------------------------|------------------|
| Claude Code | Launched with permission-mode flag | acceptEdits, plan, auto, dontAsk, bypassPermissions |
| Aider | Launched with auto-approve flags | yes, yes-always |
| Codex | Launched with approval bypass flag | full-auto, bypass-permissions |
| Gemini | Launched with auto-approve flag | yolo |
| GitHub Copilot | Launched via CLI | (not specified) |

### 8.2 Agent Detection

The system automatically detects which AI agent is running in a terminal by analyzing output:
- Startup/identification line patterns unique to each provider
- Version strings, model names, configuration displays
- Confidence scores assigned to detections

### 8.3 Ghost Text / Suggestions

(See Section 7.7 above for the suggestion engine details)

**Ghost text overlay behavior:**
- Renders as faded text at cursor position using terminal cell dimensions for pixel-accurate placement
- Suppressed if shell has native autosuggest capability and intelligence mode is "augment"
- Cleared on any printable keystroke, backspace, paste, or overlay interaction

### 8.4 Context Injection

**Lifecycle:**
1. Shell becomes ready (first prompt detected)
2. AI agent is detected
3. System skips first prompt (agent still rendering startup)
4. On second prompt (agent truly idle), injects context reference
5. Context formatted based on detected agent type:
   - Claude: natural language instruction to read the context file
   - Aider: uses read command
   - Copilot: uses workspace reference
6. Deduplication via version tracking (prevents re-injection of same content)
7. If agent is busy when update arrives, nudge is deferred and delivered when agent next becomes idle

**Context lifecycle states:** Clean → Dirty (pins/memory changed) → Applying → Clean (success) or ApplyFailed (error)

### 8.5 Token Tracking

Per-provider token extraction via regex patterns:
- Input/output token counts with K/M/B/T suffix parsing
- Cost estimation based on provider/model and token counts
- Cumulative vs. incremental detection
- History maintained for sparkline visualization (30 samples)

### 8.6 Execution Modes

| Mode | Behavior |
|------|----------|
| Manual | User controls everything; suggestions shown but not auto-executed |
| Assisted | Command predictions shown as ghost text; accepted manually |
| Autonomous | Commands may auto-execute with countdown toast (configurable delay); user can cancel |

### 8.7 Transcript Monitoring

For Claude Code sessions, the system can watch the AI agent's transcript file (JSONL format):

**Events parsed:**
- Tool use starts (tool name, arguments)
- Text responses
- Thinking blocks
- Tool results
- Turn completion events

**Watcher behavior:**
- Polls file every 500ms for new lines
- Starts at end-of-file (only new events, not history)
- Background thread per watcher
- Clean shutdown via atomic stop flag

### 8.8 Error Pattern Matching

The system tracks error patterns:
- Fingerprints error messages
- Counts occurrences
- Tracks resolutions and verification status
- Known resolutions included in AI context for future reference

### 8.9 Stuck Detection

- Stuck score maintained per session
- Silence detection checks every 2 seconds
- Fallback state transitions if prompt patterns don't match
- Latency percentile tracking (p50, p95) for response time awareness

---

## 9. Plugin System

### 9.1 Plugin Directory Structure

Plugins live in `{app_data_dir}/plugins/{plugin-id}/` with:
- `hermes-plugin.json` — Manifest file
- `dist/index.js` — IIFE JavaScript bundle

### 9.2 Plugin Discovery & Installation

**Discovery:**
- Scans plugins directory for subdirectories with valid manifests
- Checks against disabled plugin list in database
- Registry fetched from remote URL for browsable plugins

**Installation:**
- Downloads .tgz archive from URL
- Extracts to temporary directory
- Validates manifest (checks for path traversal characters in plugin ID)
- Moves to final location
- Tamper protection: verifies plugin only registers under its own ID

**Update checking:**
- Configurable frequency: startup, daily, weekly, never
- Compares installed vs. registry versions via semver
- Auto-installs default plugins if missing
- Optional auto-update support
- Per-version ignore list

### 9.3 Plugin Lifecycle

**State machine:** Registered → Activating → Active (success) or Error (failure) → Inactive (deactivated)

**Rollback:** On partial activation failure, cleans up commands and panels registered before the error.

**Activation events:**
- `onStartup` — Auto-activate on app launch
- `onCommand` — Activate when specific command executed
- `onView` — Activate when panel shown

### 9.4 Plugin Manifest Schema

**Contributions a plugin can declare:**
- **Commands:** Handler function, title, category, keybinding
- **Panels:** React component, position (left/right/bottom), icon (SVG)
- **Session actions:** Icon button linked to panel
- **Status bar items:** Text with optional command trigger
- **Settings:** Typed configuration schema (string, number, boolean, select)

### 9.5 Plugin API Surface

**UI capabilities:**
- Register/show/hide/toggle panels
- Toast notifications
- Status bar updates
- Session action badges
- File handlers (override default file rendering)

**Commands:** Register and execute with namespaced identifiers

**Clipboard:** Read/write text

**Storage:** Plugin-private key-value store

**Settings:** Typed get/update/onChange with schema validation

**Events subscribed:**
- `theme.changed`, `session.created`, `session.closed`
- `session.phase_changed`, `session.focus_changed`
- `window.focused`, `window.blurred`

**Notifications:** System notifications

**Network:** HTTP GET and POST with headers (requires `network` permission)

**Shell:** Execute commands with stdout/stderr capture (requires `shell.exec` permission)

**Sessions:** Query active session and list all sessions, focus session by ID

**Agents:** Watch AI agent transcript events in real-time

### 9.6 Plugin Permissions

| Permission | Capabilities |
|-----------|-------------|
| clipboard.read | Read from system clipboard |
| clipboard.write | Write to system clipboard |
| storage | Key-value persistent storage |
| terminal.read | Read terminal output |
| terminal.write | Write to terminal |
| sessions.read | Query session info |
| notifications | Send system notifications |
| network | HTTP GET/POST requests |
| shell.exec | Execute system commands |

Permissions are declared in manifest, auto-granted for `storage` if settings schema exists, persisted to database, and enforced at backend level.

### 9.7 Plugin Event Bus

Simple publish-subscribe system:
- Register listeners per event type
- Emit calls all listeners (errors swallowed)
- Returns disposable for unsubscription
- Supports removeAllListeners for cleanup

### 9.8 Plugin SDK

A separate package provides TypeScript types for plugin development:
- Re-exports all manifest and API types
- `definePlugin()` helper with type inference for activation/deactivation
- CodeMirror modules exposed globally for plugins to use syntax highlighting without bundling duplicates

---

## 10. Platform Handling

### 10.1 macOS-Specific

| Feature | Behavior |
|---------|----------|
| Process spawning | POSIX spawn with helper binary trampoline for controlling terminal |
| Window chrome | Overlay title bar, traffic light buttons, hidden title |
| Shell integration | Temporary ZDOTDIR redirection for zsh |
| Send Interrupt | Native menu intercepts Ctrl+C, emits as event |
| File manager | `open -R` to reveal in Finder |
| Dead key composition | Custom handler blocks duplicate WKWebView events |
| PATH detection | Login shell spawned to source user profile (GUI apps get minimal PATH) |
| Keyboard shortcuts | Command (⌘) key used as modifier |
| Services menu | Standard macOS services submenu |
| Minimum version | macOS 13.0 |
| Private API | macOS private API enabled for enhanced window management |

### 10.2 Linux-Specific

| Feature | Behavior |
|---------|----------|
| Process spawning | Standard portable-pty spawn |
| File manager | `xdg-open` on parent directory |
| Default shell | Falls back to `/bin/bash` |
| Dependencies | WebKitGTK, GTK3, libsoup3, JavaScriptCoreGTK |
| Keyboard shortcuts | Ctrl key used as modifier |

### 10.3 Windows-Specific

| Feature | Behavior |
|---------|----------|
| Process spawning | Standard portable-pty spawn |
| Shell detection | PowerShell (pwsh/powershell), cmd.exe, or COMSPEC |
| File manager | `explorer /select,` with shell metacharacter validation |
| Process kill | `taskkill /F` instead of signals |
| Keyboard shortcuts | Ctrl key used as modifier |
| Installer | NSIS installer with icon |

### 10.4 Cross-Platform

| Feature | Details |
|---------|---------|
| Command existence check | `which` (Unix) vs `where` (Windows) |
| Protected process lists | Per-platform lists of system processes that cannot be killed |
| Protected PID threshold | Root processes with PID < 200 protected |
| Environment variables | TERM=xterm-256color, COLORTERM=truecolor, UTF-8 locale |
| Image clipboard | Platform-native clipboard APIs (NSPasteboard/WinAPI/X11-Wayland) |
| Terminal themes | 30+ themes with ANSI color mapping, 5 font families |
| UI scale | Configurable 0.9–1.5× multiplier |
| Window state | Dimensions and position saved/restored with 500ms debounce |

---

## 11. Workspace Detection

### 11.1 Project Registry

The database maintains a persistent registry of discovered projects:
- Path, name, detected languages, frameworks
- Scan status and timestamps
- Usage metrics (session count, last opened)
- Existence check on disk (sorted with existing first)

### 11.2 Session-Project Attachment

- Sessions can attach multiple projects with optional role designation
- Attachment triggers context file generation
- Detachment updates context file
- Events emitted on attachment/detachment changes
- Context can be forked from one session to another (for cloning sessions)

### 11.3 Auto-Detection

When a session's working directory changes (detected via OSC 7):
1. Frontend receives `cwd-changed` event
2. Attempts to detect project at new path
3. If project found and not already attached, auto-attaches it
4. Context is invalidated and can be re-applied

### 11.4 Workspace Save/Restore

**Auto-save loop (every 10 seconds when dirty):**
1. Collect live sessions with metadata
2. Save terminal scrollback snapshots
3. Serialize layout tree
4. Persist workspace JSON

**Restore on cold start:**
1. Load settings and apply theme
2. Check for live sessions (hot reload case)
3. If no live sessions, attempt workspace restore (if enabled)
4. For each saved session: generate new ID, create terminal, recreate with old metadata
5. Remap layout tree IDs to new session IDs
6. Restore scrollback from snapshots
7. Clear saved workspace only after full success

**Save triggers:** Session updates, layout changes, pane operations set a dirty flag that the auto-save loop checks.

### 11.5 Startup Sequence

1. Install crash handler (writes panics to log file)
2. Create async runtime context
3. Initialize application data and context directories
4. Check for dirty shutdown marker (previous crash detection)
5. Initialize database with migrations (including name migration from older versions)
6. Clean up stale worktrees: remove orphans, validate paths, replay incomplete journal operations
7. Clean up stale shell integration files
8. Start worktree file watcher
9. Initialize system information baseline
10. Register application state (database, PTY manager, system monitor, crash marker, watcher)
11. Install window close handler for workspace save
12. Build and set native menu bar
13. Initialize Tauri plugins (shell, notification, dialog, updater, process, analytics)

### 11.6 Shutdown Sequence

1. Window close event intercepted
2. Workspace save executed (guarded by atomic flag for single execution)
3. For each active session: save metadata, save scrollback, record token usage
4. Remove dirty shutdown marker (signals clean exit)
5. Application exits

### 11.7 Crash Recovery

- Dirty shutdown marker written on startup, removed on clean shutdown
- If marker present on next startup, previous session was abnormal
- Worktree journal replayed for incomplete operations
- Orphaned worktree directories detected and offered for cleanup

---

## Appendix A: Native Menu Structure

### Application Menu (macOS only as system menu)
- About
- Settings (Mod+,)
- Services submenu
- Hide / Hide Others / Show All
- Quit (Mod+Q)

### File Menu
- New Session (Mod+N)
- New Tab (Mod+T)
- Close Pane (Mod+W)
- File Explorer (Mod+F)

### Edit Menu
- Undo, Redo, Cut, Copy, Paste, Select All
- Find (Mod+K)
- Send Interrupt (Ctrl+C, macOS only)

### View Menu
- Sidebar (Mod+B, toggleable)
- Command Palette (Mod+K)
- Prompt Composer (Mod+J)
- Process Panel (Mod+P, toggleable)
- Git Panel (Mod+G, toggleable)
- Context Panel (Mod+E, toggleable)
- Search Panel (Mod+Shift+F, toggleable)
- Split: Right (Mod+D), Down (Mod+Shift+D)
- Flow Mode (Mod+Shift+Z, toggleable)
- Cost Dashboard (Mod+$)
- Keyboard Shortcuts (Mod+/)
- Fullscreen

### Session Menu
- Copy Context (Mod+Shift+C)

### Window Menu
- Minimize, Maximize

### Help Menu
- Check for Updates, Website, Privacy/Terms/License, Report Bug, Keyboard Shortcuts

---

## Appendix B: Event Catalog

### Backend → Frontend Events

| Event Name | Payload | Description |
|------------|---------|-------------|
| session-updated | Session state | Phase change, metrics update |
| session-removed | Session ID | Session terminated |
| cwd-changed-{id} | Path | Working directory changed |
| pty-exit-{id} | Exit status | PTY process terminated |
| command-prediction-{id} | Suggestions | Command autocomplete |
| transcript-event:{id} | Event data | AI transcript line |
| worktree-path-deleted | Session/project/branch | Worktree directory removed |
| worktree-cleanup-complete | Results | Batch cleanup finished |
| worktree-cleanup-failed | Error | Cleanup error |
| missing-worktree | Path info | Expected worktree not found |
| ai-launch-failed | Session ID, error | AI agent command not found |
| menu-action | Action ID | Native menu item clicked |
| native-sigint | — | Ctrl+C from native menu |
| session-projects-updated-{id} | — | Project attachment changed |
| project-updated | — | Project metadata changed |

### Frontend Custom Events

| Event Name | Description |
|------------|-------------|
| hermes:shared-worktree | Branch already checked out elsewhere |
| hermes:worktree-errors | Worktree creation failure |

---

## Appendix C: Settings Keys

### General
- Default shell, scrollback buffer size, default working directory
- Command palette shortcut binding, preferred external editor
- Session restore on/off, close confirmation on/off

### Appearance
- Theme name, UI scale factor, font size, font family
- Window width, window height, window position

### Intelligence
- Suggestion mode (augment/replace/off)
- Ghost text enabled/disabled
- Tab acceptance behavior

### Autonomous
- Command minimum frequency threshold
- Cancel delay (milliseconds)
- Permission mode per provider

### Git
- Poll interval (milliseconds, 0 to disable)

### Privacy
- Analytics opt-in/out
- Onboarding completed flag

### Plugin
- Update check frequency (startup/daily/weekly/never)
- Auto-update enabled/disabled
- Disabled plugin IDs
- Explicitly uninstalled default plugins

---

## Appendix D: AI Provider Token Patterns

The system recognizes multiple token reporting formats from different AI providers:

| Provider | Token Format Description |
|----------|------------------------|
| Claude | Input/output with optional cache read/write metrics and cost |
| Aider | Sent/received with cost per message and cumulative session cost |
| Codex | Total with input/output breakdown |
| Gemini | Stats table with per-column parsing |

Token counts support K (thousands), M (millions), B (billions), T (trillions) suffixes and comma separators.

Cost estimation uses provider and model identification to apply appropriate per-token pricing.

---

## Appendix E: Keyboard Shortcuts Reference

### General
| Shortcut | Action |
|----------|--------|
| Mod+N | New session |
| Mod+T | New tab |
| Mod+W | Close pane |
| Mod+, | Settings |
| Mod+K | Command palette / Find |
| Mod+Q | Quit |

### Panels
| Shortcut | Action |
|----------|--------|
| Mod+B | Toggle sidebar |
| Mod+E | Toggle context panel |
| Mod+G | Toggle git panel |
| Mod+P | Toggle process panel |
| Mod+F | Toggle file explorer |
| Mod+J | Toggle prompt composer |
| Mod+Shift+F | Toggle search panel |
| Mod+$ | Cost dashboard |
| Mod+/ | Keyboard shortcuts |
| Mod+Shift+Z | Toggle flow mode |

### Panes & Sessions
| Shortcut | Action |
|----------|--------|
| Mod+D | Split right |
| Mod+Shift+D | Split down |
| Mod+Alt+Arrow | Navigate between panes |
| Mod+1-9 | Switch to session by position |
| Mod+Shift+C | Copy context to clipboard |

---

*End of specification*
