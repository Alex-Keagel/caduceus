# Deep Research: Cline & Cursor — New Capabilities for Caduceus

## Executive Summary

Cline and Cursor represent the two dominant paradigms in AI coding agents in 2026. **Cline** is the leading open-source VS Code extension (Apache 2.0) with a plugin-based tool system, browser automation, checkpointing, and MCP marketplace. **Cursor 3 "Glass"** is the leading commercial AI IDE with an agent-first workspace, background/cloud agents, automations (trigger-based), BugBot auto-reviews, and multi-repo editing. This report identifies **23 capabilities** from these tools that Caduceus should adopt.

## Confidence Assessment

- **High confidence**: Feature descriptions from official docs, VS Code Marketplace, and source code
- **Medium confidence**: Internal architecture details inferred from public source (Cline) and blog posts (Cursor — closed source)
- **Low confidence**: Cursor pricing/limits details (change frequently)

---

## Part 1: Cline — Architecture & Capabilities

### Source: [cline/cline](https://github.com/cline/cline) (Apache 2.0, TypeScript)

### Architecture

```
┌──────────────────────────────────────────────────┐
│                  Cline Extension                  │
│                                                   │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────┐│
│  │  Controller  │  │  Task System │  │ Webview  ││
│  │  (orchestr.) │  │  Plan → Act  │  │ (React)  ││
│  └──────┬──────┘  └──────┬───────┘  └────┬─────┘│
│         │                │               │       │
│  ┌──────▼──────────────────────────────────────┐ │
│  │              Core Modules                    │ │
│  │  api/ │ commands/ │ context/ │ controller/  │ │
│  │  hooks/ │ ignore/ │ locks/ │ mentions/     │ │
│  │  permissions/ │ prompts/ │ slash-commands/ │ │
│  │  storage/ │ task/ │ webview/ │ workspace/  │ │
│  └──────┬──────────────────────────────────────┘ │
│         │                                        │
│  ┌──────▼──────────────────────────────────────┐ │
│  │              Services                        │ │
│  │  browser/ │ mcp/ │ tree-sitter/ │ ripgrep/ │ │
│  │  search/ │ telemetry/ │ glob/ │ logging/   │ │
│  │  auth/ │ feature-flags/ │ test/ │ temp/    │ │
│  └─────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────┘
```

### Key Capabilities to Adopt for Caduceus

#### 1. Plan & Act Modes ⭐
- **Plan mode**: Agent analyzes codebase, discusses strategy, NO file modifications
- **Act mode**: Executes planned changes step-by-step with approval
- **Why for Caduceus**: Separates reasoning from execution. Prevents runaway modifications. Users can review the plan before any code changes.
- **Implementation**: Add `AgentMode::Plan | AgentMode::Act` to orchestrator. In Plan mode, tool calls return "would do X" instead of executing.

#### 2. Checkpointing System ⭐
- At each tool call, snapshot the project state (git shadow commits)
- User can compare any checkpoint, restore to any previous state
- Like "infinite undo" for AI changes across the entire workspace
- **Why for Caduceus**: Essential safety net. Users can let the agent go wild knowing they can always roll back.
- **Implementation**: Before each tool execution, create a lightweight git stash/commit. Store checkpoint metadata in SQLite.

#### 3. Browser Automation Service
- Headless Chromium control: launch apps, click, type, scroll, screenshot, read console logs
- Agent can debug runtime errors by interacting with the running application
- **Why for Caduceus**: AI can test its own code changes by running the app and checking results.
- **Implementation**: Use `chromiumoxide` (Rust headless Chrome) or shell out to `playwright`.

#### 4. @Mentions System
- `@file` — include specific file in context
- `@folder` — include directory structure
- `@url` — fetch and include web content
- `@problems` — include VS Code diagnostics
- `@git` — include git diff/status
- **Why for Caduceus**: Precise context control. User tells the agent exactly what to look at.
- **Implementation**: Parse `@` prefixed tokens in user input, resolve to context chunks, inject into system prompt.

#### 5. .clineignore / .clinerules
- `.clineignore` — files the agent should never read/modify (like .gitignore syntax)
- `.clinerules` — project-specific instructions (similar to CLAUDE.md but Cline-specific)
- **Why for Caduceus**: Already have CADUCEUS.md — add `.caduceusignore` for file exclusion.

#### 6. XML-Based Tool Invocation
- Tools are called via structured XML blocks in the LLM output
- Makes parsing more reliable than JSON in streaming contexts
- **Why for Caduceus**: Consider as alternative to JSON tool_use blocks for local models that struggle with JSON.

#### 7. Memory Bank (Persistent Context)
- Structured files: `projectBrief.md`, `activeContext.md`, `progress.md`
- Auto-updated by the agent, loaded at session start
- Rebuilds project understanding across sessions
- **Why for Caduceus**: Already have `.caduceus/memory.md` — extend with structured memory files.

