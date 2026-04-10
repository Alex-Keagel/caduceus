# Caduceus Wiring Plan — Parts 4–6

> **Generated from:** all 10 spec files (`spec-claurst-blackbox.md`, `spec-claurst-full.md`, `spec-claw-code.md`, `spec-e2b.md`, `spec-hermes-ide.md`, `spec-hermes-ide-supplement.md`, `spec-open-multi-agent.md`, `spec-qdrant.md`, `spec-tree-sitter.md`, `spec-zed-crdt.md`)

---

## Part 4: Data Model

### 4.1 SQLite Schema

**Runtime decision:** use **SQLite as the authoritative store** for IDE/runtime state, with **JSONL transcript export/import** for claw-code/claurst compatibility. Hermes gives the strongest local-state contract; claurst/claw-code add transcript, cost, hook, and session semantics.

**Database settings**
- `PRAGMA journal_mode = WAL;`
- `PRAGMA foreign_keys = ON;`
- idempotent migrations (`CREATE TABLE IF NOT EXISTS`, additive `ALTER TABLE` only)
- JSON payload columns stored as `TEXT` containing validated JSON

```sql
CREATE TABLE sessions (
  id                TEXT PRIMARY KEY,
  label             TEXT,
  color             TEXT,
  group_id          TEXT,
  phase             TEXT NOT NULL CHECK (phase IN (
                        'creating','initializing','shell_ready','launching_agent',
                        'idle','busy','needs_input','error','closing',
                        'disconnected','destroyed'
                      )),
  working_dir       TEXT,
  shell_type        TEXT,
  provider          TEXT,
  model             TEXT,
  permission_mode   TEXT,
  workspace_paths   TEXT,
  bridge_state      TEXT,
  ssh_host          TEXT,
  ssh_port          INTEGER,
  ssh_user          TEXT,
  ssh_identity      TEXT,
  ssh_tmux          TEXT,
  created_at        TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at        TEXT NOT NULL DEFAULT (datetime('now')),
  last_active_at    TEXT,
  closed_at         TEXT
);
CREATE INDEX idx_sessions_phase ON sessions(phase);
CREATE INDEX idx_sessions_group ON sessions(group_id);
CREATE INDEX idx_sessions_updated_at ON sessions(updated_at DESC);

CREATE TABLE messages (
  id                TEXT PRIMARY KEY,
  session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  parent_id         TEXT REFERENCES messages(id) ON DELETE SET NULL,
  role              TEXT NOT NULL CHECK (role IN ('system','user','assistant','tool_result','summary')),
  entry_type        TEXT NOT NULL,
  content_json      TEXT NOT NULL,
  summary_text      TEXT,
  cwd               TEXT,
  git_branch        TEXT,
  is_sidechain      INTEGER NOT NULL DEFAULT 0,
  user_type         TEXT,
  sequence_no       INTEGER NOT NULL,
  version           INTEGER NOT NULL DEFAULT 1,
  input_tokens      INTEGER NOT NULL DEFAULT 0,
  output_tokens     INTEGER NOT NULL DEFAULT 0,
  created_at        TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(session_id, sequence_no)
);
CREATE INDEX idx_messages_session_created ON messages(session_id, created_at);
CREATE INDEX idx_messages_parent ON messages(parent_id);
CREATE INDEX idx_messages_role ON messages(role);

CREATE TABLE tool_calls (
  id                TEXT PRIMARY KEY,
  session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  message_id        TEXT REFERENCES messages(id) ON DELETE CASCADE,
  tool_name         TEXT NOT NULL,
  input_json        TEXT,
  output_json       TEXT,
  status            TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','running','completed','error','cancelled')),
  permission_level  TEXT,
  duration_ms       INTEGER,
  error_code        TEXT,
  error_text        TEXT,
  started_at        TEXT NOT NULL DEFAULT (datetime('now')),
  finished_at       TEXT
);
CREATE INDEX idx_tool_calls_session ON tool_calls(session_id, started_at DESC);
CREATE INDEX idx_tool_calls_message ON tool_calls(message_id);
CREATE INDEX idx_tool_calls_name ON tool_calls(tool_name);
CREATE INDEX idx_tool_calls_status ON tool_calls(status);

CREATE TABLE costs (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  provider          TEXT NOT NULL,
  model             TEXT NOT NULL,
  prompt_tokens     INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  cache_write_tokens INTEGER NOT NULL DEFAULT 0,
  reasoning_tokens  INTEGER NOT NULL DEFAULT 0,
  cost_usd          REAL NOT NULL DEFAULT 0.0,
  source            TEXT NOT NULL DEFAULT 'local',
  created_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_costs_session_provider ON costs(session_id, provider, created_at DESC);
CREATE INDEX idx_costs_model_day ON costs(model, created_at DESC);

CREATE TABLE cost_daily (
  usage_date        TEXT NOT NULL,
  provider          TEXT NOT NULL,
  model             TEXT NOT NULL,
  session_count     INTEGER NOT NULL DEFAULT 0,
  total_prompt_tokens INTEGER NOT NULL DEFAULT 0,
  total_completion_tokens INTEGER NOT NULL DEFAULT 0,
  total_cost_usd    REAL NOT NULL DEFAULT 0.0,
  PRIMARY KEY (usage_date, provider, model)
);

CREATE TABLE token_snapshots (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  provider          TEXT NOT NULL,
  model             TEXT NOT NULL,
  prompt_tokens     INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0,
  cost_usd          REAL NOT NULL DEFAULT 0.0,
  recorded_at       TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_token_snapshots_session ON token_snapshots(session_id, recorded_at DESC);
CREATE INDEX idx_token_snapshots_recorded_at ON token_snapshots(recorded_at DESC);

CREATE TABLE audit_log (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id        TEXT REFERENCES sessions(id) ON DELETE CASCADE,
  actor_type        TEXT NOT NULL,
  actor_id          TEXT,
  event_type        TEXT NOT NULL,
  severity          TEXT NOT NULL DEFAULT 'info',
  detail_json       TEXT NOT NULL,
  created_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_audit_log_session ON audit_log(session_id, created_at DESC);
CREATE INDEX idx_audit_log_event ON audit_log(event_type, created_at DESC);

CREATE TABLE projects (
  id                TEXT PRIMARY KEY,
  path              TEXT NOT NULL UNIQUE,
  name              TEXT NOT NULL,
  repo_root         TEXT,
  vcs               TEXT,
  default_branch    TEXT,
  languages_json    TEXT,
  frameworks_json   TEXT,
  architecture_json TEXT,
  conventions_hash  TEXT,
  scan_depth        TEXT NOT NULL DEFAULT 'surface',
  scan_status       TEXT NOT NULL DEFAULT 'queued',
  last_scanned_at   TEXT,
  created_at        TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_projects_scan_status ON projects(scan_status, updated_at DESC);
CREATE INDEX idx_projects_name ON projects(name);

CREATE TABLE session_projects (
  session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  role              TEXT,
  attached_at       TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (session_id, project_id)
);
CREATE INDEX idx_session_projects_project ON session_projects(project_id);

CREATE TABLE memories (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  scope             TEXT NOT NULL CHECK (scope IN ('session','project','global')),
  scope_id          TEXT NOT NULL,
  category          TEXT NOT NULL DEFAULT 'general',
  key               TEXT NOT NULL,
  value             TEXT NOT NULL,
  source            TEXT NOT NULL DEFAULT 'auto',
  confidence        REAL NOT NULL DEFAULT 1.0,
  access_count      INTEGER NOT NULL DEFAULT 0,
  expires_at        TEXT,
  created_at        TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at        TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(scope, scope_id, key)
);
CREATE INDEX idx_memories_scope ON memories(scope, scope_id, updated_at DESC);
CREATE INDEX idx_memories_category ON memories(category);

CREATE TABLE commands (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id        TEXT REFERENCES sessions(id) ON DELETE CASCADE,
  tool_call_id      TEXT REFERENCES tool_calls(id) ON DELETE SET NULL,
  raw_command       TEXT NOT NULL,
  normalized_command TEXT,
  working_dir       TEXT,
  exit_code         INTEGER,
  duration_ms       INTEGER,
  source            TEXT NOT NULL DEFAULT 'shell',
  created_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_commands_session ON commands(session_id, created_at DESC);
CREATE INDEX idx_commands_normalized ON commands(normalized_command);

CREATE TABLE command_patterns (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  pattern           TEXT NOT NULL UNIQUE,
  frequency         INTEGER NOT NULL DEFAULT 1,
  last_used_at      TEXT
);

CREATE TABLE errors (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id        TEXT REFERENCES sessions(id) ON DELETE CASCADE,
  command_id        INTEGER REFERENCES commands(id) ON DELETE SET NULL,
  tool_call_id      TEXT REFERENCES tool_calls(id) ON DELETE SET NULL,
  fingerprint       TEXT NOT NULL,
  error_class       TEXT,
  message_text      TEXT NOT NULL,
  resolution_text   TEXT,
  provider          TEXT,
  verified          INTEGER NOT NULL DEFAULT 0,
  occurrences       INTEGER NOT NULL DEFAULT 1,
  first_seen_at     TEXT NOT NULL DEFAULT (datetime('now')),
  last_seen_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX idx_errors_fingerprint ON errors(fingerprint);
CREATE INDEX idx_errors_session ON errors(session_id, last_seen_at DESC);

CREATE TABLE contexts (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  version           INTEGER NOT NULL,
  lifecycle_state   TEXT NOT NULL CHECK (lifecycle_state IN ('clean','dirty','applying','apply_failed')),
  assembled_markdown TEXT NOT NULL,
  token_budget      INTEGER NOT NULL,
  estimated_tokens  INTEGER NOT NULL,
  trimmed           INTEGER NOT NULL DEFAULT 0,
  source_hash       TEXT,
  created_at        TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(session_id, version)
);
CREATE INDEX idx_contexts_session ON contexts(session_id, version DESC);

CREATE TABLE context_pins (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id        TEXT REFERENCES sessions(id) ON DELETE CASCADE,
  project_id        TEXT REFERENCES projects(id) ON DELETE CASCADE,
  kind              TEXT NOT NULL CHECK (kind IN ('file','memory','text','snippet')),
  target            TEXT NOT NULL,
  label             TEXT,
  priority          INTEGER NOT NULL DEFAULT 0,
  created_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_context_pins_session ON context_pins(session_id, priority DESC);
CREATE INDEX idx_context_pins_project ON context_pins(project_id, priority DESC);

CREATE TABLE context_snapshots (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  context_id        INTEGER NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
  snapshot_json     TEXT NOT NULL,
  created_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_context_snapshots_context ON context_snapshots(context_id, created_at DESC);

CREATE TABLE session_worktrees (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  branch            TEXT NOT NULL,
  worktree_path     TEXT NOT NULL,
  last_active_at    TEXT,
  UNIQUE(session_id, project_id, branch)
);
CREATE INDEX idx_session_worktrees_project ON session_worktrees(project_id, last_active_at DESC);

CREATE TABLE settings (
  key               TEXT PRIMARY KEY,
  value_json        TEXT NOT NULL,
  source            TEXT NOT NULL DEFAULT 'sqlite',
  updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE config_cache (
  project_id        TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  config_hash       TEXT NOT NULL,
  config_json       TEXT NOT NULL,
  cached_at         TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE conventions (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  rule              TEXT NOT NULL,
  source            TEXT,
  priority          INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_conventions_project ON conventions(project_id, priority DESC);

CREATE TABLE ssh_hosts (
  id                TEXT PRIMARY KEY,
  host              TEXT NOT NULL,
  port              INTEGER NOT NULL DEFAULT 22,
  user              TEXT,
  identity_file     TEXT,
  jump_host         TEXT,
  label             TEXT
);

CREATE TABLE plugins (
  id                TEXT PRIMARY KEY,
  name              TEXT NOT NULL,
  version           TEXT NOT NULL,
  permissions_json  TEXT NOT NULL,
  enabled           INTEGER NOT NULL DEFAULT 1,
  installed_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE plugin_storage (
  plugin_id         TEXT NOT NULL REFERENCES plugins(id) ON DELETE CASCADE,
  key               TEXT NOT NULL,
  value_json        TEXT NOT NULL,
  PRIMARY KEY (plugin_id, key)
);
```

