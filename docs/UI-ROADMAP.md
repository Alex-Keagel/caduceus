# Caduceus UI Roadmap

## Current State

The app has 28 React components, 50 IPC commands, and VS Code-style CSS but is missing critical IDE UX patterns. This roadmap addresses the gaps by studying SideX (Tauri VS Code fork), VS Code, and IntelliJ.

## Reference: SideX Architecture (Tauri + React VS Code fork)

SideX (github.com/Sidenai/sidex) maps VS Code's architecture to Tauri:
- Monaco Editor for code editing
- portable-pty (Rust) for terminal
- Tauri invoke() for all IPC
- VS Code's layout engine ported to webview
- 70+ Rust backend commands (fs, terminal, search, git, window, storage, etc.)

## Phase 1: Core UX Fixes (Blocking — app feels broken without these)

### P0 — Must work for basic usability

| # | Feature | What's broken | Fix |
|---|---------|---------------|-----|
| U1 | **File/Folder Open Dialog** | No way to open a project | Add Tauri `dialog.open()` with folder picker + recent projects list |
| U2 | **Terminal Input** | PTY exists but keyboard may not be connected | Verify xterm.js → PTY write path, ensure focus on click |
| U3 | **Provider Setup Flow** | No UI to enter API keys | First-launch settings modal: pick provider, enter key, test connection |
| U4 | **Welcome Screen** | Blank app on launch | Show welcome view: recent projects, quick actions, provider setup |
| U5 | **File Explorer Sidebar** | No file tree | Tree view reading from project_scan IPC, with icons/expand/collapse |
| U6 | **Monaco Editor** | No code editor | Integrate Monaco Editor for viewing/editing files (the core of any IDE) |

### P1 — Expected IDE features

| # | Feature | Description |
|---|---------|-------------|
| U7 | **Activity Bar** | Left-edge 48px icon strip: Explorer, Search, Git, Kanban, Marketplace, Chat |
| U8 | **Editor Tabs** | Open files in tabs, switch between them, close with X, modified indicator |
| U9 | **File Tree Context Menu** | Right-click: New File, New Folder, Rename, Delete, Copy Path |
| U10 | **Breadcrumb Navigation** | File path breadcrumbs above editor |
| U11 | **Minimap** | Code minimap on right side of editor (Monaco built-in) |
| U12 | **Search Across Files** | Ctrl+Shift+F → search all files with results panel |
| U13 | **Integrated Terminal Tabs** | Multiple terminal instances with tab bar |
| U14 | **Settings UI** | Visual settings editor (provider, model, keybindings, theme) |
| U15 | **Recent Projects** | Remember and list recently opened projects |

## Phase 2: AI-IDE Features (What makes Caduceus unique)

| # | Feature | Description |
|---|---------|-------------|
| U16 | **Inline Chat** | Ctrl+I in editor → ask AI about selected code |
| U17 | **AI Diff Preview** | Show proposed changes as diff before applying |
| U18 | **Agent Status in StatusBar** | Real-time: model, tokens/budget, mode, context % |
| U19 | **Tool Call Visualization** | Show tool calls as collapsible blocks in chat |
| U20 | **Approval Flow** | Modal for tool call approval with diff preview |
| U21 | **Context Meter** | Visual bar showing context window usage with zones |
| U22 | **Security Findings Panel** | Show SAST scan results with file links |
| U23 | **Session Timeline** | Visual replay of agent actions with branch tree |

## Phase 3: Power User Features

| # | Feature | Description |
|---|---------|-------------|
| U24 | **Multi-Window** | Open multiple projects in separate windows |
| U25 | **Extension Panel** | Browse/install marketplace extensions |
| U26 | **Output Panel** | Logs, build output, test results tabs |
| U27 | **Problems Panel** | Diagnostics from LSP, lint errors, security findings |
| U28 | **Source Control Panel** | Full git GUI: stage, commit, push, pull, branch, stash |
| U29 | **Debug Panel** | Breakpoints, variables, call stack, watch |
| U30 | **Drag-Drop Layout** | Resize/rearrange panels like VS Code |

## Implementation Strategy

### Option A: Build from scratch (current approach)
- ✅ Full control
- ❌ Months of work to match VS Code UX
- ❌ Reinventing Monaco integration, file tree, etc.

### Option B: Fork SideX and merge our backend (RECOMMENDED)
- SideX already has: Monaco, file tree, terminal, git panel, tabs, layout engine
- We add: our 14 Rust crates as additional Tauri commands
- We replace: SideX's basic AI with our orchestrator/agents/skills
- ✅ Get a real IDE in days instead of months
- ✅ Same stack (Tauri + React + Rust)

### Option C: Embed Monaco only
- Add `@monaco-editor/react` package
- Build the rest of the IDE shell ourselves
- Middle ground between A and B

## Immediate Next Steps

1. **Fix Terminal** — verify PTY keyboard input path
2. **Add Folder Open** — Tauri dialog plugin
3. **Add Welcome Screen** — with provider setup + recent projects
4. **Add File Explorer** — tree view from project_scan
5. **Evaluate SideX fork** — could save months of UI work
