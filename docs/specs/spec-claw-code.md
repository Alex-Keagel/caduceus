# Caduceus Behavioral Specification for the `claw-code` Rust Workspace

## Provenance
- **Source repository:** `~/caduceus-reference/claw-code/`
- **Commit SHA:** `a3d0c9e5e717a730960091ff78462c6fc3627405`
- **License basis:** MIT-licensed source repository
- **Clean-room statement:** This document carries forward behavior, interfaces, data contracts, state transitions, and externally observable semantics only. No copyrightable source expression was intentionally reproduced.

## Scope
This specification covers every Rust source file under `rust/crates/` for these crates:
- `api`
- `commands`
- `compat-harness`
- `mock-anthropic-service`
- `plugins`
- `runtime`
- `tools`
- `rusty-claude-cli`
- `telemetry`

---

## 1. Agent Harness / CLI (`rusty-claude-cli`)

### 1.1 Entry modes
The binary supports four user-facing operating modes:
1. **Interactive REPL** when no prompt is supplied and standard input is a terminal.
2. **One-shot prompt mode** when prompt text is supplied on the command line or piped through stdin.
3. **Resume mode** when a saved session is reopened and one or more resume-safe commands are executed non-interactively.
4. **Administrative fast paths** for status/help/version/system-prompt/init/export/manifest inspection and similar local actions.

### 1.2 Global CLI flags
Supported global flags include:
- `--help`, `-h`
- `--version`, `-V`
- `--model <name>`
- `--output-format text|json`
- `--permission-mode read-only|workspace-write|danger-full-access`
- `--dangerously-skip-permissions`
- `--compact`
- `--base-commit <ref>`
- `--reasoning-effort low|medium|high`
- `--allow-broad-cwd`
- `-p <prompt...>`
- `--print`
- `--resume [session]`
- `--allowedTools` / `--allowed-tools`

Resolution order for the active model is: explicit CLI flag, model environment/config aliases, then default model.

### 1.3 Default behavior
- If no positional action is given and stdin is non-interactive, the entire stdin payload becomes the prompt.
- Otherwise the CLI launches the REPL.
- Piped stdin is merged into a one-shot prompt only in fully unattended mode, because other permission modes reserve stdin for approval prompts.

### 1.4 Output formats
- **Text mode:** human-oriented terminal rendering with Markdown formatting, colors, tables, lists, syntax-highlighted code fences, and streaming spinner/status behavior.
- **JSON mode:** machine-oriented envelopes with stable top-level `kind`/shape expectations for core commands.
- **Compact mode:** prints only the final assistant text, with no tool telemetry, spinners, or framing.

### 1.5 Error behavior
- Normal errors are written to stderr as human-readable text.
- When JSON output is requested, failures are emitted as a JSON error object.
- Unknown option handling includes typo suggestions for known flags.

### 1.6 Conversation turn lifecycle
Each non-local conversation turn follows this sequence:
1. Enforce broad-working-directory policy unless explicitly bypassed.
2. Run stale-base preflight if a base commit was supplied or configured.
3. Build a runtime object containing model, system prompt, permission policy, tool registry, and session state.
4. Send the turn to the runtime conversation loop.
5. Stream or accumulate assistant output.
6. Persist the updated session.
7. Render either terminal text or a JSON summary.

### 1.7 REPL state machine
The interactive session can be modeled as:
- **Idle / awaiting input**
- **Slash-command dispatch** if input starts with `/`
- **Model turn executing** otherwise
- **Permission approval pending** when a tool requires user consent
- **Cancelled input** when Ctrl-C is pressed with buffered text
- **Exit requested** on EOF or Ctrl-C with empty input

History, slash-command completions, and multiline editing are built into the line editor.

### 1.8 Session-related user actions
The CLI exposes local behaviors for:
- showing session status
- listing/switching/forking/deleting sessions
- clearing the current session with backup
- exporting the conversation
- compacting the session history
- resuming a saved session by path or newest-alias

### 1.9 System prompt assembly entrypoint
The CLI always obtains its system prompt from the runtime prompt builder, then injects that prompt into every model request. A dedicated command prints the assembled prompt without contacting a model.

### 1.10 Rendering rules
Observable terminal rendering rules include:
- syntax-highlighted fenced code blocks
- colored heading levels
- distinct styling for emphasis, strong text, inline code, links, block quotes, task lists, ordered/unordered lists, and tables
- incremental Markdown streaming with buffering until complete renderable units are available
- a spinner for in-flight model/tool status

---

## 2. Slash Commands (`commands`)

### 2.1 Command registry model
Each registered slash command has:
- canonical name
- optional aliases
- summary text
- optional argument hint
- a flag indicating whether it may run through `--resume`

### 2.2 Parsing behavior
- Inputs not starting with `/` are not treated as slash commands.
- Missing command names produce a structured parse error with guidance to use `/help`.
- Some commands parse arguments into typed slots; others are accepted as stubs for future use.
- Unknown commands generate suggestion text based on fuzzy matching.

