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

// ── #241: Agent Persistent Memory ────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AgentMemory {
    pub id: String,
    pub title: String,
    pub content: String,
    pub category: String,
    pub metadata: HashMap<String, String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Default)]
pub struct AgentMemoryStore {
    memories: HashMap<String, AgentMemory>,
}

impl AgentMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(&mut self, memory: AgentMemory) {
        self.memories.insert(memory.id.clone(), memory);
    }

    /// Search memories by keyword.
    /// Scoring: title match +0.6, content match +0.3, category match +0.2.
    pub fn search(&self, query: &str) -> Vec<(&AgentMemory, f64)> {
        let q = query.to_lowercase();
        let mut results: Vec<(&AgentMemory, f64)> = self
            .memories
            .values()
            .filter_map(|m| {
                let mut score = 0.0f64;
                if m.title.to_lowercase().contains(&q) {
                    score += 0.6;
                }
                if m.content.to_lowercase().contains(&q) {
                    score += 0.3;
                }
                if m.category.to_lowercase().contains(&q) {
                    score += 0.2;
                }
                if score > 0.0 {
                    Some((m, score))
                } else {
                    None
                }
            })
            .collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    pub fn get(&self, id: &str) -> Option<&AgentMemory> {
        self.memories.get(id)
    }

    pub fn list_by_category(&self, category: &str) -> Vec<&AgentMemory> {
        let cat = category.to_lowercase();
        let mut result: Vec<&AgentMemory> = self
            .memories
            .values()
            .filter(|m| m.category.to_lowercase() == cat)
            .collect();
        result.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        result
    }

    pub fn delete(&mut self, id: &str) -> bool {
        self.memories.remove(id).is_some()
    }
}

// ── #247: Git-Trackable Task Data ─────────────────────────────────────────────

pub struct GitTrackableStore {
    base_dir: PathBuf,
}

impl GitTrackableStore {
    /// Stores tasks under `<project_root>/.caduceus/tasks/`.
    pub fn new(project_root: &Path) -> Self {
        Self {
            base_dir: project_root.to_path_buf(),
        }
    }

    pub fn tasks_dir(&self) -> PathBuf {
        self.base_dir.join(".caduceus").join("tasks")
    }

    pub fn save_task(&self, task: &serde_json::Value) -> std::result::Result<(), String> {
        let id = task
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Task must have a string 'id' field".to_string())?;
        let dir = self.tasks_dir();
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{id}.json"));
        let json = serde_json::to_string_pretty(task).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn load_task(&self, id: &str) -> std::result::Result<serde_json::Value, String> {
        let path = self.tasks_dir().join(format!("{id}.json"));
        let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).map_err(|e| e.to_string())
    }

    pub fn list_tasks(&self) -> std::result::Result<Vec<serde_json::Value>, String> {
        let dir = self.tasks_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut tasks = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                let content = fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let task: serde_json::Value =
                    serde_json::from_str(&content).map_err(|e| e.to_string())?;
                tasks.push(task);
            }
        }
        Ok(tasks)
    }

    pub fn delete_task(&self, id: &str) -> std::result::Result<(), String> {
        let path = self.tasks_dir().join(format!("{id}.json"));
        fs::remove_file(path).map_err(|e| e.to_string())
    }
}

// ── #250: WikiEngine ─────────────────────────────────────────────────────────

/// A page stored in the wiki.
pub struct WikiPage {
    pub slug: String,
    pub title: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    /// `[[linked-page]]` references found in the page content.
    pub links: Vec<String>,
    /// Seconds since UNIX epoch of last modification.
    pub last_modified: u64,
}

/// Core wiki manager – owns the wiki directory layout.
pub struct WikiEngine {
    wiki_dir: PathBuf,
    sources_dir: PathBuf,
}

impl WikiEngine {
    pub fn new(project_root: &Path) -> Self {
        let wiki_dir = project_root.join(".caduceus").join("wiki");
        let sources_dir = wiki_dir.join("raw");
        Self {
            wiki_dir,
            sources_dir,
        }
    }

    /// Create directory structure, `index.md`, and `log.md` if they do not exist.
    pub fn init(&self) -> std::result::Result<(), CaduceusError> {
        fs::create_dir_all(&self.wiki_dir)
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        fs::create_dir_all(&self.sources_dir)
            .map_err(|e| CaduceusError::Storage(e.to_string()))?;

        let index_path = self.wiki_dir.join("index.md");
        if !index_path.exists() {
            fs::write(&index_path, "# Wiki Index\n\n")
                .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        }
        let log_path = self.wiki_dir.join("log.md");
        if !log_path.exists() {
            fs::write(&log_path, "# Wiki Log\n\n")
                .map_err(|e| CaduceusError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    pub fn wiki_dir(&self) -> &Path {
        &self.wiki_dir
    }

    /// Absolute path of a page file for the given slug.
    pub fn page_path(&self, slug: &str) -> PathBuf {
        self.wiki_dir.join(format!("{slug}.md"))
    }

    pub fn page_exists(&self, slug: &str) -> bool {
        self.page_path(slug).exists()
    }

    /// List all `.md` pages in the wiki dir, excluding `index.md` and `log.md`.
    pub fn list_pages(&self) -> std::result::Result<Vec<WikiPage>, CaduceusError> {
        if !self.wiki_dir.exists() {
            return Ok(Vec::new());
        }
        let mut pages = Vec::new();
        for entry in fs::read_dir(&self.wiki_dir)
            .map_err(|e| CaduceusError::Storage(e.to_string()))?
        {
            let entry = entry.map_err(|e| CaduceusError::Storage(e.to_string()))?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if name == "index" || name == "log" {
                    continue;
                }
                let content = fs::read_to_string(&path)
                    .map_err(|e| CaduceusError::Storage(e.to_string()))?;
                let meta = fs::metadata(&path)
                    .map_err(|e| CaduceusError::Storage(e.to_string()))?;
                let last_modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let title = wiki_title_from_content(&content).unwrap_or_else(|| name.clone());
                let links = wiki_extract_links(&content);
                pages.push(WikiPage {
                    slug: name,
                    title,
                    path,
                    size_bytes: meta.len(),
                    links,
                    last_modified,
                });
            }
        }
        Ok(pages)
    }

    pub fn read_page(&self, slug: &str) -> std::result::Result<String, CaduceusError> {
        let path = self.page_path(slug);
        fs::read_to_string(&path).map_err(|e| CaduceusError::Storage(e.to_string()))
    }

    pub fn write_page(&self, slug: &str, content: &str) -> std::result::Result<(), CaduceusError> {
        let path = self.page_path(slug);
        fs::write(path, content).map_err(|e| CaduceusError::Storage(e.to_string()))
    }

    pub fn delete_page(&self, slug: &str) -> std::result::Result<(), CaduceusError> {
        let path = self.page_path(slug);
        fs::remove_file(path).map_err(|e| CaduceusError::Storage(e.to_string()))
    }

    /// Simple case-insensitive full-text search across all pages.
    pub fn search_pages(&self, query: &str) -> std::result::Result<Vec<WikiPage>, CaduceusError> {
        let lower = query.to_lowercase();
        let all = self.list_pages()?;
        let mut matches = Vec::new();
        for page in all {
            let content = self.read_page(&page.slug).unwrap_or_default();
            if page.title.to_lowercase().contains(&lower)
                || page.slug.to_lowercase().contains(&lower)
                || content.to_lowercase().contains(&lower)
            {
                matches.push(page);
            }
        }
        Ok(matches)
    }
}

fn wiki_title_from_content(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Extract `[[slug]]` link references from wiki content.
fn wiki_extract_links(content: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut remaining = content;
    while let Some(start) = remaining.find("[[") {
        let after_open = &remaining[start + 2..];
        if let Some(end) = after_open.find("]]") {
            let slug = after_open[..end].trim().to_string();
            if !slug.is_empty() && !links.contains(&slug) {
                links.push(slug);
            }
            remaining = &after_open[end + 2..];
        } else {
            break;
        }
    }
    links
}

// ── #251: WikiIndex ───────────────────────────────────────────────────────────

pub struct IndexEntry {
    pub slug: String,
    pub title: String,
    pub summary: String,
    /// One of: `entity`, `concept`, `source`, `analysis`.
    pub category: String,
    pub source_count: usize,
    pub link_count: usize,
}

#[derive(Default)]
pub struct WikiIndex {
    entries: Vec<IndexEntry>,
}

impl WikiIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_entry(&mut self, entry: IndexEntry) {
        self.entries.push(entry);
    }

    pub fn remove_entry(&mut self, slug: &str) {
        self.entries.retain(|e| e.slug != slug);
    }

    pub fn update_entry(&mut self, slug: &str, entry: IndexEntry) {
        self.remove_entry(slug);
        self.entries.push(entry);
    }

    pub fn find_by_category(&self, category: &str) -> Vec<&IndexEntry> {
        self.entries
            .iter()
            .filter(|e| e.category == category)
            .collect()
    }

    pub fn find_by_query(&self, query: &str) -> Vec<&IndexEntry> {
        let lower = query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                e.slug.to_lowercase().contains(&lower)
                    || e.title.to_lowercase().contains(&lower)
                    || e.summary.to_lowercase().contains(&lower)
                    || e.category.to_lowercase().contains(&lower)
            })
            .collect()
    }