#### 8. Feature Flags Service
- Runtime feature toggles for gradual rollout
- Enables A/B testing of new agent behaviors
- **Why for Caduceus**: Useful for safely deploying new agent capabilities.

#### 9. Hooks System (27 Events)
- Pre/post hooks for: tool calls, file edits, terminal commands, errors, sessions
- Shell script or custom command execution on events
- **Why for Caduceus**: Already planned in P1 — confirm alignment with Cline's 27-event model.

---

## Part 2: Cursor 3 "Glass" — Architecture & Capabilities

### Source: Closed source (VS Code fork), blog posts + documentation

### Architecture (3 Layers)

```
┌─────────────────────────────────────────────────────┐
│              CURSOR 3 "GLASS"                        │
│                                                      │
│  ┌────────────────────────────────────────────────┐  │
│  │  INTERACTIVE LAYER                              │  │
│  │  Tab completion │ Inline chat │ File editing    │  │
│  │  Composer model │ Multi-file changes            │  │
│  └────────────────────────┬───────────────────────┘  │
│                           │                          │
│  ┌────────────────────────▼───────────────────────┐  │
│  │  AGENT LAYER                                    │  │
│  │  Agents Window (parallel tabs)                  │  │
│  │  Background agents │ Cloud agents               │  │
│  │  BugBot │ Automations │ Multi-repo              │  │
│  └────────────────────────┬───────────────────────┘  │
│                           │                          │
│  ┌────────────────────────▼───────────────────────┐  │
│  │  CONTROL LAYER                                  │  │
│  │  Org policies │ Privacy │ Model routing         │  │
│  │  Memory retention │ Deployment (cloud/self)     │  │
│  └────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
```

### Key Capabilities to Adopt for Caduceus

#### 10. Agents Window (Multi-Agent Tabs) ⭐⭐
- Multiple independent agents running in parallel, each in its own tab
- Assign different tasks to different agents simultaneously
- Monitor progress, approve, or cancel each independently
- **Why for Caduceus**: Our multi-agent workers system already supports this — need the UI.
- **Implementation**: React `AgentsWindow` component with tab bar, each tab showing an independent agent session.

#### 11. Background/Cloud Agents ⭐⭐
- Agents that keep running when you close the editor
- Can hand off from local → cloud and back
- Long-running tasks: test generation, code review, dependency updates
- **Why for Caduceus**: Huge productivity gain. Start a refactoring task, go to lunch, come back to a PR.
- **Implementation**: Agent sessions persist in SQLite. Cloud execution via E2B sandbox. Status polling via Tauri events.

#### 12. Automations (Trigger-Based Agents) ⭐⭐⭐
- Always-on agents triggered by: GitHub PR, push, schedule (cron), Slack message, PagerDuty alert, webhook
- Run in cloud VMs, produce PRs/comments/artifacts
- Templates: nightly test coverage, PR review, incident response, security audit
- **Why for Caduceus**: This is the killer feature. Turns Caduceus from a tool into a team member.
- **Implementation**: `AutomationConfig` with trigger type, agent config, output target. Webhook listener in Tauri.

#### 13. BugBot (Automated PR Review) ⭐⭐
- Automatically reviews every PR for bugs, security issues, style violations
- Multi-pass agentic architecture (70%+ fix rate)
- "Fix in Cursor" one-click remediation
- **Why for Caduceus**: Add as a built-in agent in the marketplace.
- **Implementation**: Agent that reads git diff, runs analysis, posts comments. Connect to GitHub API.

#### 14. Design Mode (Visual Annotations)
- Annotate UI elements in a browser view
- Agent makes code changes based on visual annotations
- Screenshot-to-code workflow
- **Why for Caduceus**: Innovative for frontend development.
- **Implementation**: Browser integration + screenshot tool + annotation overlay.

#### 15. Predictive Tab Completion
- Multi-line, context-aware completions
- Understands project structure, not just current file
- Auto-imports modules
- **Why for Caduceus**: Core productivity feature. 45-60 min/day savings reported.
- **Implementation**: Requires streaming inline suggestions from LLM into terminal/editor.

#### 16. Multi-Repo Workspace
- Open and manage several repositories in unified interface
- Agents can execute changes across repos
- **Why for Caduceus**: Essential for microservices/monorepo workflows.
- **Implementation**: Multiple `ProjectContext` instances, workspace-spanning tool routing.

#### 17. Composer 2 (Proprietary Model)
- Cursor's own fine-tuned coding model
- High usage limits, optimized for their editor
- **Why for Caduceus**: We can't use their model, but we can optimize for specific models (Claude, GPT) with fine-tuned system prompts and tool schemas.

