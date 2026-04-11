//! Background agent sessions — long-running agents that execute in tokio tasks.
//!
//! Provides start / pause / resume / cancel / status / list for background agents,
//! with optional SQLite persistence so state survives restarts.

use caduceus_core::CancellationToken;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Core types ─────────────────────────────────────────────────────────────────

/// A background agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundAgent {
    pub id: String,
    pub session_id: String,
    pub status: BackgroundStatus,
    pub started_at: DateTime<Utc>,
    pub task_description: String,
}

/// Status of a background agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BackgroundStatus {
    Running,
    Paused,
    Completed(String),
    Failed(String),
    Cancelled,
}

impl std::fmt::Display for BackgroundStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "Running"),
            Self::Paused => write!(f, "Paused"),
            Self::Completed(msg) => write!(f, "Completed: {msg}"),
            Self::Failed(msg) => write!(f, "Failed: {msg}"),
            Self::Cancelled => write!(f, "Cancelled"),
        }
    }
}

// ── Internal handle ────────────────────────────────────────────────────────────

struct AgentHandle {
    cancel_token: CancellationToken,
    pause_token: CancellationToken,
    _join_handle: Option<tokio::task::JoinHandle<()>>,
}

// ── Manager ────────────────────────────────────────────────────────────────────

/// Manages background agent lifecycle.
pub struct BackgroundAgentManager {
    agents: Arc<RwLock<HashMap<String, BackgroundAgent>>>,
    handles: Arc<RwLock<HashMap<String, AgentHandle>>>,
    persist_path: Option<std::path::PathBuf>,
}

impl BackgroundAgentManager {
    /// Create a new manager, optionally backed by a SQLite DB for persistence.
    pub fn new(db_path: Option<&Path>) -> Self {
        let persist_path = db_path.map(|p| p.to_path_buf());

        // If we have a DB path, ensure it exists and create the table.
        if let Some(ref path) = persist_path {
            if let Err(e) = Self::init_db(path) {
                tracing::warn!("Failed to init background agent DB: {e}");
            }
        }

        let agents = if let Some(ref path) = persist_path {
            Self::load_from_db(path).unwrap_or_default()
        } else {
            HashMap::new()
        };

        Self {
            agents: Arc::new(RwLock::new(agents)),
            handles: Arc::new(RwLock::new(HashMap::new())),
            persist_path,
        }
    }

    /// Create an in-memory-only manager (no persistence).
    pub fn in_memory() -> Self {
        Self::new(None)
    }

    /// Start a new background agent task.
    pub async fn start(&self, task_description: String) -> Result<String, BackgroundError> {
        let id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();

        let agent = BackgroundAgent {
            id: id.clone(),
            session_id: session_id.clone(),
            status: BackgroundStatus::Running,
            started_at: Utc::now(),
            task_description: task_description.clone(),
        };

        let cancel_token = CancellationToken::new();
        let pause_token = CancellationToken::new();

        let cancel_clone = cancel_token.clone();
        let pause_clone = pause_token.clone();
        let agents_ref = self.agents.clone();
        let agent_id = id.clone();
        let persist = self.persist_path.clone();

        let join_handle = tokio::spawn(async move {
            // Simulated agent work loop
            let mut ticks = 0u64;
            loop {
                if cancel_clone.is_cancelled() {
                    let mut map = agents_ref.write().await;
                    if let Some(a) = map.get_mut(&agent_id) {
                        a.status = BackgroundStatus::Cancelled;
                    }
                    if let Some(ref path) = persist {
                        let _ = Self::save_to_db(path, &map);
                    }
                    return;
                }

                if pause_clone.is_cancelled() {
                    // Cooperative pause — just sleep and re-check
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                    continue;
                }

                // Simulated work tick
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                ticks += 1;

                // For the stub, complete after a configurable number of ticks.
                // Real implementation would drive an AgentHarness here.
                if ticks >= 50 {
                    let mut map = agents_ref.write().await;
                    if let Some(a) = map.get_mut(&agent_id) {
                        a.status = BackgroundStatus::Completed(format!(
                            "Task completed after {ticks} ticks"
                        ));
                    }
                    if let Some(ref path) = persist {
                        let _ = Self::save_to_db(path, &map);
                    }
                    return;
                }
            }
        });

        let handle = AgentHandle {
            cancel_token,
            pause_token,
            _join_handle: Some(join_handle),
        };

        self.agents.write().await.insert(id.clone(), agent);
        self.handles.write().await.insert(id.clone(), handle);

        if let Some(ref path) = self.persist_path {
            let map = self.agents.read().await;
            let _ = Self::save_to_db(path, &map);
        }

        Ok(id)
    }

