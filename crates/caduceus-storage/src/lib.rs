use caduceus_core::{
    AuditDecision, AuditEntry, AuthStore, CaduceusError, ContentBlock, LlmMessage, ModelId,
    ProviderId, Result, Role, SessionId, SessionPhase, SessionState, SessionStorage, TokenBudget,
    ToolCallId,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use uuid::Uuid;

const CURRENT_SCHEMA_VERSION: i64 = 5;
const BOOTSTRAP_SCHEMA_VERSION: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    version INTEGER NOT NULL
);
INSERT OR IGNORE INTO schema_version (id, version) VALUES (1, 0);
"#;

const MIGRATIONS: [&str; CURRENT_SCHEMA_VERSION as usize] = [
    r#"
    CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        phase TEXT NOT NULL,
        project_root TEXT NOT NULL,
        provider_id TEXT NOT NULL,
        model_id TEXT NOT NULL,
        turn_count INTEGER NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        role TEXT NOT NULL,
        content TEXT NOT NULL,
        tokens INTEGER,
        timestamp TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS tool_calls (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        tool_name TEXT NOT NULL,
        input TEXT NOT NULL,
        output TEXT,
        is_error INTEGER NOT NULL DEFAULT 0,
        duration_ms INTEGER NOT NULL,
        timestamp TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS audit_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        capability TEXT NOT NULL,
        tool_name TEXT NOT NULL,
        args_redacted TEXT NOT NULL,
        decision TEXT NOT NULL,
        timestamp TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS costs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        provider_id TEXT NOT NULL,
        model_id TEXT NOT NULL,
        input_tokens INTEGER NOT NULL,
        output_tokens INTEGER NOT NULL,
        cost_usd REAL NOT NULL,
        timestamp TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_sessions_updated_at ON sessions(updated_at DESC);
    CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id, id);
    CREATE INDEX IF NOT EXISTS idx_tool_calls_session_id ON tool_calls(session_id, timestamp);
    CREATE INDEX IF NOT EXISTS idx_audit_log_session_id ON audit_log(session_id, id);
    CREATE INDEX IF NOT EXISTS idx_costs_session_id ON costs(session_id, id);
    "#,
    r#"
    ALTER TABLE sessions ADD COLUMN context_limit INTEGER NOT NULL DEFAULT 200000;
    ALTER TABLE sessions ADD COLUMN used_input INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE sessions ADD COLUMN used_output INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE sessions ADD COLUMN reserved_output INTEGER NOT NULL DEFAULT 8192;
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS memory (
        id TEXT PRIMARY KEY,
        scope TEXT NOT NULL,
        key TEXT NOT NULL,
        value TEXT NOT NULL,
        source TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        UNIQUE(scope, key)
    );

    CREATE TABLE IF NOT EXISTS auth_keys (
        provider_id TEXT PRIMARY KEY,
        api_key TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_memory_scope_key ON memory(scope, key);
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS session_trace (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL,
        event_type TEXT NOT NULL,
        event_data TEXT NOT NULL,
        duration_ms INTEGER,
        timestamp TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_session_trace_session_id ON session_trace(session_id, id);
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS telemetry_snapshots (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL,
        snapshot_type TEXT NOT NULL,
        data TEXT NOT NULL,
        timestamp TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_telemetry_session ON telemetry_snapshots(session_id);
    "#,
];

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: i64,
    pub session_id: SessionId,
    pub message: LlmMessage,
    pub tokens: Option<u32>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct StoredToolCall {
    pub id: ToolCallId,
    pub session_id: SessionId,
    pub tool_name: String,
    pub input: String,
    pub output: Option<String>,
    pub is_error: bool,
    pub duration_ms: u64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct StoredCost {
    pub id: i64,
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptLine {
    pub role: String,
    pub content: serde_json::Value,
    pub tokens: Option<u32>,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResumedSession {
    pub state: SessionState,
    pub messages: Vec<StoredMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecord {
    pub id: String,
    pub scope: String,
    pub key: String,
    pub value: String,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── Structured memory bank constants ─────────────────────────────────────────

pub const MEMORY_KEY_PROJECT_BRIEF: &str = "project_brief";
pub const MEMORY_KEY_ACTIVE_CONTEXT: &str = "active_context";
pub const MEMORY_KEY_PROGRESS: &str = "progress";
pub const MEMORY_KEY_DECISIONS: &str = "decisions";
pub const MEMORY_SCOPE_STRUCTURED: &str = "structured";

// ── Session trace types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    pub session_id: String,
    pub event_type: TraceEventType,
    pub event_data: serde_json::Value,
    pub duration_ms: Option<u64>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceEventType {
    LlmCall,
    ToolExec,
    Permission,
}

#[derive(Debug)]
struct SessionRow {
    id: String,
    phase: String,
    project_root: String,
    provider_id: String,
    model_id: String,
    turn_count: i64,
    created_at: String,
    updated_at: String,
    context_limit: i64,
    used_input: i64,
    used_output: i64,
    reserved_output: i64,
}

pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

impl SqliteStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let conn = Connection::open(path).map_err(storage_error)?;
        Self::from_connection(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(storage_error)?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        Self::configure_connection(&conn)?;
        let storage = Self {
            conn: Mutex::new(conn),
        };
        storage.migrate()?;
        storage.recover_crashed_sessions()?;
        Ok(storage)
    }

    fn configure_connection(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(storage_error)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(storage_error)?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(storage_error)?;
        Ok(())
    }

    fn with_connection<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|err| CaduceusError::Storage(format!("sqlite mutex poisoned: {err}")))?;
        f(&mut conn)
    }

    fn migrate(&self) -> Result<()> {
        self.with_connection(|conn| {
            conn.execute_batch(BOOTSTRAP_SCHEMA_VERSION)
                .map_err(storage_error)?;

            let current: i64 = conn
                .query_row(
                    "SELECT version FROM schema_version WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(storage_error)?;

            for version in (current + 1)..=CURRENT_SCHEMA_VERSION {
                let tx = conn.transaction().map_err(storage_error)?;
                tx.execute_batch(MIGRATIONS[(version - 1) as usize])
                    .map_err(storage_error)?;
                tx.execute(
                    "UPDATE schema_version SET version = ?1 WHERE id = 1",
                    params![version],
                )
                .map_err(storage_error)?;
                tx.commit().map_err(storage_error)?;
            }

            Ok(())
        })
    }

    pub async fn save_message(
        &self,
        session_id: &SessionId,
        message: &LlmMessage,
        tokens: Option<u32>,
    ) -> Result<i64> {
        let role = role_to_str(message.role);
        let content = serde_json::to_string(&message.content)?;
        let timestamp = Utc::now().to_rfc3339();
        let tokens = tokens.map(i64::from);
        let session_id = session_id.to_string();

        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO messages (session_id, role, content, tokens, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, role, content, tokens, timestamp],
            )
            .map_err(storage_error)?;
            Ok(conn.last_insert_rowid())
        })
    }

    pub async fn list_messages(&self, session_id: &SessionId) -> Result<Vec<StoredMessage>> {
        let session_id_value = session_id.to_string();
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, session_id, role, content, tokens, timestamp
                     FROM messages
                     WHERE session_id = ?1
                     ORDER BY id ASC",
                )
                .map_err(storage_error)?;

            let rows = stmt
                .query_map(params![session_id_value], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(storage_error)?;

            let mut messages = Vec::new();
            for row in rows {
                let (id, session_id, role, content, tokens, timestamp) =
                    row.map_err(storage_error)?;
                messages.push(StoredMessage {
                    id,
                    session_id: parse_session_id(&session_id)?,
                    message: LlmMessage {
                        role: role_from_str(&role)?,
                        content: serde_json::from_str::<Vec<ContentBlock>>(&content)?,
                    },
                    tokens: transpose_u32(tokens)?,
                    timestamp: parse_timestamp(&timestamp)?,
                });
            }
            Ok(messages)
        })
    }

    pub async fn record_tool_call(&self, call: &StoredToolCall) -> Result<()> {
        let session_id = call.session_id.to_string();
        let tool_call_id = call.id.0.clone();
        let duration_ms = i64::try_from(call.duration_ms).map_err(|_| {
            CaduceusError::Storage("duration_ms exceeds SQLite INTEGER range".into())
        })?;
        let timestamp = call.timestamp.to_rfc3339();

        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO tool_calls (id, session_id, tool_name, input, output, is_error, duration_ms, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    tool_call_id,
                    session_id,
                    call.tool_name,
                    call.input,
                    call.output,
                    call.is_error,
                    duration_ms,
                    timestamp
                ],
            )
            .map_err(storage_error)?;
            Ok(())
        })
    }

    pub async fn list_tool_calls(&self, session_id: &SessionId) -> Result<Vec<StoredToolCall>> {
        let session_id_value = session_id.to_string();
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, session_id, tool_name, input, output, is_error, duration_ms, timestamp
                     FROM tool_calls
                     WHERE session_id = ?1
                     ORDER BY timestamp ASC, id ASC",
                )
                .map_err(storage_error)?;

            let rows = stmt
                .query_map(params![session_id_value], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                })
                .map_err(storage_error)?;

            let mut tool_calls = Vec::new();
            for row in rows {
                let (id, session_id, tool_name, input, output, is_error, duration_ms, timestamp) =
                    row.map_err(storage_error)?;
                tool_calls.push(StoredToolCall {
                    id: ToolCallId::new(id),
                    session_id: parse_session_id(&session_id)?,
                    tool_name,
                    input,
                    output,
                    is_error,
                    duration_ms: transpose_u64(duration_ms)?,
                    timestamp: parse_timestamp(&timestamp)?,
                });
            }
            Ok(tool_calls)
        })
    }

    pub async fn append_audit(&self, entry: &AuditEntry) -> Result<i64> {
        let session_id = entry.session_id.to_string();
        let timestamp = entry.timestamp.to_rfc3339();
        let decision = audit_decision_to_str(entry.decision);

        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO audit_log (session_id, capability, tool_name, args_redacted, decision, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    session_id,
                    entry.capability,
                    entry.tool_name,
                    entry.args_redacted,
                    decision,
                    timestamp
                ],
            )
            .map_err(storage_error)?;
            Ok(conn.last_insert_rowid())
        })
    }

    pub async fn list_audit(&self, session_id: &SessionId) -> Result<Vec<AuditEntry>> {
        let session_id_value = session_id.to_string();
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT session_id, capability, tool_name, args_redacted, decision, timestamp
                     FROM audit_log
                     WHERE session_id = ?1
                     ORDER BY id ASC",
                )
                .map_err(storage_error)?;

            let rows = stmt
                .query_map(params![session_id_value], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(storage_error)?;

            let mut entries = Vec::new();
            for row in rows {
                let (session_id, capability, tool_name, args_redacted, decision, timestamp) =
                    row.map_err(storage_error)?;
                entries.push(AuditEntry {
                    timestamp: parse_timestamp(&timestamp)?,
                    session_id: parse_session_id(&session_id)?,
                    capability,
                    tool_name,
                    args_redacted,
                    decision: audit_decision_from_str(&decision)?,
                });
            }
            Ok(entries)
        })
    }

    pub async fn record_cost(
        &self,
        session_id: &SessionId,
        provider_id: &ProviderId,
        model_id: &ModelId,
        input_tokens: u32,
        output_tokens: u32,
        cost_usd: f64,
    ) -> Result<i64> {
        let timestamp = Utc::now().to_rfc3339();
        let input_tokens = i64::from(input_tokens);
        let output_tokens = i64::from(output_tokens);
        let session_id = session_id.to_string();

        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO costs (session_id, provider_id, model_id, input_tokens, output_tokens, cost_usd, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    session_id,
                    provider_id.0,
                    model_id.0,
                    input_tokens,
                    output_tokens,
                    cost_usd,
                    timestamp
                ],
            )
            .map_err(storage_error)?;
            Ok(conn.last_insert_rowid())
        })
    }

    pub async fn list_costs(&self, session_id: &SessionId) -> Result<Vec<StoredCost>> {
        let session_id_value = session_id.to_string();
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, session_id, provider_id, model_id, input_tokens, output_tokens, cost_usd, timestamp
                     FROM costs
                     WHERE session_id = ?1
                     ORDER BY id ASC",
                )
                .map_err(storage_error)?;

            let rows = stmt
                .query_map(params![session_id_value], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, f64>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                })
                .map_err(storage_error)?;

            let mut costs = Vec::new();
            for row in rows {
                let (id, session_id, provider_id, model_id, input_tokens, output_tokens, cost_usd, timestamp) =
                    row.map_err(storage_error)?;
                costs.push(StoredCost {
                    id,
                    session_id: parse_session_id(&session_id)?,
                    provider_id: ProviderId::new(provider_id),
                    model_id: ModelId::new(model_id),
                    input_tokens: transpose_u32(Some(input_tokens))?.unwrap_or_default(),
                    output_tokens: transpose_u32(Some(output_tokens))?.unwrap_or_default(),
                    cost_usd,
                    timestamp: parse_timestamp(&timestamp)?,
                });
            }
            Ok(costs)
        })
    }

    pub async fn total_cost(&self, session_id: &SessionId) -> Result<f64> {
        let session_id_value = session_id.to_string();
        self.with_connection(|conn| {
            let total = conn
                .query_row(
                    "SELECT COALESCE(SUM(cost_usd), 0.0) FROM costs WHERE session_id = ?1",
                    params![session_id_value],
                    |row| row.get::<_, f64>(0),
                )
                .map_err(storage_error)?;
            Ok(total)
        })
    }

    /// Persist a telemetry snapshot (context health, attention, degradation stage, etc.)
    pub async fn save_telemetry_snapshot(
        &self,
        session_id: &SessionId,
        snapshot_type: &str,
        data: &serde_json::Value,
    ) -> Result<()> {
        let sid = session_id.to_string();
        let stype = snapshot_type.to_string();
        let json =
            serde_json::to_string(data).map_err(|e| CaduceusError::Storage(e.to_string()))?;
        let ts = Utc::now().to_rfc3339();
        self.with_connection(move |conn| {
            conn.execute(
                "INSERT INTO telemetry_snapshots (session_id, snapshot_type, data, timestamp)
                 VALUES (?1, ?2, ?3, ?4)",
                params![sid, stype, json, ts],
            )
            .map_err(storage_error)?;
            Ok(())
        })
    }

    /// Load telemetry snapshots for a session, optionally filtered by type
    pub async fn load_telemetry_snapshots(
        &self,
        session_id: &SessionId,
        snapshot_type: Option<&str>,
    ) -> Result<Vec<(String, serde_json::Value, String)>> {
        let sid = session_id.to_string();
        let stype = snapshot_type.map(|s| s.to_string());
        self.with_connection(move |conn| {
            let (query, params_vec): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) =
                if let Some(ref st) = stype {
                    (
                        "SELECT snapshot_type, data, timestamp FROM telemetry_snapshots
                     WHERE session_id = ?1 AND snapshot_type = ?2 ORDER BY id",
                        vec![Box::new(sid.clone()), Box::new(st.clone())],
                    )
                } else {
                    (
                        "SELECT snapshot_type, data, timestamp FROM telemetry_snapshots
                     WHERE session_id = ?1 ORDER BY id",
                        vec![Box::new(sid.clone())],
                    )
                };
            let mut stmt = conn.prepare(query).map_err(storage_error)?;
            let params_refs: Vec<&dyn rusqlite::types::ToSql> =
                params_vec.iter().map(|b| b.as_ref()).collect();
            let mut rows = stmt.query(params_refs.as_slice()).map_err(storage_error)?;
            let mut results = Vec::new();
            while let Some(row) = rows.next().map_err(storage_error)? {
                let stype: String = row.get(0).map_err(storage_error)?;
                let data_str: String = row.get(1).map_err(storage_error)?;
                let ts: String = row.get(2).map_err(storage_error)?;
                let data: serde_json::Value =
                    serde_json::from_str(&data_str).unwrap_or(serde_json::Value::String(data_str));
                results.push((stype, data, ts));
            }
            Ok(results)
        })
    }

    pub async fn export_transcript(
        &self,
        session_id: &SessionId,
        path: impl AsRef<Path>,
    ) -> Result<()> {
        let messages = self.list_messages(session_id).await?;
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let mut file = fs::File::create(path)?;
        for message in messages {
            let line = TranscriptLine {
                role: role_to_str(message.message.role).to_string(),
                content: serde_json::to_value(&message.message.content)?,
                tokens: message.tokens,
                timestamp: message.timestamp,
                tool_call_id: extract_tool_call_id(&message.message.content),
            };
            serde_json::to_writer(&mut file, &line)?;
            file.write_all(b"\n")?;
        }
        Ok(())
    }

    pub async fn resume_session(&self, session_id: &SessionId) -> Result<ResumedSession> {
        let state = self
            .load_session(session_id)
            .await?
            .ok_or_else(|| CaduceusError::SessionNotFound(session_id.clone()))?;
        let messages = self.list_messages(session_id).await?;
        Ok(ResumedSession { state, messages })
    }

    pub fn recover_crashed_sessions(&self) -> Result<Vec<SessionId>> {
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT id FROM sessions WHERE phase = 'running'")
                .map_err(storage_error)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(storage_error)?;

            let ids = rows
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage_error)?;
            let mut recovered = Vec::new();
            let now = Utc::now().to_rfc3339();

            for id in ids {
                conn.execute(
                    "UPDATE sessions SET phase = 'error', updated_at = ?2 WHERE id = ?1",
                    params![id, now],
                )
                .map_err(storage_error)?;
                conn.execute(
                    "INSERT INTO messages (session_id, role, content, tokens, timestamp)
                     VALUES (?1, 'system', ?2, NULL, ?3)",
                    params![
                        id,
                        serde_json::to_string(&vec![ContentBlock::Text(
                            "Recovered crashed session: previous run ended unexpectedly.".into()
                        )])?,
                        now
                    ],
                )
                .map_err(storage_error)?;
                recovered.push(parse_session_id(&id)?);
            }

            Ok(recovered)
        })
    }

    pub async fn set_memory(
        &self,
        scope: &str,
        key: &str,
        value: &str,
        source: &str,
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO memory (id, scope, key, value, source, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(scope, key) DO UPDATE SET
                     value = excluded.value,
                     source = excluded.source,
                     updated_at = excluded.updated_at",
                params![id, scope, key, value, source, now],
            )
            .map_err(storage_error)?;
            Ok(())
        })
    }

    pub async fn get_memory(&self, scope: &str, key: &str) -> Result<Option<MemoryRecord>> {
        self.with_connection(|conn| {
            conn.query_row(
                "SELECT id, scope, key, value, source, created_at, updated_at
                 FROM memory WHERE scope = ?1 AND key = ?2",
                params![scope, key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_error)?
            .map(memory_record_from_row)
            .transpose()
        })
    }

    pub async fn list_memories(&self, scope: Option<&str>) -> Result<Vec<MemoryRecord>> {
        self.with_connection(|conn| {
            let sql = if scope.is_some() {
                "SELECT id, scope, key, value, source, created_at, updated_at
                 FROM memory WHERE scope = ?1 ORDER BY updated_at DESC, key ASC"
            } else {
                "SELECT id, scope, key, value, source, created_at, updated_at
                 FROM memory ORDER BY updated_at DESC, key ASC"
            };
            let mut stmt = conn.prepare(sql).map_err(storage_error)?;
            let mut rows = if let Some(scope) = scope {
                stmt.query(params![scope]).map_err(storage_error)?
            } else {
                stmt.query([]).map_err(storage_error)?
            };
            let mut results = Vec::new();
            while let Some(row) = rows.next().map_err(storage_error)? {
                results.push(memory_record_from_row((
                    row.get(0).map_err(storage_error)?,
                    row.get(1).map_err(storage_error)?,
                    row.get(2).map_err(storage_error)?,
                    row.get(3).map_err(storage_error)?,
                    row.get(4).map_err(storage_error)?,
                    row.get(5).map_err(storage_error)?,
                    row.get(6).map_err(storage_error)?,
                ))?);
            }
            Ok(results)
        })
    }

    pub async fn delete_memory(&self, scope: &str, key: &str) -> Result<()> {
        self.with_connection(|conn| {
            conn.execute(
                "DELETE FROM memory WHERE scope = ?1 AND key = ?2",
                params![scope, key],
            )
            .map_err(storage_error)?;
            Ok(())
        })
    }

    // ── Structured memory bank ───────────────────────────────────────────────

    pub async fn set_structured(&self, key: &str, value: serde_json::Value) -> Result<()> {
        let serialized = serde_json::to_string(&value)?;
        self.set_memory(
            MEMORY_SCOPE_STRUCTURED,
            key,
            &serialized,
            "structured_memory_bank",
        )
        .await
    }

    pub async fn get_structured(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let record = self.get_memory(MEMORY_SCOPE_STRUCTURED, key).await?;
        match record {
            Some(rec) => {
                let value: serde_json::Value = serde_json::from_str(&rec.value)?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    // ── Session fork ─────────────────────────────────────────────────────────

    pub async fn fork_session(&self, session_id: &SessionId) -> Result<SessionId> {
        let session_id_str = session_id.to_string();
        self.with_connection(|conn| {
            let row = SqliteStorage::load_session_row(conn, session_id)?
                .ok_or_else(|| CaduceusError::SessionNotFound(session_id.clone()))?;

            let new_id = Uuid::new_v4();
            let new_id_str = new_id.to_string();
            let now = Utc::now().to_rfc3339();

            conn.execute(
                "INSERT INTO sessions (
                    id, phase, project_root, provider_id, model_id, turn_count, created_at, updated_at,
                    context_limit, used_input, used_output, reserved_output
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    new_id_str,
                    "idle",
                    row.project_root,
                    row.provider_id,
                    row.model_id,
                    0i64,
                    now,
                    now,
                    row.context_limit,
                    row.used_input,
                    row.used_output,
                    row.reserved_output,
                ],
            )
            .map_err(storage_error)?;

            conn.execute(
                "INSERT INTO messages (session_id, role, content, tokens, timestamp)
                 SELECT ?1, role, content, tokens, timestamp
                 FROM messages WHERE session_id = ?2 ORDER BY id ASC",
                params![new_id_str, session_id_str],
            )
            .map_err(storage_error)?;

            Ok(SessionId(new_id))
        })
    }

    // ── Session trace ────────────────────────────────────────────────────────

    pub async fn record_trace_event(&self, event: &TraceEvent) -> Result<i64> {
        let timestamp = event.timestamp.to_rfc3339();
        let event_type = serde_json::to_string(&event.event_type)?;
        let event_type = event_type.trim_matches('"').to_string();
        let event_data = serde_json::to_string(&event.event_data)?;
        let duration_ms = event.duration_ms.map(|d| d as i64);
        let session_id = event.session_id.clone();

        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO session_trace (session_id, event_type, event_data, duration_ms, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, event_type, event_data, duration_ms, timestamp],
            )
            .map_err(storage_error)?;
            Ok(conn.last_insert_rowid())
        })
    }

    pub async fn list_trace_events(&self, session_id: &str) -> Result<Vec<TraceEvent>> {
        let session_id = session_id.to_string();
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT session_id, event_type, event_data, duration_ms, timestamp
                     FROM session_trace
                     WHERE session_id = ?1
                     ORDER BY id ASC",
                )
                .map_err(storage_error)?;

            let rows = stmt
                .query_map(params![session_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(storage_error)?;

            let mut events = Vec::new();
            for row in rows {
                let (sid, etype, edata, duration_ms, timestamp) = row.map_err(storage_error)?;
                let event_type = match etype.as_str() {
                    "llm_call" => TraceEventType::LlmCall,
                    "tool_exec" => TraceEventType::ToolExec,
                    "permission" => TraceEventType::Permission,
                    other => {
                        return Err(CaduceusError::Storage(format!(
                            "unknown trace event type `{other}`"
                        )))
                    }
                };
                events.push(TraceEvent {
                    session_id: sid,
                    event_type,
                    event_data: serde_json::from_str(&edata)?,
                    duration_ms: duration_ms.map(|d| d as u64),
                    timestamp: parse_timestamp(&timestamp)?,
                });
            }
            Ok(events)
        })
    }

    fn load_session_row(conn: &Connection, id: &SessionId) -> Result<Option<SessionRow>> {
        conn.query_row(
            "SELECT id, phase, project_root, provider_id, model_id, turn_count, created_at, updated_at,
                    context_limit, used_input, used_output, reserved_output
             FROM sessions
             WHERE id = ?1",
            params![id.to_string()],
            |row| {
                Ok(SessionRow {
                    id: row.get(0)?,
                    phase: row.get(1)?,
                    project_root: row.get(2)?,
                    provider_id: row.get(3)?,
                    model_id: row.get(4)?,
                    turn_count: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                    context_limit: row.get(8)?,
                    used_input: row.get(9)?,
                    used_output: row.get(10)?,
                    reserved_output: row.get(11)?,
                })
            },
        )
        .optional()
        .map_err(storage_error)
    }
}