**Notes**
- `messages.content_json` is the canonical content-block envelope: `text`, `tool_use`, `tool_result`, `thinking`, `redacted_thinking`, `image`, `summary`, `attachment`.
- `contexts` is the assembled prompt-context artifact; `context_pins` and `memories` are its inputs.
- `costs` is the canonical event table; `cost_daily` and `token_snapshots` are rollups/history for dashboards.
- `commands` tracks actual execution; `command_patterns` feeds Hermes-style suggestion/prediction.
- `errors` stores reusable fingerprints/resolutions for Hermes error intelligence.

### 4.2 CRDT Document Model

#### Replica identity

| Replica ID | Meaning | Caduceus usage |
|---|---|---|
| `0` | `LOCAL` | primary human editor on current machine |
| `1` | `REMOTE_SERVER` | remote SSH/bridge authority |
| `2` | `AGENT` | default AI editor identity |
| `3` | `LOCAL_BRANCH` | branch/worktree shadow buffer |
| `>= 8` | `FIRST_COLLAB_ID` | additional humans or named AI workers |

**Rule:** single-agent v1 can use `ReplicaId(2)` for all AI edits; post-v1 multi-agent mode should allocate a unique replica ID per worker so selections, undo ownership, and causality remain attributable.

#### Clock model

```rust
pub struct ReplicaId(pub u16);

pub struct Lamport {
    pub value: u32,
    pub replica_id: ReplicaId,
}

pub struct Global(pub SmallVec<[u32; 4]>); // version vector by replica slot
```