### 2.3 Commands with implemented behavior in this build
These commands have concrete local behavior:
- `/help`
- `/status`
- `/sandbox`
- `/compact`
- `/model [model]`
- `/permissions [mode]`
- `/clear [--confirm]`
- `/cost`
- `/resume <session>`
- `/config [section]`
- `/mcp [list|show <server>|help]`
- `/memory`
- `/init`
- `/diff`
- `/version`
- `/export [file]`
- `/session [list|switch <id>|fork [branch]|delete <id> [--force]]`
- `/plugin`, `/plugins`, `/marketplace`
- `/agents [list|help]`
- `/skills [list|install|help|<skill> [args]]`
- `/doctor`
- `/history [count]`
- `/stats`

Behavior highlights:
- `/skills <name> ...` can either perform local registry actions or transform into a model prompt that invokes a skill.
- `/agents` and `/skills` inspect disk-based registries from project and user roots.
- `/mcp` inspects merged MCP configuration, including per-server detail views.
- `/plugins` supports list/install/enable/disable/uninstall/update and returns whether runtime reload is needed.
- `/resume` outside the REPL loads a saved session and executes only commands marked resume-safe.

### 2.4 Registered-but-not-implemented commands in this build
Many additional commands are recognized, documented in help, and surfaced in completion, but currently answer with “not yet implemented in this build” when invoked interactively, or are marked interactive-only when invoked from direct CLI mode.

Registered names include:
`/bughunter`, `/commit`, `/pr`, `/issue`, `/ultraplan`, `/teleport`, `/debug-tool-call`, `/login`, `/logout`, `/plan`, `/review`, `/tasks`, `/theme`, `/vim`, `/voice`, `/upgrade`, `/usage`, `/rename`, `/copy`, `/share`, `/feedback`, `/hooks`, `/files`, `/context`, `/color`, `/effort`, `/fast`, `/exit`, `/branch`, `/rewind`, `/summary`, `/desktop`, `/ide`, `/tag`, `/brief`, `/advisor`, `/stickers`, `/insights`, `/thinkback`, `/release-notes`, `/security-review`, `/keybindings`, `/privacy-settings`, `/output-style`, `/add-dir`, `/allowed-tools`, `/api-key`, `/approve`, `/deny`, `/undo`, `/stop`, `/retry`, `/paste`, `/screenshot`, `/image`, `/terminal-setup`, `/search`, `/listen`, `/speak`, `/language`, `/profile`, `/max-tokens`, `/temperature`, `/system-prompt`, `/tool-details`, `/format`, `/pin`, `/unpin`, `/bookmarks`, `/workspace`, `/tokens`, `/cache`, `/providers`, `/notifications`, `/changelog`, `/test`, `/lint`, `/build`, `/run`, `/git`, `/stash`, `/blame`, `/log`, `/cron`, `/team`, `/benchmark`, `/migrate`, `/reset`, `/telemetry`, `/env`, `/project`, `/templates`, `/explain`, `/refactor`, `/docs`, `/fix`, `/perf`, `/chat`, `/focus`, `/unfocus`, `/web`, `/map`, `/symbols`, `/references`, `/definition`, `/hover`, `/diagnostics`, `/autofix`, `/multi`, `/macro`, `/alias`, `/parallel`, `/agent`, `/subagent`, `/reasoning`, `/budget`, `/rate-limit`, `/metrics`.

### 2.5 Help rendering
Help output is categorized into Session, Tools, Config, and Debug sections. It also annotates commands that are resume-safe.

---

## 3. Tool Registry (`tools`)

### 3.1 Registry model
The system exposes three tool layers:
1. **Built-in tools** compiled into the binary.
2. **Runtime tools** injected dynamically at startup.
3. **Plugin tools** discovered from installed plugins.

Name collisions are rejected:
- plugin tools cannot shadow built-ins
- runtime tools cannot shadow built-ins or plugins

### 3.2 Dispatch model
Tool execution works as follows:
1. Tool call name is matched against built-in/runtime/plugin registries.
2. Input JSON is deserialized into a typed input object.
3. If permission enforcement is active, the requested tool name plus serialized input are checked before execution.
4. Built-in handlers return pretty-printed JSON strings.
5. Plugin tools execute external commands and return the plugin’s stdout.
6. Failures become tool-result error payloads and are fed back into the conversation loop.

### 3.3 Tool results in the conversation loop
A successful assistant tool-use request produces:
- an assistant message containing one or more tool-use blocks
- one tool-result message per executed tool call
- optional hook-generated annotations merged into tool output
- a final assistant turn after the tool results are re-submitted to the model

### 3.4 Built-in tool catalog
Each built-in tool below lists externally visible description, input schema, output shape, and minimum permission level.

#### Shell / file / search tools
- **`bash`** — Execute a shell command in the current workspace.  
  **Input:** `command` required; optional `timeout`, `description`, `run_in_background`, `dangerouslyDisableSandbox`, `namespaceRestrictions`, `isolateNetwork`, `filesystemMode` (`off|workspace-only|allow-list`), `allowedMounts[]`.  
  **Output:** JSON object with `stdout`, `stderr`, `interrupted`, optional background process id, return-code interpretation, sandbox status, and flags indicating whether output was expected.  
  **Permission:** danger-full-access.

- **`read_file`** — Read a text file from the workspace.  
  **Input:** `path` required; optional `offset`, `limit`.  
  **Output:** JSON object containing file path, selected text, line counts, 1-based starting line, and total lines.  
  **Permission:** read-only.

