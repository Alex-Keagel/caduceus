# Hermes IDE Supplementary Behavioral Specification

## Scope

This document supplements the existing `spec-hermes-ide.md`. It focuses on source areas that were shallow in the prior draft: architectural intent, the full Tauri IPC surface, PTY internals, SQLite schema/migrations, project scanning + attunement, git/auth/worktrees, plugin contracts, frontend API wrappers, state data flow, terminal intelligence, and major React contracts.

## 1. Architectural Intent from Root Docs

### 1.1 ARCHITECTURE.md additions
- The frontend is intentionally split into: React state/context, flat component files, a typed `src/api/` invoke layer, and a **module-level terminal pool** rather than React-owned xterm instances.
- The backend is intentionally split into PTY, DB, Git, Project, Process, Workspace, Menu, Clipboard, and Transcript subsystems behind Tauri IPC.
- Core domain vocabulary used throughout the codebase:
  - **Cartography** = project scan pipeline with `surface`, `deep`, `full` depths.
  - **Attunement** = assembly of project/session context into a token-budgeted Markdown file for agents.
  - **Ghost text** = inline command suggestion suffix rendered over the terminal.
  - **Execution node** = command/AI interaction record with timing and summary.
  - **Provider adapter** = per-agent output parser for Claude/Aider/Codex/Gemini/Copilot-like CLIs.
  - **Injection lock** = per-session frontend mutex preventing concurrent context injection.
  - **Nudge** = lightweight terminal write telling the agent to reread `$HERMES_CONTEXT`.
- The architecture guide explicitly states that the SessionProvider is not just a context store: it also restores sessions, loads settings/themes, registers Tauri listeners, and runs autosave.
- The guide explicitly documents the terminal intelligence target as **<5 ms per suggestion run** and the suggestion sources as history + static command index + project context.

### 1.2 DESIGN_PRINCIPLES.md additions
The root principles explain several concrete implementation choices visible in code:
- **Focused, not full-featured**: many systems are intentionally narrow and opinionated rather than generic (e.g., fixed scan heuristics, limited settings, explicit plugin permission categories).
- **Fast by default**: bounded deques, capped file/status enumeration, 2/3/5 depth scan ceilings, lazy directory expansion, debounce windows, and early no-op guards all reflect this.
- **Opinionated over configurable**: token budget defaults, permission-mode mappings, fixed shell integration behavior, stable menu structure, and hard-coded scan skip lists follow this principle.
- **Core vs. extension**: plugin loading, network/shell/storage permission gates, and DB-backed plugin metadata implement the “extensions for subset features” principle directly.
- **Stable over novel** / **saying no is a feature**: the backend favors conservative CLI/library flows (git CLI for worktrees, SSH ControlMaster reuse, atomic file writes, explicit auth fallback chain) over experimental abstractions.

## 2. Tauri Backend Registration and App Lifecycle

### 2.1 Builder/setup behaviors in `src-tauri/src/lib.rs`
- Registers Tauri plugins for shell, notifications, dialog, updater, process, and Aptabase telemetry.
- On setup:
  - resolves app data dir;
  - creates app data and `context/` directories;
  - writes `running.marker` for dirty-shutdown detection;
  - migrates `axon_v3.db` to `hermes_idea_v3.db` if needed;
  - initializes SQLite DB and runs migrations;
  - cleans stale worktrees and shell integration temp files;
  - initializes `sysinfo::System` baseline;
  - starts worktree watcher;
  - stores `AppState` and transcript watcher state in Tauri managed state;
  - installs native menu;
  - wires close/destroy/exit events to a one-shot workspace save path.
- Emits startup/cleanup events:
  - `worktree-cleanup-summary`
  - `worktree-paths-missing`
- Workspace save is guarded by a global atomic so shutdown paths do not double-save.

### 2.2 Shared app state
`AppState` holds:
- `db: Mutex<Database>`
- `pty_manager: Mutex<PtyManager>`
- `sys: Mutex<sysinfo::System>`
- startup marker path
- worktree watcher handle

The dominant lock pattern is:
- lock only the subsystem needed;
- convert poison via `unwrap_or_else(|e| e.into_inner())` on PTY manager in many handlers;
- drop DB locks before touching PTY manager when a flow spans both subsystems.

## 3. Complete IPC Command Catalog

### 3.1 Exact backend command signatures