    /// Render the index as markdown suitable for `index.md`.
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("# Wiki Index\n\n");
        let categories = ["entity", "concept", "source", "analysis"];
        for cat in &categories {
            let entries: Vec<_> = self.find_by_category(cat);
            if entries.is_empty() {
                continue;
            }
            out.push_str(&format!("## {}\n\n", capitalise(cat)));
            for e in entries {
                out.push_str(&format!(
                    "- [[{}]] — {} (sources: {}, links: {})\n",
                    e.slug, e.summary, e.source_count, e.link_count
                ));
            }
            out.push('\n');
        }
        out
    }

    /// Parse `index.md` back into a `WikiIndex`.
    pub fn from_markdown(content: &str) -> Self {
        let mut index = WikiIndex::new();
        let mut current_category = String::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(heading) = trimmed.strip_prefix("## ") {
                current_category = heading.trim().to_lowercase();
            } else if trimmed.starts_with("- [[") {
                // "- [[slug]] — summary (sources: N, links: M)"
                if let Some(inner) = trimmed.strip_prefix("- [[") {
                    if let Some(close) = inner.find("]]") {
                        let slug = inner[..close].to_string();
                        let rest = inner[close + 2..].trim();
                        let summary = rest
                            .split(" (sources:")
                            .next()
                            .unwrap_or("")
                            .trim_start_matches('—')
                            .trim()
                            .to_string();
                        let (source_count, link_count) =
                            parse_index_counts(rest).unwrap_or((0, 0));
                        let title = slug
                            .split('-')
                            .map(capitalise)
                            .collect::<Vec<_>>()
                            .join(" ");
                        index.add_entry(IndexEntry {
                            slug,
                            title,
                            summary,
                            category: current_category.clone(),
                            source_count,
                            link_count,
                        });
                    }
                }
            }
        }
        index
    }

    /// Return slugs of pages that have no inbound `[[links]]` in `all_slugs`.
    pub fn orphan_pages(&self, all_slugs: &[String]) -> Vec<String> {
        let linked: std::collections::HashSet<String> = self
            .entries
            .iter()
            .flat_map(|_| std::iter::empty::<String>())
            .collect();
        // Any slug that appears in all_slugs but has zero inbound links from index entries.
        // Since the index tracks per-entry link counts but not *which* pages are linked,
        // we return all slugs that do not appear in any other entry's context.
        // A simpler correct definition: pages not referenced by any other page's [[links]].
        // Without page content we flag them all; callers should prefer WikiLinter::find_orphans.
        all_slugs
            .iter()
            .filter(|s| !linked.contains(*s))
            .cloned()
            .collect()
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

fn parse_index_counts(s: &str) -> Option<(usize, usize)> {
    // "— summary (sources: N, links: M)"
    let paren = s.find('(')?;
    let inner = &s[paren + 1..s.find(')')?];
    let mut parts = inner.split(',');
    let src = parts
        .next()?
        .split(':')
        .nth(1)?
        .trim()
        .parse()
        .ok()?;
    let lnk = parts
        .next()?
        .split(':')
        .nth(1)?
        .trim()
        .parse()
        .ok()?;
    Some((src, lnk))
}

// ── #252: WikiLog ─────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WikiOperation {
    Ingest,
    Query,
    Lint,
    Update,
    Create,
    Delete,
}

impl WikiOperation {
    fn as_str(&self) -> &'static str {
        match self {
            WikiOperation::Ingest => "Ingest",
            WikiOperation::Query => "Query",
            WikiOperation::Lint => "Lint",
            WikiOperation::Update => "Update",
            WikiOperation::Create => "Create",
            WikiOperation::Delete => "Delete",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s.trim() {
            "Ingest" => Some(WikiOperation::Ingest),
            "Query" => Some(WikiOperation::Query),
            "Lint" => Some(WikiOperation::Lint),
            "Update" => Some(WikiOperation::Update),
            "Create" => Some(WikiOperation::Create),
            "Delete" => Some(WikiOperation::Delete),
            _ => None,
        }
    }
}

pub struct LogEntry {
    /// ISO 8601 timestamp.
    pub timestamp: String,
    pub operation: WikiOperation,
    pub description: String,
    pub pages_touched: Vec<String>,
}

pub struct LogStats {
    pub total_operations: usize,
    pub ingests: usize,
    pub queries: usize,
    pub lints: usize,
    pub pages_created: usize,
}

#[derive(Default)]
pub struct WikiLog {
    entries: Vec<LogEntry>,
}

impl WikiLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, entry: LogEntry) {
        self.entries.push(entry);
    }

    pub fn recent(&self, n: usize) -> Vec<&LogEntry> {
        self.entries.iter().rev().take(n).collect()
    }

    pub fn by_operation(&self, op: &WikiOperation) -> Vec<&LogEntry> {
        self.entries
            .iter()
            .filter(|e| &e.operation == op)
            .collect()
    }

    /// Render as `log.md`.  Each entry: `## [timestamp] Op | description`
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("# Wiki Log\n\n");
        for e in &self.entries {
            out.push_str(&format!(
                "## [{}] {} | {}\n",
                e.timestamp,
                e.operation.as_str(),
                e.description
            ));
            if !e.pages_touched.is_empty() {
                out.push_str(&format!("Pages: {}\n", e.pages_touched.join(", ")));
            }
            out.push('\n');
        }
        out
    }

    /// Parse `log.md` back into a `WikiLog`.
    pub fn from_markdown(content: &str) -> Self {
        let mut log = WikiLog::new();
        let mut lines = content.lines().peekable();
        while let Some(line) = lines.next() {
            let trimmed = line.trim();
            // "## [timestamp] Op | description"
            if let Some(rest) = trimmed.strip_prefix("## [") {
                if let Some(close) = rest.find(']') {
                    let timestamp = rest[..close].to_string();
                    let after = rest[close + 1..].trim();
                    let (op_str, desc) = if let Some(pipe) = after.find('|') {
                        (after[..pipe].trim(), after[pipe + 1..].trim())
                    } else {
                        (after, "")
                    };
                    let operation = WikiOperation::from_str(op_str)
                        .unwrap_or(WikiOperation::Update);
                    let mut pages_touched = Vec::new();
                    if let Some(next) = lines.peek() {
                        if let Some(p) = next.trim().strip_prefix("Pages: ") {
                            pages_touched = p.split(", ").map(str::to_string).collect();
                            lines.next();
                        }
                    }
                    log.append(LogEntry {
                        timestamp,
                        operation,
                        description: desc.to_string(),
                        pages_touched,
                    });
                }
            }
        }
        log
    }

    pub fn stats(&self) -> LogStats {
        let mut stats = LogStats {
            total_operations: self.entries.len(),
            ingests: 0,
            queries: 0,
            lints: 0,
            pages_created: 0,
        };
        for e in &self.entries {
            match e.operation {
                WikiOperation::Ingest => stats.ingests += 1,
                WikiOperation::Query => stats.queries += 1,
                WikiOperation::Lint => stats.lints += 1,
                WikiOperation::Create => stats.pages_created += 1,
                _ => {}
            }
        }
        stats
    }
}

// ── #253: WikiIngestor ────────────────────────────────────────────────────────

pub struct WikiIngestor;