- **Lamport** provides total ordering for inserts, deletes, undo events, and transactions.
- **Global/version vector** provides causal dependency tracking, reconnect diffing, and deferred-op replay.
- Local edit application increments Lamport and updates the local slot in `Global`.

#### Fragment and locator model

```rust
pub struct Locator(pub SmallVec<[u64; 2]>); // fractional position identifier

pub struct Fragment {
    pub id: Locator,
    pub timestamp: Lamport,
    pub insertion_offset: u32,
    pub len: u32,
    pub visible: bool,
    pub deletions: SmallVec<[Lamport; 2]>,
    pub max_undos: Global,
}
```

- `Locator::between(lhs, rhs)` generates stable fractional positions.
- `Locator::min()` / `Locator::max()` bound the sequence.
- Text storage uses **rope + sum-tree** backing, with fragments referencing visible and tombstoned spans.

#### Operation types

```rust
pub enum Operation {
    Edit(EditOperation),
    Undo(UndoOperation),
}

pub struct EditOperation {
    pub timestamp: Lamport,
    pub version: Global,
    pub ranges: Vec<Range<FullOffset>>,
    pub new_text: Vec<Arc<str>>,
}

pub struct UndoOperation {
    pub timestamp: Lamport,
    pub version: Global,
    pub counts: HashMap<Lamport, u32>,
}
```

- **Edit** = insert/delete/replace in one atomic operation.
- **Undo** flips visibility by incrementing undo counts for prior edits.
- **Undo rule:** odd undo count = undone, even = active.
- **Conflict rule:** for concurrent inserts at same position, higher Lamport wins; tie-breaker is higher `replica_id`.

#### Anchor system

```rust
pub struct Anchor {
    pub timestamp_replica_id: ReplicaId,
    pub timestamp_value: u32,
    pub offset: u32,
    pub bias: Bias,   // Left | Right
    pub buffer_id: BufferId,
}
```

Anchors are required for:
- stable cursors and selections
- diagnostics/lint ranges
- AI edit targets that survive concurrent human edits
- cross-buffer references from tool results and semantic search hits

#### Buffer/snapshot model

```rust
pub struct Buffer {
    pub replica_id: ReplicaId,
    pub version: Global,
    pub visible_text: Rope,
    pub deleted_text: Rope,
    pub fragments: SumTree<Fragment>,
    pub insertions: SumTree<InsertionFragment>,
    pub undo_map: UndoMap,
    pub line_ending: LineEnding,
}

pub struct BufferSnapshot { /* immutable, Send + Sync */ }
```

- `Buffer` is mutable and not thread-safe.
- `BufferSnapshot` is the read-only artifact for background parse/index tasks.
- Transactions group adjacent typing bursts (300 ms window) into undo units.

### 4.3 Vector Index Schema

#### Deployment model
- Use **`qdrant-edge` / `EdgeShard` embedded mode**.
- One shard per workspace or repo-set.
- Use `query()`, not deprecated `search()`.

#### Collection config

**v1 recommendation:** a single named vector, `text`, for simplicity and reliable ingestion.

```rust
EdgeConfig {
  on_disk_payload: true,
  vectors: {
    "text": EdgeVectorParams {
      size: 768,
      distance: Distance::Cosine,
      on_disk: Some(false),
      datatype: None,                 // Float32
      quantization_config: None,
      hnsw_config: Some(HnswConfigDiff {
        m: Some(16),
        ef_construct: Some(100),
        full_scan_threshold: Some(10_000),
        ..Default::default()
      }),
      multivector_config: None,
    }
  },
  sparse_vectors: {},
  hnsw_config: Default::default(),
  quantization_config: None,
  optimizers: Default::default(),
}
```

**Post-v1 extension:** add named vectors `symbol` and `docstring` once retrieval quality and reindex cost are understood. In embedded mode, avoid scalar/product quantization; only binary quantization is appendable-safe.

#### Payload schema

| Field | Type | Index | Notes |
|---|---|---|---|
| `repo` | keyword | yes | logical repository/workspace id |
| `workspace` | keyword | yes | absolute workspace path |
| `file_path` | keyword | yes | canonical relative path |
| `language` | keyword | yes | tree-sitter language id |
| `symbol_type` | keyword | yes | function/class/method/interface/module/doc |
| `symbol_name` | text/keyword | yes | searchable symbol identifier |
| `container` | keyword | yes | parent type/module/impl |
| `ast_kind` | keyword | optional | raw tree-sitter node kind |
| `start_line` | integer | yes | start line of chunk |
| `end_line` | integer | yes | end line of chunk |
| `byte_start` | integer | optional | byte offset |
| `byte_end` | integer | optional | byte offset |
| `tokens` | integer | yes | estimated chunk token count |
| `content_hash` | keyword | yes | dedupe/staleness key |
| `branch` | keyword | optional | branch-sensitive indexing |
| `commit` | keyword | optional | commit pin for reproducibility |
| `contains_error` | bool | optional | parser error flag |
| `parse_error_density` | float | optional | down-ranking signal |
| `code` | text payload | no field index | full source chunk |
| `embedding_model` | keyword | optional | migration/audit support |
| `chunk_version` | integer | optional | chunking schema version |

#### Chunking strategy
- chunk at **tree-sitter semantic units**, not fixed token windows
- primary units: **function, method, class/struct, trait/interface, module, large doc-comment block, config block**
- target **40–300 tokens** per chunk
- preserve `container`, `symbol_name`, and `symbol_type`
- incremental reindex flow:
  1. build `InputEdit` from rope delta
  2. `old_tree.edit(&edit)`
  3. parse with previous tree
  4. compute `changed_ranges`
  5. rerun chunk queries only in intersecting ranges
  6. upsert changed chunks, delete removed chunks

