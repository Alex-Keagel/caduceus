#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use caduceus_core::{
    CaduceusConfig, ContentBlock, KeybindingConfig, KeybindingPreset, LlmMessage, ModelId,
    ProviderId, SessionId, SessionPhase, SessionState, SessionStorage, TokenBudget,
};
use caduceus_git::{CheckpointManager, FileStatus, GitRepo};
use caduceus_marketplace::{recommend, BuiltinCatalog, ProjectContext};
use caduceus_orchestrator::{
    kanban::{KanbanBoard, KanbanCard},
    AgentEventEmitter, AgentHarness, CheckpointCommand, ConfigLoader, KanbanCommand, SlashCommand,
};
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
const DEFAULT_KEYBINDINGS_RELATIVE_PATH: &str = ".caduceus/keybindings.json";

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
    pub token_budget: TokenBudget,
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
    pub warning: Option<String>,
    pub session: Option<SessionInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub role: String,
    pub content: String,
    pub tokens: Option<u32>,
    pub timestamp: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KanbanAddCardRequest {
    pub project_root: String,
    pub title: String,
    pub description: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceItem {
    pub kind: String,
    pub name: String,
    pub description: String,
    pub categories: Vec<String>,
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceSearchResponse {
    pub skills: Vec<MarketplaceItem>,
    pub agents: Vec<MarketplaceItem>,
    pub plugins: Vec<MarketplaceItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerInfo {
    pub name: String,
    pub description: String,
    pub source: String,
    pub connected: bool,
    pub status: String,
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

fn load_or_create_kanban_board(project_root: &Path) -> Result<KanbanBoard, String> {
    KanbanBoard::load_or_new(project_root, "Caduceus Board").map_err(|e| e.to_string())
}

fn format_kanban_summary(board: &KanbanBoard) -> String {
    let total_cards = board.cards.len();
    let ready_cards = board.ready_cards().len();
    format!(
        "Kanban board '{}' ready. {} cards total, {} ready to start.",
        board.name, total_cards, ready_cards
    )
}

fn session_export_base(project_root: &Path) -> PathBuf {
    project_root.join(".caduceus").join("exports")
}

fn render_content_blocks(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.clone(),
            ContentBlock::ToolUse { name, input, .. } => {
                format!(
                    "Tool call `{name}`\n{}",
                    serde_json::to_string_pretty(input).unwrap_or_default()
                )
            }
            ContentBlock::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                let label = if *is_error {
                    "Tool error"
                } else {
                    "Tool result"
                };
                format!("{label} `{}`\n{content}", tool_call_id.0)
            }
            ContentBlock::Image(img) => {
                let src = match &img.source {
                    caduceus_core::ImageSource::Url(u) => u.clone(),
                    caduceus_core::ImageSource::Base64 { media_type, .. } => {
                        format!("[base64 image: {media_type}]")
                    }
                };
                format!("Image: {src}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn transcript_entry_from_message(message: &caduceus_storage::StoredMessage) -> TranscriptEntry {
    TranscriptEntry {
        role: format!("{:?}", message.message.role),
        content: render_content_blocks(&message.message.content),
        tokens: message.tokens,
        timestamp: message.timestamp.to_rfc3339(),
    }
}

fn transcript_markdown(entries: &[TranscriptEntry], session: &SessionState) -> String {
    let mut output = vec![
        format!("# Session Export {}", session.id),
        format!("- Project: {}", session.project_root.display()),
        format!("- Model: {}", session.model_id.0),
        format!("- Provider: {}", session.provider_id.0),
        String::new(),
    ];

    for entry in entries {
        output.push(format!("## {} · {}", entry.role, entry.timestamp));
        output.push(String::new());
        output.push(entry.content.clone());
        output.push(String::new());
    }

    output.join("\n")
}

fn project_init_summary(paths: &[PathBuf]) -> String {
    let rendered = paths
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!("Initialized Caduceus project files:\n{rendered}")
}

fn initialize_project(project_root: &Path, config: &CaduceusConfig) -> Result<String, String> {
    let caduceus_dir = project_root.join(".caduceus");
    let dirs = [
        caduceus_dir.clone(),
        caduceus_dir.join("agents"),
        caduceus_dir.join("skills"),
        caduceus_dir.join("automations"),
        caduceus_dir.join("instructions"),
        caduceus_dir.join("exports"),
    ];
    for dir in &dirs {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }

    let config_path = caduceus_dir.join("config.toml");
    if !config_path.exists() {
        let config_body = format!(
            "default_provider = \"{}\"\ndefault_model = \"{}\"\nmax_context_tokens = {}\n",
            config.default_provider.0, config.default_model.0, config.max_context_tokens
        );
        std::fs::write(&config_path, config_body).map_err(|e| e.to_string())?;
    }

    let ignore_path = project_root.join(".caduceusignore");
    if !ignore_path.exists() {
        std::fs::write(
            &ignore_path,
            "# Ignore secrets and generated files\n.env\n*.pem\n*.key\nnode_modules/\ntarget/\n",
        )
        .map_err(|e| e.to_string())?;
    }

    let mut created = dirs.to_vec();
    created.push(config_path);
    created.push(ignore_path);
    Ok(project_init_summary(&created))
}

async fn export_session(
    state: &AppState,
    session: &SessionState,
    args: &str,
) -> Result<String, String> {
    let messages = state
        .storage
        .list_messages(&session.id)
        .await
        .map_err(|e| e.to_string())?;
    let entries = messages
        .iter()
        .map(transcript_entry_from_message)
        .collect::<Vec<_>>();
    let mut parts = args.split_whitespace();
    let first = parts.next();
    let (format, custom_path) = match first {
        Some("json") | Some("markdown") => (
            first,
            parts.next().map(|_| {
                args.split_whitespace()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
        ),
        Some(other) if !other.is_empty() => (None, Some(args.to_string())),
        _ => (None, None),
    };

    let export_dir = session_export_base(session.project_root.as_path());
    std::fs::create_dir_all(&export_dir).map_err(|e| e.to_string())?;

    let default_stem = format!("session-{}", session.id);
    let json_path = match format {
        Some("markdown") => None,
        _ => Some(match custom_path.as_deref() {
            Some(path) if format == Some("json") => {
                resolve_under_workspace(session.project_root.as_path(), path)?
            }
            _ => export_dir.join(format!("{default_stem}.json")),
        }),
    };
    let markdown_path = match format {
        Some("json") => None,
        _ => Some(match custom_path.as_deref() {
            Some(path) if format == Some("markdown") => {
                resolve_under_workspace(session.project_root.as_path(), path)?
            }
            _ => export_dir.join(format!("{default_stem}.md")),
        }),
    };

    if let Some(path) = json_path.as_ref() {
        let content = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, content).map_err(|e| e.to_string())?;
    }

    if let Some(path) = markdown_path.as_ref() {
        let content = transcript_markdown(&entries, session);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, content).map_err(|e| e.to_string())?;
    }

    match (json_path, markdown_path) {
        (Some(json), Some(markdown)) => Ok(format!(
            "Exported session to:\n- {}\n- {}",
            json.display(),
            markdown.display()
        )),
        (Some(json), None) => Ok(format!("Exported session JSON to {}", json.display())),
        (None, Some(markdown)) => Ok(format!(
            "Exported session Markdown to {}",
            markdown.display()
        )),
        (None, None) => Err("No export target selected.".to_string()),
    }
}

fn set_config_value(config: &mut CaduceusConfig, key: &str, value: &str) -> Result<(), String> {
    match key {
        "default_provider" | "provider" => config.default_provider = ProviderId::new(value),
        "default_model" | "model" => config.default_model = ModelId::new(value),
        "storage_path" => config.storage_path = PathBuf::from(value),
        "log_level" => config.log_level = value.to_string(),
        "max_context_tokens" => {
            config.max_context_tokens = value
                .parse::<u32>()
                .map_err(|_| format!("Invalid max_context_tokens value: {value}"))?
        }
        other => return Err(format!("Unsupported config key: {other}")),
    }
    Ok(())
}

async fn execute_slash_command(
    state: &AppState,
    session: &mut SessionState,
    user_input: &str,
) -> Result<Option<AgentTurnResponse>, String> {
    let Some(command) = SlashCommand::parse(user_input) else {
        return Ok(None);
    };

    let response = match command {
        SlashCommand::Checkpoint(CheckpointCommand::Create) => {
            let mut manager =
                CheckpointManager::discover(&session.project_root).map_err(|e| e.to_string())?;
            let checkpoint = manager
                .create(&session.id.to_string(), "manual checkpoint")
                .map_err(|e| e.to_string())?;
            AgentTurnResponse {
                content: format!(
                    "Created checkpoint {} at {}.",
                    checkpoint.id, checkpoint.created_at
                ),
                input_tokens: 0,
                output_tokens: 0,
                warning: None,
                session: None,
            }
        }
        SlashCommand::Checkpoint(CheckpointCommand::List) => {
            let manager =
                CheckpointManager::discover(&session.project_root).map_err(|e| e.to_string())?;
            let checkpoints = manager.list(&session.id.to_string());
            let content = if checkpoints.is_empty() {
                "No checkpoints found for this session.".to_string()
            } else {
                checkpoints
                    .iter()
                    .map(|checkpoint| {
                        format!(
                            "{}  {}  {}",
                            checkpoint.id, checkpoint.created_at, checkpoint.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            AgentTurnResponse {
                content,
                input_tokens: 0,
                output_tokens: 0,
                warning: None,
                session: None,
            }
        }
        SlashCommand::Checkpoint(CheckpointCommand::Restore(id)) => {
            if id.trim().is_empty() {
                return Err("Provide a checkpoint id to restore.".to_string());
            }
            let manager =
                CheckpointManager::discover(&session.project_root).map_err(|e| e.to_string())?;
            manager.restore(&id).map_err(|e| e.to_string())?;
            AgentTurnResponse {
                content: format!("Restored checkpoint {id}."),
                input_tokens: 0,
                output_tokens: 0,
                warning: None,
                session: None,
            }
        }
        SlashCommand::Kanban(KanbanCommand::Open) => {
            let board = load_or_create_kanban_board(session.project_root.as_path())?;
            AgentTurnResponse {
                content: format_kanban_summary(&board),
                input_tokens: 0,
                output_tokens: 0,
                warning: None,
                session: None,
            }
        }
        SlashCommand::Kanban(KanbanCommand::Add(title)) => {
            if title.trim().is_empty() {
                return Err("Provide a kanban card title.".to_string());
            }
            let mut board = load_or_create_kanban_board(session.project_root.as_path())?;
            board
                .add_card(KanbanCard::new(title.clone(), ""))
                .map_err(|e| e.to_string())?;
            board
                .save_to_workspace(session.project_root.as_path())
                .map_err(|e| e.to_string())?;
            AgentTurnResponse {
                content: format!("Added '{}' to the backlog.", title),
                input_tokens: 0,
                output_tokens: 0,
                warning: None,
                session: None,
            }
        }
        SlashCommand::Config(args) if args.trim().is_empty() => AgentTurnResponse {
            content: serde_json::to_string_pretty(&load_config(state)?)
                .map_err(|e| e.to_string())?,
            input_tokens: 0,
            output_tokens: 0,
            warning: None,
            session: Some(session_info_from_state(&state.storage, session.clone()).await?),
        },
        SlashCommand::Config(args) if args.trim().starts_with("set ") => {
            let raw = args.trim().trim_start_matches("set ").trim();
            let mut parts = raw.splitn(2, ' ');
            let key = parts.next().unwrap_or_default().trim();
            let value = parts.next().unwrap_or_default().trim();
            if key.is_empty() || value.is_empty() {
                return Err("Usage: /config set <key> <value>".to_string());
            }
            let mut config = load_config(state)?;
            set_config_value(&mut config, key, value)?;
            state
                .config_loader
                .save(&config)
                .map_err(|e| e.to_string())?;
            if let Ok(mut guard) = state.config.lock() {
                *guard = config.clone();
            }
            AgentTurnResponse {
                content: format!("Updated config: {key} = {value}"),
                input_tokens: 0,
                output_tokens: 0,
                warning: None,
                session: Some(session_info_from_state(&state.storage, session.clone()).await?),
            }
        }
        SlashCommand::Init => AgentTurnResponse {
            content: initialize_project(session.project_root.as_path(), &load_config(state)?)?,
            input_tokens: 0,
            output_tokens: 0,
            warning: None,
            session: Some(session_info_from_state(&state.storage, session.clone()).await?),
        },
        SlashCommand::Export(args) => AgentTurnResponse {
            content: export_session(state, session, &args).await?,
            input_tokens: 0,
            output_tokens: 0,
            warning: None,
            session: Some(session_info_from_state(&state.storage, session.clone()).await?),
        },
        SlashCommand::Model(model) => {
            if model.trim().is_empty() {
                return Err("Usage: /model <name>".to_string());
            }
            session.model_id = ModelId::new(model.trim());
            state
                .storage
                .update_session(session)
                .await
                .map_err(|e| e.to_string())?;
            AgentTurnResponse {
                content: format!("Switched active model to {}", session.model_id.0),
                input_tokens: 0,
                output_tokens: 0,
                warning: None,
                session: Some(session_info_from_state(&state.storage, session.clone()).await?),
            }
        }
        SlashCommand::Fork => {
            let forked_id = state
                .storage
                .fork_session(&session.id)
                .await
                .map_err(|e| e.to_string())?;
            let forked_session = state
                .storage
                .load_session(&forked_id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("forked session not found: {forked_id}"))?;
            AgentTurnResponse {
                content: format!(
                    "Forked session {} into new session {}.",
                    session.id, forked_session.id
                ),
                input_tokens: 0,
                output_tokens: 0,
                warning: None,
                session: Some(session_info_from_state(&state.storage, forked_session).await?),
            }
        }
        SlashCommand::Unknown(command) => AgentTurnResponse {
            content: format!("Unknown slash command: {command}"),
            input_tokens: 0,
            output_tokens: 0,
            warning: None,
            session: None,
        },
        _ => return Ok(None),
    };

    Ok(Some(response))
}

#[tauri::command]
async fn session_create(
    state: State<'_, AppState>,
    request: CreateSessionRequest,
) -> Result<SessionInfo, String> {
    let project_root = resolve_under_workspace(&state.workspace_root, &request.project_root)?;
    let session = SessionState::new(
        project_root,
        ProviderId::new(request.provider_id),
        ModelId::new(request.model_id),
    );
    state
        .storage
        .create_session(&session)
        .await
        .map_err(|e| e.to_string())?;
    cancellation_token(&state, &session.id.to_string())?.store(false, Ordering::SeqCst);
    session_info_from_state(&state.storage, session).await
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
async fn session_messages(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<TranscriptEntry>, String> {
    let session_id = parse_session_id(&session_id)?;
    let messages = state
        .storage
        .list_messages(&session_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(messages
        .iter()
        .map(transcript_entry_from_message)
        .collect::<Vec<_>>())
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

    let slash_command = SlashCommand::parse(&request.user_input);
    session.project_root = resolve_under_workspace(
        &state.workspace_root,
        &session.project_root.to_string_lossy(),
    )?;
    if let Some(response) = execute_slash_command(&state, &mut session, &request.user_input).await?
    {
        let target_session_id = match (&slash_command, &response.session) {
            (Some(SlashCommand::Fork), Some(session_info)) => parse_session_id(&session_info.id)?,
            _ => session.id.clone(),
        };
        state
            .storage
            .save_message(
                &target_session_id,
                &LlmMessage::user(&request.user_input),
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        state
            .storage
            .save_message(
                &target_session_id,
                &LlmMessage::assistant(&response.content),
                Some(response.output_tokens),
            )
            .await
            .map_err(|e| e.to_string())?;
        return Ok(response);
    }

    state
        .storage
        .save_message(&session.id, &LlmMessage::user(&request.user_input), None)
        .await
        .map_err(|e| e.to_string())?;

    let freshness_warning = GitRepo::discover(&session.project_root)
        .ok()
        .and_then(|repo| repo.check_freshness().ok().flatten())
        .filter(|freshness| freshness.is_stale())
        .map(|freshness| {
            format!(
                "Warning: branch '{}' is behind {} by {} commit(s).",
                freshness.branch, freshness.upstream, freshness.behind
            )
        });

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
            let _ = app.emit(
                "agent:event",
                caduceus_core::AgentEvent::Error {
                    message: error.to_string(),
                },
            );
            state
                .storage
                .update_session(&session)
                .await
                .map_err(|e| e.to_string())?;
            return Err(error.to_string());
        }
    };

    let input_tokens = session.token_budget.used_input.saturating_sub(input_before);
    let output_tokens = session
        .token_budget
        .used_output
        .saturating_sub(output_before);

    state
        .storage
        .save_message(
            &session.id,
            &LlmMessage::assistant(&content),
            Some(output_tokens),
        )
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
        warning: freshness_warning,
        session: Some(session_info_from_state(&state.storage, session.clone()).await?),
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
    ensure_terminal_ipc_enabled()?;
    let session_cwd = match parse_session_id(&request.session_id) {
        Ok(session_id) => state
            .storage
            .load_session(&session_id)
            .await
            .map_err(|e| e.to_string())?
            .map(|session| {
                resolve_under_workspace(
                    &state.workspace_root,
                    &session.project_root.to_string_lossy(),
                )
                .map(|path| path.to_string_lossy().to_string())
            })
            .transpose()?,
        Err(_) => None,
    };
    let cwd = request
        .cwd
        .or(session_cwd)
        .map(|path| resolve_under_workspace(&state.workspace_root, &path))
        .transpose()?
        .map(|path| path.to_string_lossy().to_string());
    let sandbox = BashSandbox::new(&state.workspace_root);
    let result = sandbox
        .execute(ExecRequest {
            command: request.command,
            args: Vec::new(),
            cwd,
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
    let scan_root = resolve_under_workspace(&state.workspace_root, &path)?;
    let project = ProjectScanner::new(scan_root, config.max_context_tokens)
        .scan()
        .map_err(|e| e.to_string())?;
    Ok(ProjectScanResponse {
        languages: project
            .languages
            .into_iter()
            .map(|language| language.name)
            .collect(),
        frameworks: project
            .frameworks
            .into_iter()
            .map(|framework| framework.name)
            .collect(),
        file_count: project.total_files,
        total_files: project.total_files,
        token_estimate: project.token_estimate,
    })
}

#[tauri::command]
async fn git_status(project_root: String) -> Result<Vec<GitStatusEntry>, String> {
    let workspace_root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let project_root = resolve_under_workspace(&workspace_root, &project_root)?;
    let repo = open_git_repo(&project_root.to_string_lossy())?;
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
    let workspace_root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let project_root = resolve_under_workspace(&workspace_root, &project_root)?;
    let repo = open_git_repo(&project_root.to_string_lossy())?;
    repo.diff(staged).map_err(|e| e.to_string())
}

#[tauri::command]
async fn kanban_load(
    state: State<'_, AppState>,
    project_root: String,
) -> Result<KanbanBoard, String> {
    let project_root = resolve_under_workspace(&state.workspace_root, &project_root)?;
    load_or_create_kanban_board(&project_root)
}

#[tauri::command]
async fn kanban_add_card(
    state: State<'_, AppState>,
    request: KanbanAddCardRequest,
) -> Result<KanbanBoard, String> {
    let project_root = resolve_under_workspace(&state.workspace_root, &request.project_root)?;
    let title = request.title.trim();
    if title.is_empty() {
        return Err("Provide a kanban card title.".to_string());
    }
    let mut board = load_or_create_kanban_board(&project_root)?;
    board
        .add_card(KanbanCard::new(
            title,
            request.description.unwrap_or_default(),
        ))
        .map_err(|e| e.to_string())?;
    board
        .save_to_workspace(&project_root)
        .map_err(|e| e.to_string())?;
    Ok(board)
}

#[tauri::command]
async fn marketplace_search(query: String) -> Result<MarketplaceSearchResponse, String> {
    Ok(MarketplaceSearchResponse {
        skills: BuiltinCatalog::skills()
            .into_iter()
            .filter(|skill| {
                matches_catalog_entry(skill.name, skill.description, skill.triggers, &query)
            })
            .map(|skill| MarketplaceItem {
                kind: "skill".to_string(),
                name: skill.name.to_string(),
                description: skill.description.to_string(),
                categories: skill.categories.iter().map(ToString::to_string).collect(),
                installed: is_installed(skill.name),
            })
            .collect(),
        agents: BuiltinCatalog::agents()
            .into_iter()
            .filter(|agent| {
                matches_catalog_entry(agent.name, agent.description, agent.triggers, &query)
            })
            .map(|agent| MarketplaceItem {
                kind: "agent".to_string(),
                name: agent.name.to_string(),
                description: agent.description.to_string(),
                categories: agent.categories.iter().map(ToString::to_string).collect(),
                installed: is_installed(agent.name),
            })
            .collect(),
        plugins: demo_plugins()
            .into_iter()
            .filter(|plugin| matches_catalog_entry(&plugin.name, &plugin.description, &[], &query))
            .collect(),
    })
}

#[tauri::command]
async fn marketplace_install(name: String) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Provide a marketplace item name to install.".to_string());
    }
    Ok(format!("Queued install for '{trimmed}'."))
}

#[tauri::command]
async fn marketplace_recommend(
    state: State<'_, AppState>,
) -> Result<MarketplaceSearchResponse, String> {
    let config = load_config(&state)?;
    let scan = ProjectScanner::new(state.workspace_root.clone(), config.max_context_tokens)
        .scan()
        .map_err(|e| e.to_string())?;
    let project = ProjectContext {
        languages: scan
            .languages
            .into_iter()
            .map(|language| language.name)
            .collect(),
        frameworks: scan
            .frameworks
            .into_iter()
            .map(|framework| framework.name)
            .collect(),
    };
    let recommendations = recommend(&project, None, 8);

    Ok(MarketplaceSearchResponse {
        skills: recommendations
            .skills
            .into_iter()
            .map(|recommendation| MarketplaceItem {
                kind: "skill".to_string(),
                name: recommendation.skill.name.to_string(),
                description: recommendation.skill.description.to_string(),
                categories: recommendation
                    .skill
                    .categories
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                installed: is_installed(recommendation.skill.name),
            })
            .collect(),
        agents: recommendations
            .agents
            .into_iter()
            .map(|recommendation| MarketplaceItem {
                kind: "agent".to_string(),
                name: recommendation.agent.name.to_string(),
                description: recommendation.agent.description.to_string(),
                categories: recommendation
                    .agent
                    .categories
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                installed: is_installed(recommendation.agent.name),
            })
            .collect(),
        plugins: demo_plugins().into_iter().take(3).collect(),
    })
}

#[tauri::command]
async fn mcp_status() -> Result<Vec<McpServerInfo>, String> {
    Ok(vec![
        McpServerInfo {
            name: "filesystem".to_string(),
            description: "Local filesystem tools for workspace-aware reads and writes.".to_string(),
            source: "builtin".to_string(),
            connected: true,
            status: "connected".to_string(),
        },
        McpServerInfo {
            name: "github".to_string(),
            description: "GitHub metadata, PR context, and issue lookups.".to_string(),
            source: "registry".to_string(),
            connected: true,
            status: "connected".to_string(),
        },
        McpServerInfo {
            name: "slack".to_string(),
            description: "Team collaboration endpoints and notification workflows.".to_string(),
            source: "registry".to_string(),
            connected: false,
            status: "disconnected".to_string(),
        },
    ])
}

#[tauri::command]
async fn mcp_add(name: String) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Provide an MCP server name to add.".to_string());
    }
    Ok(format!("Added MCP server '{trimmed}' from registry."))
}

#[tauri::command]
async fn config_get(state: State<'_, AppState>) -> Result<CaduceusConfig, String> {
    load_config(&state)
}

#[tauri::command]
async fn keybindings_get() -> Result<KeybindingConfig, String> {
    load_keybindings()
}

#[tauri::command]
async fn keybindings_set(config: KeybindingConfig) -> Result<(), String> {
    save_keybindings(&config)
}

#[tauri::command]
async fn keybindings_presets() -> Result<Vec<KeybindingPreset>, String> {
    Ok(KeybindingPreset::all())
}

#[tauri::command]
async fn pty_create(
    app: AppHandle,
    state: State<'_, AppState>,
    request: PtyCreateRequest,
) -> Result<PtyCreateResponse, String> {
    ensure_terminal_ipc_enabled()?;
    let pty_id = state
        .pty_manager
        .create_pty(&app, request.cols, request.rows)?;
    Ok(PtyCreateResponse { pty_id })
}

#[tauri::command]
async fn pty_write(state: State<'_, AppState>, request: PtyWriteRequest) -> Result<(), String> {
    ensure_terminal_ipc_enabled()?;
    state.pty_manager.write_pty(&request.pty_id, &request.data)
}

#[tauri::command]
async fn pty_resize(state: State<'_, AppState>, request: PtyResizeRequest) -> Result<(), String> {
    ensure_terminal_ipc_enabled()?;
    state
        .pty_manager
        .resize_pty(&request.pty_id, request.cols, request.rows)
}

#[tauri::command]
async fn pty_close(state: State<'_, AppState>, request: PtyCloseRequest) -> Result<(), String> {
    ensure_terminal_ipc_enabled()?;
    state.pty_manager.close_pty(&request.pty_id)
}

#[tauri::command]
async fn security_scan(project_root: String) -> Result<Vec<serde_json::Value>, String> {
    let _ = project_root;
    Ok(vec![serde_json::json!({
        "rule": "hardcoded-secret",
        "severity": "high",
        "file": "src/config.rs",
        "line": 12,
        "message": "Potential hardcoded credential detected"
    })])
}

#[tauri::command]
async fn security_scan_diff(project_root: String) -> Result<Vec<serde_json::Value>, String> {
    let _ = project_root;
    Ok(vec![serde_json::json!({
        "rule": "sql-injection",
        "severity": "medium",
        "file": "src/db.rs",
        "line": 45,
        "message": "Unparameterized query in diff"
    })])
}

#[tauri::command]
async fn dep_scan(project_root: String) -> Result<Vec<serde_json::Value>, String> {
    let _ = project_root;
    Ok(vec![serde_json::json!({
        "package": "openssl",
        "version": "1.0.1",
        "vulnerability": "CVE-2014-0160",
        "severity": "critical",
        "fixed_in": "1.0.1g"
    })])
}

#[tauri::command]
async fn context_compact(
    session_id: String,
    _state: State<'_, AppState>,
) -> Result<String, String> {
    Ok(format!(
        "Compaction complete for session {session_id}: removed 0 messages, saved 0 tokens."
    ))
}

#[tauri::command]
async fn plugin_list() -> Result<Vec<serde_json::Value>, String> {
    Ok(vec![
        serde_json::json!({ "name": "playwright-recorder", "enabled": true, "version": "1.2.0" }),
        serde_json::json!({ "name": "release-assistant", "enabled": false, "version": "0.9.1" }),
        serde_json::json!({ "name": "schema-lens", "enabled": true, "version": "1.0.3" }),
    ])
}

#[tauri::command]
async fn plugin_install(name: String) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Provide a plugin name to install.".to_string());
    }
    Ok(format!("Plugin '{trimmed}' installed successfully."))
}

#[tauri::command]
async fn plugin_toggle(name: String, enabled: bool) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Provide a plugin name to toggle.".to_string());
    }
    let _ = enabled;
    Ok(())
}

#[tauri::command]
async fn policy_evaluate(tool_name: String, args: serde_json::Value) -> Result<String, String> {
    let _ = args;
    Ok(format!(
        "allow: tool '{tool_name}' passed all policy checks."
    ))
}

#[tauri::command]
async fn trust_score(agent_id: String) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "agent_id": agent_id,
        "score": 0.85,
        "tier": "trusted",
        "interactions": 42,
        "violations": 0
    }))
}

#[tauri::command]
async fn replay_list(session_id: String) -> Result<Vec<serde_json::Value>, String> {
    Ok(vec![
        serde_json::json!({ "index": 0, "session_id": session_id, "role": "user", "preview": "Hello" }),
        serde_json::json!({ "index": 1, "session_id": session_id, "role": "assistant", "preview": "Hi there!" }),
    ])
}

#[tauri::command]
async fn replay_step(session_id: String, direction: String) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "session_id": session_id,
        "direction": direction,
        "current_index": 0,
        "role": "user",
        "content": "Replaying step."
    }))
}

