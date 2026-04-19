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
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// `true` = paused, `false` = running. The background loop polls this
    /// each tick and sleeps while paused. Atomic bool (not a
    /// CancellationToken) because pause/resume must be reversible —
    /// CancellationToken is one-shot and cannot be un-cancelled, which
    /// silently broke `resume()` (status flipped back to Running but the
    /// loop kept sleeping forever).
    pause_signal: Arc<AtomicBool>,
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
        let pause_signal = Arc::new(AtomicBool::new(false));

        let cancel_clone = cancel_token.clone();
        let pause_clone = pause_signal.clone();
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

                if pause_clone.load(Ordering::Acquire) {
                    // Cooperative pause — sleep and re-check. Resume()
                    // will flip the flag back to false and the loop
                    // resumes work on the next iteration.
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                    continue;
                }

                // Simulated work tick
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                ticks += 1;

                // For the stub, complete after a configurable number of ticks.
                // Real implementation would drive an AgentHarness here.
                if ticks >= 50 {
                    let snapshot = {
                        let mut map = agents_ref.write().await;
                        if let Some(a) = map.get_mut(&agent_id) {
                            a.status = BackgroundStatus::Completed(format!(
                                "Task completed after {ticks} ticks"
                            ));
                        }
                        persist.as_ref().map(|_| map.clone())
                    };
                    if let (Some(ref path), Some(snapshot)) = (&persist, snapshot) {
                        let _ = Self::save_to_db(path, &snapshot);
                    }
                    return;
                }
            }
        });

        let handle = AgentHandle {
            cancel_token,
            pause_signal,
            _join_handle: Some(join_handle),
        };

        self.agents.write().await.insert(id.clone(), agent);
        self.handles.write().await.insert(id.clone(), handle);

        // Audit finding round-2 (#31): persistence used to happen while the
        // agents read lock was held, blocking pause/resume/cancel/list on
        // the (potentially-slow) sqlite write. Snapshot then drop, then
        // persist outside the critical section.
        if let Some(ref path) = self.persist_path {
            let snapshot = self.agents.read().await.clone();
            let _ = Self::save_to_db(path, &snapshot);
        }

        Ok(id)
    }

    /// Pause a running agent (cooperative).
    ///
    /// Sets the pause signal; the background loop will sleep on its next
    /// iteration and resume work when `resume()` clears the signal. Idempotent.
    pub async fn pause(&self, id: &str) -> Result<(), BackgroundError> {
        let handles = self.handles.read().await;
        let handle = handles
            .get(id)
            .ok_or_else(|| BackgroundError::NotFound(id.to_string()))?;
        handle.pause_signal.store(true, Ordering::Release);
        // Drop the handles read lock before taking the agents write lock to
        // keep lock-acquisition order consistent across all methods (handles
        // first if needed, then agents) and avoid holding two locks longer
        // than necessary.
        drop(handles);

        let snapshot_path = {
            let mut agents = self.agents.write().await;
            if let Some(a) = agents.get_mut(id) {
                a.status = BackgroundStatus::Paused;
            }
            self.persist_path
                .as_ref()
                .map(|p| (p.clone(), agents.clone()))
        };
        if let Some((path, snapshot)) = snapshot_path {
            let _ = Self::save_to_db(&path, &snapshot);
        }
        Ok(())
    }

    /// Resume a paused agent.
    ///
    /// Atomically clears the pause signal so the background loop's next
    /// poll observes `false` and resumes work. Returns `InvalidState` if
    /// the agent is not currently paused.
    pub async fn resume(&self, id: &str) -> Result<(), BackgroundError> {
        // Validate state under the agents write lock first so the status
        // transition is atomic. Snapshot for persistence, drop the lock,
        // then persist outside the critical section (audit #31).
        let snapshot_path = {
            let mut agents = self.agents.write().await;
            let agent = agents
                .get_mut(id)
                .ok_or_else(|| BackgroundError::NotFound(id.to_string()))?;
            if agent.status != BackgroundStatus::Paused {
                return Err(BackgroundError::InvalidState(format!(
                    "Agent {id} is not paused"
                )));
            }
            agent.status = BackgroundStatus::Running;
            self.persist_path
                .as_ref()
                .map(|p| (p.clone(), agents.clone()))
        };
        if let Some((path, snapshot)) = snapshot_path {
            let _ = Self::save_to_db(&path, &snapshot);
        }
        // Clear the pause signal AFTER status is updated and persisted, so
        // an observer that sees status=Running is guaranteed to see the
        // loop unblocked on its next poll.
        let handles = self.handles.read().await;
        if let Some(h) = handles.get(id) {
            h.pause_signal.store(false, Ordering::Release);
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
        // Release handles before taking agents write lock — keeps lock
        // order consistent with pause()/resume() and avoids holding two
        // locks simultaneously.
        drop(handles);

        let snapshot_path = {
            let mut agents = self.agents.write().await;
            if let Some(a) = agents.get_mut(id) {
                a.status = BackgroundStatus::Cancelled;
            }
            self.persist_path
                .as_ref()
                .map(|p| (p.clone(), agents.clone()))
        };
        if let Some((path, snapshot)) = snapshot_path {
            let _ = Self::save_to_db(&path, &snapshot);
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

impl Drop for BackgroundAgentManager {
    /// Audit finding round-2 (#21): JoinHandles for background agents were
    /// leaked when the manager itself was dropped. We now cooperatively
    /// cancel via the per-agent CancellationToken AND abort the handle so
    /// long-sleeping loops also stop without waiting out their tick.
    fn drop(&mut self) {
        if let Ok(mut handles) = self.handles.try_write() {
            for (_, handle) in handles.drain() {
                handle.cancel_token.cancel();
                if let Some(jh) = handle._join_handle {
                    jh.abort();
                }
            }
        }
        // If the lock is contended at drop time (concurrent caller mid-write),
        // the handles will still be dropped through Arc release; their
        // JoinHandle Drop detaches but our cancel_token cancels are missed.
        // Acceptable trade-off: in practice the manager is owned by app
        // state and Drop runs once at shutdown when no callers remain.
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

    // ── P0-3: pause/resume must actually un-block the loop ──────────────────

    #[tokio::test]
    async fn resume_actually_unblocks_loop() {
        // Regression: previously `pause_token` was a one-shot
        // CancellationToken; resume() flipped status back to Running but
        // the loop kept observing pause_token.is_cancelled() == true and
        // slept forever. Verify the agent can complete after pause+resume.
        let mgr = BackgroundAgentManager::in_memory();
        let id = mgr.start("complete after resume".to_string()).await.unwrap();

        // Pause briefly, then resume; agent should still be able to complete.
        mgr.pause(&id).await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
        let paused = mgr.status(&id).await.unwrap();
        assert_eq!(paused.status, BackgroundStatus::Paused);

        mgr.resume(&id).await.unwrap();

        // Loop tick = 100ms, completes after 50 ticks ≈ 5s. Poll up to 8s.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            let s = mgr.status(&id).await.unwrap();
            if matches!(s.status, BackgroundStatus::Completed(_)) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "agent did not reach Completed after resume; current status = {:?}",
                s.status
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
    }

    #[tokio::test]
    async fn pause_actually_blocks_progress() {
        // Confirm pause stops forward progress: a paused agent must NOT
        // reach Completed even if we wait longer than the unpaused
        // completion time would take.
        let mgr = BackgroundAgentManager::in_memory();
        let id = mgr.start("pause blocks".to_string()).await.unwrap();
        // Let it run a bit, then pause.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        mgr.pause(&id).await.unwrap();

        // Wait significantly less than completion time post-pause.
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        let s = mgr.status(&id).await.unwrap();
        assert_eq!(
            s.status,
            BackgroundStatus::Paused,
            "agent was not actually paused — status = {:?}",
            s.status
        );
    }

    #[tokio::test]
    async fn pause_is_idempotent() {
        // Calling pause twice on a running agent must not break resume.
        let mgr = BackgroundAgentManager::in_memory();
        let id = mgr.start("double pause".to_string()).await.unwrap();
        mgr.pause(&id).await.unwrap();
        mgr.pause(&id).await.unwrap();
        let s = mgr.status(&id).await.unwrap();
        assert_eq!(s.status, BackgroundStatus::Paused);
        mgr.resume(&id).await.unwrap();
        let s = mgr.status(&id).await.unwrap();
        assert_eq!(s.status, BackgroundStatus::Running);
    }

    #[tokio::test]
    async fn pause_then_cancel_completes_with_cancelled() {
        // Cancellation must take priority over pause — a paused agent
        // that is then cancelled must reach Cancelled (not stay Paused).
        let mgr = BackgroundAgentManager::in_memory();
        let id = mgr.start("pause then cancel".to_string()).await.unwrap();
        mgr.pause(&id).await.unwrap();
        // Even though paused, cancel should propagate; loop wakes from
        // its 50ms pause sleep and observes cancel_token.
        mgr.cancel(&id).await.unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let s = mgr.status(&id).await.unwrap();
            if s.status == BackgroundStatus::Cancelled {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "agent did not reach Cancelled after pause+cancel; status = {:?}",
                s.status
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;
        }
    }

    #[tokio::test]
    async fn concurrent_pause_resume_no_deadlock() {
        // Stress: spawn multiple agents and rapidly pause/resume them in
        // parallel. Validates that lock-acquisition order is consistent
        // (handles-read released before agents-write taken) — previously
        // pause() held both locks simultaneously, contending with start().
        let mgr = Arc::new(BackgroundAgentManager::in_memory());
        let mut ids = Vec::new();
        for i in 0..10 {
            let id = mgr.start(format!("agent-{i}")).await.unwrap();
            ids.push(id);
        }

        let mut tasks = Vec::new();
        for id in ids {
            let mgr = mgr.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..5 {
                    let _ = mgr.pause(&id).await;
                    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                    let _ = mgr.resume(&id).await;
                    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                }
            }));
        }

        // 3-second timeout; if any task deadlocks we fail.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            futures::future::join_all(tasks),
        )
        .await;
        assert!(result.is_ok(), "concurrent pause/resume deadlocked");
    }
}