```text
[pty]
ssh_list_directory(state: State<'_, AppState>, session_id: String, path: Option<String>,) -> Result<Vec<SshFileEntry>, String>
ssh_read_file(state: State<'_, AppState>, session_id: String, file_path: String,) -> Result<SshFileContent, String>
ssh_write_file(state: State<'_, AppState>, session_id: String, file_path: String, content: String,) -> Result<(), String>
ssh_list_tmux_sessions(host: String, port: Option<u16>, user: Option<String>,) -> Result<Vec<TmuxSessionEntry>, String>
ssh_list_tmux_windows(host: String, port: Option<u16>, user: Option<String>, tmux_session: String,) -> Result<Vec<TmuxWindowEntry>, String>
ssh_tmux_select_window(host: String, port: Option<u16>, user: Option<String>, tmux_session: String, window_index: u32,) -> Result<(), String>
ssh_tmux_new_window(host: String, port: Option<u16>, user: Option<String>, tmux_session: String, window_name: Option<String>,) -> Result<(), String>
ssh_tmux_rename_window(host: String, port: Option<u16>, user: Option<String>, tmux_session: String, window_index: u32, new_name: String,) -> Result<(), String>
check_ai_providers() -> std::collections::HashMap<String, bool>
create_session(app: AppHandle, state: State<'_, AppState>, session_id: Option<String>, label: Option<String>, working_directory: Option<String>, color: Option<String>, workspace_paths: Option<Vec<String>>, ai_provider: Option<String>, project_ids: Option<Vec<String>>, auto_approve: Option<bool>, permission_mode: Option<String>, custom_suffix: Option<String>, channels: Option<Vec<String>>, ssh_host: Option<String>, ssh_port: Option<u16>, ssh_user: Option<String>, tmux_session: Option<String>, ssh_identity_file: Option<String>, initial_rows: Option<u16>, initial_cols: Option<u16>,) -> Result<SessionUpdate, String>
write_to_session(state: State<'_, AppState>, session_id: String, data: String,) -> Result<(), String>
is_shell_foreground(state: State<'_, AppState>, session_id: String) -> Result<bool, String>
nudge_project_context(state: State<'_, AppState>, session_id: String,) -> Result<bool, String>
resize_session(state: State<'_, AppState>, session_id: String, rows: u16, cols: u16,) -> Result<(), String>
close_session(app: AppHandle, state: State<'_, AppState>, session_id: String,) -> Result<(), String>
get_sessions(state: State<'_, AppState>) -> Result<Vec<SessionUpdate>, String>
save_all_snapshots(state: State<'_, AppState>) -> Result<(), String>
get_session_detail(state: State<'_, AppState>, session_id: String,) -> Result<SessionUpdate, String>
update_session_label(app: AppHandle, state: State<'_, AppState>, session_id: String, label: String,) -> Result<(), String>
update_session_description(app: AppHandle, state: State<'_, AppState>, session_id: String, description: String,) -> Result<(), String>
update_session_color(app: AppHandle, state: State<'_, AppState>, session_id: String, color: String,) -> Result<(), String>
add_workspace_path(app: AppHandle, state: State<'_, AppState>, session_id: String, path: String,) -> Result<(), String>
remove_workspace_path(app: AppHandle, state: State<'_, AppState>, session_id: String, path: String,) -> Result<(), String>
update_session_group(app: AppHandle, state: State<'_, AppState>, session_id: String, group: Option<String>,) -> Result<(), String>
get_session_output(state: State<'_, AppState>, session_id: String,) -> Result<String, String>
get_session_metadata(state: State<'_, AppState>, session_id: String,) -> Result<SessionMetrics, String>
get_available_shells() -> Vec<ShellInfo>
detect_shell_environment(state: State<'_, AppState>, session_id: String,) -> Result<ShellEnvironment, String>
read_shell_history(shell: String, limit: usize) -> Result<Vec<String>, String>
get_session_commands(state: State<'_, AppState>, session_id: String, limit: usize,) -> Result<Vec<String>, String>
get_project_context(path: String) -> Result<ProjectContextInfo, String>
ssh_upload_file(state: State<'_, AppState>, session_id: String, local_path: String, remote_dir: String,) -> Result<(), String>
ssh_download_file(state: State<'_, AppState>, session_id: String, remote_path: String, local_path: String,) -> Result<(), String>
ssh_add_port_forward(state: State<'_, AppState>, session_id: String, local_port: u16, remote_host: String, remote_port: u16, label: Option<String>,) -> Result<(), String>
ssh_remove_port_forward(state: State<'_, AppState>, session_id: String, local_port: u16,) -> Result<(), String>
ssh_list_port_forwards(state: State<'_, AppState>, session_id: String,) -> Result<Vec<PortForward>, String>
ssh_get_remote_cwd(state: State<'_, AppState>, session_id: String,) -> Result<String, String>
ssh_get_remote_git_info(state: State<'_, AppState>, session_id: String, remote_path: String,) -> Result<RemoteGitInfo, String>

[project]
create_project(state: State<'_, AppState>, app: AppHandle, path: String, name: Option<String>,) -> Result<Project, String>
get_registered_projects(state: State<'_, AppState>) -> Result<Vec<Project>, String>
get_projects_ordered(state: State<'_, AppState>) -> Result<Vec<ProjectOrdered>, String>
get_project(state: State<'_, AppState>, id: String) -> Result<Option<Project>, String>
delete_project(state: State<'_, AppState>, id: String) -> Result<(), String>
attach_session_project(state: State<'_, AppState>, app: AppHandle, session_id: String, project_id: String, role: Option<String>,) -> Result<(), String>
detach_session_project(state: State<'_, AppState>, app: AppHandle, session_id: String, project_id: String,) -> Result<(), String>
get_session_projects(state: State<'_, AppState>, session_id: String,) -> Result<Vec<Project>, String>
scan_project(state: State<'_, AppState>, app: AppHandle, id: String, depth: Option<String>,) -> Result<(), String>

[project.attunement]
assemble_session_context(state: State<'_, AppState>, session_id: String, token_budget: Option<usize>,) -> Result<SessionContext, String>
apply_context(state: State<'_, AppState>, app: AppHandle, session_id: String, execution_mode: Option<String>,) -> Result<ApplyContextResult, String>
fork_session_context(state: State<'_, AppState>, source_session_id: String, target_session_id: String,) -> Result<usize, String>
load_hermes_project_config(state: State<'_, AppState>, project_id: String, project_path: String,) -> Result<Option<HermesProjectConfig>, String>

[db]
get_recent_sessions(state: State<'_, AppState>, limit: Option<i64>,) -> Result<Vec<SessionHistoryEntry>, String>
get_session_snapshot(state: State<'_, AppState>, session_id: String,) -> Result<Option<String>, String>
get_token_usage_today(state: State<'_, AppState>) -> Result<Vec<TokenUsageEntry>, String>
get_cost_history(state: State<'_, AppState>, days: Option<i64>,) -> Result<Vec<CostDailyEntry>, String>
save_memory(state: State<'_, AppState>, scope: String, scope_id: String, key: String, value: String, source: Option<String>, category: Option<String>, confidence: Option<f64>,) -> Result<(), String>
delete_memory(state: State<'_, AppState>, scope: String, scope_id: String, key: String,) -> Result<(), String>
get_all_memory(state: State<'_, AppState>, scope: String, scope_id: String,) -> Result<Vec<MemoryEntry>, String>
get_settings(state: State<'_, AppState>) -> Result<HashMap<String, String>, String>
set_setting(state: State<'_, AppState>, key: String, value: String) -> Result<(), String>
log_execution(state: State<'_, AppState>, session_id: String, event_type: String, content: String, exit_code: Option<i32>, working_directory: Option<String>,) -> Result<(), String>
get_execution_log(state: State<'_, AppState>, session_id: String, limit: Option<i64>,) -> Result<Vec<ExecutionEntry>, String>
add_context_pin(state: State<'_, AppState>, app: AppHandle, session_id: Option<String>, project_id: Option<String>, kind: String, target: String, label: Option<String>, priority: Option<i64>,) -> Result<i64, String>
remove_context_pin(state: State<'_, AppState>, app: AppHandle, id: i64,) -> Result<(), String>
get_context_pins(state: State<'_, AppState>, session_id: Option<String>, project_id: Option<String>,) -> Result<Vec<ContextPin>, String>
save_context_snapshot(state: State<'_, AppState>, session_id: String, version: i64, context_json: String,) -> Result<(), String>
get_context_snapshots(state: State<'_, AppState>, session_id: String,) -> Result<Vec<ContextSnapshotEntry>, String>
get_context_snapshot(state: State<'_, AppState>, session_id: String, version: i64,) -> Result<Option<ContextSnapshotEntry>, String>
get_cost_by_project(state: State<'_, AppState>, days: Option<i64>,) -> Result<Vec<ProjectCostEntry>, String>
export_settings(state: State<'_, AppState>, path: String) -> Result<(), String>
import_settings(state: State<'_, AppState>, path: String,) -> Result<HashMap<String, String>, String>
export_prompt_bundle(path: String, data: String) -> Result<(), String>
import_prompt_bundle(path: String) -> Result<String, String>
save_plugin_metadata(plugin_id: String, version: String, name: String, permissions: Vec<String>, state: State<'_, AppState>,) -> Result<(), String>
get_plugin_permissions(plugin_id: String, state: State<'_, AppState>,) -> Result<Vec<String>, String>
get_plugin_setting(key: String, plugin_id: String, state: State<'_, AppState>,) -> Result<Option<String>, String>
set_plugin_setting(key: String, value: String, plugin_id: String, state: State<'_, AppState>,) -> Result<(), String>
delete_plugin_setting(key: String, plugin_id: String, state: State<'_, AppState>,) -> Result<(), String>
set_plugin_enabled(plugin_id: String, enabled: bool, state: State<'_, AppState>,) -> Result<(), String>
cleanup_plugin_data(plugin_id: String, state: State<'_, AppState>) -> Result<(), String>
get_plugin_settings_batch(plugin_id: String, state: State<'_, AppState>,) -> Result<std::collections::HashMap<String, String>, String>
get_disabled_plugin_ids(state: State<'_, AppState>) -> Result<Vec<String>, String>
list_ssh_saved_hosts(state: State<'_, AppState>) -> Result<Vec<SshSavedHost>, String>
upsert_ssh_saved_host(state: State<'_, AppState>, host: SshSavedHost) -> Result<(), String>
delete_ssh_saved_host(state: State<'_, AppState>, id: String) -> Result<(), String>

[git]
git_status(state: State<'_, AppState>, session_id: String,) -> Result<GitSessionStatus, String>
git_stage(state: State<'_, AppState>, session_id: String, project_id: String, paths: Vec<String>,) -> Result<GitOperationResult, String>
git_unstage(state: State<'_, AppState>, session_id: String, project_id: String, paths: Vec<String>,) -> Result<GitOperationResult, String>
git_discard_changes(state: State<'_, AppState>, session_id: String, project_id: String, paths: Vec<String>,) -> Result<GitOperationResult, String>
git_commit(state: State<'_, AppState>, session_id: String, project_id: String, message: String, author_name: Option<String>, author_email: Option<String>,) -> Result<GitOperationResult, String>
git_push(state: State<'_, AppState>, session_id: String, project_id: String, remote: Option<String>,) -> Result<GitOperationResult, String>
git_pull(state: State<'_, AppState>, session_id: String, project_id: String, remote: Option<String>,) -> Result<GitOperationResult, String>
git_diff(state: State<'_, AppState>, session_id: String, project_id: String, file_path: String, staged: bool,) -> Result<GitDiff, String>
git_open_file(state: State<'_, AppState>, session_id: String, project_id: String, file_path: String,) -> Result<(), String>
read_file_content(state: State<'_, AppState>, session_id: String, project_id: String, file_path: String,) -> Result<FileContent, String>
write_file_content(state: State<'_, AppState>, session_id: String, project_id: String, file_path: String, content: String,) -> Result<u64, String>
open_file_in_editor(state: State<'_, AppState>, session_id: String, project_id: String, file_path: String, editor: Option<String>,) -> Result<(), String>
git_list_branches(state: State<'_, AppState>, session_id: String, project_id: String,) -> Result<Vec<GitBranch>, String>
git_list_branches_for_project(state: State<'_, AppState>, project_id: String,) -> Result<Vec<GitBranch>, String>
git_branches_ahead_behind(state: State<'_, AppState>, session_id: String, project_id: String,) -> Result<HashMap<String, (u32, u32)>, String>
git_create_branch(state: State<'_, AppState>, session_id: String, project_id: String, name: String, checkout: bool,) -> Result<GitOperationResult, String>
git_checkout_branch(app: AppHandle, state: State<'_, AppState>, session_id: String, project_id: String, name: String,) -> Result<GitOperationResult, String>
git_delete_branch(state: State<'_, AppState>, session_id: String, project_id: String, name: String, force: bool,) -> Result<GitOperationResult, String>
list_directory(state: State<'_, AppState>, session_id: String, project_id: String, relative_path: Option<String>,) -> Result<Vec<FileEntry>, String>
git_stash_list(state: State<'_, AppState>, session_id: String, project_id: String,) -> Result<Vec<GitStashEntry>, String>
git_stash_save(state: State<'_, AppState>, session_id: String, project_id: String, message: Option<String>, include_untracked: Option<bool>,) -> Result<GitOperationResult, String>
git_stash_apply(state: State<'_, AppState>, session_id: String, project_id: String, index: usize,) -> Result<GitOperationResult, String>
git_stash_pop(state: State<'_, AppState>, session_id: String, project_id: String, index: usize,) -> Result<GitOperationResult, String>
git_stash_drop(state: State<'_, AppState>, session_id: String, project_id: String, index: usize,) -> Result<GitOperationResult, String>
git_stash_clear(state: State<'_, AppState>, session_id: String, project_id: String,) -> Result<GitOperationResult, String>
git_log(state: State<'_, AppState>, session_id: String, project_id: String, limit: Option<usize>, offset: Option<usize>,) -> Result<GitLogResult, String>
git_commit_detail(state: State<'_, AppState>, session_id: String, project_id: String, commit_hash: String,) -> Result<GitCommitDetail, String>
git_merge_status(state: State<'_, AppState>, session_id: String, project_id: String,) -> Result<MergeStatus, String>
git_get_conflict_content(state: State<'_, AppState>, session_id: String, project_id: String, file_path: String,) -> Result<ConflictContent, String>
git_resolve_conflict(state: State<'_, AppState>, session_id: String, project_id: String, file_path: String, strategy: String, manual_content: Option<String>,) -> Result<GitOperationResult, String>
git_abort_merge(state: State<'_, AppState>, session_id: String, project_id: String,) -> Result<GitOperationResult, String>
git_continue_merge(state: State<'_, AppState>, session_id: String, project_id: String, message: Option<String>, author_name: Option<String>, author_email: Option<String>,) -> Result<GitOperationResult, String>
search_project(state: State<'_, AppState>, session_id: String, project_id: String, query: String, is_regex: bool, case_sensitive: bool, max_results: Option<u32>,) -> Result<SearchResponse, String>
git_create_worktree(app: AppHandle, state: State<'_, AppState>, session_id: String, project_id: String, branch_name: String, create_branch: bool, from_remote: Option<String>,) -> Result<worktree::WorktreeCreateResult, String>
git_remove_worktree(app: AppHandle, state: State<'_, AppState>, session_id: String, project_id: String,) -> Result<GitOperationResult, String>
git_list_worktrees(state: State<'_, AppState>, project_id: String,) -> Result<Vec<worktree::WorktreeInfo>, String>
git_check_branch_available(state: State<'_, AppState>, project_id: String, branch_name: String,) -> Result<worktree::BranchAvailability, String>
git_session_worktree_info(state: State<'_, AppState>, session_id: String, project_id: String,) -> Result<Option<crate::db::SessionWorktreeRow>, String>
git_list_branches_for_projects(state: State<'_, AppState>, project_ids: Vec<String>,) -> Result<HashMap<String, Vec<GitBranch>>, String>
git_fetch_remote_branches(state: State<'_, AppState>, project_id: String,) -> Result<Vec<GitBranch>, String>
git_is_git_repo(state: State<'_, AppState>, project_id: String) -> Result<bool, String>
git_worktree_has_changes(state: State<'_, AppState>, session_id: String, project_id: String,) -> Result<WorktreeChanges, String>
git_stash_worktree(state: State<'_, AppState>, session_id: String, project_id: String, message: Option<String>,) -> Result<GitOperationResult, String>
git_list_all_worktrees(state: State<'_, AppState>,) -> Result<Vec<WorktreeOverviewEntry>, String>
git_detect_orphan_worktrees(app: AppHandle, state: State<'_, AppState>,) -> Result<Vec<OrphanWorktree>, String>
git_worktree_disk_usage(worktree_path: String) -> Result<u64, String>
git_cleanup_orphan_worktrees(state: State<'_, AppState>, paths: Vec<String>,) -> Result<Vec<CleanupResult>, String>

[plugins]
list_installed_plugins(app: tauri::AppHandle) -> Result<Vec<InstalledPlugin>, String>
read_plugin_bundle(app: tauri::AppHandle, plugin_dir: String) -> Result<String, String>
get_plugins_dir(app: tauri::AppHandle) -> Result<String, String>
uninstall_plugin(app: tauri::AppHandle, plugin_dir: String) -> Result<(), String>
install_plugin(app: tauri::AppHandle, data: Vec<u8>) -> Result<String, String>
fetch_plugin_registry(url: String) -> Result<String, String>
download_and_install_plugin(app: tauri::AppHandle, url: String,) -> Result<String, String>
plugin_fetch_url(url: String, headers: Option<std::collections::HashMap<String, String>>, plugin_id: String, state: State<'_, AppState>,) -> Result<String, String>
plugin_post_json(url: String, body: String, headers: Option<std::collections::HashMap<String, String>>, plugin_id: String, state: State<'_, AppState>,) -> Result<String, String>
plugin_exec_command(command: String, args: Vec<String>, plugin_id: String, state: State<'_, AppState>,) -> Result<PluginExecResult, String>

[process]
list_processes(state: State<'_, AppState>) -> Result<ProcessSnapshot, String>
kill_process(state: State<'_, AppState>, pid: u32, signal: String) -> Result<(), String>
kill_process_tree(state: State<'_, AppState>, pid: u32, signal: String,) -> Result<(), String>
get_process_detail(state: State<'_, AppState>, pid: u32) -> Result<ProcessInfo, String>
reveal_process_in_finder(path: String) -> Result<(), String>

[workspace]
scan_directory(state: State<'_, AppState>, path: String, max_depth: Option<usize>,) -> Result<Vec<ProjectInfo>, String>
detect_project(state: State<'_, AppState>, path: String,) -> Result<Option<ProjectInfo>, String>
get_projects(state: State<'_, AppState>) -> Result<Vec<ProjectInfo>, String>

[menu]
show_context_menu(window: tauri::Window, items: Vec<ContextMenuItem>,) -> Result<(), String>
update_menu_state(app: AppHandle, updates: Vec<MenuItemUpdate>) -> Result<(), String>

[clipboard]
copy_image_to_clipboard(path: String) -> Result<(), String>

[transcript]
start_transcript_watcher(app: AppHandle, state: State<'_, AppState>, transcript_watchers: State<'_, Mutex<TranscriptWatcherState>>, session_id: String,) -> Result<String, String>
stop_transcript_watcher(transcript_watchers: State<'_, Mutex<TranscriptWatcherState>>, watcher_id: String,) -> Result<(), String>
```