#[async_trait::async_trait]
impl SessionStorage for SqliteStorage {
    async fn create_session(&self, state: &SessionState) -> Result<()> {
        let session = decompose_session(state)?;
        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO sessions (
                    id, phase, project_root, provider_id, model_id, turn_count, created_at, updated_at,
                    context_limit, used_input, used_output, reserved_output
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    session.id,
                    session.phase,
                    session.project_root,
                    session.provider_id,
                    session.model_id,
                    session.turn_count,
                    session.created_at,
                    session.updated_at,
                    session.context_limit,
                    session.used_input,
                    session.used_output,
                    session.reserved_output,
                ],
            )
            .map_err(storage_error)?;
            Ok(())
        })
    }

    async fn load_session(&self, id: &SessionId) -> Result<Option<SessionState>> {
        self.with_connection(|conn| {
            SqliteStorage::load_session_row(conn, id)?
                .map(session_from_row)
                .transpose()
        })
    }

    async fn update_session(&self, state: &SessionState) -> Result<()> {
        let session = decompose_session(state)?;
        self.with_connection(|conn| {
            let updated = conn
                .execute(
                    "UPDATE sessions
                     SET phase = ?1,
                         project_root = ?2,
                         provider_id = ?3,
                         model_id = ?4,
                         turn_count = ?5,
                         created_at = ?6,
                         updated_at = ?7,
                         context_limit = ?8,
                         used_input = ?9,
                         used_output = ?10,
                         reserved_output = ?11
                     WHERE id = ?12",
                    params![
                        session.phase,
                        session.project_root,
                        session.provider_id,
                        session.model_id,
                        session.turn_count,
                        session.created_at,
                        session.updated_at,
                        session.context_limit,
                        session.used_input,
                        session.used_output,
                        session.reserved_output,
                        session.id,
                    ],
                )
                .map_err(storage_error)?;

            if updated == 0 {
                return Err(CaduceusError::SessionNotFound(state.id.clone()));
            }
            Ok(())
        })
    }

    async fn list_sessions(&self, limit: usize) -> Result<Vec<SessionState>> {
        let limit = i64::try_from(limit).map_err(|_| {
            CaduceusError::Storage("session list limit exceeds SQLite INTEGER range".into())
        })?;
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, phase, project_root, provider_id, model_id, turn_count, created_at, updated_at,
                            context_limit, used_input, used_output, reserved_output
                     FROM sessions
                     ORDER BY updated_at DESC
                     LIMIT ?1",
                )
                .map_err(storage_error)?;

            let rows = stmt
                .query_map(params![limit], |row| {
                    Ok(SessionRow {
                        id: row.get(0)?,
                        phase: row.get(1)?,
                        project_root: row.get(2)?,
                        provider_id: row.get(3)?,
                        model_id: row.get(4)?,
                        turn_count: row.get(5)?,
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                        context_limit: row.get(8)?,
                        used_input: row.get(9)?,
                        used_output: row.get(10)?,
                        reserved_output: row.get(11)?,
                    })
                })
                .map_err(storage_error)?;

            let mut sessions = Vec::new();
            for row in rows {
                sessions.push(session_from_row(row.map_err(storage_error)?)?);
            }
            Ok(sessions)
        })
    }

    async fn delete_session(&self, id: &SessionId) -> Result<()> {
        self.with_connection(|conn| {
            conn.execute(
                "DELETE FROM sessions WHERE id = ?1",
                params![id.to_string()],
            )
            .map_err(storage_error)?;
            Ok(())
        })
    }
}