- **`write_file`** — Write a text file in the workspace.  
  **Input:** `path`, `content`.  
  **Output:** JSON object describing create-vs-update, final content, original file contents when present, and a structured patch summary.  
  **Permission:** workspace-write.

- **`edit_file`** — Replace text in a workspace file.  
  **Input:** `path`, `old_string`, `new_string`; optional `replace_all`.  
  **Output:** JSON object containing original text, replacement text, structured patch, whether multiple replacements were requested, and whether the file had diverged.  
  **Permission:** workspace-write.

- **`glob_search`** — Find files by glob pattern.  
  **Input:** `pattern` required; optional `path`.  
  **Output:** JSON object with filenames, duration, count, and truncation flag.  
  **Permission:** read-only.

- **`grep_search`** — Search file contents with a regex pattern.  
  **Input:** `pattern` required; optional `path`, `glob`, `output_mode`, `-B`, `-A`, `-C`, `context`, `-n`, `-i`, `type`, `head_limit`, `offset`, `multiline`.  
  **Output:** JSON object containing mode, file list, optional concatenated content, match counts, and applied offset/limit metadata.  
  **Permission:** read-only.

#### Web and content tools
- **`WebFetch`** — Fetch a URL, normalize the content, and answer a fetch prompt.  
  **Input:** `url`, `prompt`.  
  **Output:** JSON object with final URL, status code and reason, byte count, duration, and a synthesized textual result.  
  **Permission:** read-only.

- **`WebSearch`** — Search the web for current information.  
  **Input:** `query` required; optional `allowed_domains[]`, `blocked_domains[]`.  
  **Output:** JSON object with the query, elapsed seconds, a commentary item instructing the model how to use the results, and up to eight deduplicated search hits.  
  **Permission:** read-only.

- **`NotebookEdit`** — Replace, insert, or delete a Jupyter notebook cell.  
  **Input:** `notebook_path` required; optional `cell_id`, `new_source`, `cell_type` (`code|markdown`), `edit_mode` (`replace|insert|delete`).  
  **Output:** JSON object with notebook path, cell id/type, edit mode, original and updated notebook text, and optional error field.  
  **Permission:** workspace-write.

- **`StructuredOutput`** — Return already-structured output to the caller.  
  **Input:** arbitrary object; must not be empty.  
  **Output:** JSON object with a generic success string plus the exact structured payload echoed back.  
  **Permission:** read-only.

- **`Sleep`** — Wait without holding a shell process.  
  **Input:** `duration_ms`.  
  **Output:** JSON object echoing the duration and a completion message.  
  **Permission:** read-only.

#### Session workflow tools
- **`TodoWrite`** — Update the session task list.  
  **Input:** `todos[]`, each containing `content`, `activeForm`, and `status` (`pending|in_progress|completed`).  
  **Output:** JSON object containing the previous todo list, new todo list, and an optional verification nudge flag when a large todo list was fully completed without mentioning verification.  
  **Permission:** workspace-write.

- **`Skill`** — Load a local skill definition.  
  **Input:** `skill` required; optional `args`.  
  **Output:** JSON object containing skill name, resolved file path, optional description, optional args, and the full skill prompt text.  
  **Permission:** read-only.

- **`Agent`** — Launch a specialized background sub-agent.  
  **Input:** `description`, `prompt`; optional `subagent_type`, `name`, `model`.  
  **Output:** JSON manifest containing agent id, normalized name, type, model, status, manifest/output file paths, timestamps, lane events, derived state, and optional error.  
  **Permission:** danger-full-access.

- **`ToolSearch`** — Search deferred/specialized tool definitions.  
  **Input:** `query` required; optional `max_results`.  
  **Output:** JSON object containing the original and normalized query, matching tool names, total deferred-tool count, and optional degraded-MCP report.  
  **Permission:** read-only.

- **`SendUserMessage`** — Emit a message to the user channel.  
  **Input:** `message`, `status` (`normal|proactive`); optional `attachments[]`.  
  **Output:** JSON object containing the message, resolved attachment metadata, and send timestamp.  
  **Permission:** read-only.

- **`AskUserQuestion`** — Ask a question and synchronously read stdin for the answer.  
  **Input:** `question` required; optional `options[]`.  
  **Output:** JSON object with `question`, chosen or typed `answer`, and `status: "answered"`.  
  **Permission:** read-only.

#### Configuration and planning tools
- **`Config`** — Get or set selected settings keys.  
  **Input:** `setting` required; optional `value` (string/boolean/number).  
  **Output:** JSON object containing success flag, get/set operation, setting name, current/previous/new value fields, and error text for unknown settings.  
  **Permission:** workspace-write.

- **`EnterPlanMode`** — Turn on a worktree-local planning override.  
  **Input:** empty object.  
  **Output:** JSON object describing whether plan mode is active, whether the tool is managing the override, previous/current local values, and the settings/state file paths used.  
  **Permission:** workspace-write.

- **`ExitPlanMode`** — Undo the worktree-local planning override.  
  **Input:** empty object.  
  **Output:** JSON object mirroring `EnterPlanMode`, with operation `exit`, including whether state was restored or stale state was merely cleared.  
  **Permission:** workspace-write.

