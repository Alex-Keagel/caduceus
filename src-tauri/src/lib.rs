use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Mutex;

// ── Shared app state ──────────────────────────────────────────────────────────

pub struct AppState {
    pub config: Mutex<caduceus_core::CaduceusConfig>,
}

// ── Session commands ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub project_root: String,
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub project_root: String,
    pub phase: String,
    pub message_count: usize,
}

#[tauri::command]
pub async fn session_create(
    request: CreateSessionRequest,
    _state: State<'_, AppState>,
) -> Result<SessionInfo, String> {
    todo!("session_create IPC command")
}

#[tauri::command]
pub async fn session_list(_state: State<'_, AppState>) -> Result<Vec<SessionInfo>, String> {
    todo!("session_list IPC command")
}

#[tauri::command]
pub async fn session_delete(
    id: String,
    _state: State<'_, AppState>,
) -> Result<(), String> {
    todo!("session_delete IPC command")
}

// ── Agent commands ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentTurnRequest {
    pub session_id: String,
    pub user_input: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentTurnResponse {
    pub content: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[tauri::command]
pub async fn agent_turn(
    request: AgentTurnRequest,
    _state: State<'_, AppState>,
) -> Result<AgentTurnResponse, String> {
    todo!("agent_turn IPC command")
}

#[tauri::command]
pub async fn agent_abort(
    session_id: String,
    _state: State<'_, AppState>,
) -> Result<(), String> {
    todo!("agent_abort IPC command")
}

// ── Terminal commands ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct TerminalExecRequest {
    pub session_id: String,
    pub command: String,
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TerminalExecResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[tauri::command]
pub async fn terminal_exec(
    request: TerminalExecRequest,
    _state: State<'_, AppState>,
) -> Result<TerminalExecResponse, String> {
    todo!("terminal_exec IPC command")
}

// ── Project commands ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectScanResponse {
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub total_files: usize,
    pub token_estimate: u32,
}

#[tauri::command]
pub async fn project_scan(
    path: String,
    _state: State<'_, AppState>,
) -> Result<ProjectScanResponse, String> {
    todo!("project_scan IPC command")
}

#[tauri::command]
pub async fn project_open(
    path: String,
    _state: State<'_, AppState>,
) -> Result<(), String> {
    todo!("project_open IPC command")
}

// ── Git commands ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct GitStatusEntry {
    pub path: String,
    pub status: String,
}

#[tauri::command]
pub async fn git_status(
    project_root: String,
    _state: State<'_, AppState>,
) -> Result<Vec<GitStatusEntry>, String> {
    todo!("git_status IPC command")
}

#[tauri::command]
pub async fn git_diff(
    project_root: String,
    staged: bool,
    _state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    todo!("git_diff IPC command")
}

#[tauri::command]
pub async fn git_commit(
    project_root: String,
    message: String,
    _state: State<'_, AppState>,
) -> Result<String, String> {
    todo!("git_commit IPC command")
}

// ── Config commands ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigResponse {
    pub default_provider: String,
    pub default_model: String,
    pub log_level: String,
}

#[tauri::command]
pub async fn config_get(
    _state: State<'_, AppState>,
) -> Result<ConfigResponse, String> {
    todo!("config_get IPC command")
}

#[tauri::command]
pub async fn config_set_provider(
    provider_id: String,
    api_key: String,
    _state: State<'_, AppState>,
) -> Result<(), String> {
    todo!("config_set_provider IPC command")
}

// ── App entry point ───────────────────────────────────────────────────────────

pub fn run() {
    let config = caduceus_core::CaduceusConfig::default();
    let state = AppState { config: Mutex::new(config) };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            session_create,
            session_list,
            session_delete,
            agent_turn,
            agent_abort,
            terminal_exec,
            project_scan,
            project_open,
            git_status,
            git_diff,
            git_commit,
            config_get,
            config_set_provider,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