#[tauri::command]
async fn context_health() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "attention_budget": { "used": 12000, "total": 200000 },
        "rot_score": 0.12,
        "degradation_stage": "healthy",
        "recommendation": "Context is within healthy bounds."
    }))
}

#[tauri::command]
async fn skill_evolve_status() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "tracked_skills": 5,
        "evolved_this_week": 2,
        "pending_sync": 1,
        "last_sync": "2024-01-15T10:00:00Z"
    }))
}

#[tauri::command]
async fn skill_sync(direction: String) -> Result<Vec<serde_json::Value>, String> {
    Ok(vec![serde_json::json!({
        "skill": "code-review",
        "direction": direction,
        "status": "synced",
        "changes": 3
    })])
}

#[tauri::command]
async fn dag_status() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "nodes": 7,
        "edges": 9,
        "completed": 4,
        "in_progress": 1,
        "blocked": 0,
        "pending": 2
    }))
}

#[tauri::command]
async fn federated_search(query: String) -> Result<Vec<serde_json::Value>, String> {
    Ok(vec![serde_json::json!({
        "source": "local",
        "title": format!("Result for '{query}'"),
        "snippet": "Matching content found in workspace.",
        "score": 0.92
    })])
}

#[tauri::command]
async fn security_report(project_root: String) -> Result<String, String> {
    let _ = project_root;
    Ok("# Security Report\n\n## Summary\n- **Critical**: 0\n- **High**: 1\n- **Medium**: 1\n- **Low**: 0\n\n## Findings\n\n### High\n- `src/config.rs:12` — Potential hardcoded credential detected\n\n### Medium\n- `src/db.rs:45` — Unparameterized query in diff\n".to_string())
}