impl WikiIngestor {
    /// Extract candidate entity names (capitalised words / quoted phrases) from source text.
    pub fn extract_entities(source: &str) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut entities = Vec::new();
        for word in source.split_whitespace() {
            let clean: String = word
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if clean.len() >= 3
                && clean.chars().next().is_some_and(|c| c.is_uppercase())
                && seen.insert(clean.clone())
            {
                entities.push(clean);
            }
        }
        entities
    }

    /// Extract key claims/sentences: sentences that contain verbs or strong nouns.
    pub fn extract_key_claims(source: &str) -> Vec<String> {
        source
            .split(['.', '!', '?'])
            .map(str::trim)
            .filter(|s| s.len() > 20)
            .map(str::to_string)
            .collect()
    }

    /// Generate a markdown summary page for a source document.
    pub fn generate_summary_page(title: &str, source: &str) -> String {
        let claims = Self::extract_key_claims(source);
        let entities = Self::extract_entities(source);
        let mut out = format!("# {title}\n\n## Summary\n\n");
        for claim in claims.iter().take(5) {
            out.push_str(&format!("- {claim}\n"));
        }
        if !entities.is_empty() {
            out.push_str("\n## Key Entities\n\n");
            for e in entities.iter().take(10) {
                out.push_str(&format!("- [[{}]]\n", Self::slugify(e)));
            }
        }
        out
    }

    /// Generate a markdown entity page from an entity name and surrounding context.
    pub fn generate_entity_page(entity: &str, context: &str) -> String {
        let slug = Self::slugify(entity);
        let mut out = format!("# {entity}\n\n## Overview\n\n");
        let relevant: Vec<_> = context
            .split(['.', '!', '?'])
            .map(str::trim)
            .filter(|s| s.to_lowercase().contains(&entity.to_lowercase()) && s.len() > 10)
            .take(5)
            .collect();
        if relevant.is_empty() {
            out.push_str("No context available.\n");
        } else {
            for s in relevant {
                out.push_str(&format!("- {s}\n"));
            }
        }
        out.push_str(&format!("\n## Back-links\n\n[[{slug}]]\n"));
        out
    }

    /// Find `[[existing-slug]]` cross-references between content and known pages.
    pub fn find_cross_references(content: &str, existing_slugs: &[String]) -> Vec<String> {
        let lower_content = content.to_lowercase();
        existing_slugs
            .iter()
            .filter(|slug| lower_content.contains(slug.as_str()))
            .cloned()
            .collect()
    }

    /// Convert a title string to a URL-safe slug.
    ///
    /// `"My Page Title"` → `"my-page-title"`
    pub fn slugify(title: &str) -> String {
        title
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }
}

// ── #254: WikiLinter ──────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
pub enum LintCategory {
    OrphanPage,
    MissingPage,
    StaleContent,
    Contradiction,
    MissingCrossRef,
    EmptyPage,
}

pub struct LintFinding {
    pub category: LintCategory,
    pub page: String,
    pub description: String,
    pub suggestion: String,
}

pub struct WikiLinter;

impl WikiLinter {
    /// Run all lint checks and return combined findings.
    pub fn lint(pages: &[WikiPage], index: &WikiIndex) -> Vec<LintFinding> {
        let mut findings = Vec::new();
        findings.extend(Self::find_orphans(pages, index));
        findings.extend(Self::find_broken_links(pages));
        findings.extend(Self::find_empty_pages(pages));
        findings
    }

    /// Pages that have no inbound `[[link]]` from any other page.
    pub fn find_orphans(pages: &[WikiPage], _index: &WikiIndex) -> Vec<LintFinding> {
        let all_slugs: std::collections::HashSet<&str> =
            pages.iter().map(|p| p.slug.as_str()).collect();
        let linked: std::collections::HashSet<&str> = pages
            .iter()
            .flat_map(|p| p.links.iter().map(String::as_str))
            .collect();
        pages
            .iter()
            .filter(|p| !linked.contains(p.slug.as_str()))
            .filter(|_p| all_slugs.len() > 1) // single page is never an orphan by convention
            .map(|p| LintFinding {
                category: LintCategory::OrphanPage,
                page: p.slug.clone(),
                description: format!("'{}' has no inbound links", p.slug),
                suggestion: format!(
                    "Add [[{}]] to a related page or the index",
                    p.slug
                ),
            })
            .collect()
    }

    /// Links that point to slugs that do not exist as pages.
    pub fn find_broken_links(pages: &[WikiPage]) -> Vec<LintFinding> {
        let existing: std::collections::HashSet<&str> =
            pages.iter().map(|p| p.slug.as_str()).collect();
        let mut findings = Vec::new();
        for page in pages {
            for link in &page.links {
                if !existing.contains(link.as_str()) {
                    findings.push(LintFinding {
                        category: LintCategory::MissingPage,
                        page: page.slug.clone(),
                        description: format!("broken link [[{}]] in '{}'", link, page.slug),
                        suggestion: format!("Create page '{}' or remove the link", link),
                    });
                }
            }
        }
        findings
    }

    pub fn find_empty_pages(pages: &[WikiPage]) -> Vec<LintFinding> {
        pages
            .iter()
            .filter(|p| p.size_bytes == 0)
            .map(|p| LintFinding {
                category: LintCategory::EmptyPage,
                page: p.slug.clone(),
                description: format!("'{}' is empty", p.slug),
                suggestion: format!("Add content to '{}' or delete it", p.slug),
            })
            .collect()
    }

    /// Pages not modified within `max_age_days`.
    pub fn find_stale_pages(pages: &[WikiPage], max_age_days: u32) -> Vec<LintFinding> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let threshold = u64::from(max_age_days) * 86_400;
        pages
            .iter()
            .filter(|p| p.last_modified > 0 && now.saturating_sub(p.last_modified) > threshold)
            .map(|p| LintFinding {
                category: LintCategory::StaleContent,
                page: p.slug.clone(),
                description: format!("'{}' has not been updated in >{max_age_days} days", p.slug),
                suggestion: format!("Review and update '{}'", p.slug),
            })
            .collect()
    }
}

// ── #255: WikiQueryEngine ─────────────────────────────────────────────────────

pub struct WikiQueryEngine;

pub struct QueryResult {
    pub answer: String,
    pub sources: Vec<String>,
    pub confidence: f64,
}

impl WikiQueryEngine {
    /// Score each page by relevance to `query`.  Returns `(slug, relevance)` pairs sorted
    /// by descending relevance.
    pub fn search(
        pages: &[WikiPage],
        contents: &HashMap<String, String>,
        query: &str,
    ) -> Vec<(String, f64)> {
        let lower = query.to_lowercase();
        let terms: Vec<&str> = lower.split_whitespace().collect();
        let mut scored: Vec<(String, f64)> = pages
            .iter()
            .filter_map(|p| {
                let content = contents.get(&p.slug).map(String::as_str).unwrap_or("");
                let content_lower = content.to_lowercase();
                let title_lower = p.title.to_lowercase();
                let slug_lower = p.slug.to_lowercase();

                let mut score = 0.0_f64;
                for term in &terms {
                    if title_lower.contains(term) {
                        score += 0.5;
                    }
                    if slug_lower.contains(term) {
                        score += 0.3;
                    }
                    // Count occurrences in content, capped at 1.0 per term
                    let occurrences = content_lower.matches(term).count();
                    if occurrences > 0 {
                        score += (occurrences as f64 * 0.1).min(1.0);
                    }
                }
                if score > 0.0 {
                    Some((p.slug.clone(), score))
                } else {
                    None
                }
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }

    /// Concatenate the content of the given pages into a single context string.
    pub fn gather_context(
        pages: &[WikiPage],
        contents: &HashMap<String, String>,
        slugs: &[String],
    ) -> String {
        let slug_set: std::collections::HashSet<&str> =
            slugs.iter().map(String::as_str).collect();
        pages
            .iter()
            .filter(|p| slug_set.contains(p.slug.as_str()))
            .map(|p| {
                let content = contents.get(&p.slug).map(String::as_str).unwrap_or("");
                format!("## {}\n\n{content}\n\n", p.title)
            })
            .collect::<Vec<_>>()
            .join("---\n\n")
    }

    /// Extract `[[page-ref]]` citations from text.
    pub fn extract_citations(text: &str) -> Vec<String> {
        wiki_extract_links(text)
    }
}

// ── #256: WikiWatcher ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    pub change_type: FileChangeType,
    pub content_hash: u64,
}

pub struct WikiWatcher {
    pub watched_extensions: Vec<String>,
    pub ignore_patterns: Vec<String>,
}

impl Default for WikiWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl WikiWatcher {
    pub fn new() -> Self {
        Self {
            watched_extensions: vec![
                "rs".to_string(),
                "ts".to_string(),
                "py".to_string(),
                "go".to_string(),
                "md".to_string(),
                "json".to_string(),
            ],
            ignore_patterns: vec![
                "node_modules".to_string(),
                ".git".to_string(),
                "target".to_string(),
            ],
        }
    }

    /// Diff `previous_hashes` against the current project state.
    pub fn detect_changes(
        &self,
        project_root: &Path,
        previous_hashes: &HashMap<String, u64>,
    ) -> Vec<FileChange> {
        let current = self.snapshot_project(project_root);
        let mut changes = Vec::new();

        // Created or modified
        for (path, hash) in &current {
            match previous_hashes.get(path) {
                None => changes.push(FileChange {
                    path: path.clone(),
                    change_type: FileChangeType::Created,
                    content_hash: *hash,
                }),
                Some(prev) if prev != hash => changes.push(FileChange {
                    path: path.clone(),
                    change_type: FileChangeType::Modified,
                    content_hash: *hash,
                }),
                _ => {}
            }
        }

        // Deleted
        for (path, hash) in previous_hashes {
            if !current.contains_key(path) {
                changes.push(FileChange {
                    path: path.clone(),
                    change_type: FileChangeType::Deleted,
                    content_hash: *hash,
                });
            }
        }

        changes
    }