#### 18. MCP Apps (One-Click Install)
- OAuth-integrated MCP connections
- One-click install from marketplace
- Pre-configured for GitHub, Linear, Datadog, Slack, Notion
- **Why for Caduceus**: Already building MCP marketplace — add OAuth flow for enterprise MCPs.

#### 19. Self-Verification (Agent QA)
- Agents test their own code, run apps, capture logs/screenshots/videos
- Attach artifacts to PRs for human review
- **Why for Caduceus**: Critical for trust. Agent proves its work before asking for approval.
- **Implementation**: After code changes, agent runs tests via sandbox, captures output, attaches to session.

---

## Part 3: Feature Gap Analysis — What Caduceus Should Add

### Priority Matrix

| # | Feature | Source | Caduceus Status | Priority | Effort |
|---|---------|--------|----------------|----------|--------|
| 1 | Plan & Act modes | Cline | ❌ Missing | P0 | S |
| 2 | Checkpointing | Cline | ❌ Missing | P0 | M |
| 3 | Automations (trigger-based) | Cursor | ❌ Missing | P0 | L |
| 4 | Agents Window (multi-tab) | Cursor | 🔧 Partial (workers) | P0 | M |
| 5 | Background agents | Cursor | ❌ Missing | P1 | L |
| 6 | BugBot (auto PR review) | Cursor | ❌ Missing | P1 | M |
| 7 | Browser automation | Cline | ❌ Missing | P1 | M |
| 8 | @Mentions context | Cline | ❌ Missing | P1 | S |
| 9 | Self-verification (test own code) | Cursor | 🔧 Partial (sandbox) | P1 | M |
| 10 | .caduceusignore | Cline | ❌ Missing | P1 | S |
| 11 | Multi-repo workspace | Cursor | ❌ Missing | P2 | M |
| 12 | Design mode (visual) | Cursor | ❌ Missing | P2 | L |
| 13 | Tab completion | Cursor | ❌ Missing | P2 | XL |
| 14 | Memory bank (structured) | Cline | 🔧 Partial (memory.md) | P2 | S |

### Recommended Implementation Order

1. **Plan & Act modes** (S) — Simple flag, huge safety improvement
2. **Checkpointing** (M) — Git stash before each tool call
3. **@Mentions** (S) — Parse @file/@folder/@url in user input
4. **.caduceusignore** (S) — Glob-based file exclusion
5. **Agents Window UI** (M) — Multi-tab agent view
6. **BugBot agent** (M) — Built-in PR review agent
7. **Browser automation** (M) — Headless Chrome integration
8. **Self-verification** (M) — Agent runs tests after changes
9. **Background agents** (L) — Persistent agent sessions
10. **Automations** (L) — Trigger-based agent execution

---

## Part 4: Cline Source Code — Key Files Reference

| Module | Path | Purpose |
|--------|------|---------|
| Controller | `src/core/controller/` | Main orchestration logic |
| Task System | `src/core/task/` | Plan/Act state machine |
| Hooks | `src/core/hooks/` | 27 lifecycle event hooks |
| Permissions | `src/core/permissions/` | Human-in-the-loop approvals |
| Prompts | `src/core/prompts/` | System prompt assembly |
| Context | `src/core/context/` | Context management, @mentions |
| Commands | `src/core/commands/` | Slash command handling |
| Storage | `src/core/storage/` | Persistent state, checkpoints |
| Workspace | `src/core/workspace/` | Project detection, file watching |
| MCP Service | `src/services/mcp/` | MCP client + marketplace |
| Browser | `src/services/browser/` | Headless browser automation |
| Tree-sitter | `src/services/tree-sitter/` | AST parsing for code intelligence |
| Ripgrep | `src/services/ripgrep/` | Fast code search |
| Search | `src/services/search/` | Semantic search service |
| Feature Flags | `src/services/feature-flags/` | Runtime toggles |

---

## Footnotes

[^1]: [cline/cline](https://github.com/cline/cline) — `src/core/` directory listing
[^2]: [Cursor 3 "Glass" announcement](https://cursor.com/blog/cursor-3) — Agent-first workspace
[^3]: [Cursor Automations blog post](https://cursor.com/blog/automations) — Trigger-based agents
[^4]: [Cline VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=saoudrizwan.claude-dev)
[^5]: [Cline architecture overview (DeepWiki)](https://deepwiki.com/cline/cline/1.1-architecture-overview)
[^6]: [Cursor BugBot + Background Agents](https://www.cursor.fan/blog/2025/06/04/cursor-1-0-bugbot-background-agent-mcp-install/)
[^7]: [TechCrunch — Cursor agentic coding](https://techcrunch.com/2026/03/05/cursor-is-rolling-out-a-new-system-for-agentic-coding/)
[^8]: [Cline breakdown (memo.d.foundation)](https://memo.d.foundation/breakdown/cline)
[^9]: [Cursor Beta Features 2026](https://markaicode.com/cursor-beta-features-2026/)