### 3.2 Cross-cutting IPC error-handling patterns
- Most commands return `Result<_, String>` and stringify lower-level errors immediately.
- Common guard failures:
  - missing session/project/plugin/worktree -> explicit `"... not found"` errors;
  - wrong session mode (e.g. non-SSH session for SSH-only command) -> explicit typed rejection;
  - lock failures -> `map_err(|e| e.to_string())` or formatted `Lock error`;
  - path validation failures -> traversal/escape rejection before filesystem access;
  - CLI subprocess failures -> stderr returned in the error string.
- Session lifecycle commands frequently emit Tauri events after mutating in-memory state and sometimes persist to DB second.
- DB-to-PTY flows intentionally drop the DB lock before acquiring PTY manager to avoid deadlock.

## 4. PTY System Deep Dive

### 4.1 Core in-memory structures
- `PtySession` stores:
  - PTY master
  - PTY writer
  - shared `Session`
  - shared `OutputAnalyzer`
  - child process handle
  - macOS tty path (for direct SIGINT/process-group checks)
  - shell integration cleanup state
- `Session` adds fields that the earlier spec underemphasized:
  - `description`, `group`
  - `permission_mode`, `custom_suffix`, `channels`
  - `context_injected`, `has_initial_context`, `last_nudged_version`
  - `ssh_info` with `port_forwards`
  - deferred `pending_nudge`
- `SessionMetrics` is backed by analyzer state and includes latency samples, token history, recent actions, files touched, tool-call summary, and memory facts.

### 4.2 Session creation path (`create_session`)
- Session ID is caller-provided or UUID.
- Default shell comes from DB setting `default_shell`; fallback is platform shell detection.
- Working directory resolution prefers an existing linked worktree for the pre-generated session ID; falls back to requested working directory or process cwd/home.
- `has_initial_context` is true only for local sessions with attached projects.
- PTY is opened at frontend-provided dimensions if available; fallback `80x24`.
- macOS immediately resizes the PTY after `openpty()` because the initial size may be ignored.
- Local session shell integration is applied only for non-SSH sessions.
- SSH sessions build a persistent control-socket-backed `ssh -t` command with keepalives and optional identity file.
- If `tmux_session` is present, the remote command becomes `tmux new-session -A -s <name> -x <cols> -y <rows>`.
- Local shell launch behavior:
  - wraps via `env -u CLAUDECODE -u CLAUDE_CODE -u COLUMNS -u LINES <shell>` on Unix;
  - `bash` uses `--rcfile`; `fish` uses `-C <init command>`; zsh/other use login shell mode.
- Environment injected into local shells:
  - `TERM=xterm-256color`
  - `COLORTERM=truecolor`
  - `TERM_PROGRAM=HERMES-IDE`
  - UTF-8 locale defaults if absent
  - `PROMPT_EOL_MARK=` to suppress zsh inverse-percent marker
  - `HERMES_CONTEXT=<app_data_dir>/context/<session>.md`
  - `HERMES_SESSION_ID=<session>`
- AI auto-launch command is derived from provider + permission mode:
  - Claude: `--permission-mode acceptEdits|plan|auto|dontAsk|bypassPermissions`
  - Aider: `--yes` / `--yes-always`
  - Codex: `--full-auto` / `--dangerously-bypass-approvals-and-sandbox`
  - Gemini: `--yolo`
  - Copilot: `gh copilot` base command
- Claude channel arguments are appended separately as repeated `--channels <value>` suffixes.

### 4.3 Reader loop / analyzer interplay
- The reader thread continuously reads raw PTY bytes, base64-encodes raw output for frontend terminal rendering, and separately sends stripped/analyzed output into the analyzer.
- Analyzer responsibilities:
  - strips ANSI once and reuses the stripped chunk for parsing;
  - detects visible content before marking busy;
  - extracts OSC 7 cwd reports before ANSI stripping;
  - tracks token totals, tool calls, files, actions, latency, memory facts, execution nodes, recent commands;
  - keeps bounded deques/sets (e.g. tool calls, file order, memory facts, token history, latency samples, completed nodes) to keep memory bounded.
- Execution-node model:
  - `mark_input_sent()` timestamps latency start;
  - the input-line buffer accumulates printable characters from `write_to_session`;
  - Enter finalizes the current command line into a new node builder;
  - output lines are appended up to 50 lines;
  - final node summary is capped to 500 chars and only 20 recent completed nodes are retained.
- Busy/prompt logic:
  - `pending_phase` carries prompt/work/input-needed hints from adapters;
  - `lastStablePhase` on the frontend is used because shell echo can transiently produce `busy` flicker;
  - a silence detector/polling fallback promotes readiness when prompt heuristics fail.
- AI launch failure detection:
  - after an AI CLI launch, analyzer scans a fixed post-launch window for “command not found”-style failures and emits `ai-launch-failed`.

### 4.4 Provider adapters
The provider-adapter layer contributes:
- agent detection (`detect_agent`)
- line analysis (`analyze_line`) for token deltas, tool calls, actions, phase hints, memory facts
- prompt detection
- provider-specific action templates

Shared utilities include:
- model name extraction across Claude/OpenAI/Gemini/DeepSeek markers;
- token suffix parsing (`K/M/B/T`);
- price estimation by provider/model family;
- slash-command label dictionaries for Claude/Aider/Codex/Gemini commands;
- generic shell prompt detection covering standard prompts plus starship/oh-my-zsh/p10k-style glyphs.

### 4.5 Shell integration details
- `ShellIntegration` variants: `Zsh { zdotdir }`, `Bash { rcfile }`, `Fish`, `None`.
- zsh strategy:
  - create temp ZDOTDIR with `.zshenv`, `.zprofile`, `.zshrc`, `.zlogin`;
  - temporarily source user files from real ZDOTDIR/HOME;
  - restore Hermes temp ZDOTDIR between stages so later rc files are the Hermes wrappers;
  - disable `zsh-autosuggestions` via multiple layers (function overrides + precmd hook + variable nuking);
  - suppress `zsh-autocomplete`; enable `HIST_IGNORE_SPACE`; export `HERMES_TERMINAL=1`; force `kill -WINCH $$` at the end.