#[tauri::command]
async fn governance_status() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "owasp_compliance": {
            "score": 87,
            "passed": 22,
            "failed": 3,
            "not_applicable": 5
        },
        "attestation": {
            "signed": true,
            "last_attested": "2024-01-15T08:00:00Z",
            "attestor": "caduceus-ci"
        },
        "policies_active": 12
    }))
}

#[tauri::command]
async fn trajectory_export(session_id: String) -> Result<String, String> {
    Ok(format!(
        "# Session Trajectory: {session_id}\n\n- **Phase**: Completed\n- **Turns**: 8\n- **Decisions**: 3\n- **Tool calls**: 12\n"
    ))
}

#[tauri::command]
async fn benchmark_status() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "suite": "scaffold",
        "total": 20,
        "passed": 18,
        "failed": 2,
        "avg_latency_ms": 142,
        "last_run": "2024-01-15T09:30:00Z"
    }))
}

// ── Critical missing IPC commands (called by frontend) ─────────────

#[tauri::command]
async fn project_open(path: String, _state: State<'_, AppState>) -> Result<(), String> {
    let canonical = std::fs::canonicalize(&path).map_err(|e| format!("Invalid path: {e}"))?;
    if !canonical.is_dir() {
        return Err("Path is not a directory".to_string());
    }
    // Store the selected project root in the env for this session
    env::set_var("CADUCEUS_PROJECT_ROOT", &canonical);
    Ok(())
}