#### Code execution tools
- **`REPL`** — Execute code in a language-specific subprocess.  
  **Input:** `code`, `language`; optional `timeout_ms`.  
  **Output:** JSON object with language, stdout, stderr, exit code, and elapsed time.  
  **Permission:** danger-full-access.

- **`PowerShell`** — Execute a PowerShell command.  
  **Input:** `command` required; optional `timeout`, `description`, `run_in_background`.  
  **Output:** Same shape as the shell tool, including optional background process id and timeout metadata.  
  **Permission:** danger-full-access.

#### Background task tools
- **`TaskCreate`** — Create a background task.  
  **Input:** `prompt` required; optional `description`.  
  **Output:** JSON object with task id, status, prompt, description, optional structured task packet, and creation time.  
  **Permission:** danger-full-access.

- **`RunTaskPacket`** — Create a background task from a structured packet.  
  **Input:** `objective`, `scope`, `repo`, `branch_policy`, `acceptance_tests[]`, `commit_policy`, `reporting_contract`, `escalation_policy`.  
  **Output:** Same shape as `TaskCreate`.  
  **Permission:** danger-full-access.

- **`TaskGet`** — Inspect a task by id.  
  **Input:** `task_id`.  
  **Output:** JSON object containing task metadata, timestamps, messages, and optional team assignment.  
  **Permission:** read-only.

- **`TaskList`** — List tasks.  
  **Input:** empty object.  
  **Output:** JSON object with `tasks[]` and `count`.  
  **Permission:** read-only.

- **`TaskStop`** — Stop a running task.  
  **Input:** `task_id`.  
  **Output:** JSON object with task id, new status, and a stop message.  
  **Permission:** danger-full-access.

- **`TaskUpdate`** — Send an update to a task.  
  **Input:** `task_id`, `message`.  
  **Output:** JSON object with task id, status, message count, and the last message text.  
  **Permission:** danger-full-access.

- **`TaskOutput`** — Retrieve task output.  
  **Input:** `task_id`.  
  **Output:** JSON object with output text and a boolean indicating whether any output exists.  
  **Permission:** read-only.

#### Worker / lane orchestration tools
- **`WorkerCreate`** — Create a coding worker boot session.  
  **Input:** `cwd` required; optional `trusted_roots[]`, `auto_recover_prompt_misdelivery`.  
  **Output:** serialized worker snapshot including trust/ready state and event history.  
  **Permission:** danger-full-access.

- **`WorkerGet`** — Inspect a worker.  
  **Input:** `worker_id`.  
  **Output:** serialized worker snapshot.  
  **Permission:** read-only.

- **`WorkerObserve`** — Feed terminal text into worker-boot detection.  
  **Input:** `worker_id`, `screen_text`.  
  **Output:** updated worker snapshot.  
  **Permission:** read-only.

- **`WorkerResolveTrust`** — Resolve a trust prompt.  
  **Input:** `worker_id`.  
  **Output:** updated worker snapshot.  
  **Permission:** danger-full-access.

- **`WorkerAwaitReady`** — Ask whether a worker is ready for prompt delivery.  
  **Input:** `worker_id`.  
  **Output:** JSON ready-state snapshot.  
  **Permission:** read-only.

- **`WorkerSendPrompt`** — Send a prompt once the worker is ready.  
  **Input:** `worker_id` required; optional `prompt`.  
  **Output:** updated worker snapshot.  
  **Permission:** danger-full-access.

- **`WorkerRestart`** — Restart worker boot state.  
  **Input:** `worker_id`.  
  **Output:** updated worker snapshot.  
  **Permission:** danger-full-access.

- **`WorkerTerminate`** — Terminate a worker.  
  **Input:** `worker_id`.  
  **Output:** updated worker snapshot.  
  **Permission:** danger-full-access.

- **`WorkerObserveCompletion`** — Report completion status back to a worker.  
  **Input:** `worker_id`, `finish_reason`, `tokens_output`.  
  **Output:** updated worker snapshot.  
  **Permission:** danger-full-access.

#### Team / cron tools
- **`TeamCreate`** — Create a team of tasks.  
  **Input:** `name`, `tasks[]` where each item has `prompt` and optional `description`.  
  **Output:** JSON object with team id, name, task count, task ids, status, and creation time.  
  **Permission:** danger-full-access.

- **`TeamDelete`** — Delete a team.  
  **Input:** `team_id`.  
  **Output:** JSON object with team id, name, status, and a deletion message.  
  **Permission:** danger-full-access.

- **`CronCreate`** — Create a recurring task.  
  **Input:** `schedule`, `prompt`; optional `description`.  
  **Output:** JSON object with cron id, schedule, prompt, description, enabled flag, and creation time.  
  **Permission:** danger-full-access.

- **`CronDelete`** — Delete a recurring task.  
  **Input:** `cron_id`.  
  **Output:** JSON object with cron id, schedule, status `deleted`, and a message.  
  **Permission:** danger-full-access.

- **`CronList`** — List recurring tasks.  
  **Input:** empty object.  
  **Output:** JSON object with `crons[]` and `count`.  
  **Permission:** read-only.

#### Code-intelligence and remote-integration tools
- **`LSP`** — Query code intelligence state.  
  **Input:** `action` (`symbols|references|diagnostics|definition|hover`) required; optional `path`, `line`, `character`, `query`.  
  **Output:** either a structured result object or `{action, error, status:"error"}`.  
  **Permission:** read-only.