#### Embedding model recommendation
- **Primary:** `nomic-embed-text-v1.5` (768 dims, strong local/OSS default, aligned to the Qdrant spec example)
- **Fallback hosted option:** `text-embedding-3-large` only if hosted inference is acceptable
- keep `embedding_model` in payload so collections can be migrated without silent mismatch

### 4.4 Session State Model

#### Session lifecycle

| State | Meaning |
|---|---|
| `creating` | session object allocated |
| `initializing` | shell/runtime bootstrap in progress |
| `shell_ready` | prompt detected or fallback fired |
| `launching_agent` | AI process/provider startup |
| `idle` | ready for input |
| `busy` | model/tool/PTY work in progress |
| `needs_input` | permission request or user unblock required |
| `error` | terminal/runtime/provider fault |
| `closing` | teardown in progress |
| `disconnected` | remote/SSH session detached |
| `destroyed` | fully terminated |

**Canonical runtime state machine:** Hermes' 11-state machine, with claw-code/claurst mapped into it (`ModelTurn -> busy`, `PermissionPending -> needs_input`).

#### Context window management

```rust
pub struct TokenBudget {
    pub tokens_used: u64,
    pub context_window: u64,
    pub tokens_remaining: u64,
    pub fill_fraction: f64,
    pub warning_level: WarningLevel,
}
```

**Thresholds to implement**
- warning at **80%**
- critical at **95%**
- recommend compact at **90%**
- collapse at **97%**
- claurst auto-compact trigger: **85%** (`compact_threshold`)
- hard warning: **20k tokens before limit**
- block further input: **3k tokens before limit**
- Hermes project-context assembly budget: **4,000 tokens** by default

#### Compaction layers
1. full compact (`/compact`) — LLM summary replacement
2. cached/API-native microcompact
3. time-based microcompact — trim stale tool results
4. session-memory compact — preserve tool-use/tool-result boundaries
5. autoDream/background consolidation — file-based long-term memory

#### Conversation history format

```json
{
  "session_id": "uuid",
  "sequence_no": 42,
  "entry_type": "assistant",
  "role": "assistant",
  "parent_id": "uuid",
  "timestamp": "2026-01-01T12:00:00Z",
  "cwd": "/workspace",
  "git_branch": "main",
  "is_sidechain": false,
  "user_type": "human",
  "content": [
    {"type":"text","text":"I will inspect the repo."},
    {"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls"}}
  ]
}
```

**Rules**
- transcript is append-only
- unknown entry types must be tolerated for compatibility
- tool-use/tool-result pairs must never be orphaned across compaction
- tool results are reintroduced in provider-safe role order
- JSONL export is a serialization of the canonical SQLite transcript

### 4.5 Config File Format

#### Config precedence
1. CLI flags
2. runtime `/connect` and interactive session overrides
3. local worktree config
4. project config
5. legacy project config
6. user config (`~/.claurst/settings.json`, `~/.claude.json`)
7. system admin config (`/etc/claude-code/...`)
8. SQLite `settings` overlay at runtime

**Merge rule:** objects merge recursively; arrays/scalars replace wholesale; MCP servers merge by server name.

#### Unified config schema

```json
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-6",
  "providers": {
    "anthropic": {
      "api_key": "${ANTHROPIC_API_KEY}",
      "api_base": null,
      "enabled": true,
      "models_whitelist": [],
      "models_blacklist": [],
      "options": {}
    }
  },
  "permissions": {
    "mode": "default",
    "rules": [
      { "tool": "bash", "action": "ask", "pattern": "rm -rf .*" }
    ]
  },
  "scanner": {
    "depth": "surface",
    "exclude": ["node_modules", "dist", ".git"],
    "languages": [],
    "framework_detection": true,
    "architecture_detection": true,
    "convention_learning": true,
    "semantic_chunking": true
  },
  "sandbox": {
    "provider": "e2b",
    "secure": true,
    "timeout_seconds": 300,
    "on_timeout": "terminate",
    "auto_resume": false,
    "allow_public_traffic": false,
    "envd_version_min": "0.1.0"
  },
  "session": {
    "compact_threshold": 0.85,
    "auto_compact_enabled": true,
    "reasoning_effort": "medium",
    "max_budget_usd": null,
    "output_format": "text"
  },
  "mcpServers": {
    "example": {
      "command": "node",
      "args": ["server.js"],
      "env": { "TOKEN": "${TOKEN:-}" },
      "transport": "stdio"
    }
  },
  "hooks": [
    { "event": "PreToolUse", "command": "./guard.sh", "blocking": true }
  ],
  "plugins": {
    "auto_update": false,
    "disabled_ids": [],
    "update_frequency": "weekly"
  },
  "ui": {
    "theme": "system",
    "ui_scale": 1.0,
    "font_size": 13,
    "font_family": "monospace",
    "default_shell": "zsh",
    "scrollback_buffer": 10000,
    "session_restore": true,
    "close_confirm": true,
    "suggestion_mode": "augment",
    "ghost_text_enabled": true,
    "analytics_optin": false
  }
}
```

---

## Part 5: Complete Capability Matrix

### 5.1 Core agent/runtime capabilities

| ID | Capability | Source repo | Caduceus crate | v1/post-v1 | Priority | Dependencies |
|---|---|---|---|---|---|---|
| CAP-001 | shared ids, enums, config envelopes | claurst, claw-code, hermes-ide | `caduceus-core` | v1 | critical | — |
| CAP-002 | provider abstraction (`ProviderRequest`/`ProviderResponse`) | claurst, claw-code | `caduceus-api::providers` | v1 | critical | CAP-001 |
| CAP-003 | model registry with context window + max output metadata | claurst, claurst-blackbox | `caduceus-api::models` | v1 | critical | CAP-002 |
| CAP-004 | system prompt style negotiation per provider | claurst | `caduceus-api::providers` | v1 | critical | CAP-002 |
| CAP-005 | normalized stream event model | claurst, open-multi-agent | `caduceus-api::streaming` | v1 | critical | CAP-002 |
| CAP-006 | content-block transcript model | claurst, claw-code, open-multi-agent | `caduceus-core::transcript` | v1 | critical | CAP-001 |
| CAP-007 | multi-turn conversation loop | claw-code, claurst | `caduceus-agent::runtime` | v1 | critical | CAP-005, CAP-006, CAP-015 |
| CAP-008 | structured output retry loop | open-multi-agent, claw-code | `caduceus-agent::runtime` | v1 | important | CAP-007 |
| CAP-009 | loop detection for repeated agent/tool patterns | open-multi-agent, claw-code | `caduceus-agent::runtime` | v1 | important | CAP-007 |
| CAP-010 | JSONL transcript persistence + replay | claw-code, claurst | `caduceus-session::jsonl` | v1 | important | CAP-006, CAP-033 |
| CAP-011 | SQLite-backed session/message persistence | claurst, hermes-ide | `caduceus-db` | v1 | critical | CAP-033 |
| CAP-012 | session fork/sidechain cloning with fresh UUIDs | claurst-blackbox, claurst | `caduceus-session::forking` | v1 | important | CAP-010, CAP-011 |
| CAP-013 | cloud/bridge session event model | claurst | `caduceus-sync::bridge` | post-v1 | important | CAP-011, CAP-050 |
| CAP-014 | ACP/JSON-RPC session + model listing | claurst | `caduceus-remote::acp` | post-v1 | important | CAP-003, CAP-011 |
| CAP-015 | tool registry, schema validation, executor | claw-code, claurst, open-multi-agent | `caduceus-tools` | v1 | critical | CAP-001 |
| CAP-016 | built-in tools (bash/read/edit/write/glob/grep/web/LSP/apply-patch) | claw-code, claurst | `caduceus-tools::builtin` | v1 | critical | CAP-015 |
| CAP-017 | tool result rendering + progress surfacing | claw-code, claurst | `caduceus-ui::tooling` | v1 | important | CAP-015, CAP-039 |