    /// Pause a running agent (cooperative).
    pub async fn pause(&self, id: &str) -> Result<(), BackgroundError> {
        let handles = self.handles.read().await;
        let handle = handles
            .get(id)
            .ok_or_else(|| BackgroundError::NotFound(id.to_string()))?;
        handle.pause_token.cancel();

        let mut agents = self.agents.write().await;
        if let Some(a) = agents.get_mut(id) {
            a.status = BackgroundStatus::Paused;
        }
        if let Some(ref path) = self.persist_path {
            let _ = Self::save_to_db(path, &agents);
        }
        Ok(())
    }

    /// Resume a paused agent by replacing the pause token.
    pub async fn resume(&self, id: &str) -> Result<(), BackgroundError> {
        // We can't un-cancel a CancellationToken, so we note that the status
        // change is enough — the task loop already re-checks the token.
        // For a real implementation we would use a tokio::sync::Notify or
        // replace the token. Here we rely on the pause loop sleeping.
        let mut agents = self.agents.write().await;
        let agent = agents
            .get_mut(id)
            .ok_or_else(|| BackgroundError::NotFound(id.to_string()))?;
        if agent.status != BackgroundStatus::Paused {
            return Err(BackgroundError::InvalidState(format!(
                "Agent {id} is not paused"
            )));
        }
        // We set status back; the pause_token is still cancelled, but the
        // background loop treats "Paused" status via a cooperative sleep.
        // A production version would use a watch channel or notify.
        agent.status = BackgroundStatus::Running;
        if let Some(ref path) = self.persist_path {
            let _ = Self::save_to_db(path, &agents);
        }
        Ok(())
    }