- **`ListMcpResources`** — List resources from an MCP server.  
  **Input:** optional `server`.  
  **Output:** JSON object with server name, resource array, and count; errors return the server name plus an `error` string.  
  **Permission:** read-only.

- **`ReadMcpResource`** — Read one MCP resource by URI.  
  **Input:** `uri` required; optional `server`.  
  **Output:** JSON object containing server, uri, resource metadata, or an error string.  
  **Permission:** read-only.

- **`McpAuth`** — Inspect MCP authentication/connection state.  
  **Input:** `server`.  
  **Output:** JSON object containing server status, server info, tool count, and resource count; disconnected servers return a guidance message.  
  **Permission:** danger-full-access.

- **`RemoteTrigger`** — Trigger a remote URL.  
  **Input:** `url` required; optional `method`, `headers`, `body`.  
  **Output:** JSON object with URL, method, status code or error, truncated body, and `success` boolean.  
  **Permission:** danger-full-access.

- **`MCP`** — Execute an MCP-provided tool.  
  **Input:** `server`, `tool`; optional `arguments` object.  
  **Output:** JSON object containing server, tool, result and `status:"success"`, or `error` and `status:"error"`.  
  **Permission:** danger-full-access.

- **`TestingPermission`** — Test-only permission probe.  
  **Input:** `action`.  
  **Output:** JSON stub with the action, `permitted:true`, and a placeholder message.  
  **Permission:** danger-full-access.

---

## 4. Runtime (`runtime`)

### 4.1 Conversation engine
The runtime conversation loop maintains a session transcript consisting of:
- system messages
- user messages
- assistant messages
- tool-result messages

A single user turn may produce multiple model iterations:
1. send current transcript and system prompt to the model API
2. assemble streamed text/tool-use events into an assistant message
3. if the assistant requested tools, execute them one by one
4. append tool-result messages
5. repeat until the assistant stops requesting tools or an iteration limit is reached

### 4.2 Auto-compaction and truncation
Compaction exists to keep the transcript within an estimated token budget.
- Older messages are summarized into a synthetic continuation system message.
- Recent messages are preserved verbatim.
- Tool-use/tool-result boundaries are preserved so provider adapters never see orphaned tool-result messages.
- The continuation text explicitly instructs the next model turn to continue directly without restating the summary.
- A second compression layer can shrink summaries to configurable line/character budgets.

### 4.3 System prompt assembly
The runtime prompt builder constructs a prompt from:
- static behavioral scaffold
- environment metadata
- working directory and date
- project-level git context
- discovered instruction files up the ancestor chain
- merged runtime configuration
- optional output-style section
- optional caller-supplied appended sections

Instruction-file inclusion rules:
- project and ancestor roots are scanned for several conventional filenames
- duplicates are removed by content hash
- each file is truncated to a per-file limit and there is an overall prompt budget for all instruction-file text

### 4.4 Session persistence
Sessions persist to disk using a JSONL-based appendable transcript format, with legacy JSON load support.
Observable behavior:
- new sessions receive unique ids and timestamps
- session files can be incrementally appended as messages arrive
- full snapshots are written atomically
- large session files rotate into numbered backups
- sessions can be forked, preserving history and prompt history while creating a new identity
- sessions are namespaced by a fingerprint of the workspace path
- `latest` / `last` / `recent` resolve to the most recently modified session in the current workspace namespace

### 4.5 Shell execution
Shell execution behavior includes:
- always runs in the current working directory
- fresh shell per invocation
- optional timeout for foreground runs
- optional immediate background spawn returning a process id string
- output truncation to bounded size
- optional sandbox settings derived from config plus per-call overrides
- redirected `HOME` and `TMPDIR` into workspace-local sandbox directories when filesystem sandboxing is active

### 4.6 Shell validation
Before a command is allowed under constrained modes, a validation pipeline classifies it.
Key externally visible rules:
- read-only mode blocks obvious state-mutating commands, shell redirection writes, package-manager/service-admin actions, and write-oriented git subcommands
- workspace-write mode warns on commands that target known system locations
- destructive patterns such as recursive deletion are treated as warnings
- path checks are heuristic string checks, not a full filesystem-resolution engine

### 4.7 File operations
File behavior contracts:
- reads reject files larger than 10 MiB
- reads reject likely-binary files by scanning for NUL bytes
- reads return line-windowed text with line metadata
- writes auto-create parent directories and reject content over 10 MiB
- edits require the old string to exist and can replace first occurrence or all occurrences
- workspace-safe wrappers resolve canonical paths and reject paths escaping the workspace boundary
- symlink escape detection exists explicitly for symlink targets leaving the workspace

### 4.8 Sandbox model
Sandbox configuration includes:
- enable/disable state
- namespace restrictions
- optional network isolation
- filesystem modes (`off`, `workspace-only`, `allow-list`)
- allow-listed mounts

On Linux, namespace-based sandbox launching is attempted. Filesystem-related environment variables are always wired consistently when filesystem isolation is active.

### 4.9 Permission enforcement
Permission decisions combine:
- default mode (`read-only`, `workspace-write`, `danger-full-access`, plus planning-related local overrides)
- per-tool required level
- allow/deny/ask rules from config
- optional hook overrides
- optional interactive approval prompts

