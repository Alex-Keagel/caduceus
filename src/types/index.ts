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