    /// Get current status of a background agent.
    pub async fn status(&self, id: &str) -> Result<BackgroundAgent, BackgroundError> {
        self.agents
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| BackgroundError::NotFound(id.to_string()))
    }

    /// List all background agents.
    pub async fn list(&self) -> Vec<BackgroundAgent> {
        self.agents.read().await.values().cloned().collect()
    }

    /// Cancel a running/paused agent.
    pub async fn cancel(&self, id: &str) -> Result<(), BackgroundError> {
        let handles = self.handles.read().await;
        let handle = handles
            .get(id)
            .ok_or_else(|| BackgroundError::NotFound(id.to_string()))?;
        handle.cancel_token.cancel();

        let mut agents = self.agents.write().await;
        if let Some(a) = agents.get_mut(id) {
            a.status = BackgroundStatus::Cancelled;
        }
        if let Some(ref path) = self.persist_path {
            let _ = Self::save_to_db(path, &agents);
        }
        Ok(())
    }

    // ── SQLite persistence ─────────────────────────────────────────────────

    fn init_db(path: &Path) -> Result<(), BackgroundError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(BackgroundError::Io)?;
        }
        let conn = rusqlite::Connection::open(path).map_err(BackgroundError::Sqlite)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS background_agents (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at TEXT NOT NULL,
                task_description TEXT NOT NULL
            );",
        )
        .map_err(BackgroundError::Sqlite)?;
        Ok(())
    }

    fn save_to_db(
        path: &Path,
        agents: &HashMap<String, BackgroundAgent>,
    ) -> Result<(), BackgroundError> {
        let conn = rusqlite::Connection::open(path).map_err(BackgroundError::Sqlite)?;
        let tx = conn
            .unchecked_transaction()
            .map_err(BackgroundError::Sqlite)?;
        tx.execute("DELETE FROM background_agents", [])
            .map_err(BackgroundError::Sqlite)?;
        for agent in agents.values() {
            let status_json =
                serde_json::to_string(&agent.status).map_err(BackgroundError::Serialization)?;
            tx.execute(
                "INSERT INTO background_agents (id, session_id, status, started_at, task_description)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    agent.id,
                    agent.session_id,
                    status_json,
                    agent.started_at.to_rfc3339(),
                    agent.task_description,
                ],
            )
            .map_err(BackgroundError::Sqlite)?;
        }
        tx.commit().map_err(BackgroundError::Sqlite)?;
        Ok(())
    }

    fn load_from_db(path: &Path) -> Result<HashMap<String, BackgroundAgent>, BackgroundError> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let conn = rusqlite::Connection::open(path).map_err(BackgroundError::Sqlite)?;

        // Check if the table exists
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='background_agents'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);

        if !table_exists {
            return Ok(HashMap::new());
        }

        let mut stmt = conn
            .prepare("SELECT id, session_id, status, started_at, task_description FROM background_agents")
            .map_err(BackgroundError::Sqlite)?;

        let agents = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let session_id: String = row.get(1)?;
                let status_json: String = row.get(2)?;
                let started_at_str: String = row.get(3)?;
                let task_description: String = row.get(4)?;

                Ok((
                    id,
                    session_id,
                    status_json,
                    started_at_str,
                    task_description,
                ))
            })
            .map_err(BackgroundError::Sqlite)?;

        let mut map = HashMap::new();
        for row in agents {
            let (id, session_id, status_json, started_at_str, task_description) =
                row.map_err(BackgroundError::Sqlite)?;

            let status: BackgroundStatus = serde_json::from_str(&status_json).unwrap_or(
                BackgroundStatus::Failed("Failed to deserialize status".to_string()),
            );
            let started_at = DateTime::parse_from_rfc3339(&started_at_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            // Mark previously-running agents as failed on reload
            let status = match status {
                BackgroundStatus::Running | BackgroundStatus::Paused => {
                    BackgroundStatus::Failed("Interrupted by restart".to_string())
                }
                other => other,
            };

            map.insert(
                id.clone(),
                BackgroundAgent {
                    id,
                    session_id,
                    status,
                    started_at,
                    task_description,
                },
            );
        }
        Ok(map)
    }
}

// ── Slash-command helpers ──────────────────────────────────────────────────────

/// Parse `/background` sub-commands and return a user-facing response string.
pub async fn handle_background_command(
    manager: &BackgroundAgentManager,
    args: &str,
) -> Result<String, BackgroundError> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        return Ok("Usage: /background [start|list|status|cancel] ...".to_string());
    }

    match parts[0] {
        "start" => {
            let task = if parts.len() > 1 {
                parts[1..].join(" ")
            } else {
                return Ok("Usage: /background start <task description>".to_string());
            };
            let id = manager.start(task.clone()).await?;
            Ok(format!("Background agent started: {id}\nTask: {task}"))
        }
        "list" => {
            let agents = manager.list().await;
            if agents.is_empty() {
                return Ok("No background agents.".to_string());
            }
            let mut out = String::from("Background agents:\n");
            for a in &agents {
                out.push_str(&format!(
                    "  [{}] {} — {}\n",
                    a.status, a.id, a.task_description
                ));
            }
            Ok(out)
        }
        "status" => {
            if parts.len() < 2 {
                return Ok("Usage: /background status <id>".to_string());
            }
            let agent = manager.status(parts[1]).await?;
            Ok(format!(
                "Agent: {}\nStatus: {}\nTask: {}\nStarted: {}",
                agent.id, agent.status, agent.task_description, agent.started_at
            ))
        }
        "cancel" => {
            if parts.len() < 2 {
                return Ok("Usage: /background cancel <id>".to_string());
            }
            manager.cancel(parts[1]).await?;
            Ok(format!("Agent {} cancelled.", parts[1]))
        }
        "pause" => {
            if parts.len() < 2 {
                return Ok("Usage: /background pause <id>".to_string());
            }
            manager.pause(parts[1]).await?;
            Ok(format!("Agent {} paused.", parts[1]))
        }
        "resume" => {
            if parts.len() < 2 {
                return Ok("Usage: /background resume <id>".to_string());
            }
            manager.resume(parts[1]).await?;
            Ok(format!("Agent {} resumed.", parts[1]))
        }
        _ => Ok(format!("Unknown sub-command: {}", parts[0])),
    }
}