#[async_trait::async_trait]
impl AuthStore for SqliteStorage {
    async fn get_api_key(&self, provider_id: &ProviderId) -> Result<Option<String>> {
        self.with_connection(|conn| {
            conn.query_row(
                "SELECT api_key FROM auth_keys WHERE provider_id = ?1",
                params![provider_id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_error)
        })
    }

    async fn set_api_key(&self, provider_id: &ProviderId, key: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO auth_keys (provider_id, api_key, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(provider_id) DO UPDATE SET
                    api_key = excluded.api_key,
                    updated_at = excluded.updated_at",
                params![provider_id.0, key, now],
            )
            .map_err(storage_error)?;
            Ok(())
        })
    }

    async fn delete_api_key(&self, provider_id: &ProviderId) -> Result<()> {
        self.with_connection(|conn| {
            conn.execute(
                "DELETE FROM auth_keys WHERE provider_id = ?1",
                params![provider_id.0],
            )
            .map_err(storage_error)?;
            Ok(())
        })
    }
}

#[derive(Debug)]
struct PersistedSession {
    id: String,
    phase: String,
    project_root: String,
    provider_id: String,
    model_id: String,
    turn_count: i64,
    created_at: String,
    updated_at: String,
    context_limit: i64,
    used_input: i64,
    used_output: i64,
    reserved_output: i64,
}