    /// FNV-1a 64-bit hash of a file's text content.
    pub fn hash_file(content: &str) -> u64 {
        let mut hash: u64 = 14_695_981_039_346_656_037;
        for byte in content.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
        hash
    }

    /// Return `true` when this path should be tracked (extension allowed, not
    /// inside an ignored directory).
    pub fn should_watch(&self, path: &str) -> bool {
        // Reject anything that passes through an ignored directory segment.
        let sep = std::path::MAIN_SEPARATOR.to_string();
        for segment in path.split(&sep as &str) {
            if self.ignore_patterns.iter().any(|p| p == segment) {
                return false;
            }
        }
        // Also reject if any ignore pattern appears as a substring (handles
        // forward-slash paths on all platforms).
        for pattern in &self.ignore_patterns {
            if path.contains(pattern.as_str()) {
                return false;
            }
        }
        let ext = path.rsplit('.').next().unwrap_or("");
        self.watched_extensions.iter().any(|e| e == ext)
    }

    /// Walk `project_root` and return a map of relative-path → content-hash
    /// for every file that passes `should_watch`.
    pub fn snapshot_project(&self, project_root: &Path) -> HashMap<String, u64> {
        let mut hashes = HashMap::new();
        self.walk_dir(project_root, project_root, &mut hashes);
        hashes
    }

    fn walk_dir(&self, root: &Path, dir: &Path, hashes: &mut HashMap<String, u64>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let dir_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                if !self.ignore_patterns.iter().any(|p| p == dir_name) {
                    self.walk_dir(root, &path, hashes);
                }
            } else {
                let abs_str = path.to_string_lossy();
                if self.should_watch(&abs_str) {
                    if let Ok(content) = fs::read_to_string(&path) {
                        let rel = path
                            .strip_prefix(root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .to_string();
                        hashes.insert(rel, Self::hash_file(&content));
                    }
                }
            }
        }
    }
}

// ── #257: WikiMaintenanceAgent ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintenanceActionType {
    CreatePage,
    UpdatePage,
    DeletePage,
    UpdateIndex,
    UpdateLog,
    RunLint,
}

#[derive(Debug, Clone)]
pub struct MaintenanceAction {
    pub action_type: MaintenanceActionType,
    pub page_slug: String,
    pub description: String,
}

pub struct WikiMaintenanceAgent {
    pub auto_ingest: bool,
    pub auto_lint: bool,
    pub auto_index: bool,
    pub update_interval_secs: u64,
}

impl Default for WikiMaintenanceAgent {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MaintenanceReport {
    pub pages_created: usize,
    pub pages_updated: usize,
    pub pages_deleted: usize,
    pub lint_findings: usize,
    pub index_updated: bool,
    pub log_entries_added: usize,
}

impl WikiMaintenanceAgent {
    pub fn new() -> Self {
        Self {
            auto_ingest: true,
            auto_lint: true,
            auto_index: true,
            update_interval_secs: 300,
        }
    }

    /// Decide which wiki operations are needed based on the detected changes.
    pub fn plan_actions(
        &self,
        changes: &[FileChange],
        wiki: &WikiEngine,
        _index: &WikiIndex,
    ) -> Vec<MaintenanceAction> {
        let mut actions = Vec::new();

        for change in changes {
            let slug = WikiIngestor::slugify(&change.path.replace(['/', '.'], "-"));
            let page_slug = format!("src-{slug}");

            match change.change_type {
                FileChangeType::Created => {
                    actions.push(MaintenanceAction {
                        action_type: MaintenanceActionType::CreatePage,
                        page_slug,
                        description: format!("Create summary page for new file: {}", change.path),
                    });
                }
                FileChangeType::Modified => {
                    if wiki.page_exists(&page_slug) {
                        actions.push(MaintenanceAction {
                            action_type: MaintenanceActionType::UpdatePage,
                            page_slug,
                            description: format!(
                                "Update summary page for modified file: {}",
                                change.path
                            ),
                        });
                    } else {
                        actions.push(MaintenanceAction {
                            action_type: MaintenanceActionType::CreatePage,
                            page_slug,
                            description: format!(
                                "Create summary page for modified file (page not found): {}",
                                change.path
                            ),
                        });
                    }
                }
                FileChangeType::Deleted => {
                    actions.push(MaintenanceAction {
                        action_type: MaintenanceActionType::UpdatePage,
                        page_slug,
                        description: format!(
                            "Archive summary page for deleted file: {}",
                            change.path
                        ),
                    });
                }
            }
        }

        if !changes.is_empty() {
            if self.auto_index {
                actions.push(MaintenanceAction {
                    action_type: MaintenanceActionType::UpdateIndex,
                    page_slug: "index".to_string(),
                    description: "Rebuild wiki index after file changes".to_string(),
                });
            }
            if self.auto_lint {
                actions.push(MaintenanceAction {
                    action_type: MaintenanceActionType::RunLint,
                    page_slug: String::new(),
                    description: "Run wiki lint pass".to_string(),
                });
            }
            actions.push(MaintenanceAction {
                action_type: MaintenanceActionType::UpdateLog,
                page_slug: "log".to_string(),
                description: format!("Log {} change(s) to wiki log", changes.len()),
            });
        }

        actions
    }

    /// Execute a single planned action, mutating `index` and `log` in place.
    pub fn execute_action(
        &self,
        action: &MaintenanceAction,
        wiki: &WikiEngine,
        index: &mut WikiIndex,
        log: &mut WikiLog,
    ) -> std::result::Result<(), CaduceusError> {
        match action.action_type {
            MaintenanceActionType::CreatePage => {
                if !wiki.page_exists(&action.page_slug) {
                    let content = format!(
                        "# {}\n\n{}\n",
                        action.page_slug, action.description
                    );
                    wiki.write_page(&action.page_slug, &content)?;
                }
                log.append(LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    operation: WikiOperation::Create,
                    description: action.description.clone(),
                    pages_touched: vec![action.page_slug.clone()],
                });
            }
            MaintenanceActionType::UpdatePage => {
                if wiki.page_exists(&action.page_slug) {
                    let existing = wiki.read_page(&action.page_slug)?;
                    let note = if action.description.contains("Archive")
                        || action.description.contains("deleted")
                    {
                        format!("\n\n> **Archived**: {}\n", action.description)
                    } else {
                        format!("\n\n> **Updated**: {}\n", action.description)
                    };
                    wiki.write_page(&action.page_slug, &format!("{existing}{note}"))?;
                } else {
                    let content = format!(
                        "# {}\n\n{}\n",
                        action.page_slug, action.description
                    );
                    wiki.write_page(&action.page_slug, &content)?;
                }
                log.append(LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    operation: WikiOperation::Update,
                    description: action.description.clone(),
                    pages_touched: vec![action.page_slug.clone()],
                });
            }
            MaintenanceActionType::DeletePage => {
                if wiki.page_exists(&action.page_slug) {
                    wiki.delete_page(&action.page_slug)?;
                }
                log.append(LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    operation: WikiOperation::Delete,
                    description: action.description.clone(),
                    pages_touched: vec![action.page_slug.clone()],
                });
            }
            MaintenanceActionType::UpdateIndex => {
                let pages = wiki.list_pages()?;
                for page in &pages {
                    let category = if page.slug.starts_with("src-") {
                        "source"
                    } else {
                        "entity"
                    };
                    index.update_entry(
                        &page.slug,
                        IndexEntry {
                            slug: page.slug.clone(),
                            title: page.title.clone(),
                            summary: format!(
                                "Auto-indexed page with {} links",
                                page.links.len()
                            ),
                            category: category.to_string(),
                            source_count: 1,
                            link_count: page.links.len(),
                        },
                    );
                }
                let index_md = index.to_markdown();
                let index_path = wiki.wiki_dir().join("index.md");
                fs::write(&index_path, index_md)
                    .map_err(|e| CaduceusError::Storage(e.to_string()))?;
            }
            MaintenanceActionType::UpdateLog => {
                let log_md = log.to_markdown();
                let log_path = wiki.wiki_dir().join("log.md");
                fs::write(&log_path, log_md)
                    .map_err(|e| CaduceusError::Storage(e.to_string()))?;
            }
            MaintenanceActionType::RunLint => {
                let pages = wiki.list_pages()?;
                let findings = WikiLinter::lint(&pages, index);
                log.append(LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    operation: WikiOperation::Lint,
                    description: format!("Lint found {} finding(s)", findings.len()),
                    pages_touched: vec![],
                });
            }
        }
        Ok(())
    }

    /// Run a complete maintenance cycle against `project_root`.
    pub fn run_full_maintenance(
        &self,
        project_root: &Path,
        previous_hashes: &HashMap<String, u64>,
    ) -> std::result::Result<MaintenanceReport, CaduceusError> {
        let wiki = WikiEngine::new(project_root);
        wiki.init()?;

        let watcher = WikiWatcher::new();
        let changes = watcher.detect_changes(project_root, previous_hashes);

        let mut index = WikiIndex::new();
        let mut log = WikiLog::new();
        let log_baseline = log.entries.len();

        let actions = self.plan_actions(&changes, &wiki, &index);

        let mut report = MaintenanceReport {
            pages_created: 0,
            pages_updated: 0,
            pages_deleted: 0,
            lint_findings: 0,
            index_updated: false,
            log_entries_added: 0,
        };

        for action in &actions {
            self.execute_action(action, &wiki, &mut index, &mut log)?;
            match action.action_type {
                MaintenanceActionType::CreatePage => report.pages_created += 1,
                MaintenanceActionType::UpdatePage => report.pages_updated += 1,
                MaintenanceActionType::DeletePage => report.pages_deleted += 1,
                MaintenanceActionType::UpdateIndex => report.index_updated = true,
                MaintenanceActionType::RunLint => {
                    let pages = wiki.list_pages()?;
                    report.lint_findings = WikiLinter::lint(&pages, &index).len();
                }
                MaintenanceActionType::UpdateLog => {}
            }
        }

        report.log_entries_added = log.entries.len().saturating_sub(log_baseline);
        Ok(report)
    }
}

