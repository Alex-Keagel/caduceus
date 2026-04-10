#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use caduceus_core::{
    CaduceusConfig, LlmMessage, ModelId, ProviderId, SessionId, SessionPhase, SessionState,
    SessionStorage,
};
use caduceus_git::{FileStatus, GitRepo};
use caduceus_orchestrator::{AgentEventEmitter, AgentHarness, ConfigLoader};
use caduceus_providers::{AnthropicAdapter, LlmAdapter, OpenAiCompatibleAdapter};
use caduceus_runtime::{BashSandbox, ExecRequest};
use caduceus_scanner::ProjectScanner;
use caduceus_storage::SqliteStorage;
use caduceus_tools::default_registry_with_root;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

const DEFAULT_SYSTEM_PROMPT: &str = "You are Caduceus, a helpful desktop coding assistant.";
const DEFAULT_CONFIG_RELATIVE_PATH: &str = ".caduceus/config.json";
const DEFAULT_DB_RELATIVE_PATH: &str = ".caduceus/db.sqlite";

pub struct AppState {
    pub storage: Arc<SqliteStorage>,
    pub config_loader: ConfigLoader,
    pub config: Mutex<CaduceusConfig>,
    pub workspace_root: PathBuf,
    pub cancellations: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub pty_manager: PtyManager,
}

struct PtySession {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send>,
}

pub struct PtyManager {
    workspace_root: PathBuf,
    sessions: Mutex<HashMap<String, PtySession>>,
}

