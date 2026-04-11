# Deep Research: Telemetry Infrastructure for Caduceus

## Executive Summary

All major AI coding agents (Claude Code, GitHub Copilot CLI, Gemini CLI, Cline, Cursor) have converged on **OpenTelemetry (OTLP)** as the standard telemetry protocol. The best telemetry infrastructure for Caduceus should track 5 categories of data: **token usage**, **cost**, **session activity**, **tool execution**, and **developer productivity**. All should be opt-in, privacy-preserving (no code/prompts by default), exportable via OTLP, and locally stored in SQLite with optional remote export.

## Confidence Assessment

- **High**: Metric names and data structures from Claude Code (documented OTLP setup), Gemini CLI (open source), Cline (open source)
- **Medium**: Cursor metrics (closed source, inferred from docs/dashboards)
- **Medium**: Copilot CLI metrics (inferred from API docs and changelog posts)

---

## Tool-by-Tool Telemetry Comparison

### Metric Categories by Tool

| Category | Claude Code | Copilot CLI | Gemini CLI | Cline | Cursor |
|----------|:-----------:|:-----------:|:----------:|:-----:|:------:|
| Token usage (in/out/cache) | ✅ | ✅ | ✅ | ✅ | ✅ |
| Cost (USD) | ✅ | ✅ | ✅ | ✅ | ✅ |
| Session count/duration | ✅ | ✅ | ✅ | ✅ | ✅ |
| Active time | ✅ | ✅ | ✅ | ❌ | ✅ |
| Tool call tracking | ✅ | ❌ | ✅ | ✅ | ✅ |
| Commits/PRs created | ✅ | ❌ | ❌ | ❌ | ✅ |
| Lines of code changed | ✅ | ❌ | ✅ | ❌ | ✅ |
| Error/failure events | ✅ | ❌ | ✅ | ✅ | ✅ |
| Model breakdown | ✅ | ✅ | ✅ | ✅ | ✅ |
| Per-user attribution | ✅ | ✅ | ✅ | ✅ | ✅ |
| Context window usage | ❌ | ✅ | ✅ | ❌ | ✅ |
| Permission audit trail | ✅ | ❌ | ❌ | ✅ | ❌ |
| OpenTelemetry export | ✅ | ❌ (API) | ✅ | ✅ | ❌ (CSV) |
| Local-first storage | ✅ | ✅ | ✅ | ✅ | ✅ |
| Privacy controls | ✅ | ✅ | ✅ | ✅ | ✅ |

### Protocol/Export Comparison

| Tool | Protocol | Export Format | Default State | Local Storage |
|------|----------|--------------|---------------|---------------|
| Claude Code | OpenTelemetry (OTLP) | Metrics + Logs + Traces | Opt-in (`CLAUDE_CODE_ENABLE_TELEMETRY=1`) | `~/.claude/projects/` |
| Copilot CLI | REST API (NDJSON) | NDJSON files via API | Opt-in (org setting) | `~/.copilot/session-state/` |
| Gemini CLI | OpenTelemetry (OTLP) | OTLP + Clearcut (Google internal) | Opt-in (`.gemini/settings.json`) | Local logs |
| Cline | OpenTelemetry (OTLP) + Portkey | Anonymous metrics | Opt-in | VS Code extension storage |
| Cursor | Proprietary dashboard | CSV export + Chrome extension | Always-on (for billing) | Cloud dashboard |

---

## Recommended Caduceus Telemetry Architecture

### Design Principles (derived from all 5 tools)

1. **Opt-in by default** — no telemetry unless user enables it
2. **Local-first** — all data stored in SQLite, never leaves machine without consent
3. **Privacy-preserving** — no code, prompts, or file contents in telemetry (unless explicitly enabled)
4. **OpenTelemetry compatible** — export via OTLP to any backend (Grafana, Datadog, etc.)
5. **Granular controls** — enable/disable per category, per session, per user

### Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                CADUCEUS TELEMETRY SYSTEM                      │
│                                                               │
│  ┌─────────────┐  ┌──────────────┐  ┌────────────────────┐  │
│  │ Metric       │  │ Event        │  │ Trace              │  │
│  │ Collectors   │  │ Logger       │  │ Spans              │  │
│  │              │  │              │  │                    │  │
│  │ Tokens       │  │ ToolCall     │  │ LLM Request        │  │
│  │ Cost         │  │ Permission   │  │ Tool Execution     │  │
│  │ Session      │  │ Error        │  │ Context Assembly   │  │
│  │ Productivity │  │ FileChange   │  │ Session Lifecycle  │  │
│  └──────┬──────┘  └──────┬───────┘  └────────┬───────────┘  │
│         │                │                    │              │
│         ▼                ▼                    ▼              │
│  ┌──────────────────────────────────────────────────────┐    │
│  │              TelemetryStore (SQLite)                  │    │
│  │  metrics table │ events table │ spans table           │    │
│  │  (local, append-only, privacy-filtered)               │    │
│  └──────────────────────┬───────────────────────────────┘    │
│                         │                                    │
│              ┌──────────▼──────────┐                         │
│              │  Export Pipeline     │                         │
│              │  (opt-in)           │                         │
│              │                     │                         │
│              │  ┌───────────────┐  │                         │
│              │  │ OTLP Exporter │  │  → Grafana/Prometheus   │
│              │  │ JSON Exporter │  │  → File/NDJSON          │
│              │  │ CSV Exporter  │  │  → Spreadsheet          │
│              │  │ Dashboard API │  │  → React UI             │
│              │  └───────────────┘  │                         │
│              └─────────────────────┘                         │
└──────────────────────────────────────────────────────────────┘
```

### Metric Schema (Unified from all 5 tools)

```rust
/// Core telemetry types for Caduceus
pub struct TelemetryConfig {
    pub enabled: bool,                     // Master switch (default: false)
    pub log_prompts: bool,                 // Log user prompts (default: false)
    pub log_tool_args: bool,               // Log tool arguments (default: false)
    pub export_otlp: Option<OtlpConfig>,   // OpenTelemetry export
    pub export_file: Option<PathBuf>,      // NDJSON file export
    pub metrics_interval_secs: u64,        // Export interval (default: 60)
    pub privacy_mode: PrivacyMode,         // Strict, Standard, Verbose
}

pub enum PrivacyMode {
    Strict,    // No prompts, no args, no file paths — only counts/costs
    Standard,  // Tool names + redacted args, no prompts
    Verbose,   // Full prompts + args (for debugging, never in prod)
}

/// Token usage metrics (from Claude Code's model)
pub struct TokenMetric {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub token_type: TokenType,    // Input, Output, CacheRead, CacheWrite
    pub count: u32,
}

pub enum TokenType { Input, Output, CacheRead, CacheWrite }

/// Cost tracking (from all tools)
pub struct CostMetric {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,            // Calculated from provider pricing
    pub cumulative_session_usd: f64,
}

/// Session metrics (from Claude Code + Copilot)
pub struct SessionMetric {
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub active_time_secs: u64,    // Time actually interacting
    pub turn_count: u32,
    pub tool_call_count: u32,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub total_cost_usd: f64,
    pub provider_id: String,
    pub model_id: String,
}

/// Tool execution events (from Claude Code + Cline)
pub struct ToolEvent {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub event_type: ToolEventType,
    pub tool_name: String,
    pub duration_ms: u64,
    pub success: bool,
    pub error_message: Option<String>,
    pub args_redacted: Option<String>,   // Only in Standard/Verbose mode
}

pub enum ToolEventType { Started, Completed, Failed, PermissionDenied }

/// Developer productivity metrics (from Claude Code + Cursor)
pub struct ProductivityMetric {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub metric_type: ProductivityType,
    pub count: i64,
}

pub enum ProductivityType {
    CommitCreated,
    PullRequestCreated,
    LinesAdded,
    LinesRemoved,
    FilesModified,
    TestsGenerated,
    TestsPassed,
    TestsFailed,
}

/// Permission audit trail (from Claude Code + Cline)
pub struct AuditEvent {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub capability: String,       // fs.read, process.exec, etc.
    pub resource: String,         // Redacted path or command
    pub decision: AuditDecision,  // Allowed, Denied, UserApproved, UserDenied
    pub decision_source: String,  // config, hook, manual, bypass
}

/// Context window tracking (from Copilot CLI + Gemini CLI)
pub struct ContextMetric {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub context_window_size: u32,
    pub tokens_used: u32,
    pub fill_percentage: f64,
    pub warning_level: Option<String>,  // 70%, 85%, 95%
}
```

### SQLite Tables

```sql
CREATE TABLE telemetry_tokens (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    session_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    token_type TEXT NOT NULL,  -- input, output, cache_read, cache_write
    count INTEGER NOT NULL
);

CREATE TABLE telemetry_costs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    session_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cost_usd REAL NOT NULL,
    cumulative_usd REAL NOT NULL
);

CREATE TABLE telemetry_sessions (
    session_id TEXT PRIMARY KEY,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    active_time_secs INTEGER DEFAULT 0,
    turn_count INTEGER DEFAULT 0,
    tool_call_count INTEGER DEFAULT 0,
    total_tokens INTEGER DEFAULT 0,
    total_cost_usd REAL DEFAULT 0.0,
    provider_id TEXT,
    model_id TEXT
);

CREATE TABLE telemetry_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    session_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    tool_name TEXT,
    duration_ms INTEGER,
    success INTEGER,
    error_message TEXT,
    metadata TEXT  -- JSON, privacy-filtered
);

CREATE TABLE telemetry_productivity (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    session_id TEXT NOT NULL,
    metric_type TEXT NOT NULL,
    count INTEGER NOT NULL
);

