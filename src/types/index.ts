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
  cached_tokens: number;
}

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