Decision order is effectively:
1. explicit deny rules
2. hook override
3. explicit ask rules
4. sufficient mode or explicit allow rules
5. prompt escalation if a prompter is present
6. deny

If prompting would be required but no prompter exists, the action is denied.

### 4.10 Hooks
Hook execution exists at three points:
- before tool use
- after successful tool use
- after failed tool use

Hook behavior:
- commands run sequentially
- stdin receives a JSON payload describing the tool call
- environment variables also expose tool metadata
- exit code `0` means allow, `2` means deny, any other non-zero status means hook failure
- hooks may emit messages, deny execution, or influence permission/input handling in the runtime hook layer

### 4.11 Git freshness checks
The runtime can detect:
- stale base-commit mismatch between the current worktree and the expected base
- stale or diverged branches relative to main/origin-main
- recent commit summaries and staged file lists for prompt context

Workspace-wide test commands can be preflight-blocked when the branch is stale or diverged.

### 4.12 Worker boot / lane state
The worker subsystem models a worker boot state machine with trust-gate detection, ready-for-prompt detection, prompt misdelivery recovery, and terminal completion classification. Observable lane events include started/blocked/failed/finished/merged/superseded/reconciled and branch-staleness events.

### 4.13 Task / team / cron registries
The runtime exposes in-memory registries for:
- background tasks
- task teams
- recurring scheduled prompts

These registries provide ids, timestamps, state transitions, and query operations, but are process-local rather than durable.

### 4.14 LSP client
The LSP subsystem is a registry/cache of language-server state rather than a full end-to-end protocol bridge. It supports diagnostics plus placeholder or cached responses for definitions, hover, references, symbols, and related actions once a matching server is known.

### 4.15 MCP bridge
MCP support covers:
- merged configuration for multiple transport families
- local stdio JSON-RPC transport management
- cached discovery of tools and resources
- execution of connected MCP tools
- resource listing/reading
- optional OAuth/credential state inspection

The stdio manager validates JSON-RPC framing, request ids, and timeouts. Resource listing in the bridge is metadata-driven from cached discovery state.

---

## 5. API Layer (`api`)

### 5.1 Unified request model
Model requests contain:
- model id
- max output tokens
- ordered messages
- optional system prompt text
- optional tool definitions
- optional tool-choice policy
- stream flag
- optional sampling/tuning fields
- optional stop sequences
- optional reasoning effort

### 5.2 Message/content schema
User and assistant messages are content-block based.
Observable content block types include:
- text
- tool use
- tool result
- thinking
- redacted thinking

Tool results may carry either plain text or JSON-valued sub-blocks.

### 5.3 Provider routing
Provider choice is resolved from model naming conventions first, then credential presence as fallback. Supported provider families are:
- Anthropic-native wire format
- OpenAI-compatible wire format
- xAI via OpenAI-compatible wire format
- DashScope/Qwen through OpenAI-compatible wire format

### 5.4 Anthropic-native behavior
Behavioral rules include:
- API-key and bearer-token auth modes, including combined-header mode
- request headers for anthropic version, user agent, and beta flags
- body normalization that strips unsupported OpenAI-style tuning keys and renames stop sequences
- best-effort context-window preflight using local estimation and optional count-tokens endpoint
- retry with exponential backoff and jitter on transient transport or server-side failures
- non-streaming and streaming request paths
- prompt-cache integration when enabled
- request-id capture from headers or body

### 5.5 OpenAI-compatible behavior
Behavioral rules include:
- bearer-token authentication
- system prompt mapped into a leading system-role message
- tool definitions wrapped in function-tool wire format
- provider-specific output-token field naming for newer reasoning-family models
- reasoning-model stripping of unsupported tuning parameters
- stop-reason normalization (`stop` to end-turn, `tool_calls` to tool-use)
- safe handling of malformed tool-argument JSON by preserving raw text under a fallback object
- inline top-level error extraction before full deserialization

### 5.6 Streaming
Both provider families produce a unified stream event model with events for:
- message start
- content-block start
- content-block delta
- content-block stop
- message delta
- message stop

The SSE parser:
- accepts chunked frames
- ignores comments, ping frames, empty frames, and `[DONE]`
- joins multiline `data:` payloads with newline separators
- enriches JSON-deserialization errors with provider/model/body-snippet context

### 5.7 Error taxonomy
The API layer distinguishes at least these external failure classes:
- missing credentials
- context-window overflow
- expired OAuth token
- auth failure
- invalid environment configuration
- HTTP/transport failure
- JSON parse failure with body snippet
- provider API failure with status, request id, retryability, and raw body
- retries exhausted
- invalid SSE frame
- exponential-backoff overflow

It also exposes machine-usable notions of retryability, safe failure class, request id extraction, context-window detection, and generic-provider-fatal-wrapper detection.

### 5.8 Prompt cache
Prompt caching behavior includes:
- per-session cache directory layout
- completion cache entries keyed by request fingerprint
- TTLs for prompt state and completion reuse
- detection of expected vs unexpected cache breaks by comparing cache-read token drops
- stats on hits, misses, writes, invalidations, and token categories
- persistence of stats and prior request state to disk

---

## 6. Plugins (`plugins`)