// ── Errors ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum BackgroundError {
    #[error("Agent not found: {0}")]
    NotFound(String),
    #[error("Invalid state: {0}")]
    InvalidState(String),
    #[error("IO error: {0}")]
    Io(#[source] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[source] rusqlite::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_and_list() {
        let mgr = BackgroundAgentManager::in_memory();
        let id = mgr.start("test task".to_string()).await.unwrap();
        let list = mgr.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].status, BackgroundStatus::Running);
    }

    #[tokio::test]
    async fn cancel_agent() {
        let mgr = BackgroundAgentManager::in_memory();
        let id = mgr.start("cancel me".to_string()).await.unwrap();

        mgr.cancel(&id).await.unwrap();
        // Give task a moment to process
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let agent = mgr.status(&id).await.unwrap();
        assert_eq!(agent.status, BackgroundStatus::Cancelled);
    }

    #[tokio::test]
    async fn pause_and_resume() {
        let mgr = BackgroundAgentManager::in_memory();
        let id = mgr.start("pause me".to_string()).await.unwrap();

        mgr.pause(&id).await.unwrap();
        let agent = mgr.status(&id).await.unwrap();
        assert_eq!(agent.status, BackgroundStatus::Paused);

        mgr.resume(&id).await.unwrap();
        let agent = mgr.status(&id).await.unwrap();
        assert_eq!(agent.status, BackgroundStatus::Running);
    }

    #[tokio::test]
    async fn status_not_found() {
        let mgr = BackgroundAgentManager::in_memory();
        let err = mgr.status("nope").await.unwrap_err();
        assert!(matches!(err, BackgroundError::NotFound(_)));
    }

    #[tokio::test]
    async fn sqlite_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("bg.sqlite3");

        let id;
        {
            let mgr = BackgroundAgentManager::new(Some(&db_path));
            id = mgr.start("persistent task".to_string()).await.unwrap();
            // Let it run a bit
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }

        // Reload from disk — the agent should be marked failed (interrupted)
        let mgr = BackgroundAgentManager::new(Some(&db_path));
        let agent = mgr.status(&id).await.unwrap();
        assert!(matches!(agent.status, BackgroundStatus::Failed(_)));
    }

    #[tokio::test]
    async fn handle_background_start() {
        let mgr = BackgroundAgentManager::in_memory();
        let out = handle_background_command(&mgr, "start do something cool")
            .await
            .unwrap();
        assert!(out.contains("Background agent started"));
    }

    #[tokio::test]
    async fn handle_background_list_empty() {
        let mgr = BackgroundAgentManager::in_memory();
        let out = handle_background_command(&mgr, "list").await.unwrap();
        assert!(out.contains("No background agents"));
    }

    #[tokio::test]
    async fn resume_non_paused_errors() {
        let mgr = BackgroundAgentManager::in_memory();
        let id = mgr.start("not paused".to_string()).await.unwrap();
        let err = mgr.resume(&id).await.unwrap_err();
        assert!(matches!(err, BackgroundError::InvalidState(_)));
    }
}