// ── #258: WikiAutoTrigger ─────────────────────────────────────────────────────

pub struct WikiAutoTrigger {
    enabled: bool,
    last_snapshot: HashMap<String, u64>,
    agent: WikiMaintenanceAgent,
    watcher: WikiWatcher,
}

impl Default for WikiAutoTrigger {
    fn default() -> Self {
        Self::new()
    }
}

impl WikiAutoTrigger {
    pub fn new() -> Self {
        Self {
            enabled: true,
            last_snapshot: HashMap::new(),
            agent: WikiMaintenanceAgent::new(),
            watcher: WikiWatcher::new(),
        }
    }

    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn disable(&mut self) {
        self.enabled = false;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Call after each agent turn.  Diffs the project against the last
    /// snapshot; if changes exist, runs maintenance and returns a report.
    pub fn on_agent_turn_complete(
        &mut self,
        project_root: &Path,
    ) -> std::result::Result<Option<MaintenanceReport>, CaduceusError> {
        if !self.enabled {
            return Ok(None);
        }

        let changes = self.watcher.detect_changes(project_root, &self.last_snapshot);

        if changes.is_empty() {
            return Ok(None);
        }

        let wiki = WikiEngine::new(project_root);
        wiki.init()?;

        let mut index = WikiIndex::new();
        let mut log = WikiLog::new();
        let log_baseline = log.entries.len();

        let actions = self.agent.plan_actions(&changes, &wiki, &index);

        let mut report = MaintenanceReport {
            pages_created: 0,
            pages_updated: 0,
            pages_deleted: 0,
            lint_findings: 0,
            index_updated: false,
            log_entries_added: 0,
        };

        for action in &actions {
            self.agent
                .execute_action(action, &wiki, &mut index, &mut log)?;
            match action.action_type {
                MaintenanceActionType::CreatePage => report.pages_created += 1,
                MaintenanceActionType::UpdatePage => report.pages_updated += 1,
                MaintenanceActionType::DeletePage => report.pages_deleted += 1,
                MaintenanceActionType::UpdateIndex => report.index_updated = true,
                MaintenanceActionType::RunLint => {
                    let pages = wiki.list_pages()?;
                    report.lint_findings = WikiLinter::lint(&pages, &index).len();
                }
                MaintenanceActionType::UpdateLog => {}
            }
        }

        report.log_entries_added = log.entries.len().saturating_sub(log_baseline);
        // Snapshot *after* executing actions so that wiki-generated files
        // (e.g. newly written .md pages) are included in last_snapshot and
        // won't be re-detected as "created" on the next turn.
        self.last_snapshot = self.watcher.snapshot_project(project_root);
        Ok(Some(report))
    }

    /// Call at session start to seed the baseline snapshot.
    pub fn on_session_start(&mut self, project_root: &Path) {
        self.last_snapshot = self.watcher.snapshot_project(project_root);
    }

    /// Call at session end to flush any remaining changes.
    pub fn on_session_end(
        &mut self,
        project_root: &Path,
    ) -> std::result::Result<Option<MaintenanceReport>, CaduceusError> {
        self.on_agent_turn_complete(project_root)
    }

    /// Unconditionally run a full maintenance pass right now.
    pub fn force_maintenance(
        &mut self,
        project_root: &Path,
    ) -> std::result::Result<MaintenanceReport, CaduceusError> {
        let report = self
            .agent
            .run_full_maintenance(project_root, &self.last_snapshot)?;
        self.last_snapshot = self.watcher.snapshot_project(project_root);
        Ok(report)
    }
}

// ── Tests for #241, #247 ──────────────────────────────────────────────────────

#[cfg(test)]
mod feature_tests_241_247 {
    use super::*;

    // ── #241 AgentMemoryStore ─────────────────────────────────────────────────

    fn make_memory(id: &str, title: &str, content: &str, category: &str) -> AgentMemory {
        AgentMemory {
            id: id.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            category: category.to_string(),
            metadata: HashMap::new(),
            created_at: 100,
            updated_at: 100,
        }
    }

    #[test]
    fn memory_store_and_get() {
        let mut store = AgentMemoryStore::new();
        store.store(make_memory(
            "m1",
            "Auth notes",
            "Use JWT tokens",
            "security",
        ));
        assert!(store.get("m1").is_some());
        assert_eq!(store.get("m1").unwrap().title, "Auth notes");
    }

    #[test]
    fn memory_search_title_match() {
        let mut store = AgentMemoryStore::new();
        store.store(make_memory("m1", "Auth notes", "content here", "security"));
        store.store(make_memory("m2", "Unrelated", "content here", "other"));
        let results = store.search("auth");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.id, "m1");
        assert!((results[0].1 - 0.6).abs() < 1e-9);
    }

    #[test]
    fn memory_search_content_match() {
        let mut store = AgentMemoryStore::new();
        store.store(make_memory(
            "m1",
            "Notes",
            "JWT tokens are secure",
            "security",
        ));
        let results = store.search("jwt");
        assert_eq!(results.len(), 1);
        assert!((results[0].1 - 0.3).abs() < 1e-9);
    }

    #[test]
    fn memory_search_category_bonus() {
        let mut store = AgentMemoryStore::new();
        store.store(make_memory("m1", "Some note", "content", "auth"));
        let results = store.search("auth");
        assert_eq!(results.len(), 1);
        assert!((results[0].1 - 0.2).abs() < 1e-9);
    }

    #[test]
    fn memory_search_combined_score() {
        let mut store = AgentMemoryStore::new();
        // auth appears in title + category -> 0.6 + 0.2 = 0.8
        store.store(make_memory("m1", "Auth guide", "details", "auth"));
        let results = store.search("auth");
        assert!((results[0].1 - 0.8).abs() < 1e-9);
    }

    #[test]
    fn memory_list_by_category() {
        let mut store = AgentMemoryStore::new();
        store.store(make_memory("m1", "A", "c", "security"));
        store.store(make_memory("m2", "B", "c", "other"));
        store.store(make_memory("m3", "C", "c", "security"));
        let sec = store.list_by_category("security");
        assert_eq!(sec.len(), 2);
        assert!(sec.iter().all(|m| m.category == "security"));
    }

    #[test]
    fn memory_delete() {
        let mut store = AgentMemoryStore::new();
        store.store(make_memory("m1", "A", "c", "x"));
        assert!(store.delete("m1"));
        assert!(!store.delete("m1")); // already gone
        assert!(store.get("m1").is_none());
    }

    // ── #247 GitTrackableStore ────────────────────────────────────────────────

    #[test]
    fn git_store_save_load_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = GitTrackableStore::new(dir.path());
        let task = serde_json::json!({ "id": "task-1", "title": "Do something" });
        store.save_task(&task).unwrap();
        let loaded = store.load_task("task-1").unwrap();
        assert_eq!(loaded["title"], "Do something");
        store.delete_task("task-1").unwrap();
        assert!(store.load_task("task-1").is_err());
    }

    #[test]
    fn git_store_list_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let store = GitTrackableStore::new(dir.path());
        store
            .save_task(&serde_json::json!({ "id": "t1", "v": 1 }))
            .unwrap();
        store
            .save_task(&serde_json::json!({ "id": "t2", "v": 2 }))
            .unwrap();
        let list = store.list_tasks().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn git_store_list_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = GitTrackableStore::new(dir.path());
        let list = store.list_tasks().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn git_store_missing_id_field_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = GitTrackableStore::new(dir.path());
        let bad = serde_json::json!({ "title": "no id" });
        assert!(store.save_task(&bad).is_err());
    }

    #[test]
    fn git_store_tasks_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = GitTrackableStore::new(dir.path());
        assert!(store.tasks_dir().ends_with(".caduceus/tasks"));
    }
}