CREATE TABLE telemetry_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    session_id TEXT NOT NULL,
    capability TEXT NOT NULL,
    resource TEXT NOT NULL,
    decision TEXT NOT NULL,
    decision_source TEXT NOT NULL
);

CREATE TABLE telemetry_context (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    session_id TEXT NOT NULL,
    window_size INTEGER,
    tokens_used INTEGER,
    fill_pct REAL,
    warning_level TEXT
);
```

### Export Pipeline

#### OpenTelemetry (OTLP) Export
```
Caduceus → OTLP Collector → Prometheus/Grafana
                           → Sumo Logic
                           → Datadog
                           → Google Cloud Monitoring
```

Environment variables (compatible with Claude Code):
- `CADUCEUS_ENABLE_TELEMETRY=1`
- `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318`
- `OTEL_LOG_USER_PROMPTS=1` (opt-in for debugging)
- `OTEL_METRICS_EXPORT_INTERVAL=60000`

#### NDJSON File Export (compatible with Copilot CLI)
```jsonl
{"type":"token","timestamp":"2026-04-11T06:00:00Z","session":"abc","provider":"anthropic","model":"sonnet","input":1234,"output":567}
{"type":"cost","timestamp":"2026-04-11T06:00:00Z","session":"abc","cost_usd":0.023}
{"type":"tool","timestamp":"2026-04-11T06:00:01Z","session":"abc","tool":"bash","duration_ms":1200,"success":true}
```

### React Dashboard Component

```
┌─────────────────────────────────────────────────────┐
│  📊 Caduceus Telemetry Dashboard                     │
├─────────────────────────────────────────────────────┤
│                                                      │
│  Today: $2.34 spent │ 45,678 tokens │ 12 sessions   │
│  This week: $14.52  │ 312K tokens   │ 67 sessions   │
│                                                      │
│  ┌──────────────────────────────────────────────┐   │
│  │  Token Usage by Model (bar chart)            │   │
│  │  ████████████ Claude Sonnet (68%)            │   │
│  │  ████ GPT-4o (22%)                           │   │
│  │  ██ Ollama (10%)                             │   │
│  └──────────────────────────────────────────────┘   │
│                                                      │
│  ┌──────────────────────────────────────────────┐   │
│  │  Cost Over Time (line chart)                  │   │
│  │  ╱─╲__╱─╲___╱─╲                              │   │
│  │  Mon  Tue  Wed  Thu  Fri                      │   │
│  └──────────────────────────────────────────────┘   │
│                                                      │
│  ┌──────────────────────────────────────────────┐   │
│  │  Tool Usage (pie chart)                       │   │
│  │  bash: 45% │ read_file: 30% │ write: 15%    │   │
│  └──────────────────────────────────────────────┘   │
│                                                      │
│  Budget: $2.34 / $50.00 (4.7%)  [████░░░░░░░░░░]  │
└─────────────────────────────────────────────────────┘
```

---

## Implementation Recommendations

1. **Use SQLite locally** (already have `caduceus-storage`) — add 7 telemetry tables
2. **OpenTelemetry SDK** — use `opentelemetry` + `opentelemetry-otlp` Rust crates
3. **Privacy by default** — no prompts/code in telemetry unless `VERBOSE` mode
4. **Budget enforcement** — hard-stop when `cumulative_usd > max_budget_usd`
5. **Dashboard** — React component reading from SQLite via Tauri IPC
6. **Export** — OTLP for enterprise, NDJSON for personal, CSV for spreadsheets

## Footnotes

[^1]: [Claude Code OpenTelemetry setup](https://github.com/centminmod/claude-code-opentelemetry-setup)
[^2]: [Claude Code + Grafana monitoring](https://claude-blog.setec.rs/blog/claude-code-grafana-monitoring)
[^3]: [Copilot CLI usage metrics changelog](https://github.blog/changelog/2026-03-17-copilot-usage-metrics-now-includes-organization-level-github-copilot-cli-activity/)
[^4]: [Copilot Metrics API (NDJSON)](https://dev.to/devactivity/mastering-github-copilot-metrics-adapting-to-the-new-api-for-enhanced-software-project-tracking-1d0n)
[^5]: [Gemini CLI telemetry architecture](https://deepwiki.com/google-gemini/gemini-cli/5.4-telemetry-and-observability)
[^6]: [Gemini CLI OpenTelemetry docs](https://geminicli.com/docs/cli/telemetry/)
[^7]: [Cline telemetry docs](https://docs.cline.bot/enterprise-solutions/monitoring/telemetry)
[^8]: [Cline + Portkey integration](https://docs.portkey.ai/docs/integrations/libraries/cline)
[^9]: [Cursor dashboard docs](https://cursor.com/docs/account/teams/dashboard)
[^10]: [Cursor token tracker](https://github.com/ofershap/cursor-usage-tracker)
[^11]: [Claude Code per-session cost tracking](https://bindplane.com/blog/claude-code-opentelemetry-per-session-cost-and-token-tracking)
[^12]: [TokenUse platform](https://www.tokenuse.ai/)