### 6.1 Discovery and installation model
Plugins are divided into three kinds:
- built-in
- bundled
- external

Install records store:
- source path or source URL
- install path
- install/update timestamps

Settings track whether each installed plugin is enabled.

### 6.2 Manifest format
Each plugin exposes a JSON manifest at a conventional relative path. The manifest declares:
- name
- version
- description
- permissions (`read`, `write`, `execute`)
- default-enabled flag
- hook command lists
- lifecycle command lists
- plugin-defined tools
- plugin-defined commands

### 6.3 Plugin-defined tools
Each plugin tool declares:
- tool name
- description
- JSON input schema
- external command
- optional fixed args
- required permission level (`read-only`, `workspace-write`, `danger-full-access`)

Execution behavior:
- the command receives the tool input on stdin
- environment variables identify plugin id/name/root, tool name, and raw JSON input
- stdout becomes the tool result

### 6.4 Hook lifecycle
Enabled plugins contribute hook commands to shared hook phases. Hook lists from multiple plugins concatenate in order. Hook results can deny or fail a tool execution chain.

### 6.5 Plugin lifecycle commands
Plugin manifests may also declare init/shutdown command lists. The runtime lifecycle model classifies overall health based on whether plugin-associated servers are healthy, degraded, or failed.

---

## 7. Configuration (`runtime` config subsystem)

### 7.1 Discovery and precedence
Configuration files are discovered in this order, with later files overriding earlier ones:
1. user legacy config
2. user modern config
3. project legacy config
4. project modern config
5. local worktree config

### 7.2 Merge semantics
- Objects merge recursively.
- Arrays and scalar values replace wholesale.
- Missing files are ignored.
- Empty files behave like empty objects.
- MCP server configs merge by server name, with later scope replacing the full per-server entry.

### 7.3 Parsed feature areas
The merged configuration is projected into feature-specific views for:
- hooks
- plugins
- MCP servers
- OAuth
- default model
- model aliases
- permission default mode and permission rules
- sandbox settings
- provider fallback chain
- trusted roots

### 7.4 Validation behavior
Each file is validated before merge.
- hard validation errors abort loading
- warnings are accumulated and printed
- deprecated keys are recognized and may emit warnings
- typo suggestions and source location information are available in validation output

### 7.5 Observable config-controlled behaviors
Config influences:
- default model
- default permission mode
- allow/deny/ask permission rules
- hook command lists
- sandbox policy
- plugin directories and enabled state
- MCP transport definitions
- provider fallback order
- trusted roots for worker boot and trust resolution

---

## 8. Compat Harness + Mock Parity Harness

### 8.1 Compat harness (`compat-harness`)
This crate performs pure text extraction from upstream TypeScript files to derive three manifests without executing JavaScript:
- command manifest
- tool manifest
- bootstrap-phase plan

It searches for the upstream reference repository using project-relative, ancestor-relative, environment-variable, and vendor/reference-source fallbacks.

### 8.2 Bootstrap-plan extraction
The bootstrap-plan extractor does not execute code; it pattern-matches for known fast-path markers and emits an ordered phase list beginning with CLI entry and ending with main runtime.

### 8.3 Mock Anthropic service
The mock service is a deterministic raw-TCP HTTP server that:
- records incoming requests
- detects scenario names from prompt text markers
- returns scripted non-streaming or SSE responses
- supports multiple two-turn tool roundtrips and single-turn informational scenarios
- attaches deterministic request ids per scenario

### 8.4 Deterministic parity scenarios
The parity harness runs twelve scripted scenarios covering:
- plain streaming text
- file read roundtrip
- grep chunk assembly across split SSE chunks
- permitted and denied file writes
- multi-tool single-turn assistant responses
- shell execution and approval prompts
- plugin-tool execution
- auto-compaction trigger
- token/cost reporting

Verified behaviors include:
- exact scenario ordering
- all model requests using streaming mode
- correct tool names and tool-input JSON
- permission-denied error propagation
- actual filesystem side effects for allowed writes
- plugin invocation environment wiring
- compaction path activation on high reported token usage

---

## 9. Session Management

### 9.1 Persistence model
Sessions persist as appendable transcript files plus optional rotated backups. Each session tracks message history, compaction metadata, fork provenance, workspace root, and prompt-history timestamps.

### 9.2 Resume behavior
Resuming a session restores the transcript and prompt history, then allows either:
- returning to the interactive REPL
- executing a batch of resume-safe commands in order

Newest-session aliases resolve by modification time within the current workspace-specific session namespace.

### 9.3 Compaction behavior
Compaction preserves recent context verbatim, rewrites older history into a structured continuation summary, and records the compaction event on the session.

### 9.4 Export behavior
Conversation export produces a Markdown transcript containing a title/header plus the conversation content.

---

## 10. Permission Model

### 10.1 Permission levels
The system uses three main execution levels:
- **read-only** — safe inspection and non-mutating actions
- **workspace-write** — mutations constrained to the workspace
- **danger-full-access** — unrestricted commands and side effects

### 10.2 Tool-level requirements
Every built-in tool declares a required minimum permission level. The effective session mode must meet or exceed that requirement unless an explicit allow rule applies.

### 10.3 Rule-based overrides
Permission rules may explicitly:
- allow
- deny
- ask