- bash strategy:
  - custom rcfile sources `/etc/profile`, `.bash_profile`/`.bash_login`/`.profile`, then `.bashrc`;
  - disables `ble.sh` auto-complete if present;
  - exports `HERMES_TERMINAL=1`; sends `WINCH`.
- fish strategy:
  - no temp file; uses `-C` command to disable fish autosuggestion and export `HERMES_TERMINAL=1`.
- Cleanup removes per-session temp rc artifacts and also provides crash-recovery cleanup for stale `hermes-zsh-*` and `hermes-bash-*.sh` temp files.

### 4.6 macOS-safe spawn path
- On macOS, Hermes bypasses `portable-pty` pre-exec/fork behavior and uses a custom `posix_spawn` path.
- Important flags:
  - `POSIX_SPAWN_SETSID`
  - `POSIX_SPAWN_CLOEXEC_DEFAULT`
  - reset signal handlers and signal mask
- It uses a `hermes-pty-setup` trampoline binary to call `ioctl(TIOCSCTTY)` so the child gets a controlling terminal, which is required for `/dev/tty` users like `sudo`, `ssh`, `gpg`.
- Child termination policy mirrors PTY expectations: SIGHUP first with short grace period, then SIGKILL fallback.

### 4.7 Input, resize, interrupt, and close semantics
- `write_to_session` expects base64 input, decodes it, updates analyzer input-line buffer, writes bytes to PTY, flushes, then on Unix directly delivers SIGINT to child process groups when Ctrl-C is present as a fallback.
- `is_shell_foreground` uses:
  - macOS `tcgetpgrp` vs shell pgid if tty path exists;
  - Linux `/proc/<pid>/stat` `pgrp` vs `tpgid`;
  - final fallback: shell has no children => prompt is foreground.
- `resize_session` resizes the PTY and explicitly sends `SIGWINCH` to child process groups when needed.
- `close_session` performs, in order:
  1. remove PTY session from manager;
  2. terminate child;
  3. cleanup shell integration temp files;
  4. snapshot stripped output;
  5. persist session status/token usage/memory facts;
  6. emit final `session-updated` and `session-removed`;
  7. delete session context file;
  8. cleanup session-scoped pins;
  9. cleanup linked worktrees and prune stale refs.

### 4.8 SSH/tmux/file/port-forwarding behaviors
- SSH helpers use a shared control socket directory under temp space (`hermes-ssh-mux`) and `ControlMaster=auto`, `ControlPersist=300`.
- `ssh_list_directory` uses remote `ls -1ap` plus `stat` fallback logic for GNU/BSD compatibility.
- `ssh_read_file` / `ssh_write_file` / `ssh_upload_file` / `ssh_download_file` all derive connection info from the PTY session’s `ssh_info`; non-SSH sessions are rejected.
- Upload/download use `ssh ... cat` streaming rather than `scp`, explicitly to avoid quoting/path issues while reusing the control connection.
- Tmux commands query sessions/windows and support select/new/rename operations.
- Port forward commands use `ssh -O forward` / `ssh -O cancel` against the control socket, then mirror the result into in-memory `ssh_info.port_forwards`.
- `ssh_get_remote_git_info` returns just `branch` and `change_count` for lightweight SSH-side polling.

## 5. SQLite Schema, Indices, and Migration Strategy

### 5.1 Database bootstrap
- File is opened via `rusqlite::Connection::open`.
- Pragmas enforced at startup:
  - `PRAGMA journal_mode=WAL;`
  - `PRAGMA foreign_keys=ON;`
- Migrations are idempotent and run on every startup.

### 5.2 Tables and columns

#### sessions
- `id TEXT PRIMARY KEY`
- `label TEXT NOT NULL`
- `color TEXT NOT NULL DEFAULT '#58a6ff'`
- `group_name TEXT`
- `phase TEXT NOT NULL DEFAULT 'destroyed'`
- `working_directory TEXT NOT NULL`
- `shell TEXT NOT NULL`
- `workspace_paths TEXT NOT NULL DEFAULT '[]'`
- `created_at TEXT NOT NULL`
- `closed_at TEXT`
- `scrollback_snapshot TEXT`
- migrated later: `description TEXT NOT NULL DEFAULT ''`
- migrated later: `ssh_info TEXT`

#### token_usage
- `id INTEGER PRIMARY KEY AUTOINCREMENT`
- `session_id TEXT NOT NULL`
- `provider TEXT NOT NULL`
- `model TEXT NOT NULL`
- `input_tokens INTEGER NOT NULL DEFAULT 0`
- `output_tokens INTEGER NOT NULL DEFAULT 0`
- `estimated_cost_usd REAL DEFAULT 0.0`
- `recorded_at TEXT NOT NULL DEFAULT (datetime('now'))`
- index: `idx_token_session(session_id, provider)`

#### token_snapshots
- `id INTEGER PRIMARY KEY AUTOINCREMENT`
- `session_id TEXT NOT NULL`
- `provider TEXT NOT NULL`
- `model TEXT NOT NULL`
- `input_tokens INTEGER NOT NULL`
- `output_tokens INTEGER NOT NULL`
- `cost_usd REAL NOT NULL`
- `recorded_at TEXT NOT NULL DEFAULT (datetime('now'))`
- indexes: `idx_token_snap_session(session_id)`, `idx_token_snap_date(recorded_at)`

#### cost_daily
- `date TEXT NOT NULL`
- `provider TEXT NOT NULL`
- `model TEXT NOT NULL`
- `total_input_tokens INTEGER NOT NULL DEFAULT 0`
- `total_output_tokens INTEGER NOT NULL DEFAULT 0`
- `total_cost_usd REAL NOT NULL DEFAULT 0.0`
- `session_count INTEGER NOT NULL DEFAULT 0`
- PK `(date, provider, model)`

#### memory
- `id INTEGER PRIMARY KEY AUTOINCREMENT`
- `scope TEXT NOT NULL CHECK(scope IN ('session', 'project', 'global'))`
- `scope_id TEXT NOT NULL`
- `category TEXT NOT NULL DEFAULT 'general'`
- `key TEXT NOT NULL`
- `value TEXT NOT NULL`
- `source TEXT NOT NULL DEFAULT 'auto'`
- `confidence REAL NOT NULL DEFAULT 1.0`
- `access_count INTEGER NOT NULL DEFAULT 0`
- `created_at TEXT NOT NULL DEFAULT (datetime('now'))`
- `updated_at TEXT NOT NULL DEFAULT (datetime('now'))`
- `expires_at TEXT`
- unique `(scope, scope_id, key)`
- index: `idx_memory_scope(scope, scope_id)`

#### execution_log
- `id INTEGER PRIMARY KEY AUTOINCREMENT`
- `session_id TEXT NOT NULL`
- `event_type TEXT NOT NULL`
- `content TEXT NOT NULL`
- `exit_code INTEGER`
- `working_directory TEXT`
- `timestamp TEXT NOT NULL DEFAULT (datetime('now'))`
- index: `idx_exec_session(session_id, timestamp)`

#### settings
- `key TEXT PRIMARY KEY`
- `value TEXT NOT NULL`
- `updated_at TEXT NOT NULL DEFAULT (datetime('now'))`

#### projects (legacy workspace-project table retained for migration compatibility)
- `id TEXT PRIMARY KEY`
- `path TEXT NOT NULL UNIQUE`
- `name TEXT NOT NULL`
- `detected_languages TEXT`
- `detected_frameworks TEXT`
- `file_tree_hash TEXT`
- `created_at TEXT NOT NULL DEFAULT (datetime('now'))`
- `updated_at TEXT NOT NULL DEFAULT (datetime('now'))`
- index: `idx_projects_path(path)`

#### execution_nodes
- `id INTEGER PRIMARY KEY AUTOINCREMENT`
- `session_id TEXT NOT NULL`
- `timestamp INTEGER NOT NULL`
- `kind TEXT NOT NULL DEFAULT 'command'`
- `input TEXT`
- `output_summary TEXT`
- `exit_code INTEGER`
- `working_dir TEXT NOT NULL`
- `duration_ms INTEGER DEFAULT 0`
- `metadata TEXT`
- index: `idx_exec_nodes_session(session_id, timestamp)`

#### error_patterns
- `id INTEGER PRIMARY KEY AUTOINCREMENT`
- `project_id TEXT`
- `fingerprint TEXT NOT NULL`
- `raw_sample TEXT`
- `occurrence_count INTEGER DEFAULT 1`
- `last_seen INTEGER`
- `resolution TEXT`
- `resolution_verified INTEGER DEFAULT 0`
- `created_at INTEGER DEFAULT (strftime('%s','now'))`
- unique index: `idx_error_fp(project_id, fingerprint)`

#### command_patterns
- `id INTEGER PRIMARY KEY AUTOINCREMENT`
- `project_id TEXT`
- `sequence TEXT NOT NULL`
- `next_command TEXT NOT NULL`
- `frequency INTEGER DEFAULT 1`
- `last_seen INTEGER DEFAULT (strftime('%s','now'))`
- unique `(project_id, sequence, next_command)`
- index: `idx_cmd_patterns(project_id, sequence)`

#### context_pins
- `id INTEGER PRIMARY KEY AUTOINCREMENT`
- `session_id TEXT`
- `project_id TEXT`
- `kind TEXT NOT NULL CHECK(kind IN ('file','memory','text','directory'))`
- `target TEXT NOT NULL`
- `label TEXT`
- `priority INTEGER DEFAULT 128`
- `created_at INTEGER DEFAULT (strftime('%s','now'))`
- indexes: `idx_pins_session(session_id)`, `idx_pins_project(project_id)`

#### error_sessions
- `error_pattern_id INTEGER NOT NULL`
- `session_id TEXT NOT NULL`
- `last_seen INTEGER NOT NULL`
- `occurrence_count INTEGER DEFAULT 1`
- PK `(error_pattern_id, session_id)`