### 5.2 Context, memory, and cost capabilities

| ID | Capability | Source repo | Caduceus crate | v1/post-v1 | Priority | Dependencies |
|---|---|---|---|---|---|---|
| CAP-018 | token budget tracking | claurst, claw-code | `caduceus-context::budget` | v1 | critical | CAP-003, CAP-006 |
| CAP-019 | context overflow detection across providers | claurst-blackbox, claurst | `caduceus-context::budget` | v1 | critical | CAP-018 |
| CAP-020 | full compact (`/compact`) | claurst-blackbox | `caduceus-context::compaction` | v1 | critical | CAP-018 |
| CAP-021 | session-memory compact preserving tool boundaries | claurst, claw-code | `caduceus-context::compaction` | v1 | critical | CAP-006, CAP-018 |
| CAP-022 | cached microcompact / time-based microcompact | claurst | `caduceus-context::compaction` | post-v1 | important | CAP-021 |
| CAP-023 | filesystem instruction memory (`CLAUDE.md` / `AGENTS.md`) | claurst, claurst-blackbox | `caduceus-context::instruction_memory` | v1 | important | CAP-001 |
| CAP-024 | SQLite/project/session/global memories | hermes-ide, hermes-ide-supplement | `caduceus-context::memory_store` | v1 | important | CAP-033 |
| CAP-025 | context assembly / attunement with pins, memory, errors, conventions | hermes-ide, hermes-ide-supplement | `caduceus-context::assembly` | v1 | critical | CAP-018, CAP-023, CAP-024, CAP-036 |
| CAP-026 | context lifecycle state (`clean/dirty/applying/apply_failed`) | hermes-ide | `caduceus-context::assembly` | v1 | important | CAP-025 |
| CAP-027 | spending cap / max budget USD | claurst-blackbox | `caduceus-audit::costs` | v1 | important | CAP-018, CAP-051 |
| CAP-028 | token usage, snapshots, daily rollups | hermes-ide | `caduceus-audit::costs` | v1 | important | CAP-033 |
| CAP-029 | usage bridging on outbound events | open-multi-agent, claurst | `caduceus-sync::bridge` | post-v1 | important | CAP-013, CAP-028 |
| CAP-030 | durable session tracer (memory + JSONL sinks) | claw-code | `caduceus-audit::trace` | v1 | important | CAP-033 |
| CAP-031 | hook event system (tool/session/task/config/file events) | claw-code, claurst-blackbox | `caduceus-permissions::hooks` | v1 | critical | CAP-015 |
| CAP-032 | permission modes + rule engine + approval mediation | claw-code, claurst | `caduceus-permissions` | v1 | critical | CAP-015 |

### 5.3 Hermes IDE / presentation capabilities

| ID | Capability | Source repo | Caduceus crate | v1/post-v1 | Priority | Dependencies |
|---|---|---|---|---|---|---|
| CAP-033 | SQLite migrations / local durable state | hermes-ide, hermes-ide-supplement | `caduceus-db` | v1 | critical | CAP-001 |
| CAP-034 | Tauri shell + IPC command surface | hermes-ide | `caduceus-app` | v1 | critical | CAP-001, CAP-033 |
| CAP-035 | session manager with Hermes lifecycle phases | hermes-ide | `caduceus-session` | v1 | critical | CAP-033, CAP-034, CAP-041 |
| CAP-036 | workspace/project detection and cartography | hermes-ide, hermes-ide-supplement | `caduceus-scan` | v1 | critical | CAP-033 |
| CAP-037 | terminal transcript analyzer (provider, tokens, tools, memory facts) | hermes-ide | `caduceus-terminal::analysis` | v1 | important | CAP-003, CAP-035, CAP-041 |
| CAP-038 | suggestion engine / command prediction | hermes-ide | `caduceus-terminal::suggestions` | v1 | important | CAP-037, CAP-011 |
| CAP-039 | React state tree / prompt composer / command palette | hermes-ide | `caduceus-ui` | v1 | important | CAP-034, CAP-035, CAP-025 |
| CAP-040 | session restore / crash recovery | hermes-ide | `caduceus-session` | v1 | important | CAP-033, CAP-035 |
| CAP-041 | PTY session state machine + silence detection | hermes-ide, hermes-ide-supplement, E2B | `caduceus-pty` | v1 | critical | CAP-047 |
| CAP-042 | Git integration (status/diff/stage/commit/branch) | hermes-ide | `caduceus-git` | v1 | critical | CAP-033, CAP-036 |
| CAP-043 | worktree management | hermes-ide | `caduceus-git::worktrees` | v1 | important | CAP-042 |
| CAP-044 | SSH host store + remote terminal sessions | hermes-ide | `caduceus-remote::ssh` | post-v1 | important | CAP-033, CAP-041 |
| CAP-045 | diff viewer / file editor / split panes | hermes-ide | `caduceus-ui::workspace` | v1 | important | CAP-039 |
| CAP-046 | desktop notifications on busy/idle transitions | hermes-ide | `caduceus-ui::notifications` | v1 | nice-to-have | CAP-035 |

### 5.4 Sandbox / execution capabilities