Rules match against tool names plus extracted subject strings such as file paths, URLs, commands, or other request payload fragments.

### 10.4 Approval flow
If the effective policy says “ask” and an interactive prompter is available:
- the user is prompted
- approval allows execution
- rejection turns into a tool-result error or blocked action

If no prompter is present, ask-required actions are denied.

### 10.5 Hooks and permissions
Hook output can influence tool execution by:
- blocking it outright
- supplying a permission override decision
- adding contextual messages to the eventual tool result

### 10.6 Workspace vs system boundaries
Workspace safety is enforced both by file-operation boundary checks and by permission policy. Read-only and workspace-write modes are intentionally conservative around state-changing shell commands and writes to system locations.

---

## 11. Telemetry (`telemetry`)

### 11.1 Client identity and request profile
The telemetry crate defines the default application identity, user-agent format, Anthropic API version header, and default beta flags.

### 11.2 Event model
Telemetry events include:
- HTTP request started
- HTTP request succeeded
- HTTP request failed
- analytics event
- session trace record

### 11.3 Sinks
Two sink types are supplied:
- in-memory sink for tests and inspection
- JSONL append sink for durable local traces

### 11.4 Session tracer behavior
The session tracer assigns monotonically increasing sequence numbers and records both high-level telemetry events and corresponding session-trace entries.

---

## 12. PDF Extraction Helper (`tools/pdf_extract`)

The PDF helper is a lightweight text extractor intended for prompt convenience, not full PDF fidelity.

Behavior:
- identifies likely PDF paths embedded in prompt text
- reads bytes from disk
- scans for PDF `stream ... endstream` sections
- optionally inflates zlib-compressed streams marked with a Flate hint
- extracts text from text-showing operators inside `BT ... ET` blocks
- supports escaped parentheses, standard escaped characters, and octal escapes
- returns plain extracted text joined by newlines, or nothing if extraction yields no text

---

## 13. Coverage Appendix

The following Rust files were analyzed for this specification:

### `api`
- `src/client.rs`
- `src/error.rs`
- `src/http_client.rs`
- `src/lib.rs`
- `src/prompt_cache.rs`
- `src/providers/anthropic.rs`
- `src/providers/mod.rs`
- `src/providers/openai_compat.rs`
- `src/sse.rs`
- `src/types.rs`
- `tests/client_integration.rs`
- `tests/openai_compat_integration.rs`
- `tests/provider_client_integration.rs`
- `tests/proxy_integration.rs`

### `commands`
- `src/lib.rs`

### `compat-harness`
- `src/lib.rs`

### `mock-anthropic-service`
- `src/lib.rs`
- `src/main.rs`

### `plugins`
- `src/hooks.rs`
- `src/lib.rs`

### `runtime`
- `src/bash.rs`
- `src/bash_validation.rs`
- `src/bootstrap.rs`
- `src/branch_lock.rs`
- `src/compact.rs`
- `src/config.rs`
- `src/config_validate.rs`
- `src/conversation.rs`
- `src/file_ops.rs`
- `src/git_context.rs`
- `src/green_contract.rs`
- `src/hooks.rs`
- `src/json.rs`
- `src/lane_events.rs`
- `src/lib.rs`
- `src/lsp_client.rs`
- `src/mcp.rs`
- `src/mcp_client.rs`
- `src/mcp_lifecycle_hardened.rs`
- `src/mcp_server.rs`
- `src/mcp_stdio.rs`
- `src/mcp_tool_bridge.rs`
- `src/oauth.rs`
- `src/permission_enforcer.rs`
- `src/permissions.rs`
- `src/plugin_lifecycle.rs`
- `src/policy_engine.rs`
- `src/prompt.rs`
- `src/recovery_recipes.rs`
- `src/remote.rs`
- `src/sandbox.rs`
- `src/session.rs`
- `src/session_control.rs`
- `src/sse.rs`
- `src/stale_base.rs`
- `src/stale_branch.rs`
- `src/summary_compression.rs`
- `src/task_packet.rs`
- `src/task_registry.rs`
- `src/team_cron_registry.rs`
- `src/trust_resolver.rs`
- `src/usage.rs`
- `src/worker_boot.rs`
- `tests/integration_tests.rs`

### `rusty-claude-cli`
- `build.rs`
- `src/init.rs`
- `src/input.rs`
- `src/main.rs`
- `src/render.rs`
- `tests/cli_flags_and_config_defaults.rs`
- `tests/compact_output.rs`
- `tests/mock_parity_harness.rs`
- `tests/output_format_contract.rs`
- `tests/resume_slash_commands.rs`

### `telemetry`
- `src/lib.rs`

### `tools`
- `src/lane_completion.rs`
- `src/lib.rs`
- `src/pdf_extract.rs`

---

## 14. Implementation Notes for the Separate Caduceus Team

If reimplementing this behavior clean-room, preserve these externally visible contracts first:
1. content-block message model with tool-use/tool-result turns
2. multi-iteration conversation loop with tool dispatch and session persistence
3. three-level permission model with rule overrides and approval prompts
4. disk-backed session resume/compact/export flows
5. exact slash-command names and their implemented-vs-registered behavior
6. built-in tool names, schemas, and permission levels
7. provider normalization and streaming-event adaptation
8. deterministic parity scenarios for regression testing