#### realms (current canonical project table)
- `id TEXT PRIMARY KEY`
- `path TEXT NOT NULL UNIQUE`
- `name TEXT NOT NULL`
- `languages TEXT NOT NULL DEFAULT '[]'`
- `frameworks TEXT NOT NULL DEFAULT '[]'`
- `architecture TEXT`
- `conventions TEXT NOT NULL DEFAULT '[]'`
- `scan_status TEXT NOT NULL DEFAULT 'pending'`
- `last_scanned_at TEXT`
- `created_at TEXT NOT NULL DEFAULT (datetime('now'))`
- `updated_at TEXT NOT NULL DEFAULT (datetime('now'))`
- index: `idx_realms_path(path)`

#### session_realms
- `session_id TEXT NOT NULL`
- `realm_id TEXT NOT NULL`
- `attached_at TEXT NOT NULL DEFAULT (datetime('now'))`
- `role TEXT NOT NULL DEFAULT 'primary'`
- PK `(session_id, realm_id)`
- index: `idx_session_realms_session(session_id)`

#### realm_conventions
- `id INTEGER PRIMARY KEY AUTOINCREMENT`
- `realm_id TEXT NOT NULL`
- `rule TEXT NOT NULL`
- `source TEXT NOT NULL DEFAULT 'detected'`
- `confidence REAL NOT NULL DEFAULT 0.8`
- `created_at TEXT NOT NULL DEFAULT (datetime('now'))`
- unique `(realm_id, rule)`
- index: `idx_conventions_realm(realm_id)`

#### context_snapshots
- `id INTEGER PRIMARY KEY AUTOINCREMENT`
- `session_id TEXT NOT NULL`
- `version INTEGER NOT NULL`
- `context_json TEXT NOT NULL`
- `created_at INTEGER DEFAULT (strftime('%s','now'))`
- unique `(session_id, version)`
- index: `idx_ctx_snap_session(session_id)`

#### hermes_project_config
- `realm_id TEXT PRIMARY KEY`
- `config_json TEXT NOT NULL`
- `config_hash TEXT`
- `loaded_at TEXT NOT NULL DEFAULT (datetime('now'))`

#### session_worktrees
Initial definition:
- `id TEXT PRIMARY KEY`
- `session_id TEXT NOT NULL`
- `realm_id TEXT NOT NULL`
- `worktree_path TEXT NOT NULL`
- `branch_name TEXT`
- `is_main_worktree INTEGER NOT NULL DEFAULT 0`
- `created_at TEXT NOT NULL DEFAULT (datetime('now'))`
- unique `(session_id, realm_id)`
- later migration adds `last_activity_at TEXT`
- indexes: `idx_sw_session(session_id)`, `idx_sw_realm(realm_id)`, `idx_sw_path(worktree_path)`

#### plugins
- `id TEXT PRIMARY KEY`
- `version TEXT NOT NULL`
- `name TEXT NOT NULL`
- `description TEXT`
- `author TEXT`
- `enabled INTEGER NOT NULL DEFAULT 1`
- `permissions_granted TEXT NOT NULL DEFAULT '[]'`
- `installed_at TEXT NOT NULL DEFAULT (datetime('now'))`
- `updated_at TEXT NOT NULL DEFAULT (datetime('now'))`

#### plugin_storage
- `plugin_id TEXT NOT NULL`
- `key TEXT NOT NULL`
- `value TEXT NOT NULL`
- `updated_at TEXT NOT NULL DEFAULT (datetime('now'))`
- PK `(plugin_id, key)`
- index: `idx_plugin_storage_plugin(plugin_id)`

#### ssh_saved_hosts
- `id TEXT PRIMARY KEY`
- `label TEXT NOT NULL`
- `host TEXT NOT NULL`
- `port INTEGER NOT NULL DEFAULT 22`
- `user TEXT NOT NULL`
- `identity_file TEXT`
- `jump_host TEXT`
- `port_forwards TEXT NOT NULL DEFAULT '[]'`
- `created_at TEXT NOT NULL DEFAULT (datetime('now'))`
- `updated_at TEXT NOT NULL DEFAULT (datetime('now'))`

#### project_usage
- `project_id TEXT PRIMARY KEY`
- `session_count INTEGER NOT NULL DEFAULT 0`
- `last_opened_at TEXT NOT NULL DEFAULT (datetime('now'))`

### 5.3 Migration behaviors
- Legacy `projects` rows are copied into `realms` with language/framework JSON normalization and `scan_status='surface'`.
- `sessions.description` is added idempotently.
- `sessions.ssh_info` is added idempotently.
- `session_worktrees.last_activity_at` is added idempotently.
- A special migration detects an old auto-generated unique index on `session_worktrees` and recreates the table to drop a uniqueness constraint on `worktree_path`, allowing shared worktrees across sessions.
- Project usage tracking is introduced lazily with `CREATE TABLE IF NOT EXISTS project_usage`.

### 5.4 DB behaviors that matter to higher layers
- `project_usage` drives `get_projects_ordered()` sorting (session count desc, last opened desc, name asc).
- Plugin storage commands require `storage` permission before reading/writing `plugin_storage`.
- `get_plugin_settings_batch` only returns keys prefixed with `__setting:` and strips that prefix in the response.
- Settings import/export and prompt-bundle import/export are first-class IPC commands.

## 6. Project Scanner + Attunement Algorithms

### 6.1 Cartography skip/deny policy
- Skip dirs: `node_modules`, `.git`, `vendor`, `build`, `dist`, `__pycache__`, `.next`, `.nuxt`, `target`, `.cache`, `.venv`, `venv`, `.tox`, `coverage`, `.nyc_output`, `.turbo`.
- Hard deny dirs: `.ssh`, `.aws`, `.gnupg`, `.kube`.

### 6.2 Surface scan (`surface_scan`)
- Depth cap: 2.
- Marker-file detection covers JS/TS, Deno, Rust, Python, Go, Ruby, Java/JVM, PHP, Dart, Swift, C/C++, Elixir, Scala, Haskell, Zig, Julia, Clojure, Erlang, OCaml, Perl, Gleam.
- Additional root-level detection checks for `.sln`, `.csproj`, `.fsproj` to infer C#/F#.
- Extensions are counted and languages are added once count `> 2`.
- Framework extraction is content-based for known markers (e.g. package.json, Cargo.toml, pyproject, go.mod, Gemfile, composer, pubspec).

### 6.3 Deep scan (`deep_scan`)
- Starts from surface scan.
- Adds architecture detection and convention extraction.
- Repeats extension counting at depth 3 for better recall.
- Architecture patterns recognized include:
  - `monorepo`
  - `mvc`
  - `nextjs-app-router`
  - `nextjs-pages-router`
  - `tauri-app`
  - `rust-mixed`
  - `rust-binary`
  - `rust-library`
  - generic `src-layout`
- Architecture layers are inferred from directory names such as `packages`, `apps`, `controllers`, `models`, `views`, `api`, `services`, `lib`, `components`, `hooks`, `styles`, `tests`.

### 6.4 Convention extraction
Deep scan reads common config files and emits `Convention` rows with source/confidence:
- Prettier: 2 vs 4 spaces, no semicolons, single quotes, print width configured.
- `.editorconfig`: tabs vs spaces and indent size.
- `tsconfig.json`: strict mode, path aliases.
- ESLint presence.
- Cargo edition and custom lint sections.
- `package.json` script/test framework hints (Vitest/Jest/Mocha, lint/build scripts).
- Docker presence.
- CI presence (GitHub Actions/GitLab CI).

### 6.5 Full scan (`full_scan`)
- Starts from deep scan.
- Adds entry points from a fixed list including Rust, JS/TS, Next, Python, Go candidates.
- Samples up to **200** source files, max depth **5**.
- Reads only the first **50 lines** of each sampled file for import extraction.
- Recognizes import-like lines beginning with `import`, `from`, `use`, or `require(`.
- Top 5 import modules with count > 3 become low-confidence conventions of the form `frequently-imports: <module>`.

### 6.6 Attunement config schema (`.hermes/context.json`)
`HermesProjectConfig` supports:
- `pins[]` with `kind`, `target`, optional `label`
- `memory[]` with `key`, `value`
- `conventions[]`
- optional `token_budget`

### 6.7 Context assembly (`assemble_context`)
- Default token budget: **4000**.
- Budget can be overridden by any loaded `.hermes/context.json`; the last loaded override wins.
- Attached projects are loaded from DB, not the filesystem scan directly.
- Convention precedence:
  1. dedicated DB `realm_conventions` rows if present
  2. fallback to project JSON blob
  3. prepend `.hermes` conventions if not already present
- Token estimate is coarse (`~ chars/4`) and includes per-section overhead.
- If over budget and more than one project is attached, conventions are trimmed from non-primary projects first; secondary projects can be reduced to a floor of 2 conventions or cleared entirely.
- Pin loading order:
  1. DB session pins
  2. DB project pins for the primary project
  3. synthetic `.hermes` pins not already present
- Synthetic `.hermes` pins get priority `256` vs default `128`.
- File pins read textual content only; binary-like extensions are skipped.
- Per pinned file content budget: **8192 bytes** with UTF-8 boundary-safe truncation and a truncation marker.
- Memory merging order:
  - `get_merged_memory(project_ids)` provides project/global merged entries from DB
  - `.hermes` memory is appended only if the key was not already seen
- Error resolutions are structurally supported but currently assembled as an empty vector here.
- Latest context version is derived from newest snapshot, not a separate counter.

### 6.8 Context markdown + apply behavior
- Context file path is deterministic: `<app_data_dir>/context/<session_id>.md`.
- `write_session_context_file` deletes the file entirely if there is no meaningful content.
- Writes are atomic: write `*.md.tmp`, then rename.
- `apply_context`:
  1. assembles context
  2. increments version from max snapshot + 1
  3. formats Markdown including execution mode and budget usage
  4. writes atomically
  5. saves JSON snapshot to DB
  6. drops DB lock
  7. uses `PtyManager::send_versioned_nudge`
- Nudge behavior:
  - only if agent detected;
  - deduplicated by `last_nudged_version`;
  - if agent is not in `NeedsInput`, the nudge is deferred in `pending_nudge`.
- Provider-specific nudge text differs for Aider, Claude, Copilot, and generic sessions.