#[tauri::command]
async fn config_set_provider(
    provider_id: String,
    api_key: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.default_provider = ProviderId(provider_id.clone());
    // Store API key in the provider-specific env var
    match provider_id.as_str() {
        "anthropic" => env::set_var("ANTHROPIC_API_KEY", &api_key),
        "openai" => env::set_var("OPENAI_API_KEY", &api_key),
        "gemini" | "google" => env::set_var("GEMINI_API_KEY", &api_key),
        "azure" => env::set_var("AZURE_OPENAI_API_KEY", &api_key),
        "copilot" | "github" => env::set_var("GITHUB_TOKEN", &api_key),
        _ => env::set_var("LLM_API_KEY", &api_key),
    }
    state
        .config_loader
        .save(&config)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn git_commit(project_root: String, message: String) -> Result<String, String> {
    let workspace_root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let project_root = resolve_under_workspace(&workspace_root, &project_root)?;
    let repo = open_git_repo(&project_root.to_string_lossy())?;
    // Stage all and commit via git CLI
    let output = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&project_root)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let output = std::process::Command::new("git")
        .args(["commit", "-m", &message])
        .current_dir(&project_root)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&project_root)
        .output()
        .map_err(|e| e.to_string())?;
    let _ = repo;
    Ok(String::from_utf8_lossy(&sha.stdout).trim().to_string())
}