fn decompose_session(state: &SessionState) -> Result<PersistedSession> {
    Ok(PersistedSession {
        id: state.id.to_string(),
        phase: session_phase_to_str(state.phase).to_string(),
        project_root: path_to_string(&state.project_root),
        provider_id: state.provider_id.0.clone(),
        model_id: state.model_id.0.clone(),
        turn_count: i64::from(state.turn_count),
        created_at: state.created_at.to_rfc3339(),
        updated_at: state.updated_at.to_rfc3339(),
        context_limit: i64::from(state.token_budget.context_limit),
        used_input: i64::from(state.token_budget.used_input),
        used_output: i64::from(state.token_budget.used_output),
        reserved_output: i64::from(state.token_budget.reserved_output),
    })
}

fn session_from_row(row: SessionRow) -> Result<SessionState> {
    Ok(SessionState {
        id: parse_session_id(&row.id)?,
        phase: session_phase_from_str(&row.phase)?,
        project_root: PathBuf::from(row.project_root),
        provider_id: ProviderId::new(row.provider_id),
        model_id: ModelId::new(row.model_id),
        token_budget: TokenBudget {
            context_limit: transpose_u32(Some(row.context_limit))?.unwrap_or_default(),
            used_input: transpose_u32(Some(row.used_input))?.unwrap_or_default(),
            used_output: transpose_u32(Some(row.used_output))?.unwrap_or_default(),
            reserved_output: transpose_u32(Some(row.reserved_output))?.unwrap_or_default(),
        },
        turn_count: transpose_u32(Some(row.turn_count))?.unwrap_or_default(),
        created_at: parse_timestamp(&row.created_at)?,
        updated_at: parse_timestamp(&row.updated_at)?,
    })
}

fn extract_tool_call_id(blocks: &[ContentBlock]) -> Option<String> {
    blocks.iter().find_map(|block| match block {
        ContentBlock::ToolUse { id, .. } => Some(id.0.clone()),
        ContentBlock::ToolResult { tool_call_id, .. } => Some(tool_call_id.0.clone()),
        ContentBlock::Text(_) | ContentBlock::Image(_) => None,
    })
}

fn memory_record_from_row(
    row: (String, String, String, String, String, String, String),
) -> Result<MemoryRecord> {
    let (id, scope, key, value, source, created_at, updated_at) = row;
    Ok(MemoryRecord {
        id,
        scope,
        key,
        value,
        source,
        created_at: parse_timestamp(&created_at)?,
        updated_at: parse_timestamp(&updated_at)?,
    })
}

fn storage_error(err: rusqlite::Error) -> CaduceusError {
    CaduceusError::Storage(err.to_string())
}

fn parse_session_id(value: &str) -> Result<SessionId> {
    let uuid = Uuid::parse_str(value)
        .map_err(|err| CaduceusError::Storage(format!("invalid session id `{value}`: {err}")))?;
    Ok(SessionId(uuid))
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| CaduceusError::Storage(format!("invalid timestamp `{value}`: {err}")))
}

fn transpose_u32(value: Option<i64>) -> Result<Option<u32>> {
    value
        .map(|raw| {
            u32::try_from(raw)
                .map_err(|_| CaduceusError::Storage(format!("value {raw} is out of range for u32")))
        })
        .transpose()
}

fn transpose_u64(value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| CaduceusError::Storage(format!("value {value} is out of range for u64")))
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn session_phase_to_str(phase: SessionPhase) -> &'static str {
    match phase {
        SessionPhase::Idle => "idle",
        SessionPhase::Running => "running",
        SessionPhase::AwaitingPermission => "awaiting_permission",
        SessionPhase::Cancelling => "cancelling",
        SessionPhase::Completed => "completed",
        SessionPhase::Error => "error",
    }
}

fn session_phase_from_str(value: &str) -> Result<SessionPhase> {
    match value {
        "idle" => Ok(SessionPhase::Idle),
        "running" => Ok(SessionPhase::Running),
        "awaiting_permission" => Ok(SessionPhase::AwaitingPermission),
        "cancelling" => Ok(SessionPhase::Cancelling),
        "completed" => Ok(SessionPhase::Completed),
        "error" => Ok(SessionPhase::Error),
        other => Err(CaduceusError::Storage(format!(
            "unknown session phase `{other}`"
        ))),
    }
}

fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}

fn role_from_str(value: &str) -> Result<Role> {
    match value {
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "system" => Ok(Role::System),
        other => Err(CaduceusError::Storage(format!("unknown role `{other}`"))),
    }
}

fn audit_decision_to_str(decision: AuditDecision) -> &'static str {
    match decision {
        AuditDecision::Allowed => "allowed",
        AuditDecision::Denied => "denied",
        AuditDecision::UserApproved => "user_approved",
        AuditDecision::UserDenied => "user_denied",
    }
}

fn audit_decision_from_str(value: &str) -> Result<AuditDecision> {
    match value {
        "allowed" => Ok(AuditDecision::Allowed),
        "denied" => Ok(AuditDecision::Denied),
        "user_approved" => Ok(AuditDecision::UserApproved),
        "user_denied" => Ok(AuditDecision::UserDenied),
        other => Err(CaduceusError::Storage(format!(
            "unknown audit decision `{other}`"
        ))),
    }
}

// ── Feature #187: Replay Debugging ────────────────────────────────────────

use serde::{Deserialize as ReplayDeserialize, Serialize as ReplaySerialize};

#[derive(Debug, Clone, ReplaySerialize, ReplayDeserialize, PartialEq)]
pub enum ReplayEventType {
    UserMessage,
    AssistantResponse,
    ToolCall,
    ToolResult,
    StateChange,
    Error,
}

#[derive(Debug, Clone, ReplaySerialize, ReplayDeserialize)]
pub struct ReplayEvent {
    pub timestamp: u64,
    pub event_type: ReplayEventType,
    pub data: serde_json::Value,
}

/// Records agent session events for later replay.
pub struct SessionRecorder {
    events: Vec<ReplayEvent>,
    session_id: String,
}

impl SessionRecorder {
    pub fn new(session_id: &str) -> Self {
        Self {
            events: Vec::new(),
            session_id: session_id.to_string(),
        }
    }