## 7. Git Integration Details

### 7.1 Authentication chain (`make_callbacks`)
Remote callbacks try each method at most once, in this order:
1. SSH agent (`Cred::ssh_key_from_agent`)
2. SSH private key files `~/.ssh/id_ed25519`, `~/.ssh/id_rsa` (+ optional `.pub`)
3. Git credential helper / GCM (`Cred::credential_helper`)
4. `GITHUB_TOKEN` or `GIT_TOKEN` as `x-access-token`
5. final explicit auth failure string with remediation suggestions

### 7.2 Path safety and repo resolution
- `safe_join(project_path, relative)` canonicalizes the project root and rejects any resolved path outside it.
- For non-existent paths, it manually normalizes path components and still enforces root containment.
- Worktree-only maintenance commands also validate that the path is inside a `hermes-worktrees/` subtree before touching disk.
- `resolve_worktree_path` prefers the session/project worktree DB mapping, cleans stale missing worktree rows, and falls back to the project root path.

### 7.3 Status model
- `git_status(session_id)` returns a `GitSessionStatus` covering every attached project.
- Bare repos are explicitly represented as git repos with an error string and no working tree data.
- Detached HEAD is rendered as short SHA plus `(detached)`.
- Upstream ahead/behind is computed from local branch vs upstream ref if present.
- Status collection excludes ignored files and has a hard cap of **10,000** files, returning a truncation warning if exceeded.
- File area classification is explicit:
  - `staged`
  - `unstaged`
  - `untracked`
  - conflicted as special status
- Untracked files are handled first to avoid duplicate WT_NEW classification bugs.
- Stash count is computed per repo during status collection.

### 7.4 Operation families
- Stage/unstage/discard operate on explicit path lists under the resolved worktree/project path.
- Commit accepts optional author name/email.
- Push/pull use the auth callback chain above.
- `git_diff` caps diff output at **2 MB**.
- File explorer operations include `list_directory`, `read_file_content`, `write_file_content`, `open_file_in_editor`.
- Search is backend-side (`search_project`) with regex/case/max-results controls.
- Merge/conflict family:
  - merge status
  - fetch conflict content
  - resolve by strategy or manual content
  - abort merge
  - continue merge with optional commit metadata
- Stash family supports list/save/apply/pop/drop/clear plus worktree-specific stash helper.

### 7.5 Worktree subsystem
- Stable repo hash = FNV-1a of canonical repo path.
- Worktree base path: `<app_data_dir>/hermes-worktrees/<repo_hash>/`.
- Per-worktree path: `<base>/<first8(session_id)>_<sanitized_branch>/`.
- `repo_path.txt` is written into each repo-hash directory so cleanup can map orphaned worktrees back to the source repo.
- `create_worktree` uses git CLI (`git worktree add`) rather than libgit2.
- Supports:
  - create new local branch
  - attach existing local branch
  - create from remote branch while deriving a local branch name
- If a remote-derived local branch already exists, Hermes verifies local and remote point to the same commit before reuse.
- If git reports the branch is already checked out elsewhere, Hermes resolves the existing worktree path and returns `is_shared=true`.
- Session close respects shared worktrees by ref count and skips disk deletion when multiple sessions point at the same path.
- After removal attempts, affected repos are pruned with `git worktree prune` cleanup helpers.

### 7.6 Worktree watcher + orphan management
- A recursive notify watcher watches `hermes-worktrees/` only if the directory already exists.
- Remove events are debounced per path for 500 ms.
- On external deletion, backend looks up the matching DB record and emits `worktree-path-deleted` with session/project/path/branch.
- Separate IPC commands surface orphan worktree discovery, per-path disk usage, and batch cleanup results.

## 8. Plugin System Contracts

### 8.1 Filesystem/plugin loading contract
- Installed plugins live under `<app_data_dir>/plugins/<plugin_id>/`.
- Each plugin is a directory containing `hermes-plugin.json`.
- `list_installed_plugins` returns:
  - plugin `id`
  - directory name
  - raw manifest JSON
- `read_plugin_bundle` reads the manifest, resolves `main` (default `dist/index.js`), and returns the JS bundle as text.

### 8.2 Installation/uninstallation flow
- `install_plugin` accepts raw `.tgz` bytes from the frontend.
- Extraction occurs in a plugin-dir-local temp folder `.install-tmp-<pid>`.
- Manifest can be at archive root or inside a single child directory (e.g. `package/`).
- Plugin ID is taken from manifest `id` and must not contain `..`, `/`, or `\`.
- Existing plugin directory is removed before replacement.
- If rename across filesystems fails, Hermes recursively copies the extracted directory.
- `uninstall_plugin` removes the plugin directory after canonical path validation.
- `cleanup_plugin_data` separately removes DB metadata and storage rows.

### 8.3 Security constraints
- Canonical path + `starts_with()` checks prevent plugin directory traversal on read/uninstall/bundle resolution.
- Network commands require DB-backed `network` permission.
- Shell execution requires DB-backed `shell.exec` permission.
- Plugin storage commands require DB-backed `storage` permission.
- Registry/plugin download happen in Rust (`reqwest`) specifically to bypass webview CSP restrictions.

### 8.4 DB/plugin metadata contract
- `plugins.permissions_granted` stores the granted permissions JSON array.
- `save_plugin_metadata` upserts plugin id/version/name/permissions.
- `set_plugin_enabled` upserts a placeholder row if the plugin does not already exist.
- `plugin_storage` is a general KV store; `get_plugin_settings_batch` exposes only `__setting:` keys as user-visible settings.

## 9. Frontend State Architecture and Data Flow

### 9.1 Core state tree (`SessionContext`)
`SessionState` contains:
- `sessions: Record<string, SessionData>`
- `activeSessionId`
- `recentSessions`
- `defaultMode`
- per-session `executionModes`
- `autonomousSettings { commandMinFrequency, cancelDelayMs }`
- `autoApplyEnabled`
- `injectionLocks`
- `layout { root, focusedPaneId }`
- `pendingCloseSessionId`
- `skipCloseConfirm`
- `ui` subtree for context/sidebar/palette/flow/process/git/files/search/composer/left-tab/file-preview state

### 9.2 Reducer behaviors that matter
- `SESSION_UPDATED` is aggressively deduplicated: if phase, last activity, cwd, context-injected flag, label/color/group/description, agent id/model, and key metric lengths are unchanged, the reducer returns the existing state to suppress cascaded rerenders.
- `SESSION_REMOVED` removes panes displaying the session, recalculates focused pane, active session, execution-mode map, injection locks, pending-close state, and collapses most panels when no sessions remain.
- `SET_ACTIVE` will auto-create a pane if no layout exists, otherwise focus an existing pane showing that session or swap the focused pane’s bound session ID.
- Layout actions are pure-tree transforms around `PaneLeaf` / `SplitNode` structures.
- Session actions include toggles for context/sidebar/palette/flow/process/git/files/search/composer, injection locks, auto-toast, and close-confirmation workflow.

### 9.3 Provider side effects (`SessionProvider`)
On mount it:
- initializes notifications and analytics;
- subscribes to `session-updated` and `session-removed`;
- loads settings, applies theme, restores window state, and initializes default execution mode/autonomous thresholds;
- loads current backend sessions and pre-creates terminals;
- restores saved workspace if no live sessions exist;
- loads recent sessions;
- cleans up listeners/timers on unmount.

Important flows:
- Destroyed sessions are intercepted on `session-updated`; UI waits for `session-removed` rather than rendering a destroyed session.
- Auto-attach project logic watches cwd changes and attaches the first registered project whose path is an exact prefix/subdirectory match.
- Busy→idle transitions longer than 30 seconds trigger desktop notifications only if the document is hidden.
- Workspace restore is guarded against React StrictMode double-mount by module-level restore flags.
- Restore flow preserves the saved-workspace blob until restoration fully succeeds; only then is the setting cleared.
- Session creation pre-creates the terminal before starting the PTY to avoid losing early output (especially SSH/tmux startup output).

### 9.4 Derived hooks exposed by state layer
- `useSession()` -> full context value including `createSession`, `closeSession`, `requestCloseSession`, `setActive`, `saveWorkspace`
- `useActiveSession()` -> current session or null
- `useSessionList()` -> array view of sessions
- `useSidebarOrderedSessions()` -> grouped/sorted sidebar list
- `useTotalCost()` -> aggregate token cost across sessions
- `useTotalTokens()` -> aggregate input/output counts
- `useExecutionMode(sessionId)` -> session override or default mode
- `useAutonomousSettings()` -> command frequency threshold + cancel delay

## 10. React Composition and Major Component Contracts

### 10.1 Top-level composition (`App.tsx`)
`AppContent` orchestrates:
- left activity bar
- session list
- split terminal layout
- context panel
- process/git/file/search/workspace/settings/cost/update/plugin/onboarding overlays or panels
- plugin runtime and plugin panel hosts
- toast/update/worktree notifications

Key app-level listeners in `AppContent` include:
- `worktree-cleanup-summary`
- `worktree-paths-missing`
- `worktree-cleanup-failed`
- `worktree-path-deleted`
- `ai-launch-failed`
- `window` custom event `hermes:shared-worktree`

### 10.2 Major component contracts
- `TerminalPane { sessionId, phase, color }`
  - attaches/detaches the session terminal from the pool;
  - resizes via `ResizeObserver` + double-rAF;
  - syncs session phase/cwd into the terminal pool;
  - subscribes to suggestion overlay state;
  - initializes shell environment detection and history loading;
  - listens for `cwd-changed-<id>` and `command-prediction-<id>`;
  - renders `SuggestionOverlay` and `BranchMismatchAlert`.
- `SessionList { sessions, activeSessionId, onSelect, onClose, onNewSession?, onReconnect?, activeView, onViewChange, gitBadge?, pluginSessionActions?, activePluginPanel?, onPluginActionClick? }`
  - owns session list interactions, drag/drop mediation, tmux window tabs, remote/local git summary badges, and session context menus.
- `ContextPanel { session }`
  - renders context/memory/pins/tool usage/project/domain state for one session;
  - consumes `useContextState`; manages workspace path additions/removals and memory CRUD.
- `SessionGitPanel { sessionId, projectId }`
  - wraps `useGitStatus`, `getSessionWorktreeInfo`, and `GitProjectSection` for a single session-scoped worktree view.
- `WorkspacePanel { onClose }`
  - lists registered projects, scans directories/home, triggers deep rescans, and deletes projects.

## 11. Terminal Intelligence Engine

### 11.1 Suggestion scoring (`suggestionEngine.ts`)
The scoring algorithm is explicit:
- history match base: `+200`
- history frequency boost: `+min(freq*10, 200)`
- history recency boost: `+max(0, 100 - recencyIndex*2)`
- static index base: `+100`
- context relevance boost: `+150`
- exact prefix bonus: `+100`
- long command penalty after 60 chars: `-(len-60)*2`
- duplicate same command from multiple sources: keep best score and add `+50` per extra source
- top result count: `15`

### 11.2 Suggestion inputs
- `HistoryProvider`
  - max retained history: `500`
  - tracks both recency list and frequency map
  - loads shell history plus backend execution-log commands
- `ProjectContext`
  - `hasGit`
  - package manager (`npm|yarn|pnpm|bun|null`)
  - `languages[]`
  - `frameworks[]`
- Static command index categories are boosted when they match package manager/language/framework/system relevance.

### 11.3 Shell compatibility rules
- Global intelligence config fields:
  - `enabled`
  - `mode: augment|replace|off`
  - `ghostTextEnabled`
  - `overlayEnabled`
  - `projectAware`
  - `historyWeighting`
- Ghost text is suppressed in augment mode when shell integration is not active and the shell already has native autosuggestions (especially fish).
- Tab is only consumed when the overlay is visible and compatibility rules allow it.
- If shell integration is active, Hermes assumes conflicting shell autosuggestions were already neutralized and becomes more aggressive about showing/accepting suggestions.

### 11.4 TerminalPool runtime behavior
- Suggestions are computed only when:
  - intelligence is enabled
  - overlay enabled
  - input buffer is non-empty
  - `lastStablePhase` is `idle` or `shell_ready`
  - active buffer is not the alternate screen
  - user is not scrolled up
  - cached `shellIsForeground` says the shell owns the terminal foreground process group
- Debounce is applied at the terminal-pool level after every keystroke; the suggestion computation itself rechecks all prompt/foreground guards.
- Colon-prefixed intent commands bypass normal ranking and get a dedicated suggestion list with high scores.
- Overlay keyboard behavior:
  - Up/Down wrap selection
  - Tab accepts selection if allowed
  - Enter executes highlighted suggestion
  - Escape dismisses overlay
  - Right arrow accepts ghost suffix inline
- Ghost text acceptance has two modes:
  - inline append on right arrow
  - append + Enter on Tab when overlay is hidden
- Input buffer handling is surrogate-pair safe and processes pasted control chars faithfully (`Ctrl-U`, `Ctrl-C`, Enter inside pasted payloads, embedded escape sequences).

## 12. Frontend API Wrapper Surface

Thin wrappers in `src/api/` are almost all direct `invoke()` shims; naming convention is camelCase wrapper -> snake_case Tauri command.

```text
[clipboard.ts]
copyImageToClipboard(path: string) -> Promise<void> => copy_image_to_clipboard

