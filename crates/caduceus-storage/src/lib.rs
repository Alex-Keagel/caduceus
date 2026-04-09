use caduceus_core::{
    CaduceusError, Result, Role, SessionId, SessionState, TranscriptEntry,
};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

impl SqliteStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;

        // Enable WAL mode for concurrent reads
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;

        let storage = Self { conn: Mutex::new(conn) };
        storage.migrate()?;
        Ok(storage)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        let storage = Self { conn: Mutex::new(conn) };
        storage.migrate()?;
        Ok(storage)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA)
            .map_err(|e| CaduceusError::Storage(e.to_string()))
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    state_json  TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role        TEXT NOT NULL,
    content     TEXT NOT NULL,
    tokens      INTEGER,
    timestamp   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_calls (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    tool_name   TEXT NOT NULL,
    input_json  TEXT NOT NULL,
    output_json TEXT,
    started_at  TEXT NOT NULL,
    finished_at TEXT
);

CREATE TABLE IF NOT EXISTS costs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    provider_id   TEXT NOT NULL,
    model_id      TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cost_usd      REAL NOT NULL,
    recorded_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT,
    action      TEXT NOT NULL,
    detail_json TEXT,
    recorded_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    root_path   TEXT NOT NULL UNIQUE,
    meta_json   TEXT,
    created_at  TEXT NOT NULL
);
"#;

#[async_trait::async_trait]
impl caduceus_core::SessionStorage for SqliteStorage {
    async fn create_session(&self, state: &SessionState) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let json = serde_json::to_string(state)?;
        conn.execute(
            "INSERT INTO sessions (id, state_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                state.id.to_string(),
                json,
                state.created_at.to_rfc3339(),
                state.updated_at.to_rfc3339()
            ],
        )
        .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn load_session(&self, id: &SessionId) -> Result<Option<SessionState>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT state_json FROM sessions WHERE id = ?1")
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        let mut rows = stmt
            .query(params![id.to_string()])
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        if let Some(row) = rows.next().map_err(|e| CaduceusError::Storage(e.to_string()))? {
            let json: String = row.get(0).map_err(|e| CaduceusError::Storage(e.to_string()))?;
            let state: SessionState = serde_json::from_str(&json)?;
            Ok(Some(state))
        } else {
            Ok(None)
        }
    }

    async fn update_session(&self, state: &SessionState) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let json = serde_json::to_string(state)?;
        conn.execute(
            "UPDATE sessions SET state_json = ?1, updated_at = ?2 WHERE id = ?3",
            params![json, state.updated_at.to_rfc3339(), state.id.to_string()],
        )
        .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn list_sessions(&self, limit: usize) -> Result<Vec<SessionState>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT state_json FROM sessions ORDER BY updated_at DESC LIMIT ?1")
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit as i64], |row| row.get::<_, String>(0))
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        let mut sessions = Vec::new();
        for row in rows {
            let json = row.map_err(|e| CaduceusError::Storage(e.to_string()))?;
            sessions.push(serde_json::from_str(&json)?);
        }
        Ok(sessions)
    }

    async fn delete_session(&self, id: &SessionId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id.to_string()])
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn append_entry(&self, session_id: &SessionId, entry: &TranscriptEntry) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let role = serde_json::to_string(&entry.role)?;
        conn.execute(
            "INSERT INTO messages (session_id, role, content, tokens, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id.to_string(),
                role,
                entry.content,
                entry.tokens.map(|t| t as i64),
                entry.timestamp.to_rfc3339()
            ],
        )
        .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::{ModelId, ProviderId, SessionStorage};

    #[test]
    fn it_works() {
        let storage = SqliteStorage::open_in_memory().unwrap();
        // Just verify it opens without error
        drop(storage);
    }

    #[tokio::test]
    async fn create_and_load_session() {
        let storage = SqliteStorage::open_in_memory().unwrap();
        let state = SessionState::new(
            "/tmp/project",
            ProviderId::new("anthropic"),
            ModelId::new("claude-opus-4-5"),
        );
        let id = state.id.clone();
        storage.create_session(&state).await.unwrap();
        let loaded = storage.load_session(&id).await.unwrap();
        assert!(loaded.is_some());
    }
}