    pub fn record(&mut self, event_type: ReplayEventType, data: serde_json::Value) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.events.push(ReplayEvent {
            timestamp,
            event_type,
            data,
        });
    }

    pub fn export(&self) -> String {
        serde_json::to_string(&serde_json::json!({
            "session_id": self.session_id,
            "events": self.events,
        }))
        .unwrap_or_default()
    }

    pub fn import(json: &str) -> std::result::Result<Self, CaduceusError> {
        let v: serde_json::Value = serde_json::from_str(json)?;
        let session_id = v["session_id"].as_str().unwrap_or("").to_string();
        let events: Vec<ReplayEvent> = serde_json::from_value(v["events"].clone())?;
        Ok(Self { events, session_id })
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn events_by_type(&self, event_type: &ReplayEventType) -> Vec<&ReplayEvent> {
        self.events
            .iter()
            .filter(|e| &e.event_type == event_type)
            .collect()
    }
}

/// Steps through recorded events for debugging / audit.
pub struct SessionReplayer {
    events: Vec<ReplayEvent>,
    cursor: usize,
}

impl SessionReplayer {
    pub fn new(events: Vec<ReplayEvent>) -> Self {
        Self { events, cursor: 0 }
    }

    /// Returns the event at the current cursor position and advances the cursor.
    pub fn step_forward(&mut self) -> Option<&ReplayEvent> {
        if self.cursor < self.events.len() {
            let event = &self.events[self.cursor];
            self.cursor += 1;
            Some(event)
        } else {
            None
        }
    }

    /// Moves the cursor back one position and returns the event there.
    pub fn step_back(&mut self) -> Option<&ReplayEvent> {
        if self.cursor > 0 {
            self.cursor -= 1;
            Some(&self.events[self.cursor])
        } else {
            None
        }
    }

    /// Moves the cursor to `index` and returns the event there.
    pub fn jump_to(&mut self, index: usize) -> Option<&ReplayEvent> {
        if index < self.events.len() {
            self.cursor = index;
            Some(&self.events[self.cursor])
        } else {
            None
        }
    }

    /// Returns the event at the current cursor without moving.
    pub fn current(&self) -> Option<&ReplayEvent> {
        self.events.get(self.cursor)
    }

    /// Number of events not yet consumed by `step_forward`.
    pub fn remaining(&self) -> usize {
        self.events.len().saturating_sub(self.cursor)
    }
}

// ── Memory quality and trajectory utilities ───────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteDecision {
    Allow,
    FlagForReview(String),
    Block(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySourceValidity {
    /// NOTE: Presence of source markers does NOT guarantee content authenticity. This is a heuristic, not a security boundary.
    HasSourceMarkers,
    Unverified,
    SuspiciouslyRepetitive,
}

pub struct MemoryEntrenchmentGuard {
    access_counts: HashMap<String, u32>,
    staleness_threshold: u32,
}

impl MemoryEntrenchmentGuard {
    pub fn new(threshold: u32) -> Self {
        Self {
            access_counts: HashMap::new(),
            staleness_threshold: threshold.max(1),
        }
    }

    pub fn record_access(&mut self, memory_id: &str) {
        let entry = self.access_counts.entry(memory_id.to_string()).or_insert(0);
        *entry = entry.saturating_add(1);
    }

    pub fn record_write(&mut self, memory_id: &str, content: &str) -> WriteDecision {
        match self.validate_memory_source(content) {
            MemorySourceValidity::SuspiciouslyRepetitive => {
                WriteDecision::Block("memory content appears suspiciously repetitive".to_string())
            }
            MemorySourceValidity::Unverified => {
                WriteDecision::FlagForReview("memory source could not be verified".to_string())
            }
            MemorySourceValidity::HasSourceMarkers if self.check_entrenchment(memory_id) => {
                WriteDecision::FlagForReview(
                    "memory has become entrenched and needs review".to_string(),
                )
            }
            MemorySourceValidity::HasSourceMarkers => {
                self.access_counts.insert(memory_id.to_string(), 0);
                WriteDecision::Allow
            }
        }
    }

    pub fn check_entrenchment(&self, memory_id: &str) -> bool {
        self.access_counts
            .get(memory_id)
            .copied()
            .unwrap_or_default()
            >= self.staleness_threshold
    }

    pub fn get_stale_memories(&self) -> Vec<String> {
        let mut stale: Vec<String> = self
            .access_counts
            .iter()
            .filter(|(_, count)| **count >= self.staleness_threshold)
            .map(|(memory_id, _)| memory_id.clone())
            .collect();
        stale.sort();
        stale
    }

    pub fn validate_memory_source(&self, content: &str) -> MemorySourceValidity {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return MemorySourceValidity::Unverified;
        }

        let words: Vec<String> = trimmed
            .split(|character: char| !character.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(|token| token.to_ascii_lowercase())
            .collect();
        let unique_words = words.iter().collect::<std::collections::HashSet<_>>().len();
        if words.len() >= 4 && unique_words.saturating_mul(2) <= words.len() {
            return MemorySourceValidity::SuspiciouslyRepetitive;
        }

        let lower = trimmed.to_ascii_lowercase();
        if [
            "https://",
            "http://",
            "source:",
            "according to",
            "observed",
            "documented",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            MemorySourceValidity::HasSourceMarkers
        } else {
            MemorySourceValidity::Unverified
        }
    }
}

#[derive(Debug, Clone, ReplaySerialize, ReplayDeserialize, PartialEq, Eq)]
pub enum TrajectoryEventType {
    ToolCall,
    UserMessage,
    AgentResponse,
    ModeSwitch,
    Error,
}

#[derive(Debug, Clone, ReplaySerialize, ReplayDeserialize, PartialEq, Eq)]
pub struct TrajectoryEvent {
    pub turn: usize,
    pub event_type: TrajectoryEventType,
    pub tool_name: Option<String>,
    pub input_summary: String,
    pub output_summary: String,
    pub success: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, ReplaySerialize, ReplayDeserialize)]
pub struct TrajectoryRecorder {
    session_id: String,
    events: Vec<TrajectoryEvent>,
}

impl TrajectoryRecorder {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            events: Vec::new(),
        }
    }

    pub fn record(&mut self, event: TrajectoryEvent) {
        self.events.push(event);
    }

    pub fn export_trajectory(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn import_trajectory(json: &str) -> std::result::Result<Self, CaduceusError> {
        let recorder: Self = serde_json::from_str(json)?;
        if recorder.session_id.trim().is_empty() {
            return Err(CaduceusError::Storage(
                "trajectory session_id must not be empty".to_string(),
            ));
        }
        Ok(recorder)
    }

    pub fn successful_patterns(&self) -> Vec<Vec<&TrajectoryEvent>> {
        self.recurring_patterns(true)
    }

    pub fn failure_patterns(&self) -> Vec<Vec<&TrajectoryEvent>> {
        self.recurring_patterns(false)
    }

    fn recurring_patterns(&self, success: bool) -> Vec<Vec<&TrajectoryEvent>> {
        let mut runs: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        let mut index = 0;
        while index < self.events.len() {
            if self.events[index].success != success {
                index += 1;
                continue;
            }

            let start = index;
            while index < self.events.len() && self.events[index].success == success {
                index += 1;
            }

            if index - start >= 2 {
                let signature = self.events[start..index]
                    .iter()
                    .map(pattern_key)
                    .collect::<Vec<_>>()
                    .join(" -> ");
                runs.entry(signature)
                    .or_default()
                    .push((start, index - start));
            }
        }

        let mut patterns: Vec<(usize, Vec<&TrajectoryEvent>)> = runs
            .into_values()
            .filter(|occurrences| occurrences.len() > 1)
            .map(|occurrences| {
                let (start, len) = occurrences[0];
                (
                    start,
                    self.events[start..start + len].iter().collect::<Vec<_>>(),
                )
            })
            .collect();
        patterns.sort_by_key(|(start, _)| *start);
        patterns.into_iter().map(|(_, events)| events).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelevanceEntry {
    pub id: String,
    pub initial_relevance: f64,
    pub current_relevance: f64,
    pub last_accessed: u64,
    pub access_count: u32,
}

pub struct RelevanceDecayManager {
    entries: HashMap<String, RelevanceEntry>,
    decay_rate: f64,
}

impl RelevanceDecayManager {
    pub fn new(decay_rate: f64) -> Self {
        Self {
            entries: HashMap::new(),
            decay_rate: decay_rate.clamp(0.0, 1.0),
        }
    }

    pub fn add_entry(&mut self, id: &str, relevance: f64) {
        let relevance = relevance.clamp(0.0, 1.0);
        self.entries.insert(
            id.to_string(),
            RelevanceEntry {
                id: id.to_string(),
                initial_relevance: relevance,
                current_relevance: relevance,
                last_accessed: 0,
                access_count: 0,
            },
        );
    }

    pub fn access(&mut self, id: &str) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.access_count = entry.access_count.saturating_add(1);
            entry.last_accessed = 0;
            entry.current_relevance =
                (entry.current_relevance + (self.decay_rate / 2.0).max(0.05)).min(1.0);
        }
    }

    pub fn decay_tick(&mut self) {
        let decay_factor = (1.0 - self.decay_rate).clamp(0.0, 1.0);
        for entry in self.entries.values_mut() {
            entry.current_relevance = (entry.current_relevance * decay_factor).clamp(0.0, 1.0);
            entry.last_accessed = entry.last_accessed.saturating_add(1);
        }
    }

    pub fn get_relevance(&self, id: &str) -> Option<f64> {
        self.entries.get(id).map(|entry| entry.current_relevance)
    }

    pub fn prune_below(&mut self, threshold: f64) -> Vec<String> {
        let mut pruned: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.current_relevance < threshold)
            .map(|(id, _)| id.clone())
            .collect();
        pruned.sort();
        for id in &pruned {
            self.entries.remove(id);
        }
        pruned
    }

    pub fn top_n(&self, n: usize) -> Vec<(&str, f64)> {
        let mut entries: Vec<(&str, f64)> = self
            .entries
            .values()
            .map(|entry| (entry.id.as_str(), entry.current_relevance))
            .collect();
        entries.sort_by(|left, right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(right.0)));
        entries.truncate(n);
        entries
    }
}