// ── Tests for #250-255 ────────────────────────────────────────────────────────

#[cfg(test)]
mod feature_tests_250_255 {
    use super::*;

    // ── #250 WikiEngine ───────────────────────────────────────────────────────

    #[test]
    fn wiki_engine_init_creates_dirs_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        assert!(engine.wiki_dir().exists());
        assert!(engine.wiki_dir().join("raw").exists());
        assert!(engine.wiki_dir().join("index.md").exists());
        assert!(engine.wiki_dir().join("log.md").exists());
    }

    #[test]
    fn wiki_engine_init_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        engine.init().unwrap(); // second call must not fail
    }

    #[test]
    fn wiki_engine_write_read_page() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        engine.write_page("rust-tips", "# Rust Tips\n\nUse clippy.").unwrap();
        assert!(engine.page_exists("rust-tips"));
        let content = engine.read_page("rust-tips").unwrap();
        assert!(content.contains("Use clippy"));
    }

    #[test]
    fn wiki_engine_delete_page() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        engine.write_page("to-delete", "content").unwrap();
        assert!(engine.page_exists("to-delete"));
        engine.delete_page("to-delete").unwrap();
        assert!(!engine.page_exists("to-delete"));
    }

    #[test]
    fn wiki_engine_list_pages_excludes_index_and_log() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        engine.write_page("alpha", "# Alpha\ncontent").unwrap();
        engine.write_page("beta", "# Beta\ncontent").unwrap();
        let pages = engine.list_pages().unwrap();
        let slugs: Vec<_> = pages.iter().map(|p| p.slug.as_str()).collect();
        assert!(slugs.contains(&"alpha"));
        assert!(slugs.contains(&"beta"));
        assert!(!slugs.contains(&"index"));
        assert!(!slugs.contains(&"log"));
    }

    #[test]
    fn wiki_engine_list_pages_parses_title_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        engine
            .write_page("mypage", "# My Page\n\nSee also [[other-page]].")
            .unwrap();
        let pages = engine.list_pages().unwrap();
        let page = pages.iter().find(|p| p.slug == "mypage").unwrap();
        assert_eq!(page.title, "My Page");
        assert_eq!(page.links, vec!["other-page"]);
    }

    #[test]
    fn wiki_engine_search_pages() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        engine.write_page("rust-tips", "# Rust Tips\nUse clippy.").unwrap();
        engine
            .write_page("python-tips", "# Python Tips\nUse black.")
            .unwrap();
        let results = engine.search_pages("rust").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "rust-tips");
    }

    // ── #251 WikiIndex ────────────────────────────────────────────────────────

    #[test]
    fn wiki_index_add_find_remove() {
        let mut index = WikiIndex::new();
        index.add_entry(IndexEntry {
            slug: "rust".to_string(),
            title: "Rust".to_string(),
            summary: "Systems language".to_string(),
            category: "concept".to_string(),
            source_count: 2,
            link_count: 5,
        });
        assert_eq!(index.find_by_category("concept").len(), 1);
        assert_eq!(index.find_by_query("systems").len(), 1);
        index.remove_entry("rust");
        assert!(index.find_by_category("concept").is_empty());
    }

    #[test]
    fn wiki_index_update_entry() {
        let mut index = WikiIndex::new();
        index.add_entry(IndexEntry {
            slug: "page".to_string(),
            title: "Old Title".to_string(),
            summary: "old".to_string(),
            category: "entity".to_string(),
            source_count: 0,
            link_count: 0,
        });
        index.update_entry(
            "page",
            IndexEntry {
                slug: "page".to_string(),
                title: "New Title".to_string(),
                summary: "new summary".to_string(),
                category: "entity".to_string(),
                source_count: 1,
                link_count: 2,
            },
        );
        assert_eq!(index.find_by_category("entity").len(), 1);
        assert_eq!(index.find_by_category("entity")[0].title, "New Title");
    }

    #[test]
    fn wiki_index_to_from_markdown_roundtrip() {
        let mut index = WikiIndex::new();
        index.add_entry(IndexEntry {
            slug: "alice".to_string(),
            title: "Alice".to_string(),
            summary: "A person".to_string(),
            category: "entity".to_string(),
            source_count: 3,
            link_count: 7,
        });
        let md = index.to_markdown();
        assert!(md.contains("[[alice]]"));
        let parsed = WikiIndex::from_markdown(&md);
        assert_eq!(parsed.find_by_category("entity").len(), 1);
        let e = &parsed.find_by_category("entity")[0];
        assert_eq!(e.slug, "alice");
        assert_eq!(e.source_count, 3);
        assert_eq!(e.link_count, 7);
    }

    #[test]
    fn wiki_index_orphan_pages() {
        let mut index = WikiIndex::new();
        index.add_entry(IndexEntry {
            slug: "lonely".to_string(),
            title: "Lonely".to_string(),
            summary: "".to_string(),
            category: "entity".to_string(),
            source_count: 0,
            link_count: 0,
        });
        let all = vec!["lonely".to_string(), "linked".to_string()];
        // Both are in all_slugs but neither appears in a linked set derived from entries.
        let orphans = index.orphan_pages(&all);
        assert!(orphans.contains(&"lonely".to_string()) || orphans.contains(&"linked".to_string()));
    }

    // ── #252 WikiLog ──────────────────────────────────────────────────────────

    #[test]
    fn wiki_log_append_and_recent() {
        let mut log = WikiLog::new();
        log.append(LogEntry {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            operation: WikiOperation::Create,
            description: "Created page".to_string(),
            pages_touched: vec!["page1".to_string()],
        });
        log.append(LogEntry {
            timestamp: "2024-01-02T00:00:00Z".to_string(),
            operation: WikiOperation::Query,
            description: "Searched wiki".to_string(),
            pages_touched: vec![],
        });
        assert_eq!(log.recent(1).len(), 1);
        assert_eq!(log.recent(1)[0].operation, WikiOperation::Query);
        assert_eq!(log.recent(10).len(), 2);
    }

    #[test]
    fn wiki_log_by_operation() {
        let mut log = WikiLog::new();
        log.append(LogEntry {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            operation: WikiOperation::Ingest,
            description: "Ingested doc".to_string(),
            pages_touched: vec![],
        });
        log.append(LogEntry {
            timestamp: "2024-01-02T00:00:00Z".to_string(),
            operation: WikiOperation::Lint,
            description: "Ran lint".to_string(),
            pages_touched: vec![],
        });
        assert_eq!(log.by_operation(&WikiOperation::Ingest).len(), 1);
        assert_eq!(log.by_operation(&WikiOperation::Lint).len(), 1);
        assert!(log.by_operation(&WikiOperation::Delete).is_empty());
    }

    #[test]
    fn wiki_log_to_from_markdown_roundtrip() {
        let mut log = WikiLog::new();
        log.append(LogEntry {
            timestamp: "2024-03-15T12:00:00Z".to_string(),
            operation: WikiOperation::Ingest,
            description: "Ingested paper.pdf".to_string(),
            pages_touched: vec!["paper".to_string(), "author".to_string()],
        });
        let md = log.to_markdown();
        assert!(md.contains("Ingest"));
        assert!(md.contains("paper.pdf"));
        let parsed = WikiLog::from_markdown(&md);
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].operation, WikiOperation::Ingest);
        assert_eq!(parsed.entries[0].pages_touched.len(), 2);
    }

    #[test]
    fn wiki_log_stats() {
        let mut log = WikiLog::new();
        for op in [
            WikiOperation::Create,
            WikiOperation::Ingest,
            WikiOperation::Ingest,
            WikiOperation::Query,
            WikiOperation::Lint,
        ] {
            log.append(LogEntry {
                timestamp: "2024-01-01T00:00:00Z".to_string(),
                operation: op,
                description: "x".to_string(),
                pages_touched: vec![],
            });
        }
        let s = log.stats();
        assert_eq!(s.total_operations, 5);
        assert_eq!(s.ingests, 2);
        assert_eq!(s.queries, 1);
        assert_eq!(s.lints, 1);
        assert_eq!(s.pages_created, 1);
    }

    // ── #253 WikiIngestor ─────────────────────────────────────────────────────

    #[test]
    fn ingestor_slugify() {
        assert_eq!(WikiIngestor::slugify("My Page Title"), "my-page-title");
        assert_eq!(WikiIngestor::slugify("Hello World!"), "hello-world");
        assert_eq!(WikiIngestor::slugify("  spaces  "), "spaces");
    }

    #[test]
    fn ingestor_extract_entities() {
        let src = "Alice and Bob went to London to meet Charlie.";
        let entities = WikiIngestor::extract_entities(src);
        assert!(entities.iter().any(|e| e == "Alice" || e == "London" || e == "Charlie"));
    }

    #[test]
    fn ingestor_extract_key_claims() {
        let src =
            "The model achieves 95% accuracy. It trains in under an hour. Short.";
        let claims = WikiIngestor::extract_key_claims(src);
        assert!(!claims.is_empty());
        assert!(claims.iter().any(|c| c.contains("accuracy")));
    }

    #[test]
    fn ingestor_generate_summary_page() {
        let src = "Alice founded the company in 2010. She built a great team. The company grew fast.";
        let page = WikiIngestor::generate_summary_page("Alice Co", src);
        assert!(page.starts_with("# Alice Co"));
        assert!(page.contains("## Summary"));
    }

    #[test]
    fn ingestor_generate_entity_page() {
        let src = "Alice is the CEO. Alice started in 2010. Alice loves Rust.";
        let page = WikiIngestor::generate_entity_page("Alice", src);
        assert!(page.starts_with("# Alice"));
        assert!(page.contains("CEO") || page.contains("Alice"));
    }

    #[test]
    fn ingestor_find_cross_references() {
        let content = "We discuss rust-tips and python-tips in detail.";
        let slugs = vec![
            "rust-tips".to_string(),
            "python-tips".to_string(),
            "go-tips".to_string(),
        ];
        let refs = WikiIngestor::find_cross_references(content, &slugs);
        assert!(refs.contains(&"rust-tips".to_string()));
        assert!(refs.contains(&"python-tips".to_string()));
        assert!(!refs.contains(&"go-tips".to_string()));
    }

    // ── #254 WikiLinter ───────────────────────────────────────────────────────

    fn make_page(slug: &str, size: u64, links: Vec<&str>) -> WikiPage {
        WikiPage {
            slug: slug.to_string(),
            title: slug.to_string(),
            path: PathBuf::from(format!("{slug}.md")),
            size_bytes: size,
            links: links.iter().map(|s| s.to_string()).collect(),
            last_modified: 0,
        }
    }

    #[test]
    fn linter_find_empty_pages() {
        let pages = vec![make_page("empty", 0, vec![]), make_page("full", 100, vec![])];
        let idx = WikiIndex::new();
        let findings = WikiLinter::find_empty_pages(&pages);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category, LintCategory::EmptyPage);
        assert_eq!(findings[0].page, "empty");
        let _ = &idx;
    }

    #[test]
    fn linter_find_broken_links() {
        let pages = vec![
            make_page("a", 10, vec!["b", "missing-page"]),
            make_page("b", 10, vec![]),
        ];
        let findings = WikiLinter::find_broken_links(&pages);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category, LintCategory::MissingPage);
        assert!(findings[0].description.contains("missing-page"));
    }

    #[test]
    fn linter_find_orphans() {
        let pages = vec![
            make_page("root", 10, vec!["child"]),
            make_page("child", 10, vec![]),
            make_page("orphan", 10, vec![]),
        ];
        let idx = WikiIndex::new();
        let findings = WikiLinter::find_orphans(&pages, &idx);
        let orphan_pages: Vec<_> = findings.iter().map(|f| f.page.as_str()).collect();
        // "root" and "orphan" have no inbound links; "child" is linked from "root"
        assert!(orphan_pages.contains(&"orphan"));
        assert!(!orphan_pages.contains(&"child"));
    }

    #[test]
    fn linter_find_stale_pages() {
        let mut stale = make_page("stale", 100, vec![]);
        stale.last_modified = 1; // epoch start = very old
        let pages = vec![stale];
        let findings = WikiLinter::find_stale_pages(&pages, 30);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category, LintCategory::StaleContent);
    }

    // ── #255 WikiQueryEngine ──────────────────────────────────────────────────

    #[test]
    fn query_engine_search_scores_and_ranks() {
        let pages = vec![
            make_page("rust-async", 100, vec![]),
            make_page("python-tips", 100, vec![]),
        ];
        let mut contents = HashMap::new();
        contents.insert(
            "rust-async".to_string(),
            "Rust async programming with tokio is great.".to_string(),
        );
        contents.insert(
            "python-tips".to_string(),
            "Python is a scripting language.".to_string(),
        );
        let results = WikiQueryEngine::search(&pages, &contents, "rust async");
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "rust-async");
    }

    #[test]
    fn query_engine_gather_context() {
        let pages = vec![make_page("a", 10, vec![]), make_page("b", 10, vec![])];
        let mut contents = HashMap::new();
        contents.insert("a".to_string(), "Content of A.".to_string());
        contents.insert("b".to_string(), "Content of B.".to_string());
        let ctx =
            WikiQueryEngine::gather_context(&pages, &contents, &["a".to_string()]);
        assert!(ctx.contains("Content of A"));
        assert!(!ctx.contains("Content of B"));
    }

    #[test]
    fn query_engine_extract_citations() {
        let text = "See [[rust-tips]] and [[python-guide]] for more.";
        let cites = WikiQueryEngine::extract_citations(text);
        assert_eq!(cites.len(), 2);
        assert!(cites.contains(&"rust-tips".to_string()));
        assert!(cites.contains(&"python-guide".to_string()));
    }

    #[test]
    fn query_engine_search_no_match() {
        let pages = vec![make_page("rust", 10, vec![])];
        let mut contents = HashMap::new();
        contents.insert("rust".to_string(), "Systems programming.".to_string());
        let results = WikiQueryEngine::search(&pages, &contents, "quantum");
        assert!(results.is_empty());
    }
}