[context.ts]
getContextPins(sessionId: string, projectId: string | null) -> Promise<ContextPin[]> => get_context_pins
addContextPin(opts: { sessionId: string | null; projectId: string | null; kind: string; target: string; label: string | null; priority: number | null; }) -> Promise<number> => add_context_pin
removeContextPin(id: number) -> Promise<void> => remove_context_pin
applyContext(sessionId: string, executionMode: string) -> Promise<ApplyContextResult> => apply_context
forkSessionContext(sourceSessionId: string, targetSessionId: string) -> Promise<number> => fork_session_context
loadHermesProjectConfig(projectId: string, projectPath: string) -> Promise<HermesProjectConfig | null> => load_hermes_project_config

[costs.ts]
getCostHistory(days: number) -> Promise<CostDailyEntry[]> => get_cost_history
getCostByProject(days: number) -> Promise<ProjectCostEntry[]> => get_cost_by_project

[git.ts]
gitStatus(sessionId: string) -> Promise<GitSessionStatus> => git_status
gitStage(sessionId: string, projectId: string, paths: string[]) -> Promise<GitOperationResult> => git_stage
gitUnstage(sessionId: string, projectId: string, paths: string[]) -> Promise<GitOperationResult> => git_unstage
gitDiscardChanges(sessionId: string, projectId: string, paths: string[]) -> Promise<GitOperationResult> => git_discard_changes
gitCommit(sessionId: string, projectId: string, message: string, authorName?: string, authorEmail?: string,) -> Promise<GitOperationResult> => git_commit
gitPush(sessionId: string, projectId: string, remote?: string) -> Promise<GitOperationResult> => git_push
gitPull(sessionId: string, projectId: string, remote?: string) -> Promise<GitOperationResult> => git_pull
gitDiff(sessionId: string, projectId: string, filePath: string, staged: boolean) -> Promise<GitDiff> => git_diff
gitOpenFile(sessionId: string, projectId: string, filePath: string) -> Promise<void> => git_open_file
gitListBranches(sessionId: string, projectId: string) -> Promise<GitBranch[]> => git_list_branches
gitListBranchesForProject(projectId: string) -> Promise<GitBranch[]> => git_list_branches_for_project
fetchRemoteBranches(projectId: string) -> Promise<GitBranch[]> => git_fetch_remote_branches
gitBranchesAheadBehind(sessionId: string, projectId: string) -> Promise<Record<string, [number, number]>> => git_branches_ahead_behind
gitCreateBranch(sessionId: string, projectId: string, name: string, checkout: boolean) -> Promise<GitOperationResult> => git_create_branch
gitCheckoutBranch(sessionId: string, projectId: string, name: string) -> Promise<GitOperationResult> => git_checkout_branch
gitDeleteBranch(sessionId: string, projectId: string, name: string, force: boolean) -> Promise<GitOperationResult> => git_delete_branch
listDirectory(sessionId: string, projectId: string, relativePath?: string) -> Promise<FileEntry[]> => list_directory
readFileContent(sessionId: string, projectId: string, filePath: string) -> Promise<FileContent> => read_file_content
writeFileContent(sessionId: string, projectId: string, filePath: string, content: string) -> Promise<number> => write_file_content
openFileInEditor(sessionId: string, projectId: string, filePath: string, editor: string | null) -> Promise<void> => open_file_in_editor
sshListDirectory(sessionId: string, path?: string) -> Promise<SshFileEntry[]> => ssh_list_directory
sshReadFile(sessionId: string, filePath: string) -> Promise<SshFileContent> => ssh_read_file
sshWriteFile(sessionId: string, filePath: string, content: string) -> Promise<void> => ssh_write_file
gitStashList(sessionId: string, projectId: string) -> Promise<GitStashEntry[]> => git_stash_list
gitStashSave(sessionId: string, projectId: string, message?: string, includeUntracked?: boolean,) -> Promise<GitOperationResult> => git_stash_save
gitStashApply(sessionId: string, projectId: string, index: number) -> Promise<GitOperationResult> => git_stash_apply
gitStashPop(sessionId: string, projectId: string, index: number) -> Promise<GitOperationResult> => git_stash_pop
gitStashDrop(sessionId: string, projectId: string, index: number) -> Promise<GitOperationResult> => git_stash_drop
gitStashClear(sessionId: string, projectId: string) -> Promise<GitOperationResult> => git_stash_clear
gitLog(sessionId: string, projectId: string, limit?: number, offset?: number) -> Promise<GitLogResult> => git_log
gitCommitDetail(sessionId: string, projectId: string, commitHash: string) -> Promise<GitCommitDetail> => git_commit_detail
gitMergeStatus(sessionId: string, projectId: string) -> Promise<MergeStatus> => git_merge_status
gitGetConflictContent(sessionId: string, projectId: string, filePath: string) -> Promise<ConflictContent> => git_get_conflict_content
gitResolveConflict(sessionId: string, projectId: string, filePath: string, strategy: ConflictStrategy, manualContent?: string,) -> Promise<GitOperationResult> => git_resolve_conflict
gitAbortMerge(sessionId: string, projectId: string) -> Promise<GitOperationResult> => git_abort_merge
gitContinueMerge(sessionId: string, projectId: string, message?: string, authorName?: string, authorEmail?: string,) -> Promise<GitOperationResult> => git_continue_merge
searchProject(sessionId: string, projectId: string, query: string, isRegex: boolean, caseSensitive: boolean, maxResults?: number,) -> Promise<SearchResponse> => search_project
createWorktree(sessionId: string, projectId: string, branchName: string, createBranch: boolean = false, fromRemote?: string,) -> Promise<WorktreeCreateResult> => git_create_worktree
removeWorktree(sessionId: string, projectId: string,) -> Promise<GitOperationResult> => git_remove_worktree
listWorktrees(projectId: string,) -> Promise<WorktreeInfo[]> => git_list_worktrees
checkBranchAvailable(projectId: string, branchName: string,) -> Promise<BranchAvailability> => git_check_branch_available
getSessionWorktreeInfo(sessionId: string, projectId: string,) -> Promise<SessionWorktree | null> => git_session_worktree_info
listBranchesForProjects(projectIds: string[]) -> Promise<Record<string, GitBranch[]>> => git_list_branches_for_projects
isGitRepo(projectId: string,) -> Promise<boolean> => git_is_git_repo
worktreeHasChanges(sessionId: string, projectId: string,) -> Promise<WorktreeChanges> => git_worktree_has_changes
stashWorktree(sessionId: string, projectId: string, message?: string,) -> Promise<GitOperationResult> => git_stash_worktree
listAllWorktrees() -> Promise<WorktreeOverviewEntry[]> => git_list_all_worktrees
detectOrphanWorktrees() -> Promise<OrphanWorktree[]> => git_detect_orphan_worktrees
worktreeDiskUsage(worktreePath: string) -> Promise<number> => git_worktree_disk_usage
cleanupOrphanWorktrees(paths: string[]) -> Promise<CleanupResult[]> => git_cleanup_orphan_worktrees

