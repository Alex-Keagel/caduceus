// TypeScript types mirroring caduceus-core Rust types

export type SessionPhase =
  | "Idle"
  | "Planning"
  | "Executing"
  | "AwaitingPermission"
  | "Summarizing"
  | "Error";

export interface SessionInfo {
  id: string;
  project_root: string;
  phase: SessionPhase;
  message_count: number;
  provider_id: string;
  model_id: string;
}

export interface TranscriptEntry {
  role: "User" | "Assistant" | "Tool" | "System";
  content: string;
  tokens?: number;
  timestamp: string;
}

export interface TokenBudget {
  context_limit: number;
  used_input: number;
  used_output: number;
  reserved_output: number;
}

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cached_tokens?: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
}

export type KanbanTokenUsage = TokenUsage;

export type CardStatus =
  | "Todo"
  | "Running"
  | "NeedsReview"
  | "Done"
  | { Blocked: string }
  | { Failed: string };

export interface GitStatusEntry {
  path: string;
  status: "New" | "Modified" | "Deleted" | "Renamed" | "Untracked" | "Conflicted";
}

export interface DiffSummary {
  path: string;
  insertions: number;
  deletions: number;
  patch: string;
}

export interface KanbanColumn {
  id: string;
  name: string;
  card_ids: string[];
  wip_limit: number | null;
}

export interface KanbanCard {
  id: string;
  title: string;
  description: string;
  column_id: string;
  status: CardStatus;
  worktree_branch: string | null;
  agent_session_id: string | null;
  dependencies: string[];
  auto_commit: boolean;
  auto_pr: boolean;
  token_usage: KanbanTokenUsage;
  created_at: string;
  completed_at: string | null;
}

export interface KanbanBoard {
  id: string;
  name: string;
  columns: KanbanColumn[];
  cards: KanbanCard[];
  created_at: string;
}

export interface ProjectScanResult {
  languages: string[];
  frameworks: string[];
  total_files: number;
  token_estimate: number;
}

export interface CaduceusConfig {
  default_provider: string;
  default_model: string;
  storage_path: string;
  log_level: string;
  max_context_tokens: number;
}

export interface AgentTurnResponse {
  content: string;
  input_tokens: number;
  output_tokens: number;
}

export interface TerminalExecResponse {
  stdout: string;
  stderr: string;
  exit_code: number;
}

export interface PermissionRequest {
  id: string;
  session_id: string;
  tool_name: string;
  description: string;
  risk_level: "Low" | "Medium" | "High" | "Critical";
}

export interface ToolCallBlock {
  id: string;
  name: string;
  input_json: string;
  result?: string;
  is_error?: boolean;
  collapsed: boolean;
}

export interface ChatMessage {
  id: string;
  role: "User" | "Assistant" | "Tool" | "System";
  content: string;
  tokens?: number;
  timestamp: string;
  tool_calls?: ToolCallBlock[];
  permission_request?: PermissionRequest;
}

export type AgentEvent =
  | { kind: "TextDelta"; text: string }
  | { kind: "ThinkingDelta"; text: string }
  | { kind: "ToolUseStart"; id: string; name: string }
  | { kind: "ToolInputDelta"; id: string; partial_json: string }
  | { kind: "ToolResult"; id: string; content: string; is_error: boolean }
  | { kind: "PermissionRequest"; request: PermissionRequest }
  | { kind: "MessageStop"; input_tokens: number; output_tokens: number; cached_tokens: number }
  | { kind: "PhaseChange"; phase: SessionPhase }
  | { kind: "Error"; message: string };

export interface PtyDataPayload {
  session_id: string;
  data: string;
}

export interface TerminalTab {
  id: string;
  title: string;
}

export type MarketplaceItemKind = "skill" | "agent" | "plugin";

export interface MarketplaceItem {
  kind: MarketplaceItemKind;
  name: string;
  description: string;
  categories: string[];
  installed: boolean;
}

export interface MarketplaceSearchResult {
  skills: MarketplaceItem[];
  agents: MarketplaceItem[];
  plugins: MarketplaceItem[];
}

export interface McpServerInfo {
  name: string;
  description: string;
  source: string;
  connected: boolean;
  status: string;
}

export type KeybindingPreset = "intellij" | "vscode" | "vim" | "emacs" | "custom";

export interface Keybinding {
  action: string;
  keys: string;
  context?: string | null;
}

export interface KeybindingConfig {
  preset: KeybindingPreset;
  overrides: Keybinding[];
}
