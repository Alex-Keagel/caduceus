import { invoke } from "@tauri-apps/api/core";
import type {
  SessionInfo,
  AgentTurnResponse,
  TerminalExecResponse,
  ProjectScanResult,
  GitStatusEntry,
  CaduceusConfig,
} from "../types";

// ── Session commands ──────────────────────────────────────────────────────────

export async function sessionCreate(
  projectRoot: string,
  providerId: string,
  modelId: string
): Promise<SessionInfo> {
  return invoke("session_create", {
    request: { project_root: projectRoot, provider_id: providerId, model_id: modelId },
  });
}

export async function sessionList(): Promise<SessionInfo[]> {
  return invoke("session_list");
}

export async function sessionDelete(id: string): Promise<void> {
  return invoke("session_delete", { id });
}

// ── Agent commands ────────────────────────────────────────────────────────────

export async function agentTurn(
  sessionId: string,
  userInput: string
): Promise<AgentTurnResponse> {
  return invoke("agent_turn", { request: { session_id: sessionId, user_input: userInput } });
}

export async function agentAbort(sessionId: string): Promise<void> {
  return invoke("agent_abort", { sessionId });
}

// ── Terminal commands ─────────────────────────────────────────────────────────

export async function terminalExec(
  sessionId: string,
  command: string,
  cwd?: string
): Promise<TerminalExecResponse> {
  return invoke("terminal_exec", {
    request: { session_id: sessionId, command, cwd: cwd ?? null },
  });
}

// ── Project commands ──────────────────────────────────────────────────────────

export async function projectScan(path: string): Promise<ProjectScanResult> {
  return invoke("project_scan", { path });
}

export async function projectOpen(path: string): Promise<void> {
  return invoke("project_open", { path });
}

// ── Git commands ──────────────────────────────────────────────────────────────

export async function gitStatus(projectRoot: string): Promise<GitStatusEntry[]> {
  return invoke("git_status", { projectRoot });
}

export async function gitDiff(projectRoot: string, staged: boolean): Promise<string[]> {
  return invoke("git_diff", { projectRoot, staged });
}

export async function gitCommit(projectRoot: string, message: string): Promise<string> {
  return invoke("git_commit", { projectRoot, message });
}

// ── Config commands ───────────────────────────────────────────────────────────

export async function configGet(): Promise<CaduceusConfig> {
  return invoke("config_get");
}

export async function configSetProvider(
  providerId: string,
  apiKey: string
): Promise<void> {
  return invoke("config_set_provider", { providerId, apiKey });
}