impl PtyManager {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn create_pty(&self, app: &AppHandle, cols: u16, rows: u16) -> Result<String, String> {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system.openpty(size).map_err(|e| e.to_string())?;
        let _ = pair.master.resize(size);

        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(&self.workspace_root);
        cmd.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("failed to spawn PTY shell: {e}"))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("failed to clone PTY reader: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("failed to take PTY writer: {e}"))?;

        let pty_id = Uuid::new_v4().to_string();
        let app_handle = app.clone();
        let reader_pty_id = pty_id.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let event = PtyDataEvent {
                            pty_id: reader_pty_id.clone(),
                            data: BASE64.encode(&buffer[..n]),
                        };
                        let _ = app_handle.emit("pty:data", event);
                    }
                    Err(_) => break,
                }
            }
        });

        self.sessions
            .lock()
            .map_err(|e| format!("pty session lock poisoned: {e}"))?
            .insert(
                pty_id.clone(),
                PtySession {
                    master: pair.master,
                    writer,
                    child,
                },
            );

        Ok(pty_id)
    }

    pub fn write_pty(&self, pty_id: &str, data: &str) -> Result<(), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|e| format!("pty session lock poisoned: {e}"))?;
        let session = sessions
            .get_mut(pty_id)
            .ok_or_else(|| format!("unknown PTY id: {pty_id}"))?;
        session
            .writer
            .write_all(data.as_bytes())
            .map_err(|e| format!("failed to write PTY data: {e}"))?;
        session
            .writer
            .flush()
            .map_err(|e| format!("failed to flush PTY data: {e}"))
    }

    pub fn resize_pty(&self, pty_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|e| format!("pty session lock poisoned: {e}"))?;
        let session = sessions
            .get_mut(pty_id)
            .ok_or_else(|| format!("unknown PTY id: {pty_id}"))?;
        session
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("failed to resize PTY: {e}"))
    }

    pub fn close_pty(&self, pty_id: &str) -> Result<(), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|e| format!("pty session lock poisoned: {e}"))?;
        let mut session = sessions
            .remove(pty_id)
            .ok_or_else(|| format!("unknown PTY id: {pty_id}"))?;
        let _ = session.child.kill();
        let _ = session.child.wait();
        Ok(())
    }
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
    pub provider_id: String,
    pub model_id: String,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectScanResponse {
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub file_count: usize,
    pub total_files: usize,
    pub token_estimate: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GitStatusEntry {
    pub path: String,
    pub status: String,
    pub from: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyCreateRequest {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyCreateResponse {
    pub pty_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyWriteRequest {
    pub pty_id: String,
    pub data: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyResizeRequest {
    pub pty_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyCloseRequest {
    pub pty_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyDataEvent {
    pub pty_id: String,
    pub data: String,
}

#[tauri::command]
async fn session_create(
    state: State<'_, AppState>,
    request: CreateSessionRequest,
) -> Result<SessionInfo, String> {
    let session = SessionState::new(
        normalize_path(&request.project_root),
        ProviderId::new(request.provider_id),
        ModelId::new(request.model_id),
    );
    state.storage.create_session(&session).await.map_err(|e| e.to_string())?;
    cancellation_token(&state, &session.id.to_string())?.store(false, Ordering::SeqCst);
    Ok(session_info_from_state(&state.storage, session).await?)
}

#[tauri::command]
async fn session_list(state: State<'_, AppState>) -> Result<Vec<SessionInfo>, String> {
    let sessions = state
        .storage
        .list_sessions(1_000)
        .await
        .map_err(|e| e.to_string())?;
    let mut result = Vec::with_capacity(sessions.len());
    for session in sessions {
        result.push(session_info_from_state(&state.storage, session).await?);
    }
    Ok(result)
}

#[tauri::command]
async fn session_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let session_id = parse_session_id(&id)?;
    state
        .storage
        .delete_session(&session_id)
        .await
        .map_err(|e| e.to_string())?;
    state
        .cancellations
        .lock()
        .map_err(|e| format!("cancellation lock poisoned: {e}"))?
        .remove(&id);
    Ok(())
}

#[tauri::command]
async fn agent_turn(
    app: AppHandle,
    state: State<'_, AppState>,
    request: AgentTurnRequest,
) -> Result<AgentTurnResponse, String> {
    let session_id = parse_session_id(&request.session_id)?;
    let mut session = state
        .storage
        .load_session(&session_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("session not found: {}", request.session_id))?;

    let cancel = cancellation_token(&state, &request.session_id)?;
    cancel.store(false, Ordering::SeqCst);

    state
        .storage
        .save_message(&session.id, &LlmMessage::user(&request.user_input), None)
        .await
        .map_err(|e| e.to_string())?;

    let config = load_config(&state)?;
    let provider = build_provider(&config, &session.provider_id)?;
    let tools = default_registry_with_root(&session.project_root);
    let (emitter, mut rx) = AgentEventEmitter::channel(256);
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            let _ = app_handle.emit("agent:event", event);
        }
    });

    let input_before = session.token_budget.used_input;
    let output_before = session.token_budget.used_output;
    let harness = AgentHarness::new(
        provider,
        tools,
        config.max_context_tokens,
        DEFAULT_SYSTEM_PROMPT,
    )
    .with_emitter(emitter);

    let content = match harness.run_turn(&mut session, &request.user_input).await {
        Ok(content) => content,
        Err(error) => {
            session.phase = SessionPhase::Error;
            let _ = app.emit("agent:event", caduceus_core::AgentEvent::Error {
                message: error.to_string(),
            });
            state
                .storage
                .update_session(&session)
                .await
                .map_err(|e| e.to_string())?;
            return Err(error.to_string());
        }
    };

    let input_tokens = session.token_budget.used_input.saturating_sub(input_before);
    let output_tokens = session.token_budget.used_output.saturating_sub(output_before);

    state
        .storage
        .save_message(&session.id, &LlmMessage::assistant(&content), Some(output_tokens))
        .await
        .map_err(|e| e.to_string())?;
    state
        .storage
        .update_session(&session)
        .await
        .map_err(|e| e.to_string())?;

    Ok(AgentTurnResponse {
        content,
        input_tokens,
        output_tokens,
    })
}

#[tauri::command]
async fn agent_abort(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    cancellation_token(&state, &session_id)?.store(true, Ordering::SeqCst);
    let parsed = parse_session_id(&session_id)?;
    if let Some(mut session) = state
        .storage
        .load_session(&parsed)
        .await
        .map_err(|e| e.to_string())?
    {
        session.phase = SessionPhase::Cancelling;
        state
            .storage
            .update_session(&session)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn terminal_exec(
    state: State<'_, AppState>,
    request: TerminalExecRequest,
) -> Result<TerminalExecResponse, String> {
    let session_cwd = match parse_session_id(&request.session_id) {
        Ok(session_id) => state
            .storage
            .load_session(&session_id)
            .await
            .map_err(|e| e.to_string())?
            .map(|session| session.project_root.to_string_lossy().to_string()),
        Err(_) => None,
    };
    let sandbox = BashSandbox::new(&state.workspace_root);
    let result = sandbox
        .execute(ExecRequest {
            command: request.command,
            args: Vec::new(),
            cwd: request.cwd.or(session_cwd),
            env: HashMap::new(),
            timeout_secs: Some(30),
        })
        .await
        .map_err(|e| e.to_string())?;

    Ok(TerminalExecResponse {
        stdout: result.stdout,
        stderr: result.stderr,
        exit_code: result.exit_code,
    })
}

#[tauri::command]
async fn project_scan(
    state: State<'_, AppState>,
    path: String,
) -> Result<ProjectScanResponse, String> {
    let config = load_config(&state)?;
    let project = ProjectScanner::new(normalize_path(&path), config.max_context_tokens)
        .scan()
        .map_err(|e| e.to_string())?;
    Ok(ProjectScanResponse {
        languages: project.languages.into_iter().map(|language| language.name).collect(),
        frameworks: project.frameworks.into_iter().map(|framework| framework.name).collect(),
        file_count: project.total_files,
        total_files: project.total_files,
        token_estimate: project.token_estimate,
    })
}

#[tauri::command]
async fn git_status(project_root: String) -> Result<Vec<GitStatusEntry>, String> {
    let repo = open_git_repo(&project_root)?;
    Ok(repo
        .status()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|entry| {
            let (status, from) = match entry.status {
                FileStatus::New => ("New".to_string(), None),
                FileStatus::Modified => ("Modified".to_string(), None),
                FileStatus::Deleted => ("Deleted".to_string(), None),
                FileStatus::Renamed { from } => ("Renamed".to_string(), Some(from)),
                FileStatus::Untracked => ("Untracked".to_string(), None),
                FileStatus::Conflicted => ("Conflicted".to_string(), None),
            };
            GitStatusEntry {
                path: entry.path,
                status,
                from,
            }
        })
        .collect())
}

#[tauri::command]
async fn git_diff(project_root: String, staged: bool) -> Result<String, String> {
    let repo = open_git_repo(&project_root)?;
    repo.diff(staged).map_err(|e| e.to_string())
}

#[tauri::command]
async fn config_get(state: State<'_, AppState>) -> Result<CaduceusConfig, String> {
    load_config(&state)
}

#[tauri::command]
async fn pty_create(
    app: AppHandle,
    state: State<'_, AppState>,
    request: PtyCreateRequest,
) -> Result<PtyCreateResponse, String> {
    let pty_id = state.pty_manager.create_pty(&app, request.cols, request.rows)?;
    Ok(PtyCreateResponse { pty_id })
}

#[tauri::command]
async fn pty_write(state: State<'_, AppState>, request: PtyWriteRequest) -> Result<(), String> {
    state.pty_manager.write_pty(&request.pty_id, &request.data)
}

#[tauri::command]
async fn pty_resize(
    state: State<'_, AppState>,
    request: PtyResizeRequest,
) -> Result<(), String> {
    state
        .pty_manager
        .resize_pty(&request.pty_id, request.cols, request.rows)
}

#[tauri::command]
async fn pty_close(state: State<'_, AppState>, request: PtyCloseRequest) -> Result<(), String> {
    state.pty_manager.close_pty(&request.pty_id)
}

fn main() {
    let workspace_root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let state = build_app_state(workspace_root).expect("failed to initialize app state");

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
            pty_create,
            pty_write,
            pty_resize,
            pty_close,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn build_app_state(workspace_root: PathBuf) -> Result<AppState, String> {
    let config_path = home_path(DEFAULT_CONFIG_RELATIVE_PATH)?;
    let db_path = home_path(DEFAULT_DB_RELATIVE_PATH)?;
    let config_loader = ConfigLoader::new(&config_path);
    let config_exists = config_path.exists();
    let mut config = config_loader.load().map_err(|e| e.to_string())?;
    let should_persist_config = !config_exists || config.storage_path != db_path;
    config.storage_path = db_path.clone();
    if should_persist_config {
        config_loader.save(&config).map_err(|e| e.to_string())?;
    }

    let storage = Arc::new(SqliteStorage::open(&db_path).map_err(|e| e.to_string())?);
    Ok(AppState {
        storage,
        config_loader,
        config: Mutex::new(config),
        workspace_root: workspace_root.clone(),
        cancellations: Mutex::new(HashMap::new()),
        pty_manager: PtyManager::new(workspace_root),
    })
}

fn home_path(relative: &str) -> Result<PathBuf, String> {
    let home = env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(Path::new(&home).join(relative))
}

fn normalize_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.exists() {
        candidate.canonicalize().unwrap_or(candidate)
    } else {
        candidate
    }
}

fn parse_session_id(id: &str) -> Result<SessionId, String> {
    Uuid::parse_str(id)
        .map(SessionId)
        .map_err(|e| format!("invalid session id '{id}': {e}"))
}

fn cancellation_token(state: &AppState, session_id: &str) -> Result<Arc<AtomicBool>, String> {
    let mut cancellations = state
        .cancellations
        .lock()
        .map_err(|e| format!("cancellation lock poisoned: {e}"))?;
    Ok(cancellations
        .entry(session_id.to_string())
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone())
}

async fn session_info_from_state(
    storage: &SqliteStorage,
    session: SessionState,
) -> Result<SessionInfo, String> {
    let message_count = storage
        .list_messages(&session.id)
        .await
        .map_err(|e| e.to_string())?
        .len();
    Ok(SessionInfo {
        id: session.id.to_string(),
        project_root: session.project_root.to_string_lossy().to_string(),
        phase: format!("{:?}", session.phase),
        message_count,
        provider_id: session.provider_id.0,
        model_id: session.model_id.0,
    })
}

fn load_config(state: &AppState) -> Result<CaduceusConfig, String> {
    let config = state.config_loader.load().map_err(|e| e.to_string())?;
    let mut guard = state
        .config
        .lock()
        .map_err(|e| format!("config lock poisoned: {e}"))?;
    *guard = config.clone();
    Ok(config)
}

fn build_provider(
    config: &CaduceusConfig,
    provider_id: &ProviderId,
) -> Result<Arc<dyn LlmAdapter>, String> {
    let provider_key = provider_id.0.to_lowercase();
    if provider_key == "anthropic" {
        let api_key = env_api_key(provider_id, config)?;
        let mut adapter = AnthropicAdapter::new(api_key);
        if let Some(provider_config) = config.providers.get(&provider_id.0) {
            if let Some(base_url) = &provider_config.base_url {
                adapter = adapter.with_base_url(base_url.clone());
            }
        }
        return Ok(Arc::new(adapter));
    }

    let base_url = config
        .providers
        .get(&provider_id.0)
        .and_then(|provider| provider.base_url.clone())
        .or_else(|| default_base_url(&provider_key))
        .ok_or_else(|| format!("missing base URL for provider '{}'", provider_id.0))?;
    let api_key = if is_local_base_url(&base_url) {
        env_api_key(provider_id, config).unwrap_or_default()
    } else {
        env_api_key(provider_id, config)?
    };

    Ok(Arc::new(OpenAiCompatibleAdapter::new(
        provider_id.0.clone(),
        api_key,
        base_url,
    )))
}

fn env_api_key(configured_provider: &ProviderId, config: &CaduceusConfig) -> Result<String, String> {
    let provider_key = configured_provider.0.to_lowercase();
    let explicit_env = match provider_key.as_str() {
        "anthropic" => Some("ANTHROPIC_API_KEY".to_string()),
        "openai" => Some("OPENAI_API_KEY".to_string()),
        "groq" => Some("GROQ_API_KEY".to_string()),
        "xai" => Some("XAI_API_KEY".to_string()),
        "openrouter" => Some("OPENROUTER_API_KEY".to_string()),
        _ => None,
    };
    let fallback_env = format!(
        "{}_API_KEY",
        configured_provider
            .0
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_uppercase() } else { '_' })
            .collect::<String>()
    );
    let env_names = explicit_env
        .into_iter()
        .chain(std::iter::once(fallback_env))
        .collect::<Vec<_>>();

    for env_name in env_names {
        if let Ok(value) = env::var(&env_name) {
            if !value.trim().is_empty() {
                return Ok(value);
            }
        }
    }

    if config
        .providers
        .get(&configured_provider.0)
        .and_then(|provider| provider.base_url.as_ref())
        .is_some_and(|base_url| is_local_base_url(base_url))
    {
        return Ok(String::new());
    }

    Err(format!(
        "missing API key for provider '{}' (expected environment variable like {}_API_KEY)",
        configured_provider.0,
        configured_provider.0.to_uppercase()
    ))
}

fn default_base_url(provider_key: &str) -> Option<String> {
    match provider_key {
        "openai" => Some("https://api.openai.com/v1".to_string()),
        "groq" => Some("https://api.groq.com/openai/v1".to_string()),
        "openrouter" => Some("https://openrouter.ai/api/v1".to_string()),
        "ollama" => Some("http://127.0.0.1:11434/v1".to_string()),
        _ => None,
    }
}

fn is_local_base_url(url: &str) -> bool {
    url.contains("127.0.0.1") || url.contains("localhost")
}

fn open_git_repo(project_root: &str) -> Result<GitRepo, String> {
    GitRepo::discover(project_root)
        .or_else(|_| GitRepo::open(project_root))
        .map_err(|e| e.to_string())
}