| ID | Capability | Source repo | Caduceus crate | v1/post-v1 | Priority | Dependencies |
|---|---|---|---|---|---|---|
| CAP-047 | E2B sandbox lifecycle (create/connect/kill/health) | E2B | `caduceus-runtime::sandbox` | v1 | critical | CAP-001 |
| CAP-048 | command execution with timeout/background handles | E2B | `caduceus-runtime::process` | v1 | critical | CAP-047 |
| CAP-049 | filesystem CRUD, metadata, watch | E2B | `caduceus-runtime::fs` | v1 | critical | CAP-047 |
| CAP-050 | network/ports/public traffic controls | E2B | `caduceus-runtime::network` | v1 | important | CAP-047 |
| CAP-051 | secure tokens / envd access enforcement | E2B | `caduceus-runtime::auth` | v1 | critical | CAP-047 |
| CAP-052 | snapshots, volumes, templates | E2B | `caduceus-runtime::images` | post-v1 | important | CAP-047, CAP-049 |
| CAP-053 | sandbox-local git helpers | E2B | `caduceus-runtime::git` | post-v1 | nice-to-have | CAP-047, CAP-049 |
| CAP-054 | sandbox MCP gateway | E2B, claurst, claw-code | `caduceus-mcp::gateway` | post-v1 | important | CAP-047, CAP-015 |

### 5.5 Code intelligence / semantic retrieval capabilities

| ID | Capability | Source repo | Caduceus crate | v1/post-v1 | Priority | Dependencies |
|---|---|---|---|---|---|---|
| CAP-055 | tree-sitter parser lifecycle + cancellation reset discipline | tree-sitter | `caduceus-codeintel::treesitter` | v1 | important | CAP-036 |
| CAP-056 | query/tag/highlight/lookahead support | tree-sitter | `caduceus-codeintel::queries` | v1 | important | CAP-055 |
| CAP-057 | semantic chunk extraction by function/class/method granularity | tree-sitter | `caduceus-index::chunking` | v1 | important | CAP-055, CAP-056 |
| CAP-058 | incremental reindex from changed ranges | tree-sitter | `caduceus-index::pipeline` | v1 | important | CAP-057 |
| CAP-059 | embedded Qdrant EdgeShard storage | qdrant | `caduceus-index::qdrant` | v1 | critical | CAP-033 |
| CAP-060 | payload indexes / metadata filters | qdrant | `caduceus-index::schema` | v1 | critical | CAP-057, CAP-059 |
| CAP-061 | semantic query/ranking/filter DSL | qdrant | `caduceus-search::semantic` | v1 | important | CAP-059, CAP-060 |
| CAP-062 | optimize/flush/load lifecycle for persistent vector DB | qdrant | `caduceus-index::qdrant` | v1 | important | CAP-059 |
| CAP-063 | parser-error-aware indexing/down-ranking | tree-sitter, qdrant | `caduceus-index::pipeline` | v1 | important | CAP-058, CAP-060 |
| CAP-064 | LSP bridge for hover/defs/refs/diagnostics | claw-code, hermes-ide | `caduceus-codeintel::lsp` | post-v1 | important | CAP-015, CAP-055 |

### 5.6 Multiplayer / CRDT capabilities

| ID | Capability | Source repo | Caduceus crate | v1/post-v1 | Priority | Dependencies |
|---|---|---|---|---|---|---|
| CAP-065 | Lamport clocks + version vectors | zed-crdt | `caduceus-collab::clock` | post-v1 | important | CAP-001 |
| CAP-066 | rope + sum-tree text primitives | zed-crdt | `caduceus-collab::text` | post-v1 | important | CAP-065 |
| CAP-067 | CRDT fragment buffer with tombstones | zed-crdt | `caduceus-collab::buffer` | post-v1 | important | CAP-065, CAP-066 |
| CAP-068 | undo model with per-edit undo counts | zed-crdt | `caduceus-collab::undo` | post-v1 | important | CAP-067 |
| CAP-069 | stable anchors for cursors, selections, diagnostics | zed-crdt | `caduceus-presence` | post-v1 | important | CAP-067 |
| CAP-070 | collaboration sync protocol / deferred-op replay | zed-crdt | `caduceus-sync::collab` | post-v1 | important | CAP-065, CAP-067 |
| CAP-071 | remote selections / AI cursors | zed-crdt | `caduceus-presence` | post-v1 | nice-to-have | CAP-069, CAP-070 |

### 5.7 Multi-agent / orchestration capabilities

| ID | Capability | Source repo | Caduceus crate | v1/post-v1 | Priority | Dependencies |
|---|---|---|---|---|---|---|
| CAP-072 | task queue with dependency-aware status transitions | open-multi-agent | `caduceus-orchestrator::queue` | post-v1 | important | CAP-007, CAP-011 |
| CAP-073 | agent pool + layered concurrency controls | open-multi-agent | `caduceus-orchestrator::scheduler` | post-v1 | important | CAP-072 |
| CAP-074 | coordinator decomposition + fallback path | open-multi-agent | `caduceus-orchestrator::planner` | post-v1 | important | CAP-072, CAP-073 |
| CAP-075 | team runs / role-based orchestration | open-multi-agent | `caduceus-orchestrator::teams` | post-v1 | important | CAP-072, CAP-073 |
| CAP-076 | shared memory namespaces with explicit prompt injection | open-multi-agent | `caduceus-orchestrator::memory` | post-v1 | important | CAP-024, CAP-072 |
| CAP-077 | task + team trace spans | open-multi-agent | `caduceus-audit::trace` | post-v1 | important | CAP-030, CAP-072 |
| CAP-078 | named agent definition loader | claurst, claurst-blackbox | `caduceus-agent::definitions` | post-v1 | important | CAP-023, CAP-033 |

### 5.8 Plugin / MCP / extensibility capabilities

| ID | Capability | Source repo | Caduceus crate | v1/post-v1 | Priority | Dependencies |
|---|---|---|---|---|---|---|
| CAP-079 | MCP server config discovery + env expansion | claurst | `caduceus-mcp::config` | v1 | important | CAP-001 |
| CAP-080 | plugin manifest parsing (JSON/TOML) | claurst-blackbox, hermes-ide | `caduceus-plugin::manifest` | post-v1 | important | CAP-033 |
| CAP-081 | plugin install/enable/disable/update | hermes-ide | `caduceus-plugin::manager` | post-v1 | important | CAP-080 |
| CAP-082 | plugin capability grants / sandboxed permissions | hermes-ide, claurst-blackbox | `caduceus-plugin::security` | post-v1 | important | CAP-032, CAP-080 |
| CAP-083 | plugin storage / settings / panels / commands | hermes-ide, claurst-blackbox | `caduceus-plugin::runtime` | post-v1 | important | CAP-081, CAP-082 |
| CAP-084 | skill discovery with precedence and duplicate warnings | claurst | `caduceus-agent::skills` | post-v1 | nice-to-have | CAP-023, CAP-033 |