fn pattern_key(event: &TrajectoryEvent) -> String {
    format!(
        "{:?}:{}:{}",
        event.event_type,
        event.tool_name.as_deref().unwrap_or("-"),
        event.success
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_storage() -> (TempDir, SqliteStorage) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("storage.sqlite3");
        let storage = SqliteStorage::open(&db_path).unwrap();
        (dir, storage)
    }

    fn sample_state() -> SessionState {
        let mut state = SessionState::new(
            "/workspace/demo",
            ProviderId::new("anthropic"),
            ModelId::new("claude-sonnet-4-6"),
        );
        state.phase = SessionPhase::Running;
        state.turn_count = 3;
        state.token_budget = TokenBudget {
            context_limit: 128_000,
            used_input: 1_200,
            used_output: 400,
            reserved_output: 4_096,
        };
        state
    }

    #[tokio::test]
    async fn enables_wal_mode_on_open() {
        let (_dir, storage) = temp_storage();
        let conn = storage.conn.lock().unwrap();
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");
    }

    #[tokio::test]
    async fn initializes_schema_version() {
        let (_dir, storage) = temp_storage();
        let conn = storage.conn.lock().unwrap();
        let version: i64 = conn
            .query_row(
                "SELECT version FROM schema_version WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn create_and_load_session_round_trips_full_state() {
        let (_dir, storage) = temp_storage();
        let state = sample_state();
        let id = state.id.clone();

        storage.create_session(&state).await.unwrap();
        let loaded = storage.load_session(&id).await.unwrap().unwrap();

        assert_eq!(loaded.id.to_string(), state.id.to_string());
        assert_eq!(loaded.phase as u8, state.phase as u8);
        assert_eq!(loaded.project_root, state.project_root);
        assert_eq!(loaded.provider_id.0, state.provider_id.0);
        assert_eq!(loaded.model_id.0, state.model_id.0);
        assert_eq!(loaded.turn_count, state.turn_count);
        assert_eq!(
            loaded.token_budget.context_limit,
            state.token_budget.context_limit
        );
        assert_eq!(
            loaded.token_budget.used_input,
            state.token_budget.used_input
        );
        assert_eq!(
            loaded.token_budget.used_output,
            state.token_budget.used_output
        );
        assert_eq!(
            loaded.token_budget.reserved_output,
            state.token_budget.reserved_output
        );
    }

    #[tokio::test]
    async fn update_session_persists_changes() {
        let (_dir, storage) = temp_storage();
        let mut state = sample_state();
        let id = state.id.clone();
        storage.create_session(&state).await.unwrap();

        state.phase = SessionPhase::Completed;
        state.turn_count = 9;
        state.token_budget.used_output = 777;
        state.updated_at = Utc::now();
        storage.update_session(&state).await.unwrap();

        let loaded = storage.load_session(&id).await.unwrap().unwrap();
        assert!(matches!(loaded.phase, SessionPhase::Completed));
        assert_eq!(loaded.turn_count, 9);
        assert_eq!(loaded.token_budget.used_output, 777);
    }

    #[tokio::test]
    async fn list_sessions_respects_order_and_limit() {
        let (_dir, storage) = temp_storage();
        let mut older = sample_state();
        older.updated_at = Utc::now() - chrono::TimeDelta::seconds(30);
        let mut newer = sample_state();
        newer.updated_at = Utc::now();

        storage.create_session(&older).await.unwrap();
        storage.create_session(&newer).await.unwrap();

        let sessions = storage.list_sessions(1).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id.to_string(), newer.id.to_string());
    }

    #[tokio::test]
    async fn saves_and_lists_messages() {
        let (_dir, storage) = temp_storage();
        let state = sample_state();
        storage.create_session(&state).await.unwrap();

        let message = LlmMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("hello".into()),
                ContentBlock::ToolUse {
                    id: ToolCallId::new("tool-1"),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "pwd"}),
                },
            ],
        };

        storage
            .save_message(&state.id, &message, Some(42))
            .await
            .unwrap();
        let messages = storage.list_messages(&state.id).await.unwrap();

        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0].message.role, Role::Assistant));
        assert_eq!(messages[0].tokens, Some(42));
        assert_eq!(messages[0].message.content.len(), 2);
    }

    #[tokio::test]
    async fn records_and_lists_tool_calls() {
        let (_dir, storage) = temp_storage();
        let state = sample_state();
        storage.create_session(&state).await.unwrap();

        let call = StoredToolCall {
            id: ToolCallId::new("call-1"),
            session_id: state.id.clone(),
            tool_name: "bash".into(),
            input: r#"{"command":"ls"}"#.into(),
            output: Some("ok".into()),
            is_error: false,
            duration_ms: 12,
            timestamp: Utc::now(),
        };

        storage.record_tool_call(&call).await.unwrap();
        let calls = storage.list_tool_calls(&state.id).await.unwrap();

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.0, "call-1");
        assert_eq!(calls[0].tool_name, "bash");
        assert_eq!(calls[0].duration_ms, 12);
    }

    #[tokio::test]
    async fn appends_and_lists_audit_entries() {
        let (_dir, storage) = temp_storage();
        let state = sample_state();
        storage.create_session(&state).await.unwrap();

        let entry = AuditEntry {
            timestamp: Utc::now(),
            session_id: state.id.clone(),
            capability: "process.exec".into(),
            tool_name: "bash".into(),
            args_redacted: "{\"command\":\"echo ***\"}".into(),
            decision: AuditDecision::UserApproved,
        };

        storage.append_audit(&entry).await.unwrap();
        let entries = storage.list_audit(&state.id).await.unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].capability, "process.exec");
        assert!(matches!(entries[0].decision, AuditDecision::UserApproved));
    }

    #[tokio::test]
    async fn records_costs_and_computes_total() {
        let (_dir, storage) = temp_storage();
        let state = sample_state();
        storage.create_session(&state).await.unwrap();

        storage
            .record_cost(
                &state.id,
                &state.provider_id,
                &state.model_id,
                100,
                50,
                0.125,
            )
            .await
            .unwrap();
        storage
            .record_cost(&state.id, &state.provider_id, &state.model_id, 10, 5, 0.025)
            .await
            .unwrap();

        let costs = storage.list_costs(&state.id).await.unwrap();
        let total = storage.total_cost(&state.id).await.unwrap();

        assert_eq!(costs.len(), 2);
        assert_eq!(costs[0].input_tokens, 100);
        assert!((total - 0.15).abs() < 1e-9);
    }

    #[tokio::test]
    async fn delete_session_cascades_related_records() {
        let (_dir, storage) = temp_storage();
        let state = sample_state();
        storage.create_session(&state).await.unwrap();

        storage
            .save_message(&state.id, &LlmMessage::user("hello"), Some(5))
            .await
            .unwrap();
        storage
            .record_tool_call(&StoredToolCall {
                id: ToolCallId::new("call-2"),
                session_id: state.id.clone(),
                tool_name: "read".into(),
                input: "{}".into(),
                output: None,
                is_error: false,
                duration_ms: 1,
                timestamp: Utc::now(),
            })
            .await
            .unwrap();
        storage
            .append_audit(&AuditEntry {
                timestamp: Utc::now(),
                session_id: state.id.clone(),
                capability: "fs.read".into(),
                tool_name: "read".into(),
                args_redacted: "{}".into(),
                decision: AuditDecision::Allowed,
            })
            .await
            .unwrap();
        storage
            .record_cost(&state.id, &state.provider_id, &state.model_id, 1, 1, 0.01)
            .await
            .unwrap();

        storage.delete_session(&state.id).await.unwrap();

        assert!(storage.load_session(&state.id).await.unwrap().is_none());
        assert!(storage.list_messages(&state.id).await.unwrap().is_empty());
        assert!(storage.list_tool_calls(&state.id).await.unwrap().is_empty());
        assert!(storage.list_audit(&state.id).await.unwrap().is_empty());
        assert!(storage.list_costs(&state.id).await.unwrap().is_empty());
        assert_eq!(storage.total_cost(&state.id).await.unwrap(), 0.0);
    }

    #[tokio::test]
    async fn migrates_existing_v1_database() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("legacy.sqlite3");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(BOOTSTRAP_SCHEMA_VERSION).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute("UPDATE schema_version SET version = 1 WHERE id = 1", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, phase, project_root, provider_id, model_id, turn_count, created_at, updated_at)
             VALUES (?1, 'idle', '/legacy', 'anthropic', 'claude-sonnet-4-6', 1, ?2, ?2)",
            params![Uuid::new_v4().to_string(), Utc::now().to_rfc3339()],
        )
        .unwrap();
        drop(conn);

        let storage = SqliteStorage::open(&db_path).unwrap();
        let conn = storage.conn.lock().unwrap();
        let version: i64 = conn
            .query_row(
                "SELECT version FROM schema_version WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let context_limit: i64 = conn
            .query_row("SELECT context_limit FROM sessions LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();

        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert_eq!(context_limit, 200_000);
    }

    #[tokio::test]
    async fn exports_transcript_as_jsonl() {
        let (dir, storage) = temp_storage();
        let state = sample_state();
        storage.create_session(&state).await.unwrap();
        storage
            .save_message(&state.id, &LlmMessage::user("hello"), Some(5))
            .await
            .unwrap();
        storage
            .save_message(
                &state.id,
                &LlmMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: ToolCallId::new("tool-123"),
                        name: "bash".into(),
                        input: serde_json::json!({"command":"pwd"}),
                    }],
                },
                Some(8),
            )
            .await
            .unwrap();

        let export_path = dir.path().join("transcript.jsonl");
        storage
            .export_transcript(&state.id, &export_path)
            .await
            .unwrap();
        let lines: Vec<_> = std::fs::read_to_string(export_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["role"], "user");
        assert_eq!(lines[1]["tool_call_id"], "tool-123");
    }

    #[tokio::test]
    async fn resume_session_returns_state_and_history() {
        let (_dir, storage) = temp_storage();
        let state = sample_state();
        storage.create_session(&state).await.unwrap();
        storage
            .save_message(&state.id, &LlmMessage::user("hello"), Some(3))
            .await
            .unwrap();

        let resumed = storage.resume_session(&state.id).await.unwrap();
        assert_eq!(resumed.state.id.to_string(), state.id.to_string());
        assert_eq!(resumed.messages.len(), 1);
    }

    #[tokio::test]
    async fn recovers_crashed_sessions_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("recovery.sqlite3");
        let storage = SqliteStorage::open(&db_path).unwrap();
        let mut state = sample_state();
        state.phase = SessionPhase::Running;
        storage.create_session(&state).await.unwrap();
        drop(storage);

        let reopened = SqliteStorage::open(&db_path).unwrap();
        let loaded = reopened.load_session(&state.id).await.unwrap().unwrap();
        assert!(matches!(loaded.phase, SessionPhase::Error));
        let messages = reopened.list_messages(&state.id).await.unwrap();
        assert!(messages
            .iter()
            .any(|message| matches!(message.message.role, Role::System)));
    }

    #[tokio::test]
    async fn memory_crud_round_trips() {
        let (_dir, storage) = temp_storage();
        storage
            .set_memory("project", "summary", "important fact", "test")
            .await
            .unwrap();
        storage
            .set_memory("global", "version", "1", "test")
            .await
            .unwrap();

        let record = storage
            .get_memory("project", "summary")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.value, "important fact");

        let all = storage.list_memories(None).await.unwrap();
        assert_eq!(all.len(), 2);

        storage.delete_memory("project", "summary").await.unwrap();
        assert!(storage
            .get_memory("project", "summary")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn auth_store_round_trips_api_keys() {
        let (_dir, storage) = temp_storage();
        let provider = ProviderId::new("openai");
        storage.set_api_key(&provider, "secret").await.unwrap();
        assert_eq!(
            storage.get_api_key(&provider).await.unwrap().as_deref(),
            Some("secret")
        );
        storage.delete_api_key(&provider).await.unwrap();
        assert!(storage.get_api_key(&provider).await.unwrap().is_none());
    }

    // ── Feature tests: fork, structured memory, session trace ───────────────

    #[tokio::test]
    async fn test_fork_session_copies_messages() {
        let (_dir, storage) = temp_storage();
        let state = sample_state();
        storage.create_session(&state).await.unwrap();
        storage
            .save_message(&state.id, &LlmMessage::user("hello"), Some(5))
            .await
            .unwrap();
        storage
            .save_message(&state.id, &LlmMessage::user("world"), Some(3))
            .await
            .unwrap();

        let forked_id = storage.fork_session(&state.id).await.unwrap();

        let forked_session = storage.load_session(&forked_id).await.unwrap().unwrap();
        assert_eq!(forked_session.turn_count, 0);
        assert!(matches!(forked_session.phase, SessionPhase::Idle));

        let forked_messages = storage.list_messages(&forked_id).await.unwrap();
        assert_eq!(forked_messages.len(), 2);

        let original_messages = storage.list_messages(&state.id).await.unwrap();
        assert_eq!(original_messages.len(), 2);
    }

    #[tokio::test]
    async fn test_structured_memory_roundtrip() {
        let (_dir, storage) = temp_storage();
        let data = serde_json::json!({"key": "value", "count": 42});
        storage
            .set_structured(MEMORY_KEY_PROJECT_BRIEF, data.clone())
            .await
            .unwrap();

        let loaded = storage
            .get_structured(MEMORY_KEY_PROJECT_BRIEF)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded, data);

        assert!(storage
            .get_structured("nonexistent")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn test_session_trace_record_and_list() {
        let (_dir, storage) = temp_storage();
        let state = sample_state();
        storage.create_session(&state).await.unwrap();

        let event1 = TraceEvent {
            session_id: state.id.to_string(),
            event_type: TraceEventType::LlmCall,
            event_data: serde_json::json!({"model": "claude"}),
            duration_ms: Some(150),
            timestamp: Utc::now(),
        };
        let event2 = TraceEvent {
            session_id: state.id.to_string(),
            event_type: TraceEventType::ToolExec,
            event_data: serde_json::json!({"tool": "bash"}),
            duration_ms: Some(50),
            timestamp: Utc::now(),
        };

        storage.record_trace_event(&event1).await.unwrap();
        storage.record_trace_event(&event2).await.unwrap();

        let events = storage
            .list_trace_events(&state.id.to_string())
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0].event_type, TraceEventType::LlmCall));
        assert!(matches!(events[1].event_type, TraceEventType::ToolExec));
        assert_eq!(events[0].duration_ms, Some(150));
    }

    // ── Concurrent access tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_concurrent_session_writes() {
        let (_dir, storage) = temp_storage();
        let storage = std::sync::Arc::new(storage);

        let mut handles = Vec::new();
        for i in 0..10 {
            let s = storage.clone();
            handles.push(tokio::spawn(async move {
                let state = SessionState::new(
                    format!("/workspace/project-{i}"),
                    ProviderId::new("anthropic"),
                    ModelId::new("claude-sonnet-4-6"),
                );
                s.create_session(&state).await.unwrap();
                state.id
            }));
        }

        let mut ids = Vec::new();
        for handle in handles {
            ids.push(handle.await.unwrap());
        }

        // All 10 sessions should exist and be distinct
        assert_eq!(ids.len(), 10);
        let unique: std::collections::HashSet<String> =
            ids.iter().map(|id| id.to_string()).collect();
        assert_eq!(unique.len(), 10, "all session IDs should be unique");

        // Verify each session is loadable
        for id in &ids {
            let loaded = storage.load_session(id).await.unwrap();
            assert!(loaded.is_some(), "session {} should be loadable", id);
        }
    }

    #[tokio::test]
    async fn test_concurrent_message_inserts() {
        let (_dir, storage) = temp_storage();
        let state = sample_state();
        storage.create_session(&state).await.unwrap();

        let storage = std::sync::Arc::new(storage);
        let session_id = state.id.clone();
        let mut handles = Vec::new();

        for i in 0..20 {
            let s = storage.clone();
            let sid = session_id.clone();
            handles.push(tokio::spawn(async move {
                let msg = LlmMessage::user(format!("message-{i}"));
                s.save_message(&sid, &msg, Some(i as u32)).await.unwrap();
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        let messages = storage.list_messages(&session_id).await.unwrap();
        assert_eq!(
            messages.len(),
            20,
            "all 20 concurrent message inserts should succeed"
        );

        // Verify all messages are present (order may vary due to concurrency)
        let contents: Vec<String> = messages
            .iter()
            .filter_map(|m| {
                m.message.content.first().and_then(|c| match c {
                    ContentBlock::Text(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .collect();
        for i in 0..20 {
            assert!(
                contents.iter().any(|c| c == &format!("message-{i}")),
                "message-{i} should be present"
            );
        }
    }

    #[tokio::test]
    async fn test_session_not_found_error() {
        let (_dir, storage) = temp_storage();
        let bogus_id = SessionId::new();

        // load_session returns None for nonexistent session
        let result = storage.load_session(&bogus_id).await.unwrap();
        assert!(
            result.is_none(),
            "loading nonexistent session should return None, not panic"
        );

        // resume_session returns SessionNotFound error
        let result = storage.resume_session(&bogus_id).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, CaduceusError::SessionNotFound(_)),
            "expected SessionNotFound, got: {err}"
        );
    }

    // ── Feature #187: Replay Debugging tests ────────────────────────────────

    #[test]
    fn test_recorder_record_and_count() {
        let mut recorder = SessionRecorder::new("session-abc");
        recorder.record(
            ReplayEventType::UserMessage,
            serde_json::json!({"text": "hello"}),
        );
        recorder.record(
            ReplayEventType::AssistantResponse,
            serde_json::json!({"text": "hi"}),
        );
        assert_eq!(recorder.event_count(), 2);
    }

    #[test]
    fn test_recorder_export_import_roundtrip() {
        let mut recorder = SessionRecorder::new("roundtrip-session");
        recorder.record(
            ReplayEventType::ToolCall,
            serde_json::json!({"tool": "read"}),
        );
        recorder.record(
            ReplayEventType::ToolResult,
            serde_json::json!({"output": "file content"}),
        );
        recorder.record(ReplayEventType::Error, serde_json::json!({"msg": "oops"}));

        let json = recorder.export();
        assert!(!json.is_empty());

        let imported = SessionRecorder::import(&json).unwrap();
        assert_eq!(imported.event_count(), 3);
        assert_eq!(imported.session_id, "roundtrip-session");
    }

    #[test]
    fn test_recorder_events_by_type() {
        let mut recorder = SessionRecorder::new("filter-session");
        recorder.record(
            ReplayEventType::UserMessage,
            serde_json::json!({"text": "a"}),
        );
        recorder.record(
            ReplayEventType::ToolCall,
            serde_json::json!({"tool": "grep"}),
        );
        recorder.record(
            ReplayEventType::UserMessage,
            serde_json::json!({"text": "b"}),
        );

        let messages = recorder.events_by_type(&ReplayEventType::UserMessage);
        assert_eq!(messages.len(), 2);

        let tool_calls = recorder.events_by_type(&ReplayEventType::ToolCall);
        assert_eq!(tool_calls.len(), 1);

        let errors = recorder.events_by_type(&ReplayEventType::Error);
        assert_eq!(errors.len(), 0);
    }

    #[test]
    fn test_import_invalid_json_errors() {
        let result = SessionRecorder::import("not valid json {{{{");
        assert!(result.is_err());
    }

    #[test]
    fn test_replayer_step_forward() {
        let events = vec![
            ReplayEvent {
                timestamp: 1,
                event_type: ReplayEventType::UserMessage,
                data: serde_json::json!({"n": 1}),
            },
            ReplayEvent {
                timestamp: 2,
                event_type: ReplayEventType::AssistantResponse,
                data: serde_json::json!({"n": 2}),
            },
        ];
        let mut replayer = SessionReplayer::new(events);
        assert_eq!(replayer.remaining(), 2);

        let e1 = replayer.step_forward().unwrap();
        assert_eq!(e1.data["n"], 1);
        assert_eq!(replayer.remaining(), 1);

        let e2 = replayer.step_forward().unwrap();
        assert_eq!(e2.data["n"], 2);
        assert_eq!(replayer.remaining(), 0);

        assert!(replayer.step_forward().is_none());
    }

    #[test]
    fn test_replayer_step_back() {
        let events = vec![
            ReplayEvent {
                timestamp: 1,
                event_type: ReplayEventType::UserMessage,
                data: serde_json::json!({"n": 1}),
            },
            ReplayEvent {
                timestamp: 2,
                event_type: ReplayEventType::ToolCall,
                data: serde_json::json!({"n": 2}),
            },
        ];
        let mut replayer = SessionReplayer::new(events);
        replayer.step_forward();
        replayer.step_forward();

        let back = replayer.step_back().unwrap();
        assert_eq!(back.data["n"], 2);

        // step_back at start returns None
        replayer.step_back();
        assert!(replayer.step_back().is_none());
    }

    #[test]
    fn test_replayer_jump_to() {
        let events = vec![
            ReplayEvent {
                timestamp: 0,
                event_type: ReplayEventType::UserMessage,
                data: serde_json::json!({"i": 0}),
            },
            ReplayEvent {
                timestamp: 1,
                event_type: ReplayEventType::ToolCall,
                data: serde_json::json!({"i": 1}),
            },
            ReplayEvent {
                timestamp: 2,
                event_type: ReplayEventType::ToolResult,
                data: serde_json::json!({"i": 2}),
            },
        ];
        let mut replayer = SessionReplayer::new(events);

        let e = replayer.jump_to(2).unwrap();
        assert_eq!(e.data["i"], 2);
        assert_eq!(replayer.remaining(), 1);

        // out-of-bounds returns None
        assert!(replayer.jump_to(99).is_none());
    }

    #[test]
    fn test_replayer_current() {
        let events = vec![ReplayEvent {
            timestamp: 0,
            event_type: ReplayEventType::StateChange,
            data: serde_json::json!({}),
        }];
        let mut replayer = SessionReplayer::new(events);
        assert!(replayer.current().is_some());
        replayer.step_forward();
        assert!(replayer.current().is_none());
    }

    #[test]
    fn memory_entrenchment_guard_detects_staleness_and_write_decisions() {
        let mut guard = MemoryEntrenchmentGuard::new(2);
        guard.record_access("memory-1");
        guard.record_access("memory-1");

        assert!(guard.check_entrenchment("memory-1"));
        assert_eq!(guard.get_stale_memories(), vec!["memory-1".to_string()]);
        assert!(matches!(
            guard.record_write("memory-1", "source: runbook entry"),
            WriteDecision::FlagForReview(_)
        ));
        assert!(guard.check_entrenchment("memory-1"));
        assert!(matches!(
            guard.record_write("memory-2", "echo echo echo echo"),
            WriteDecision::Block(_)
        ));
    }

    #[test]
    fn memory_entrenchment_guard_validates_sources() {
        let guard = MemoryEntrenchmentGuard::new(3);
        assert_eq!(
            guard.validate_memory_source("https://docs.example.com/reference"),
            MemorySourceValidity::HasSourceMarkers
        );
        assert_eq!(
            guard.validate_memory_source(""),
            MemorySourceValidity::Unverified
        );
        assert_eq!(
            guard.validate_memory_source("repeat repeat repeat repeat"),
            MemorySourceValidity::SuspiciouslyRepetitive
        );
    }

    #[test]
    fn trajectory_recorder_round_trips_and_extracts_patterns() {
        let mut recorder = TrajectoryRecorder::new("session-42");
        for turn in [1, 3] {
            recorder.record(TrajectoryEvent {
                turn,
                event_type: TrajectoryEventType::UserMessage,
                tool_name: None,
                input_summary: "ask".to_string(),
                output_summary: "queued".to_string(),
                success: true,
                duration_ms: 1,
            });
            recorder.record(TrajectoryEvent {
                turn,
                event_type: TrajectoryEventType::ToolCall,
                tool_name: Some("search".to_string()),
                input_summary: "query".to_string(),
                output_summary: "results".to_string(),
                success: true,
                duration_ms: 12,
            });
            recorder.record(TrajectoryEvent {
                turn,
                event_type: TrajectoryEventType::Error,
                tool_name: Some("search".to_string()),
                input_summary: "query".to_string(),
                output_summary: "timeout".to_string(),
                success: false,
                duration_ms: 12,
            });
            recorder.record(TrajectoryEvent {
                turn,
                event_type: TrajectoryEventType::ModeSwitch,
                tool_name: None,
                input_summary: "retry".to_string(),
                output_summary: "background".to_string(),
                success: false,
                duration_ms: 2,
            });
        }

        let exported = recorder.export_trajectory();
        let imported = TrajectoryRecorder::import_trajectory(&exported).unwrap();
        assert_eq!(imported.session_id, "session-42");
        assert_eq!(imported.events.len(), 8);
        assert_eq!(imported.successful_patterns().len(), 1);
        assert_eq!(imported.failure_patterns().len(), 1);
        assert_eq!(imported.successful_patterns()[0].len(), 2);
    }

    #[test]
    fn relevance_decay_manager_decays_accesses_and_prunes() {
        let mut manager = RelevanceDecayManager::new(0.2);
        manager.add_entry("alpha", 0.9);
        manager.add_entry("beta", 0.4);

        manager.decay_tick();
        let alpha_after_decay = manager.get_relevance("alpha").unwrap();
        assert!((alpha_after_decay - 0.72).abs() < 1e-9);

        manager.access("beta");
        assert!(manager.get_relevance("beta").unwrap() > 0.4);
        assert_eq!(manager.entries["beta"].last_accessed, 0);
        assert_eq!(manager.entries["beta"].access_count, 1);

        let pruned = manager.prune_below(0.5);
        assert_eq!(pruned, vec!["beta".to_string()]);
        let top = manager.top_n(1);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].0, "alpha");
    }
}
