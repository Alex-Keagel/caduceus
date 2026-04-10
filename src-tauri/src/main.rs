#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

pub struct AppState {
    pub config: Mutex<caduceus_core::CaduceusConfig>,
}

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
async fn session_create(request: CreateSessionRequest) -> Result<SessionInfo, String> {
    todo!()
}

#[tauri::command]
async fn session_list() -> Result<Vec<SessionInfo>, String> {
    todo!()
}

#[tauri::command]
async fn session_delete(id: String) -> Result<(), String> {
    todo!()
}

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
async fn agent_turn(request: AgentTurnRequest) -> Result<AgentTurnResponse, String> {
    todo!()
}

#[tauri::command]
async fn agent_abort(session_id: String) -> Result<(), String> {
    todo!()
}

#[tauri::command]
async fn terminal_exec(command: String) -> Result<String, String> {
    todo!()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectScanResponse {
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub total_files: usize,
}

#[tauri::command]
async fn project_scan(path: String) -> Result<ProjectScanResponse, String> {
    todo!()
}

#[tauri::command]
async fn git_status(project_root: String) -> Result<Vec<String>, String> {
    todo!()
}

#[tauri::command]
async fn git_diff(project_root: String) -> Result<String, String> {
    todo!()
}

#[tauri::command]
async fn config_get() -> Result<String, String> {
    todo!()
}

fn main() {
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
            git_status,
            git_diff,
            config_get,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