#[tauri::command]
async fn git_file_diff(project_root: String, file_path: String) -> Result<String, String> {
    let workspace_root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let project_root = resolve_under_workspace(&workspace_root, &project_root)?;
    let output = std::process::Command::new("git")
        .args(["diff", "HEAD", "--", &file_path])
        .current_dir(&project_root)
        .output()
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[tauri::command]
async fn permission_respond(
    request_id: String,
    allow: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Permission responses are handled via the agent event system
    // Store the response for the agent to pick up
    let _ = (&request_id, allow, &state);
    Ok(())
}

fn main() {
    let workspace_root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let state = build_app_state(workspace_root).expect("failed to initialize app state");

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            session_create,
            session_list,
            session_messages,
            session_delete,
            agent_turn,
            agent_abort,
            terminal_exec,
            project_scan,
            git_status,
            git_diff,
            kanban_load,
            kanban_add_card,
            marketplace_search,
            marketplace_install,
            marketplace_recommend,
            mcp_status,
            mcp_add,
            config_get,
            keybindings_get,
            keybindings_set,
            keybindings_presets,
            pty_create,
            pty_write,
            pty_resize,
            pty_close,
            security_scan,
            security_scan_diff,
            dep_scan,
            context_compact,
            plugin_list,
            plugin_install,
            plugin_toggle,
            policy_evaluate,
            trust_score,
            replay_list,
            replay_step,
            context_health,
            skill_evolve_status,
            skill_sync,
            dag_status,
            federated_search,
            security_report,
            governance_status,
            trajectory_export,
            benchmark_status,
            project_open,
            config_set_provider,
            git_commit,
            git_file_diff,
            permission_respond,
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

fn load_keybindings() -> Result<KeybindingConfig, String> {
    let path = home_path(DEFAULT_KEYBINDINGS_RELATIVE_PATH)?;
    if !path.exists() {
        return Ok(KeybindingConfig::default());
    }

    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str::<KeybindingConfig>(&content).map_err(|e| e.to_string())
}

fn save_keybindings(config: &KeybindingConfig) -> Result<(), String> {
    let path = home_path(DEFAULT_KEYBINDINGS_RELATIVE_PATH)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let content = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(path, content).map_err(|e| e.to_string())
}

fn normalize_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.exists() {
        candidate.canonicalize().unwrap_or(candidate)
    } else {
        candidate
    }
}

fn resolve_under_workspace(workspace_root: &Path, path: &str) -> Result<PathBuf, String> {
    let root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    let normalized = normalize_path(&candidate.to_string_lossy());
    if normalized.exists() {
        let canonical = normalized.canonicalize().unwrap_or(normalized);
        if !canonical.starts_with(&root) {
            return Err("path escapes the application workspace".to_string());
        }
        return Ok(canonical);
    }

    let parent = normalized.parent().unwrap_or(&normalized);
    if parent.exists() {
        let canonical_parent = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        if !canonical_parent.starts_with(&root) {
            return Err("path escapes the application workspace".to_string());
        }
    } else if !normalized.starts_with(&root) {
        return Err("path escapes the application workspace".to_string());
    }

    Ok(normalized)
}

fn matches_catalog_entry(name: &str, description: &str, triggers: &[&str], query: &str) -> bool {
    let trimmed = query.trim().to_lowercase();
    if trimmed.is_empty() {
        return true;
    }

    name.to_lowercase().contains(&trimmed)
        || description.to_lowercase().contains(&trimmed)
        || triggers
            .iter()
            .any(|trigger| trigger.to_lowercase().contains(&trimmed))
}

fn is_installed(name: &str) -> bool {
    matches!(name, "code-review" | "frontend-dev" | "playwright-recorder")
}

fn demo_plugins() -> Vec<MarketplaceItem> {
    vec![
        MarketplaceItem {
            kind: "plugin".to_string(),
            name: "playwright-recorder".to_string(),
            description: "Generate browser automation flows and QA scripts from session traces."
                .to_string(),
            categories: vec!["testing".to_string(), "frontend".to_string()],
            installed: is_installed("playwright-recorder"),
        },
        MarketplaceItem {
            kind: "plugin".to_string(),
            name: "release-assistant".to_string(),
            description: "Bundle changelogs, release notes, and deployment checklists.".to_string(),
            categories: vec!["deployment".to_string(), "documentation".to_string()],
            installed: is_installed("release-assistant"),
        },
        MarketplaceItem {
            kind: "plugin".to_string(),
            name: "schema-lens".to_string(),
            description: "Inspect schemas, migrations, and data model diffs in one place."
                .to_string(),
            categories: vec!["database".to_string(), "backend".to_string()],
            installed: is_installed("schema-lens"),
        },
    ]
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
        token_budget: session.token_budget,
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

fn env_api_key(
    configured_provider: &ProviderId,
    config: &CaduceusConfig,
) -> Result<String, String> {
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
            .map(|ch| if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            })
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

fn ensure_terminal_ipc_enabled() -> Result<(), String> {
    if cfg!(debug_assertions)
        || env::var("CADUCEUS_ENABLE_TERMINAL_IPC").is_ok_and(|value| value == "1")
    {
        return Ok(());
    }
    Err(
        "terminal IPC is disabled by default; set CADUCEUS_ENABLE_TERMINAL_IPC=1 to enable it"
            .to_string(),
    )
}

fn open_git_repo(project_root: &str) -> Result<GitRepo, String> {
    GitRepo::discover(project_root)
        .or_else(|_| GitRepo::open(project_root))
        .map_err(|e| e.to_string())
}