[index.ts]

[intelligence.ts]
detectShellEnvironment(sessionId: string) -> Promise<ShellEnvironment> => detect_shell_environment
readShellHistory(shell: string, limit: number) -> Promise<string[]> => read_shell_history
getSessionCommands(sessionId: string, limit: number) -> Promise<string[]> => get_session_commands
getProjectContext(path: string) -> Promise<ProjectContext> => get_project_context

[memory.ts]
saveMemory(opts: { scope: string; scopeId: string; key: string; value: string; source: string; category: string; confidence: number; }) -> Promise<void> => save_memory
getAllMemory(scope: string, scopeId: string) -> Promise<PersistedMemory[]> => get_all_memory
deleteMemory(scope: string, scopeId: string, key: string) -> Promise<void> => delete_memory
saveProjectMemory(projectId: string, key: string, value: string, source: string = "user") -> Promise<void> => helper over save_memory(scope="project")
getProjectMemory(projectId: string) -> Promise<PersistedMemory[]> => helper over get_all_memory(scope="project")

[menu.ts]
separator() -> ContextMenuItem => helper only (no IPC)
menuItem(id: string, label: string, opts?: { enabled?: boolean; accelerator?: string; checked?: boolean },) -> ContextMenuItem => helper only (no IPC)
subMenu(label: string, children: ContextMenuItem[]) -> ContextMenuItem => helper only (no IPC)
showContextMenu(items: ContextMenuItem[]) -> Promise<void> => show_context_menu
updateMenuState(updates: MenuItemUpdate[]) -> Promise<void> => update_menu_state

[processes.ts]
listProcesses() -> Promise<ProcessSnapshot> => list_processes
killProcess(pid: number, signal: string) -> Promise<void> => kill_process
killProcessTree(pid: number, signal: string) -> Promise<void> => kill_process_tree
getProcessDetail(pid: number) -> Promise<ProcessInfo> => get_process_detail
revealProcessInFinder(path: string) -> Promise<void> => reveal_process_in_finder

[projects.ts]
getProjects() -> Promise<Project[]> => get_registered_projects
isWorktreePath(path: string) -> boolean => helper only (no IPC)
createProject(path: string, name: string | null) -> Promise<Project> => create_project
deleteProject(id: string) -> Promise<void> => delete_project
getProjectsOrdered() -> Promise<ProjectOrdered[]> => get_projects_ordered
getSessionProjects(sessionId: string) -> Promise<Project[]> => get_session_projects
attachSessionProject(sessionId: string, projectId: string, role: string) -> Promise<void> => attach_session_project
detachSessionProject(sessionId: string, projectId: string) -> Promise<void> => detach_session_project
scanProject(id: string, depth: string) -> Promise<void> => scan_project
nudgeProjectContext(sessionId: string) -> Promise<void> => nudge_project_context
scanDirectory(path: string, maxDepth: number) -> Promise<void> => scan_directory
detectProject(path: string) -> Promise<void> => detect_project
assembleSessionContext(sessionId: string, tokenBudget: number) -> Promise<{ projects: ProjectContextInfo[]; estimated_tokens: number; token_budget: number }> => assemble_session_context

[promptBundle.ts]
exportPromptBundle(path: string, data: string) -> Promise<void> => export_prompt_bundle
importPromptBundle(path: string) -> Promise<string> => import_prompt_bundle

[sessions.ts]
createSession(opts: { sessionId: string | null; label: string | null; workingDirectory: string | null; color: string | null; workspacePaths: string[] | null; aiProvider: string | null; projectIds: string[] | null; autoApprove?: boolean; permissionMode?: string | null; customSuffix?: string | null; channels?: string[] | null; sshHost?: string | null; sshPort?: number | null; sshUser?: string | null; tmuxSession?: string | null; sshIdentityFile?: string | null; initialRows?: number | null; initialCols?: number | null; }) -> Promise<SessionData> => create_session
sshListTmuxSessions(host: string, port?: number, user?: string,) -> Promise<TmuxSessionEntry[]> => ssh_list_tmux_sessions
sshListTmuxWindows(host: string, tmuxSession: string, port?: number, user?: string,) -> Promise<TmuxWindowEntry[]> => ssh_list_tmux_windows
sshTmuxSelectWindow(host: string, tmuxSession: string, windowIndex: number, port?: number, user?: string,) -> Promise<void> => ssh_tmux_select_window
sshTmuxRenameWindow(host: string, tmuxSession: string, windowIndex: number, newName: string, port?: number, user?: string,) -> Promise<void> => ssh_tmux_rename_window
sshTmuxNewWindow(host: string, tmuxSession: string, port?: number, user?: string, windowName?: string,) -> Promise<void> => ssh_tmux_new_window
checkAiProviders() -> Promise<Record<string, boolean>> => check_ai_providers
closeSession(sessionId: string) -> Promise<void> => close_session
getSessions() -> Promise<SessionData[]> => get_sessions
getRecentSessions(limit: number) -> Promise<SessionHistoryEntry[]> => get_recent_sessions
getSessionSnapshot(sessionId: string) -> Promise<string | null> => get_session_snapshot
resizeSession(sessionId: string, rows: number, cols: number) -> Promise<void> => resize_session
updateSessionLabel(sessionId: string, label: string) -> Promise<void> => update_session_label
updateSessionDescription(sessionId: string, description: string) -> Promise<void> => update_session_description
updateSessionGroup(sessionId: string, group: string | null) -> Promise<void> => update_session_group
updateSessionColor(sessionId: string, color: string) -> Promise<void> => update_session_color
addWorkspacePath(sessionId: string, path: string) -> Promise<void> => add_workspace_path
removeWorkspacePath(sessionId: string, path: string) -> Promise<void> => remove_workspace_path
writeToSession(sessionId: string, data: string) -> Promise<void> => write_to_session
saveAllSnapshots() -> Promise<void> => save_all_snapshots
isShellForeground(sessionId: string) -> Promise<boolean> => is_shell_foreground
sshAddPortForward(sessionId: string, localPort: number, remoteHost: string, remotePort: number, label?: string,) -> Promise<void> => ssh_add_port_forward
sshRemovePortForward(sessionId: string, localPort: number) -> Promise<void> => ssh_remove_port_forward
sshListPortForwards(sessionId: string) -> Promise<PortForward[]> => ssh_list_port_forwards
sshGetRemoteCwd(sessionId: string) -> Promise<string> => ssh_get_remote_cwd
sshGetRemoteGitInfo(sessionId: string, remotePath: string) -> Promise<RemoteGitInfo> => ssh_get_remote_git_info
sshUploadFile(sessionId: string, localPath: string, remoteDir: string) -> Promise<void> => ssh_upload_file
sshDownloadFile(sessionId: string, remotePath: string, localPath: string) -> Promise<void> => ssh_download_file

[settings.ts]
getSettings() -> Promise<SettingsMap> => get_settings
getSetting(key: string) -> Promise<string> => get_settings
setSetting(key: string, value: string) -> Promise<void> => set_setting
exportSettings(path: string) -> Promise<void> => export_settings
importSettings(path: string) -> Promise<SettingsMap> => import_settings

[ssh.ts]
listSshSavedHosts() -> Promise<SshSavedHost[]> => list_ssh_saved_hosts
upsertSshSavedHost(host: SshSavedHost) -> Promise<void> => upsert_ssh_saved_host
deleteSshSavedHost(id: string) -> Promise<void> => delete_ssh_saved_host
```

Notes:
- `getProjects()` / `getProjectsOrdered()` filter out Hermes worktree paths client-side.
- `getSetting()` is implemented by calling `get_settings()` and selecting a key client-side rather than invoking a dedicated single-key command.
- `saveProjectMemory()` / `getProjectMemory()` are convenience wrappers over generic memory IPC.
- `menu.ts` includes pure helper constructors (`separator`, `menuItem`, `subMenu`) in addition to IPC wrappers.

## 13. Custom Hook Contracts

- `useContextState(session, executionMode?)`
  - owns assembled context, versioning, injected version, lifecycle state, last error, injected Markdown, token budget, estimated tokens;
  - performs guarded initial load, live session-field sync, project-update refresh, and apply-context orchestration;
  - avoids phantom dirty/injection states by maintaining structural-equality refs and initial-load guards.
- `useGitStatus(sessionId, enabled, pollInterval=3000)`
  - returns `{ status, error, refresh }`;
  - clears stale state when session changes;
  - drops stale async results when the session ID changes mid-request.
- `useSessionProjects(sessionId)`
  - returns `{ projects, attach, detach }`;
  - listens to both `session-projects-updated-<id>` and global `project-updated`.
- `useRemoteSshInfo(sessionId, enabled=true)`
  - polls every 8 seconds for remote cwd plus lightweight git info and returns `{ cwd, branch, changeCount, isLoading }`.
- `useFileTree(files)` builds/collapses a static file tree for context display.
- `useFileExplorer(sessionId, projectId)` provides lazy directory loading with cache, expanded/loading sets, refresh, and filtered accessors.
- Other hooks in the directory handle menu bridging, updater, plugin update checks, resize behavior, process polling, text/native context menus, git-summary caching, and toast store management.

## 14. Supplementary Takeaways

The source shows Hermes IDE is less a generic terminal wrapper and more a tightly opinionated runtime with:
- an event-rich PTY/session state machine,
- a real persistence model for context, sessions, plugins, worktrees, and token economics,
- a multi-depth project intelligence pipeline,
- a safety-conscious git/worktree layer with explicit auth and path guards,
- and a frontend architecture centered on a reducer-driven session model plus a non-React terminal pool.

The most important under-documented details were the exact IPC surface, the concrete SQLite schema/migrations, the token-budget/trimming algorithm in attunement, and the precise shell/foreground heuristics driving command suggestions. This supplement covers those gaps directly.