### 5.9 CLI / UX differentiators / miscellaneous capabilities

| ID | Capability | Source repo | Caduceus crate | v1/post-v1 | Priority | Dependencies |
|---|---|---|---|---|---|---|
| CAP-085 | headless CLI mode + slash command plumbing | claw-code, claurst-blackbox, hermes-ide | `caduceus-cli` | v1 | important | CAP-007, CAP-039 |
| CAP-086 | provider/model picker and `/connect` UX | claurst-blackbox, hermes-ide | `caduceus-ui::providers` | v1 | important | CAP-003, CAP-039 |
| CAP-087 | provider heuristic detection from PTY output | hermes-ide | `caduceus-terminal::analysis` | v1 | nice-to-have | CAP-037 |
| CAP-088 | buddy/pet gamification layer | claurst-blackbox | `caduceus-companion` | post-v1 | nice-to-have | CAP-039 |
| CAP-089 | remote bridge UI / status overlays | claurst | `caduceus-ui::bridge` | post-v1 | nice-to-have | CAP-013, CAP-039 |
| CAP-090 | auth store for API keys/OAuth/MCP tokens | claurst, claurst-blackbox | `caduceus-auth` | v1 | important | CAP-001 |
| CAP-091 | model/provider cost estimation tables | hermes-ide, claurst | `caduceus-audit::pricing` | v1 | important | CAP-028, CAP-090 |
| CAP-092 | security/audit hardening with secret scrubbing | E2B, claw-code | `caduceus-audit::security` | v1 | important | CAP-030, CAP-051 |

---

## Part 6: Implementation Order

> Ordered as a dependency-respecting linearization. Items can begin as soon as their listed dependencies are done.