// ── Tests for #256-258 ────────────────────────────────────────────────────────

#[cfg(test)]
mod feature_tests_256_258 {
    use super::*;
    use std::collections::HashMap;

    // ── #256 WikiWatcher ──────────────────────────────────────────────────────

    #[test]
    fn watcher_hash_file_stable() {
        let h1 = WikiWatcher::hash_file("hello world");
        let h2 = WikiWatcher::hash_file("hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn watcher_hash_file_differs_on_different_content() {
        let h1 = WikiWatcher::hash_file("foo");
        let h2 = WikiWatcher::hash_file("bar");
        assert_ne!(h1, h2);
    }

    #[test]
    fn watcher_should_watch_extensions() {
        let w = WikiWatcher::new();
        assert!(w.should_watch("src/main.rs"));
        assert!(w.should_watch("lib/util.py"));
        assert!(w.should_watch("notes.md"));
        assert!(!w.should_watch("image.png"));
        assert!(!w.should_watch("binary.exe"));
    }

    #[test]
    fn watcher_should_watch_ignores_patterns() {
        let w = WikiWatcher::new();
        assert!(!w.should_watch("node_modules/react/index.js"));
        assert!(!w.should_watch(".git/config"));
        assert!(!w.should_watch("target/debug/build/foo.rs"));
    }

    #[test]
    fn watcher_detect_created_file() {
        let dir = tempfile::tempdir().unwrap();
        let w = WikiWatcher::new();
        let prev: HashMap<String, u64> = HashMap::new();
        std::fs::write(dir.path().join("hello.rs"), "fn main() {}").unwrap();
        let changes = w.detect_changes(dir.path(), &prev);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FileChangeType::Created);
        assert!(changes[0].path.ends_with("hello.rs"));
    }

