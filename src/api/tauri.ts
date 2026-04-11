import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  SessionInfo,
  AgentTurnResponse,
  TerminalExecResponse,
  ProjectScanResult,
  GitStatusEntry,
  CaduceusConfig,
  PtyDataPayload,
  AgentEvent,
  KanbanBoard,
  MarketplaceSearchResult,
  McpServerInfo,
  KeybindingConfig,
  KeybindingPreset,
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


export async function kanbanLoad(projectRoot: string): Promise<KanbanBoard> {
  return invoke("kanban_load", { projectRoot });
}

export async function kanbanAddCard(
  projectRoot: string,
  title: string,
  description?: string
): Promise<KanbanBoard> {
  return invoke("kanban_add_card", {
    request: { project_root: projectRoot, title, description: description ?? null },
  });
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

export async function keybindingsGet(): Promise<KeybindingConfig> {
  return invoke("keybindings_get");
}

export async function keybindingsSet(config: KeybindingConfig): Promise<void> {
  return invoke("keybindings_set", { config });
}

export async function keybindingsPresets(): Promise<KeybindingPreset[]> {
  return invoke("keybindings_presets");
}

// ── Marketplace commands ──────────────────────────────────────────────────────

export async function marketplaceSearch(query: string): Promise<MarketplaceSearchResult> {
  return invoke("marketplace_search", { query });
}

export async function marketplaceInstall(name: string): Promise<string> {
  return invoke("marketplace_install", { name });
}

export async function marketplaceRecommend(): Promise<MarketplaceSearchResult> {
  return invoke("marketplace_recommend");
}

export async function mcpStatus(): Promise<McpServerInfo[]> {
  return invoke("mcp_status");
}

export async function mcpAdd(name: string): Promise<string> {
  return invoke("mcp_add", { name });
}

// ── PTY commands ──────────────────────────────────────────────────────────────

export async function ptyCreate(sessionId: string): Promise<void> {
  return invoke("pty_create", { sessionId });
}

export async function ptyWrite(sessionId: string, data: string): Promise<void> {
  return invoke("pty_write", { sessionId, data });
}

export async function ptyResize(sessionId: string, cols: number, rows: number): Promise<void> {
  return invoke("pty_resize", { sessionId, cols, rows });
}

// ── Permission commands ───────────────────────────────────────────────────────

export async function permissionRespond(requestId: string, allow: boolean): Promise<void> {
  return invoke("permission_respond", { requestId, allow });
}

// ── File-level diff ───────────────────────────────────────────────────────────

export async function gitFileDiff(projectRoot: string, filePath: string): Promise<string> {
  return invoke("git_file_diff", { projectRoot, filePath });
}

// ── Event listeners ───────────────────────────────────────────────────────────

export async function listenPtyData(handler: (payload: PtyDataPayload) => void): Promise<UnlistenFn> {
  return listen<PtyDataPayload>("pty-data", (event) => handler(event.payload));
}

export async function listenAgentEvent(handler: (event: AgentEvent) => void): Promise<UnlistenFn> {
  return listen<AgentEvent>("agent-event", (event) => handler(event.payload));
}