| ID | What to build | Dependencies | Acceptance criteria | Complexity |
|---|---|---|---|---|
| IMP-001 | `caduceus-core`: shared ids, enums, error model, config envelopes | — | Core crate compiles; shared types are consumed by at least two downstream crates without duplication | M |
| IMP-002 | `caduceus-db`: migration runner, WAL/foreign-key setup, baseline schema (`sessions`, `messages`, `tool_calls`, `costs`, `projects`, `memories`, `commands`, `errors`, `contexts`) | IMP-001 | Fresh DB boots cleanly; migrations are idempotent; schema round-trips with smoke inserts | M |
| IMP-003 | `caduceus-auth`: credential store for API keys, bearer, OAuth, MCP tokens | IMP-001, IMP-002 | Provider credentials can be stored/retrieved securely; expiry metadata preserved | M |
| IMP-004 | `caduceus-api::providers`: provider trait, request/response model, system-prompt-style negotiation | IMP-001, IMP-003 | Anthropic + one OpenAI-compatible provider stream through a common API without provider-specific branching in callers | L |
| IMP-005 | `caduceus-api::models`: model registry, context-window metadata, pricing metadata | IMP-001, IMP-004 | Registry resolves `provider/model` and bare names; context window + max output are available programmatically | M |
| IMP-006 | `caduceus-tools`: registry, schema validation, builtin tool contracts | IMP-001 | Tools register with typed input/output; invalid tool inputs fail before execution | M |
| IMP-007 | `caduceus-permissions`: permission modes, rule engine, approval contract | IMP-001, IMP-006 | `allow/deny/ask` decisions work per tool and per pattern; denial blocks execution deterministically | M |
| IMP-008 | `caduceus-permissions::hooks`: hook runner and event taxonomy | IMP-006, IMP-007 | Pre/post tool hooks fire; exit code `2` blocks; hook failure is logged without crashing runtime | M |
| IMP-009 | `caduceus-session`: SQLite session/message persistence + JSONL export/import bridge | IMP-002 | Sessions/messages persist in SQLite; export/import reproduces the same ordered transcript | M |
| IMP-010 | `caduceus-context::instruction_memory`: `CLAUDE.md` / `AGENTS.md` hierarchy loader | IMP-001 | Hierarchical memory resolution works across user/system/workspace scopes with deterministic precedence | M |
| IMP-011 | `caduceus-context::budget`: token budget model, warning/critical thresholds, overflow detection | IMP-005, IMP-009 | Runtime computes used/remaining/fill; overflow conditions are detected before provider hard failure | M |
| IMP-012 | `caduceus-context::memory_store`: SQLite memories + access counters + TTL handling | IMP-002 | Session/project/global memories are persisted and queried by scope | S |
| IMP-013 | `caduceus-agent::runtime`: content-block transcript handling, turn loop, tool-call round-tripping | IMP-004, IMP-005, IMP-006, IMP-007, IMP-009, IMP-011 | Runtime completes a multi-turn prompt that invokes a tool and resumes with a valid transcript | L |
| IMP-014 | `caduceus-context::compaction`: full compact + safe session-memory compact | IMP-009, IMP-011, IMP-013 | Compaction reduces token load while preserving valid tool-use/tool-result boundaries | L |
| IMP-015 | `caduceus-audit::trace` + `caduceus-audit::costs`: usage events, token snapshots, daily rollups, secret scrubbing | IMP-002, IMP-004, IMP-006, IMP-013 | Model/tool events emit durable traces; token/cost rollups populate correctly; secrets are not logged | M |
| IMP-016 | `caduceus-runtime::sandbox`: E2B sandbox lifecycle + secure token handling | IMP-001, IMP-003 | Create/connect/kill/health succeed against E2B; secure token is required on follow-up calls | L |
| IMP-017 | `caduceus-runtime::process` + `caduceus-runtime::fs`: command execution, background jobs, file CRUD, watch | IMP-016 | Shell commands, background handles, file read/write/list/watch all work in sandbox mode | L |
| IMP-018 | `caduceus-pty`: PTY create/send/resize/kill + silence detection | IMP-016, IMP-017 | PTY streams correctly, resizes, and transitions to idle after silence timeout | M |
| IMP-019 | `caduceus-app`: Tauri shell + IPC registration | IMP-001, IMP-002 | App boots; core IPC endpoints are invokable from frontend | M |
| IMP-020 | `caduceus-session`: Hermes session manager and lifecycle transitions | IMP-009, IMP-018, IMP-019 | Sessions move through Hermes states correctly for local and PTY-backed flows | M |
| IMP-021 | `caduceus-scan`: workspace/project cartography, framework/language/convention detection | IMP-002, IMP-020 | Scanner identifies repo roots, languages, frameworks, and stores normalized project metadata | M |
| IMP-022 | `caduceus-context::assembly`: attunement, pins, contexts, known errors, trimming | IMP-011, IMP-012, IMP-020, IMP-021 | Assembled context artifact is persisted in `contexts`; trimming obeys project-context budget | L |
| IMP-023 | `caduceus-git`: repo status, diff, stage, commit, branch, worktrees | IMP-002, IMP-021 | Git status/diff/stage/commit and worktree create/remove are functional from one interface | L |
| IMP-024 | `caduceus-ui`: React state tree, prompt composer, session panes, palette, diff/editor shell | IMP-019, IMP-020, IMP-022 | User can create a session, view transcript, send a prompt, and inspect context state in UI | L |
| IMP-025 | `caduceus-terminal::analysis` + suggestions: parse PTY output for provider/tokens/tools/memory facts and suggest next commands | IMP-015, IMP-018, IMP-020, IMP-021 | Terminal analyzer extracts structured metrics from PTY output and suggestions are ranked from history + context | M |
| IMP-026 | `caduceus-codeintel::treesitter` + queries: parser lifecycle, cancellation reset, semantic capture queries | IMP-021 | Parser handles incremental edits safely; chunk capture queries work on core languages | M |
| IMP-027 | `caduceus-index::chunking` + pipeline: semantic chunk extraction and changed-range reindexing | IMP-026 | Editing a file only reindexes changed semantic chunks; untouched chunk ids remain stable | M |
| IMP-028 | `caduceus-index::qdrant` + schema: embedded EdgeShard, payload indexes, optimize/flush/load lifecycle | IMP-002, IMP-027 | Collection persists across restart, payload indexes are created, and semantic chunks can be upserted/query-filtered | L |
| IMP-029 | `caduceus-search::semantic`: embedding bridge, semantic query API, metadata filtering | IMP-004, IMP-005, IMP-028 | Semantic search returns ranked code chunks filtered by repo/path/language/symbol metadata | M |
| IMP-030 | `caduceus-cli`: headless mode, slash commands, `/connect`, `/compact`, budget display | IMP-013, IMP-024 | CLI can start a session, switch provider/model, compact context, and display budget state | M |
| IMP-031 | `caduceus-audit::pricing`: model/provider pricing table and budget enforcement | IMP-005, IMP-015 | Estimated costs are attached per call and budget caps halt inference when exceeded | S |
| IMP-032 | `caduceus-auth` + UI wiring: provider auth flows and token refresh UX | IMP-003, IMP-019, IMP-024 | User can connect a provider, store credentials, and recover expired tokens without manual file edits | M |
| IMP-033 | `caduceus-mcp::config`: MCP server config discovery, env substitution, runtime exposure | IMP-001, IMP-024 | MCP configs load from files/settings, `${VAR}` expansion works, and configs are visible to the runtime | M |
| IMP-034 | `caduceus-plugin::manifest` + manager baseline | IMP-002, IMP-019, IMP-024, IMP-033 | Plugin manifests parse and enabled/disabled state persists, even if plugin runtime is feature-flagged | L |
| IMP-035 | `caduceus-collab::clock` + `caduceus-collab::text`: Lamport/global clocks, rope, sum-tree | IMP-001 | Clock and rope primitives pass deterministic convergence/unit tests | M |
| IMP-036 | `caduceus-collab::buffer` + undo + anchors | IMP-035 | Local/remote edits converge; undo toggles visibility; anchors survive adjacent inserts/deletes | L |
| IMP-037 | `caduceus-sync::collab` + presence | IMP-015, IMP-036 | Delayed/out-of-order operations converge after replay; remote selections and AI cursors render stably | L |
| IMP-038 | `caduceus-orchestrator::queue`: dependency-aware task graph, statuses, unblocking | IMP-013, IMP-015 | Task DAG runs with pending/running/completed/failed/blocked semantics and correct dependency release | L |
| IMP-039 | `caduceus-orchestrator::scheduler` + planner: concurrency control, decomposition, loop detection | IMP-038 | Multiple agents run with bounded concurrency; simple-goal fallback works when decomposition fails | XL |
| IMP-040 | `caduceus-orchestrator::teams` + shared memory injection + trace spans | IMP-012, IMP-015, IMP-038, IMP-039 | Team/task runs share explicit memory summaries, produce traces, and respect lifecycle constraints | XL |
| IMP-041 | `caduceus-agent::definitions` + skills discovery | IMP-010, IMP-024, IMP-034 | Named agent definitions and skills are discovered with precedence/duplicate warnings | M |
| IMP-042 | `caduceus-remote::ssh` + ACP/bridge integration | IMP-014, IMP-018, IMP-019, IMP-020, IMP-032 | Remote sessions, ACP listing, and bridge state updates work without breaking local sessions | L |
| IMP-043 | `caduceus-runtime::images` + sandbox extras (snapshots, volumes, templates, MCP gateway) | IMP-016, IMP-017, IMP-033 | Snapshot/restore and template bootstrap succeed in feature-flagged sandbox mode | L |
| IMP-044 | `caduceus-codeintel::lsp`: diagnostics/hover/defs/refs bridge | IMP-006, IMP-026, IMP-024 | LSP-backed diagnostics and symbol navigation are surfaced in tools/UI | M |
| IMP-045 | `caduceus-plugin::runtime`: capability grants, panels, commands, storage | IMP-034, IMP-024, IMP-032 | A sample plugin can add a command/panel with persisted plugin storage under declared permissions | L |
| IMP-046 | UX differentiators: provider picker polish, bridge overlays, buddy system, notifications | IMP-024, IMP-025, IMP-032, IMP-042 | Non-core UX features are shippable behind feature flags without destabilizing core runtime | M |

### 6.1 Parallel work lanes after the foundation
- After **IMP-002**: IMP-003, IMP-006, IMP-009, IMP-019 can proceed in parallel.
- After **IMP-016**: IMP-017 and IMP-018 split execution and terminal work.
- After **IMP-021**: IMP-022, IMP-023, and IMP-026 can proceed independently.
- After **IMP-035**: IMP-036 can advance in parallel with IMP-038/IMP-034 if resources allow.

### 6.2 Practical v1 cut line
**Ship v1 after IMP-033.**

Post-v1 stack begins with:
- CRDT collaboration: **IMP-035–IMP-037**
- multi-agent orchestration: **IMP-038–IMP-040**
- agent-definition/plugins/LSP/remote extras: **IMP-041–IMP-046**