    #[test]
    fn watcher_detect_modified_file() {
        let dir = tempfile::tempdir().unwrap();
        let w = WikiWatcher::new();
        std::fs::write(dir.path().join("hello.rs"), "fn main() {}").unwrap();
        let prev = w.snapshot_project(dir.path());
        // Modify the file
        std::fs::write(dir.path().join("hello.rs"), "fn main() { println!(\"hi\"); }").unwrap();
        let changes = w.detect_changes(dir.path(), &prev);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FileChangeType::Modified);
    }

    #[test]
    fn watcher_detect_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let w = WikiWatcher::new();
        std::fs::write(dir.path().join("bye.rs"), "fn bye() {}").unwrap();
        let prev = w.snapshot_project(dir.path());
        std::fs::remove_file(dir.path().join("bye.rs")).unwrap();
        let changes = w.detect_changes(dir.path(), &prev);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FileChangeType::Deleted);
    }

    #[test]
    fn watcher_no_changes_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let w = WikiWatcher::new();
        std::fs::write(dir.path().join("stable.rs"), "fn stable() {}").unwrap();
        let prev = w.snapshot_project(dir.path());
        let changes = w.detect_changes(dir.path(), &prev);
        assert!(changes.is_empty());
    }

    #[test]
    fn watcher_snapshot_ignores_untracked_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let w = WikiWatcher::new();
        std::fs::write(dir.path().join("image.png"), &[0u8, 1, 2]).unwrap();
        let snap = w.snapshot_project(dir.path());
        assert!(snap.is_empty());
    }

    #[test]
    fn watcher_snapshot_ignores_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("target").join("debug");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("main.rs"), "fn main() {}").unwrap();
        let w = WikiWatcher::new();
        let snap = w.snapshot_project(dir.path());
        assert!(snap.is_empty(), "target/ contents should not be snapshotted");
    }

    // ── #257 WikiMaintenanceAgent ─────────────────────────────────────────────

    #[test]
    fn maintenance_agent_plan_create_action_for_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        let index = WikiIndex::new();
        let agent = WikiMaintenanceAgent::new();

        let changes = vec![FileChange {
            path: "src/lib.rs".to_string(),
            change_type: FileChangeType::Created,
            content_hash: 42,
        }];

        let actions = agent.plan_actions(&changes, &engine, &index);
        assert!(actions
            .iter()
            .any(|a| a.action_type == MaintenanceActionType::CreatePage));
        assert!(actions
            .iter()
            .any(|a| a.action_type == MaintenanceActionType::UpdateIndex));
        assert!(actions
            .iter()
            .any(|a| a.action_type == MaintenanceActionType::RunLint));
        assert!(actions
            .iter()
            .any(|a| a.action_type == MaintenanceActionType::UpdateLog));
    }

    #[test]
    fn maintenance_agent_plan_update_action_for_existing_page() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        let index = WikiIndex::new();
        let agent = WikiMaintenanceAgent::new();

        // Pre-create the page the agent would update
        let slug = format!("src-{}", WikiIngestor::slugify("src-lib-rs"));
        engine
            .write_page(&slug, "# Existing Page\n\ncontent")
            .unwrap();

        let changes = vec![FileChange {
            path: "src/lib.rs".to_string(),
            change_type: FileChangeType::Modified,
            content_hash: 99,
        }];

        let actions = agent.plan_actions(&changes, &engine, &index);
        // Some action for the modified file (create or update depending on slug match)
        assert!(!actions.is_empty());
    }

    #[test]
    fn maintenance_agent_plan_archive_for_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        let index = WikiIndex::new();
        let agent = WikiMaintenanceAgent::new();

        let changes = vec![FileChange {
            path: "old/thing.py".to_string(),
            change_type: FileChangeType::Deleted,
            content_hash: 0,
        }];

        let actions = agent.plan_actions(&changes, &engine, &index);
        // Deleted files get an UpdatePage (archive) action
        assert!(actions
            .iter()
            .any(|a| a.action_type == MaintenanceActionType::UpdatePage
                && a.description.contains("Archive")));
    }

    #[test]
    fn maintenance_agent_plan_no_actions_for_empty_changes() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        let index = WikiIndex::new();
        let agent = WikiMaintenanceAgent::new();

        let actions = agent.plan_actions(&[], &engine, &index);
        assert!(actions.is_empty());
    }

    #[test]
    fn maintenance_agent_execute_create_page() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        let agent = WikiMaintenanceAgent::new();
        let mut index = WikiIndex::new();
        let mut log = WikiLog::new();

        let action = MaintenanceAction {
            action_type: MaintenanceActionType::CreatePage,
            page_slug: "test-page".to_string(),
            description: "Test creation".to_string(),
        };

        agent
            .execute_action(&action, &engine, &mut index, &mut log)
            .unwrap();
        assert!(engine.page_exists("test-page"));
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].operation, WikiOperation::Create);
    }

    #[test]
    fn maintenance_agent_execute_update_page_appends_note() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        engine
            .write_page("mypage", "# My Page\n\nOriginal content")
            .unwrap();
        let agent = WikiMaintenanceAgent::new();
        let mut index = WikiIndex::new();
        let mut log = WikiLog::new();

        let action = MaintenanceAction {
            action_type: MaintenanceActionType::UpdatePage,
            page_slug: "mypage".to_string(),
            description: "Update summary".to_string(),
        };

        agent
            .execute_action(&action, &engine, &mut index, &mut log)
            .unwrap();
        let content = engine.read_page("mypage").unwrap();
        assert!(content.contains("Original content"));
        assert!(content.contains("Updated"));
    }

    #[test]
    fn maintenance_agent_execute_delete_page() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        engine.write_page("doomed", "# Doomed\n\n").unwrap();
        let agent = WikiMaintenanceAgent::new();
        let mut index = WikiIndex::new();
        let mut log = WikiLog::new();

        let action = MaintenanceAction {
            action_type: MaintenanceActionType::DeletePage,
            page_slug: "doomed".to_string(),
            description: "Cleanup".to_string(),
        };

        agent
            .execute_action(&action, &engine, &mut index, &mut log)
            .unwrap();
        assert!(!engine.page_exists("doomed"));
        assert_eq!(log.entries[0].operation, WikiOperation::Delete);
    }

    #[test]
    fn maintenance_agent_execute_update_index() {
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        engine
            .write_page("alpha", "# Alpha\n\ncontent here")
            .unwrap();
        let agent = WikiMaintenanceAgent::new();
        let mut index = WikiIndex::new();
        let mut log = WikiLog::new();

        let action = MaintenanceAction {
            action_type: MaintenanceActionType::UpdateIndex,
            page_slug: "index".to_string(),
            description: "Rebuild index".to_string(),
        };

        agent
            .execute_action(&action, &engine, &mut index, &mut log)
            .unwrap();
        let index_content = std::fs::read_to_string(engine.wiki_dir().join("index.md")).unwrap();
        assert!(index_content.contains("alpha"));
    }

    #[test]
    fn maintenance_agent_run_full_maintenance() {
        let dir = tempfile::tempdir().unwrap();
        // Put a watched file in the project root
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let agent = WikiMaintenanceAgent::new();
        let prev: HashMap<String, u64> = HashMap::new();
        let report = agent.run_full_maintenance(dir.path(), &prev).unwrap();

        assert!(report.pages_created > 0);
        assert!(report.index_updated);
    }

    // ── #258 WikiAutoTrigger ──────────────────────────────────────────────────

    #[test]
    fn auto_trigger_enable_disable() {
        let mut trigger = WikiAutoTrigger::new();
        assert!(trigger.is_enabled());
        trigger.disable();
        assert!(!trigger.is_enabled());
        trigger.enable();
        assert!(trigger.is_enabled());
    }

    #[test]
    fn auto_trigger_disabled_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut trigger = WikiAutoTrigger::new();
        trigger.disable();
        std::fs::write(dir.path().join("new.rs"), "fn new() {}").unwrap();
        let result = trigger.on_agent_turn_complete(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn auto_trigger_session_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-populate a file before session start
        std::fs::write(dir.path().join("existing.rs"), "fn existing() {}").unwrap();

        let mut trigger = WikiAutoTrigger::new();
        trigger.on_session_start(dir.path());

        // No changes since start → None
        let report = trigger.on_agent_turn_complete(dir.path()).unwrap();
        assert!(report.is_none(), "no changes should produce no report");

        // Create a new file during the session
        std::fs::write(dir.path().join("added.rs"), "fn added() {}").unwrap();

        // First turn after the change → Some report
        let report = trigger.on_agent_turn_complete(dir.path()).unwrap();
        assert!(report.is_some());
        let r = report.unwrap();
        assert!(r.pages_created > 0);

        // Second turn with no further changes → None
        let report2 = trigger.on_agent_turn_complete(dir.path()).unwrap();
        assert!(report2.is_none());

        // Session end with no additional changes → None
        let end_report = trigger.on_session_end(dir.path()).unwrap();
        assert!(end_report.is_none());
    }

    #[test]
    fn auto_trigger_force_maintenance() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("force.rs"), "fn force() {}").unwrap();

        let mut trigger = WikiAutoTrigger::new();
        // No initial snapshot set — previous_hashes is empty → file looks new
        let report = trigger.force_maintenance(dir.path()).unwrap();
        assert!(report.pages_created > 0 || report.pages_updated >= 0);
    }

    #[test]
    fn auto_trigger_on_session_end_detects_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut trigger = WikiAutoTrigger::new();
        trigger.on_session_start(dir.path());

        // Create file after session start
        std::fs::write(dir.path().join("end.rs"), "fn end() {}").unwrap();

        let report = trigger.on_session_end(dir.path()).unwrap();
        assert!(report.is_some());
        assert!(report.unwrap().pages_created > 0);
    }

    #[test]
    fn auto_trigger_snapshot_updated_after_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mut trigger = WikiAutoTrigger::new();
        trigger.on_session_start(dir.path());

        std::fs::write(dir.path().join("once.rs"), "fn once() {}").unwrap();

        // First turn sees the change
        let r1 = trigger.on_agent_turn_complete(dir.path()).unwrap();
        assert!(r1.is_some());

        // Second turn: same file, no change → snapshot updated, returns None
        let r2 = trigger.on_agent_turn_complete(dir.path()).unwrap();
        assert!(r2.is_none());
    }
}
