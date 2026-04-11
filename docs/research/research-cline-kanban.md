# Research: Cline Kanban — Design for Caduceus

## Executive Summary

Cline Kanban ([cline/kanban](https://github.com/cline/kanban)) is an open-source (Apache 2.0) kanban board for orchestrating multiple AI coding agents in parallel. Each task card gets its own **git worktree** and **terminal**, enabling agents to work on separate branches without merge conflicts. Cards can be linked with **dependency chains** for autonomous completion, and include **auto-commit/auto-PR** capabilities. For Caduceus, this represents a critical missing capability: **visual multi-agent project management with isolated workspaces**.

## Confidence Assessment

- **High**: Architecture and features from official README + source code on GitHub
- **High**: Source structure from [cline/kanban](https://github.com/cline/kanban) package.json and src/ directory
- **Medium**: Internal implementation details (some modules not fully inspected)

---

## What Cline Kanban Is

Cline Kanban is not just a task board — it's a **replacement for the IDE** when running many agents. Key insight: when you have 10+ agents working simultaneously, a single editor window can't show them all. Kanban provides the bird's-eye view[^1].

### Core Architecture

```
┌─────────────────────────────────────────────────────────┐
│                 CLINE KANBAN                              │
│                                                           │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐│
│  │ Card 1   │  │ Card 2   │  │ Card 3   │  │ Card 4   ││
│  │ ┌──────┐ │  │ ┌──────┐ │  │ ┌──────┐ │  │ ┌──────┐ ││
│  │ │ Term │ │  │ │ Term │ │  │ │ Term │ │  │ │ Term │ ││
│  │ └──────┘ │  │ └──────┘ │  │ └──────┘ │  │ └──────┘ ││
│  │ Worktree │  │ Worktree │  │ Worktree │  │ Worktree ││
│  │ Branch A │  │ Branch B │  │ Branch C │  │ Branch D ││
│  │ ▶ Running│  │ ✓ Done   │  │ ⏸ Blocked│  │ 📋 Todo  ││
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘│
│                                                           │
│  Columns: Backlog → In Progress → Review → Done → Trash  │
│                                                           │
│  ┌─────────────────────────────────────────────────────┐ │
│  │ Sidebar Chat — "Break this feature into 5 tasks"    │ │
│  └─────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────┘
```

### Source Code Structure ([cline/kanban](https://github.com/cline/kanban))

| Module | Purpose |
|--------|---------|
| `src/cli.ts` | CLI entry point, launches web server |
| `src/core/` | Board state, card management, dependency chains |
| `src/workspace/` | Git worktree creation/management, symlinks for node_modules |
| `src/terminal/` | Per-card terminal (node-pty + xterm headless) |
| `src/server/` | Local web server serving the board UI |
| `src/trpc/` | tRPC API for React↔server communication |
| `src/state/` | Persistent board state (JSON) |
| `src/prompts/` | Prompt injection for board management |
| `src/security/` | Permission bypass for autonomous mode |
| `src/config/` | Settings, agent detection |
| `src/cline-sdk/` | SDK for Cline agent integration |
| `src/projects/` | Multi-project support |
| `src/telemetry/` | Usage analytics |
| `src/fs/` | File system utilities |
| `src/update/` | Auto-update mechanism |
| `web-ui/` | React + Vite frontend |

### Key Dependencies[^2]

- `node-pty` — Native PTY for per-card terminals
- `@xterm/headless` — Headless terminal emulation
- `ws` — WebSocket for terminal streaming
- `@trpc/server` + `@trpc/client` — Type-safe API
- `@modelcontextprotocol/sdk` — MCP integration
- `@clinebot/agents` + `@clinebot/core` — Cline agent SDK
- `proper-lockfile` — Safe concurrent file access
- `tree-kill` — Process tree cleanup

---

## Key Features to Adopt for Caduceus

### 1. Git Worktree Isolation ⭐⭐⭐

**What**: Each task card gets its own git worktree (a separate checkout of the same repo on a different branch). Agents work in parallel without merge conflicts[^1].

**How it works**:
```
main repo: ~/project/
├── .git/                    (shared git database)
├── worktree-task-1/         (branch: feature/auth)
├── worktree-task-2/         (branch: feature/api)
├── worktree-task-3/         (branch: fix/bug-123)
```

**Symlink optimization**: Instead of running `npm install` in each worktree, gitignored directories like `node_modules` are symlinked from the main repo[^1].

**For Caduceus**: Implement `WorktreeManager` in `caduceus-git`:
```rust
pub struct WorktreeManager {
    repo_root: PathBuf,
    worktrees: HashMap<String, WorktreeInfo>,
}

impl WorktreeManager {
    pub fn create_worktree(&mut self, task_id: &str, branch: &str) -> Result<WorktreeInfo>;
    pub fn remove_worktree(&mut self, task_id: &str) -> Result<()>;
    pub fn symlink_gitignored(&self, worktree: &Path) -> Result<()>;
    pub fn list_worktrees(&self) -> Vec<WorktreeInfo>;
}

pub struct WorktreeInfo {
    pub task_id: String,
    pub path: PathBuf,
    pub branch: String,
    pub created_at: DateTime<Utc>,
}
```

### 2. Dependency Chain Automation ⭐⭐⭐

**What**: ⌘+click links cards. When Card A completes → auto-starts Card B. Combined with auto-commit, creates autonomous pipelines[^1]:

```
Schema Migration (Card 1)
    ↓ (completes, commits)
API Endpoints (Card 2)  +  TypeScript Types (Card 3)
    ↓                        ↓
Integration Tests (Card 4)
    ↓
Deploy (Card 5)
```

**For Caduceus**: Already have `TaskDAG` in workers — extend with visual linking in the UI:
```rust
// In TaskDAG, add auto-start behavior
pub fn on_task_complete(&mut self, task_id: &str) -> Vec<String> {
    let newly_ready = self.ready_tasks(); // tasks whose deps are now all done
    for task in &newly_ready {
        self.start_task(&task.id);
    }
    newly_ready.iter().map(|t| t.id.clone()).collect()
}
```

### 3. Per-Card Terminal with Live Status ⭐⭐

**What**: Each card shows a mini-terminal preview with the agent's latest message or tool call. Click to expand to full terminal + diff view[^1].

**For Caduceus**: Each `AgentSession` gets its own PTY. The kanban card shows:
- Agent status (running/blocked/done)
- Latest message or tool call (via hooks)
- Token usage so far
- Expandable to full terminal + diff

### 4. Inline Diff Review with Comments ⭐⭐

**What**: Click a card to see all file changes in that worktree as a diff. Click on lines to leave comments that get sent back to the agent[^1].

**For Caduceus**: `DiffReviewPanel` React component:
- Show unified diff of worktree vs base branch
- Clickable lines → comment input
- Comments injected into agent's next prompt as review feedback
- Checkpoint diffs (compare from last user message)

### 5. Sidebar Chat for Board Management ⭐⭐

**What**: A chat sidebar where you ask the AI to decompose work into tasks. Kanban injects board-management instructions so the agent can create/link/start cards[^1].

**For Caduceus**: Add `BoardManagementSkill` that gives the orchestrator tools to:
- `create_task(title, description, dependencies)`
- `link_tasks(from_id, to_id)`
- `start_task(id)`
- `move_task(id, column)`

### 6. Auto-Commit and Auto-PR ⭐

**What**: Enable per-card auto-commit (agent ships as soon as done) or auto-PR (creates a PR branch). Skip review for trusted workflows[^1].

### 7. Git Interface ⭐

**What**: Built-in git UI showing branches, commit history, fetch/pull/push — all without leaving the kanban board[^1].

---

## Caduceus Kanban Feature Design

### Data Model

```rust
pub struct KanbanBoard {
    pub id: String,
    pub name: String,
    pub columns: Vec<KanbanColumn>,
    pub cards: Vec<KanbanCard>,
    pub links: Vec<CardLink>,
    pub settings: BoardSettings,
}

pub struct KanbanColumn {
    pub id: String,
    pub name: String,          // Backlog, In Progress, Review, Done
    pub card_ids: Vec<String>,
    pub wip_limit: Option<usize>,
}

pub struct KanbanCard {
    pub id: String,
    pub title: String,
    pub description: String,
    pub column_id: String,
    pub agent_session_id: Option<String>,
    pub worktree: Option<WorktreeInfo>,
    pub status: CardStatus,
    pub auto_commit: bool,
    pub auto_pr: bool,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub token_usage: TokenUsage,
    pub latest_message: Option<String>,
}

pub enum CardStatus {
    Todo,
    Running,
    Blocked(String),  // reason
    NeedsReview,
    Done,
    Failed(String),
}

pub struct CardLink {
    pub from_card_id: String,
    pub to_card_id: String,    // to_card starts when from_card completes
}

pub struct BoardSettings {
    pub default_auto_commit: bool,
    pub default_auto_pr: bool,
    pub max_parallel_agents: usize,
    pub worktree_symlinks: bool,
}
```

### React Components

| Component | Purpose |
|-----------|---------|
| `KanbanBoard.tsx` | Main board with drag-and-drop columns |
| `KanbanCard.tsx` | Task card with status, mini-terminal, token count |
| `KanbanSidebar.tsx` | Chat for task decomposition + board management |
| `CardDetail.tsx` | Full terminal + diff + comment view |
| `DependencyGraph.tsx` | Visual DAG of linked cards |
| `WorktreeStatus.tsx` | Branch info, file changes count |
| `BoardSettings.tsx` | Auto-commit, parallel limits, worktree config |

### Slash Commands

- `/kanban` — Open kanban board
- `/kanban create <title>` — Create a new card
- `/kanban decompose <description>` — AI breaks work into linked cards
- `/kanban start <card>` — Start agent on card
- `/kanban link <from> <to>` — Create dependency link
- `/kanban status` — Show board summary

### Integration Points

```
┌──────────────────┐     ┌──────────────────┐
│  Kanban Board     │────▶│  TaskDAG         │  (dependency resolution)
│  (React UI)       │     │  (workers crate)  │
└────────┬─────────┘     └────────┬─────────┘
         │                        │
         ▼                        ▼
┌──────────────────┐     ┌──────────────────┐
│  WorktreeManager  │     │  AgentHarness    │  (per-card agent)
│  (git crate)      │     │  (orchestrator)   │
└──────────────────┘     └──────────────────┘
         │                        │
         ▼                        ▼
┌──────────────────┐     ┌──────────────────┐
│  Git Worktrees    │     │  PTY per card     │  (terminal streaming)
│  (isolated dirs)  │     │  (src-tauri)      │
└──────────────────┘     └──────────────────┘
```

---

## Implementation Priority

| # | Feature | Effort | Impact |
|---|---------|--------|--------|
| 1 | Board data model + SQLite persistence | S | Foundation |
| 2 | React KanbanBoard component (drag-drop columns) | M | Visual layer |
| 3 | Git worktree manager | M | Isolation |
| 4 | Per-card agent session + terminal | L | Core feature |
| 5 | Dependency linking + auto-start | M | Automation |
| 6 | Inline diff review with comments | M | Review workflow |
| 7 | AI task decomposition via sidebar | M | Smart planning |
| 8 | Auto-commit / auto-PR | S | Shipping |
| 9 | Linear/GitHub Projects import | M | Integration |

---

## Footnotes

[^1]: [cline/kanban README.md](https://github.com/cline/kanban) — All feature descriptions from official README
[^2]: [cline/kanban package.json](https://github.com/cline/kanban/blob/main/package.json) — Dependencies list
[^3]: [Cline Kanban announcement](https://cline.ghost.io/announcing-kanban/) — Official blog post
[^4]: [Testing Catalog review](https://www.testingcatalog.com/cline-debuts-kanban-for-local-parallel-cli-coding-agents/) — Feature analysis
[^5]: [Cline + Kanban integration guide](https://jamesm.blog/ai/cline-kanban/) — Integration with Linear/Jira
[^6]: [Kanban source code](https://github.com/cline/kanban/tree/main/src) — Module structure
