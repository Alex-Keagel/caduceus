# Caduceus + caduceus-zed Feature Catalog

**Status:** Draft (P-tier index)
**Audience:** Spec authors, runtime engineers, UI engineers, release managers, downstream consumers (Symphony, Hermes, OpenClaw)
**Scope:** Canonical enumeration of every user-visible (or runtime-observable) feature across the caduceus engine and its zed-side host. Each feature is mapped to its **owning spec**, **owning subsystem**, **maturity tier**, and **cross-runtime support matrix** (VSCode / CLI / zed).

> This document is an *index*, not a *specification*. It does not define behaviour. For behaviour, follow the link in the **Owning Spec** column. When a feature has no owning spec yet, that is itself a tracked gap (see §7 Maturity Roadmap and §8 Open Questions).

---

## §0. Provenance & Cleanroom Statement

This catalog is synthesized from the following internal design artifacts, all authored cleanroom (no Anthropic/Claude-Code source consulted):

- `m-spec-analysis.md` — engine-side spec set A–G (permissions, session lifecycle, backend abstraction, tenant policy, sandbox enforcement, logging, optional background tasks).
- `m-spec-analysis-ui.md` — UI spec set UI-A–UI-G (approval card, thread-state invariants, skill autocomplete, notice/notification, tenant-policy banner, diagnostics viewer, optional MCP status panel).
- `symphony-multirepo-ux.md` — Runs Panel proposal, multi-repo workspace model, run-identity entity, orchestrator status snapshot, multi-window UX.
- `symphony-fit-analysis.md` — orchestrator dispatch surface, agent handoff to cloud, repo-owned workflow contract.
- `m-e2e-architecture.md` — 48 features (F1–F48) traced through a 9-step E2E template; this is the primary source for feature enumeration.
- Existing specs in `/docs/specs/`: see §9 Spec Index.

No Claude Code, claude.ai, Cursor, Continue.dev, or other proprietary AI-IDE source was read while authoring this catalog. All references to "claw"/"claurst" are to the in-tree cleanroom rewrites (see `spec-claw-code.md`, `spec-claurst-full.md`).

---

## §1. Scope & Purpose

The caduceus + caduceus-zed system is a multi-runtime AI coding assistant comprising:

1. **caduceus engine** (Rust) — the in-process or daemon library that owns sessions, permissions, tenant policy, MCP, skills, sandboxing, logging, and the orchestrator. Crates: `caduceus-runtime`, `caduceus-orchestrator`, `caduceus-permissions`, `caduceus-sandbox`, `caduceus-tenant`, `caduceus-mcp`, `caduceus-skills`, `caduceus-logging`. Optional daemon binary: `caduceusd` (gated on whether multi-window/multi-process UX ships — see §8 OQ-1).
2. **caduceus-zed UI** (Rust + GPUI) — the zed-side host that integrates the engine into the zed editor: `caduceus_bridge` (engine adapter), `agent_ui` (chat panel, approval card, runs panel, diagnostics viewer), `caduceus_settings`.
3. **caduceus-cli** (Rust) — terminal host wrapping the same engine for headless / CI / SSH usage.
4. **VSCode host** — a *consumer* runtime (see §6.4) that talks to the engine via a stable contract. Caduceus does not own VSCode UI code; it owns the contract.
5. **Cloud handoff target** — `OpenClaw` gateway used for "switch this run to a hosted agent" (see Feature 5.9).

**Why a catalog?** Caduceus has 16+ specs, three runtimes, and a sibling project (Symphony) consuming its surfaces. A canonical feature index prevents:

- Spec drift: two specs describing the same feature with different invariants.
- Runtime drift: a feature shipping on zed but silently broken on CLI.
- Maturity drift: a feature listed as "ready" in one place and "designed" in another.
- Ownership ambiguity: an approval-card bug filed against the engine when it lives in `agent_ui`.

This catalog is the single source of truth for **"what features exist, what tier are they at, and where do I read the spec?"**.

---

## §2. How to Read This Catalog

### 2.1. P-Tier Semantics

| Tier | Label | Meaning |
|------|-------|---------|
| **P0** | Ready | Spec is complete and reviewed; implementation is shipping or merged behind a feature flag; acceptance criteria are runnable. Bugs are bugs, not gaps. |
| **P1** | Designed | Spec is drafted and under review; implementation has not yet started or is partial. Behaviour is pinned in writing; remaining work is execution, not design. |
| **P2** | Sketched | Idea is captured (in this catalog, in `m-e2e-architecture.md`, or in an early-draft spec) but invariants are not yet pinned. Substantial design work remains. |
| **deferred** | Deferred | Out of scope for the current milestone. May be reactivated later. |

**Promotion rule:** P2 → P1 requires a written spec (`docs/specs/spec-*.md`) covering the feature's surface, invariants, error model, and cross-runtime contract. P1 → P0 requires acceptance tests checked in and a runtime that exercises them.

### 2.2. Subsystem Owners

| Owner | Code location | Process |
|-------|---------------|---------|
| **daemon** | `caduceusd` binary (or `caduceus-runtime` lib in single-process mode) | Long-lived; owns sessions, permissions, tenant policy, MCP, skills, sandbox, audit. |
| **runner** | `caduceus-runtime` worker pool inside daemon | Per-session worker that drives one agent loop (model calls, tool calls, file ops). |
| **zed UI** | `caduceus_bridge` + `agent_ui` crates in caduceus-zed | GPUI views: chat composer, approval card, runs panel, diagnostics viewer, status bar. |
| **CLI host** | `caduceus-cli` binary | Terminal frontend (TUI or line-mode) wrapping the same engine contract. |
| **cloud** | `OpenClaw` gateway + cloud agent runtime | Hosted target for "agent handoff to cloud". |
| **VSCode** | external VSCode extension (out-of-tree consumer) | Consumes the engine via the stable contract. Listed for matrix completeness. |

### 2.3. Runtime Matrix Symbols

| Symbol | Meaning |
|--------|---------|
| ✅ | Supported and shipping (or designed-and-targeted for P0 in this milestone). |
| 🚧 | Partially supported / under construction / shipping behind a flag. |
| ❌ | Not supported and not planned for this milestone. |
| N/A | Feature is conceptually meaningless on this runtime (e.g., "right-dock panel" on CLI). |
| 🌐 | Supported via remote engine only (host has no in-process engine). |

### 2.4. Per-Feature Subsection Format

Each feature (in §5) follows this fixed schema:

```
### 5.x.y. <Feature ID> — <Feature Name>

**Owner:** <subsystem>
**Owning Spec:** [<spec-name>](./spec-<name>.md) | _(none yet — see §7)_
**P-Tier:** P0 / P1 / P2 / deferred
**Runtime:** VSCode <symbol> · CLI <symbol> · zed <symbol>
**Depends on:** F<n>, F<m> (or "none")

**Current state:** 1–3 sentences describing where the feature is today (shipping / merged / draft / sketch).

**Acceptance criteria pointer:** link to the spec section that defines the runnable acceptance criteria, or `(deferred)` if none yet.

**Deferred sub-features:** bullet list of explicitly out-of-scope sub-capabilities.
```

Subsections are intentionally short. The catalog optimizes for **"can I find the spec in 10 seconds?"**, not for re-stating the spec.

---

## §3. Subsystem Owner Glossary

This section defines the canonical names for the major subsystems referenced in the **Owner** column. Cross-references in spec PRs should use these names verbatim.

### 3.1. `caduceusd` — the daemon

Long-lived OS process. Owns the singletons:

- `SessionManager` — session lifecycle, pause-oldest LRU, resume-from-disk.
- `PermissionEvaluator` — the 9-step evaluate() pipeline; ApprovalBroker.
- `TenantPolicy` — managed-source resolution, dual enforcement at config-load and runtime.
- `MCPRegistry` — installed MCP servers, status, lifecycle.
- `SkillRegistry` — installed skills, marketplace cache.
- `BackendRouter` — `pushConfig` over the engine→backend channel; offline queue with `lastConfirmedEventId` replay.
- `AuditLog` — append-only audit trail (permission decisions, profile switches, tenant violations).

Multi-window UX requires a shared daemon (one daemon, multiple UI clients over local IPC). See §8 OQ-1.

### 3.2. `caduceus-runtime` — the runner pool

In-daemon worker pool. Each worker drives one **agent run**: a sequence of model turns, tool calls, file edits, and checkpoints. The runner:

- Holds the agent loop state machine (waiting-on-model / waiting-on-tool / waiting-on-approval / waiting-on-user / paused / done).
- Speaks to the model backend (Anthropic / OpenAI / Azure / cloud-handoff target) over a `pushConfig`-style channel.
- Calls into `PermissionEvaluator` before any tool call; blocks on `ApprovalBroker` when the decision is "ask".
- Emits structured events on the engine→host event stream (turn-start, tool-call, approval-needed, file-edit, checkpoint-created, done, error).

### 3.3. `caduceus_bridge` + `agent_ui` — the zed UI

Two crates in caduceus-zed:

- `caduceus_bridge` — engine adapter. Owns the in-process `caduceus-runtime` (single-process mode) or the IPC client to `caduceusd` (daemon mode). Translates engine events into UI-readable forms.
- `agent_ui` — GPUI views: `ChatPanel`, `ApprovalCard`, `RunsPanel`, `DiagnosticsViewer`, `MCPStatusPanel`, `TenantPolicyBanner`, `NotificationCenter`. Owns the lock-after-first-message invariant and the race-via-current-id pattern.

### 3.4. `caduceus-cli` — the terminal host

Headless host for CI, SSH, and power users. Two modes:

- **Line mode** — non-interactive: `caduceus run --prompt "..." --agent claude-3-7`. One-shot.
- **TUI mode** — interactive: ratatui-based chat composer, approval prompts, runs list. Same engine; different presentation layer.

### 3.5. Cloud handoff target — `OpenClaw`

Out-of-process hosted agent runtime. Caduceus does not own its internals; it owns the **handoff contract**: a serialized session snapshot (turn history, tool-result cache, permission grants, tenant policy fingerprint) that OpenClaw can resume.

---

## §4. Master Feature Table

Features are numbered F1–F48 from `m-e2e-architecture.md`. Additional features added by this catalog (not in the original 48) carry the prefix `FC-` (Feature Catalog).

| ID | Feature | Owner | P-Tier | Owning Spec | VSCode | CLI | zed |
|----|---------|-------|--------|-------------|--------|-----|-----|
| F1 | Create session | daemon | P0 | [spec-m-session-lifecycle](./spec-m-session-lifecycle.md) | ✅ | ✅ | ✅ |
| F2 | Send first turn | runner | P0 | [spec-m-session-lifecycle](./spec-m-session-lifecycle.md) | ✅ | ✅ | ✅ |
| F3 | Resume session from disk | daemon | P1 | [spec-m-session-lifecycle](./spec-m-session-lifecycle.md) | 🚧 | 🚧 | 🚧 |
| F4 | Fork session | daemon | P1 | [spec-m-session-lifecycle](./spec-m-session-lifecycle.md) | 🚧 | 🚧 | 🚧 |
| F5 | Pause-oldest LRU | daemon | P1 | [spec-m-session-lifecycle](./spec-m-session-lifecycle.md) | 🚧 | 🚧 | 🚧 |
| F6 | Backend pushConfig | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F7 | Offline queue + replay | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F8 | `lastConfirmedEventId` ack | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F9 | 9-step permission evaluate() | daemon | P0 | [spec-m-permissions](./spec-m-permissions.md) | ✅ | ✅ | ✅ |
| F10 | Approval card (4-button) | zed UI | P1 | _(see UI-A in m-spec-analysis-ui)_ | 🚧 | 🚧 | 🚧 |
| F11 | Allow-for-session grant | daemon | P1 | [spec-m-permissions](./spec-m-permissions.md) | 🚧 | 🚧 | 🚧 |
| F12 | Always-allow grant | daemon | P1 | [spec-m-permissions](./spec-m-permissions.md) | 🚧 | 🚧 | 🚧 |
| F13 | Profile-switch picker | zed UI | P1 | _(none — see §7)_ | ❌ | 🚧 | 🚧 |
| F14 | Managed-source resolution | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F15 | Dual enforcement (load + runtime) | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F16 | Tenant-policy banner | zed UI | P1 | _(see UI-E in m-spec-analysis-ui)_ | 🚧 | N/A | 🚧 |
| F17 | MCP server list & toggle | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F18 | MCP status panel | zed UI | P2 | _(see UI-G in m-spec-analysis-ui)_ | ❌ | N/A | 🚧 |
| F19 | MCP configure modal | zed UI | P2 | _(none — see §7)_ | ❌ | 🚧 | 🚧 |
| F20 | Skill autocomplete | zed UI | P0 | [spec-skill-agent-autocomplete](./spec-skill-agent-autocomplete.md) | 🚧 | 🚧 | ✅ |
| F21 | Skill ingest / install | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F22 | Skill marketplace | daemon | P2 | _(none — see §7)_ | ❌ | ❌ | 🚧 |
| F23 | Heartbeat | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F24 | Background automation | daemon | P2 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F25 | Notification center | zed UI | P1 | [spec-notice-notification](./spec-notice-notification.md) | 🚧 | 🚧 | 🚧 |
| F26 | Path validator (sandbox) | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F27 | Access auditor | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F28 | OS sandbox primitives | daemon | P2 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F29 | OAuth login | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F30 | API key management | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F31 | Identity refresh | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F32 | Horizon: model picker | runner | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F33 | Horizon: provider routing | runner | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F34 | Horizon: cost/latency telemetry | runner | P2 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F35 | Horizon: fallback chains | runner | P2 | _(none — see §7)_ | ❌ | ❌ | ❌ |
| F36 | Teams Relay: session sharing | daemon | P2 | _(none — see §7)_ | ❌ | ❌ | ❌ |
| F37 | Teams Relay: presence | daemon | deferred | _(none)_ | ❌ | ❌ | ❌ |
| F38 | Teams Relay: comments | daemon | deferred | _(none)_ | ❌ | ❌ | ❌ |
| F39 | Logger | daemon | P1 | _(see Spec F in m-spec-analysis)_ | 🚧 | 🚧 | 🚧 |
| F40 | Telemetry pipeline | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F41 | Diagnostics viewer | zed UI | P1 | _(see UI-F in m-spec-analysis-ui)_ | ❌ | 🚧 | 🚧 |
| F42 | Chat composer | zed UI | P0 | [spec-skill-agent-autocomplete](./spec-skill-agent-autocomplete.md) | ✅ | ✅ | ✅ |
| F43 | Thread-state lock-after-first-message | zed UI | P1 | _(see UI-B in m-spec-analysis-ui)_ | 🚧 | N/A | 🚧 |
| F44 | File diff & checkpoint UI | zed UI | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F45 | Build/test results surface | zed UI | P2 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| F46 | Status bar (engine state) | zed UI | P1 | _(none — see §7)_ | 🚧 | N/A | 🚧 |
| F47 | Build matrix | — | P1 | _(out-of-scope: build infra)_ | — | — | — |
| F48 | Release packaging | — | P1 | _(out-of-scope: release infra)_ | — | — | — |
| FC-1 | Multi-repo workspace selector | zed UI | P1 | [spec-multi-repo-workspace-model](./spec-multi-repo-workspace-model.md) | ❌ | 🚧 | 🚧 |
| FC-2 | Runs Panel (right dock) | zed UI | P1 | [spec-orchestrator-status-snapshot](./spec-orchestrator-status-snapshot.md) | ❌ | N/A | 🚧 |
| FC-3 | Status snapshot subscription | daemon | P1 | [spec-orchestrator-status-snapshot](./spec-orchestrator-status-snapshot.md) | 🚧 | 🚧 | 🚧 |
| FC-4 | Retry / cascade visualization | zed UI | P2 | _(none — see §7)_ | ❌ | 🚧 | 🚧 |
| FC-5 | Multi-window / multi-session | daemon | P2 | _(none — see §8 OQ-1)_ | ❌ | N/A | 🚧 |
| FC-6 | Workspace permission grants | daemon | P1 | [spec-m-permissions](./spec-m-permissions.md) | 🚧 | 🚧 | 🚧 |
| FC-7 | Agent handoff to cloud | daemon | P2 | _(none — see §7)_ | ❌ | 🚧 | 🚧 |
| FC-8 | Telemetry settings & opt-out | daemon | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| FC-9 | Profile picker (composer) | zed UI | P1 | _(none — see §7)_ | 🚧 | 🚧 | 🚧 |
| FC-10 | Run identity entity | daemon | P1 | [spec-orchestrator-status-snapshot](./spec-orchestrator-status-snapshot.md) | 🚧 | 🚧 | 🚧 |
| FC-11 | Repo-owned workflow contract | daemon | P1 | [spec-repo-owned-workflow-contract](./spec-repo-owned-workflow-contract.md) | 🚧 | 🚧 | 🚧 |
| FC-12 | Orchestrator dispatch surface | daemon | P1 | _(none — see §7; Symphony-driven)_ | 🚧 | 🚧 | 🚧 |

> **Total:** 48 base features + 12 catalog-added features = **60 entries**. The runtime matrix is enumerated explicitly per row; see §6 for a transposed view.

---

## §5. Per-Feature Subsections

Features are grouped by user-facing concern, not by code crate. The "Owner" field in each subsection identifies the code crate; the section heading identifies the user-facing capability.

---

### 5.1. Chat & Composer

The composer is the primary user input surface. It is owned by `agent_ui` (zed) and `caduceus-cli` (CLI). It is not owned by the daemon — the daemon receives a structured `TurnRequest`, not raw composer state.

#### 5.1.1. F42 — Chat composer

**Owner:** zed UI (and CLI host for terminal mode)
**Owning Spec:** [spec-skill-agent-autocomplete](./spec-skill-agent-autocomplete.md) (composer integration), partially [spec-hermes-ide](./spec-hermes-ide.md) (IDE-side conventions)
**P-Tier:** P0
**Runtime:** VSCode ✅ · CLI ✅ · zed ✅
**Depends on:** F1 (create session), F43 (thread-state invariants)

**Current state:** Composer with text input, file-attach, and submit-on-enter is shipping in zed. CLI has line-mode and a TUI mode under construction. VSCode consumes the same `TurnRequest` contract.

**Acceptance criteria pointer:** [spec-skill-agent-autocomplete §Acceptance](./spec-skill-agent-autocomplete.md) for the autocomplete surface; composer baseline is implicit (covered by integration tests in caduceus-zed).

**Deferred sub-features:**
- Multi-line markdown preview in composer.
- Voice input.
- Inline image paste (planned for P1, no spec yet).

#### 5.1.2. F20 — Skill autocomplete

**Owner:** zed UI
**Owning Spec:** [spec-skill-agent-autocomplete](./spec-skill-agent-autocomplete.md)
**P-Tier:** P0
**Runtime:** VSCode 🚧 · CLI 🚧 · zed ✅
**Depends on:** F42 (composer), F21 (skill registry)

**Current state:** `/skill` and `@skill` triggers in the composer surface a fuzzy-matched picker over the installed skill registry. Shipping in zed; CLI TUI port in progress.

**Acceptance criteria pointer:** [spec-skill-agent-autocomplete §Acceptance](./spec-skill-agent-autocomplete.md).

**Deferred sub-features:**
- Skill argument completion (parameter hints).
- Recently-used skill ranking.

#### 5.1.3. FC-9 — Profile picker (composer)

**Owner:** zed UI
**Owning Spec:** _(none yet)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F30 (API key management), F32 (model picker)

**Current state:** Sketched. Composer needs a one-click profile switcher (Anthropic vs OpenAI vs Azure vs cloud-handoff target) that is distinct from F13 (the *post-denial* profile-switch picker). Today the user switches profiles via settings, not from the composer.

**Acceptance criteria pointer:** _(deferred — needs spec)_.

**Deferred sub-features:**
- Per-skill default profile.
- Profile presets ("draft" / "ship" / "audit").

#### 5.1.4. F32 — Horizon: model picker

**Owner:** runner
**Owning Spec:** _(none yet)_ — see §7 ("Horizon" is the codename for the multi-provider routing layer)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F30, F31

**Current state:** Selecting a model name routes the runner's backend channel to the right provider. Implementation is partial (Anthropic + OpenAI shipping; Azure under construction).

**Acceptance criteria pointer:** _(deferred — needs spec)_.

**Deferred sub-features:** F35 (fallback chains).

---

### 5.2. Session Lifecycle

Sessions are owned by the daemon. The runner is per-session; pause-oldest LRU manages runner pool capacity.

#### 5.2.1. F1 — Create session

**Owner:** daemon
**Owning Spec:** [spec-m-session-lifecycle](./spec-m-session-lifecycle.md)
**P-Tier:** P0
**Runtime:** VSCode ✅ · CLI ✅ · zed ✅
**Depends on:** none

**Current state:** Shipping. `SessionManager::create()` returns a `SessionId` and allocates a runner.

**Acceptance criteria pointer:** [spec-m-session-lifecycle §Acceptance](./spec-m-session-lifecycle.md).

**Deferred sub-features:** none.

#### 5.2.2. F2 — Send first turn

**Owner:** runner
**Owning Spec:** [spec-m-session-lifecycle](./spec-m-session-lifecycle.md)
**P-Tier:** P0
**Runtime:** VSCode ✅ · CLI ✅ · zed ✅
**Depends on:** F1, F42, F43

**Current state:** Shipping. The runner receives a `TurnRequest`, calls the model, and emits the streaming response.

**Acceptance criteria pointer:** [spec-m-session-lifecycle §Acceptance](./spec-m-session-lifecycle.md).

**Deferred sub-features:** none.

#### 5.2.3. F3 — Resume session from disk

**Owner:** daemon
**Owning Spec:** [spec-m-session-lifecycle](./spec-m-session-lifecycle.md)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F1, F39

**Current state:** Designed and partially implemented. On daemon restart, sessions persisted to disk are reconstructed; runners are *not* auto-resumed (user must explicitly resume).

**Acceptance criteria pointer:** [spec-m-session-lifecycle §Resume](./spec-m-session-lifecycle.md).

**Deferred sub-features:** auto-resume on restart (explicit decision: never, to avoid surprise tool execution).

#### 5.2.4. F4 — Fork session

**Owner:** daemon
**Owning Spec:** [spec-m-session-lifecycle](./spec-m-session-lifecycle.md)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F1, F3

**Current state:** Designed. "Fork at turn N" creates a new session sharing the prefix [0..N) of the parent. Implementation in progress.

**Acceptance criteria pointer:** [spec-m-session-lifecycle §Fork](./spec-m-session-lifecycle.md).

**Deferred sub-features:**
- Branch / merge between forks.
- Cross-session diff view.

#### 5.2.5. F5 — Pause-oldest LRU

**Owner:** daemon
**Owning Spec:** [spec-m-session-lifecycle](./spec-m-session-lifecycle.md)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F1

**Current state:** Designed. When the runner pool is at capacity, the oldest idle runner is paused (state serialized) before a new session is admitted. Resume is explicit (F3).

**Acceptance criteria pointer:** [spec-m-session-lifecycle §Pause-Oldest](./spec-m-session-lifecycle.md).

**Deferred sub-features:**
- User-pinned sessions (never pause).
- Background resume on focus.

#### 5.2.6. FC-5 — Multi-window / multi-session

**Owner:** daemon
**Owning Spec:** _(none yet — see §8 OQ-1)_
**P-Tier:** P2
**Runtime:** VSCode ❌ · CLI N/A · zed 🚧
**Depends on:** F1, F3, F5; requires daemon mode (OQ-1)

**Current state:** Sketched. Multi-window UX (two zed windows, same daemon, sessions visible from both) is a design driver for splitting `caduceusd` into a separate process. See `symphony-multirepo-ux.md §Multi-Window` and §8 OQ-1.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Cross-window session migration (drag a session from window A to window B).
- Window-local vs daemon-local notification routing.

---

### 5.3. Permissions

The most intricate subsystem. Spec-A (engine) defines the 9-step `evaluate()` pipeline; UI-A (zed) defines the approval card.

#### 5.3.1. F9 — 9-step permission evaluate()

**Owner:** daemon
**Owning Spec:** [spec-m-permissions](./spec-m-permissions.md)
**P-Tier:** P0
**Runtime:** VSCode ✅ · CLI ✅ · zed ✅
**Depends on:** F14, F15 (tenant policy participates in the pipeline)

**Current state:** Shipping. The pipeline: (1) tenant-policy-deny? (2) explicit-deny grant? (3) skill-scope match? (4) workspace grant? (5) session grant? (6) always-allow grant? (7) profile-default? (8) ask? (9) deny-by-default. Each step has a stable identifier emitted in the audit log.

**Acceptance criteria pointer:** [spec-m-permissions §9-Step Pipeline](./spec-m-permissions.md).

**Deferred sub-features:**
- Per-tool latency budget for the pipeline (currently best-effort).
- Pluggable custom pipeline stages.

#### 5.3.2. F10 — Approval card (4-button)

**Owner:** zed UI
**Owning Spec:** _(see UI-A in `m-spec-analysis-ui.md`; needs a `spec-m-approval-card.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F9

**Current state:** Designed. Four buttons: **Allow** (this call only), **Allow for session**, **Always allow**, **Deny**. The card is the UI for ApprovalBroker.ask(). CLI variant is a four-letter prompt (a/s/A/d).

**Acceptance criteria pointer:** _(see UI-A §Acceptance in `m-spec-analysis-ui.md`; promote to a checked-in spec)_.

**Deferred sub-features:**
- Inline rationale ("why is this being asked?") expandable details.
- Bulk approval ("allow these 5 file edits as one decision").

#### 5.3.3. F11 — Allow-for-session grant

**Owner:** daemon
**Owning Spec:** [spec-m-permissions](./spec-m-permissions.md)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F9, F10

**Current state:** Designed. A grant scoped to the current `SessionId`; auto-revoked when the session ends.

**Acceptance criteria pointer:** [spec-m-permissions §Grants](./spec-m-permissions.md).

**Deferred sub-features:**
- "Allow for next N tool calls" finite grants.

#### 5.3.4. F12 — Always-allow grant

**Owner:** daemon
**Owning Spec:** [spec-m-permissions](./spec-m-permissions.md)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F9, F10

**Current state:** Designed. Persisted across sessions. Subject to tenant policy override (a tenant `deny` will still win against an `always-allow` grant — that is the dual enforcement principle).

**Acceptance criteria pointer:** [spec-m-permissions §Grants](./spec-m-permissions.md).

**Deferred sub-features:**
- Time-boxed always-allow ("for 24 hours").
- Per-skill always-allow scoping (currently per-tool).

#### 5.3.5. F13 — Profile-switch picker

**Owner:** zed UI
**Owning Spec:** _(none yet — distinct from FC-9 the composer profile picker)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode ❌ · CLI 🚧 · zed 🚧
**Depends on:** F9, F10, F32

**Current state:** Designed. When permission step (8) returns ask and the user has a "switch profile and retry" option (e.g., the current profile cannot satisfy the call but a different profile can), the approval card surfaces a profile picker. ST8 PR-B work is wiring `SwitchOutcome::Switched` to re-execute the originally-denied tool.

**Acceptance criteria pointer:** _(deferred — needs spec; ST8 PR-B PR description has provisional acceptance)_.

**Deferred sub-features:**
- Auto-switch (without prompting) when policy permits.

#### 5.3.6. FC-6 — Workspace permission grants

**Owner:** daemon
**Owning Spec:** [spec-m-permissions](./spec-m-permissions.md) (workspace-grant scope) + [spec-multi-repo-workspace-model](./spec-multi-repo-workspace-model.md) (workspace identity)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F9, F12, FC-1

**Current state:** Designed. Step (4) of the 9-step pipeline. A grant scoped to a workspace identity (a multi-repo workspace, not a single repo). Persisted alongside always-allow but keyed by `WorkspaceId`.

**Acceptance criteria pointer:** [spec-m-permissions §Workspace Grants](./spec-m-permissions.md).

**Deferred sub-features:**
- Cross-workspace grant inheritance.

---

### 5.4. Tenant Policy

Spec-D. Policy is fetched from a managed source, enforced both at config-load time and at runtime (dual enforcement). The banner (UI-E) communicates policy state to the user.

#### 5.4.1. F14 — Managed-source resolution

**Owner:** daemon
**Owning Spec:** _(none yet — needs `spec-m-tenant-policy.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F29 (auth)

**Current state:** Designed. Policy is fetched from a tenant-configurable HTTPS endpoint, signed, cached locally with a TTL. On expiry the daemon falls back to the last-known-good policy and surfaces a banner.

**Acceptance criteria pointer:** _(deferred — needs spec)_.

**Deferred sub-features:**
- Multiple managed sources with priority.
- Local override file (explicitly out of scope for security reasons).

#### 5.4.2. F15 — Dual enforcement (load + runtime)

**Owner:** daemon
**Owning Spec:** _(none yet — needs `spec-m-tenant-policy.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F14, F9

**Current state:** Designed. Two enforcement points: (1) at config load, configured profiles/skills/MCP servers that violate policy are rejected; (2) at runtime, step (1) of the permission pipeline rejects tool calls that match a policy `deny`. Both points emit audit-log entries.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Soft-violation mode (warn instead of deny) — explicit non-goal.

#### 5.4.3. F16 — Tenant-policy banner

**Owner:** zed UI
**Owning Spec:** _(see UI-E in `m-spec-analysis-ui.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI N/A · zed 🚧
**Depends on:** F14, F25

**Current state:** Designed. A persistent banner above the chat composer when policy is loaded; turns red on staleness or fetch failure. CLI variant is a single-line status header.

**Acceptance criteria pointer:** _(see UI-E §Acceptance in `m-spec-analysis-ui.md`)_.

**Deferred sub-features:**
- Banner click-through to a policy-details modal.

---

### 5.5. MCP Servers

MCP (Model Context Protocol) servers are external tool providers. The daemon owns the registry; the UI surfaces status.

#### 5.5.1. F17 — MCP server list & toggle

**Owner:** daemon
**Owning Spec:** _(none yet — needs `spec-m-mcp.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F39

**Current state:** Designed. List installed MCP servers; enable/disable per session or per workspace. Subject to tenant policy F15.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Hot-reload of MCP server config.

#### 5.5.2. F18 — MCP status panel

**Owner:** zed UI
**Owning Spec:** _(see UI-G in `m-spec-analysis-ui.md`; optional)_ — see §7
**P-Tier:** P2
**Runtime:** VSCode ❌ · CLI N/A · zed 🚧
**Depends on:** F17

**Current state:** Sketched. A small panel surfacing per-server status (running / stopped / error / N tools available). UI-G is marked optional in the original spec set.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Per-server log tailing.

#### 5.5.3. F19 — MCP configure modal

**Owner:** zed UI
**Owning Spec:** _(none yet)_
**P-Tier:** P2
**Runtime:** VSCode ❌ · CLI 🚧 · zed 🚧
**Depends on:** F17

**Current state:** Sketched. A modal to add a new MCP server (command, env, args). Today this is settings.json editing.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- One-click install from a marketplace (would couple to F22).

---

### 5.6. Skills & Marketplace

Skills are reusable prompt+tool bundles. The daemon owns the registry; ingest/install is daemon-level; the marketplace is a P2 add-on.

#### 5.6.1. F21 — Skill ingest / install

**Owner:** daemon
**Owning Spec:** _(none yet — needs `spec-m-skills.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F39

**Current state:** Designed. A skill is a directory with `SKILL.md` + assets + (optional) tool schema. Install copies into the user-level skill registry; ingest also resolves dependencies on other skills.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Versioning / pinning.

#### 5.6.2. F22 — Skill marketplace

**Owner:** daemon
**Owning Spec:** _(none yet)_
**P-Tier:** P2
**Runtime:** VSCode ❌ · CLI ❌ · zed 🚧
**Depends on:** F21

**Current state:** Sketched. A browseable index of community skills with install-with-one-click. Out of scope until skill auth/sign is designed.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Voting / ranking.
- Skill author identity.

---

### 5.7. Background Tasks

Spec-G (optional). Heartbeat keeps the runner alive; automation runs scheduled flows; the notification center surfaces results to the user.

#### 5.7.1. F23 — Heartbeat

**Owner:** daemon
**Owning Spec:** _(none yet)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F1

**Current state:** Designed. Periodic in-engine pulse keeps the OS power-management state aware of in-flight runs (refcount-style) and surfaces a soft "I'm alive" event for the UI.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Configurable heartbeat interval.

#### 5.7.2. F24 — Background automation

**Owner:** daemon
**Owning Spec:** _(none yet)_
**P-Tier:** P2
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F1, F23, FC-11 (workflow contract)

**Current state:** Sketched. Scheduled or event-triggered runs (e.g., "every PR open, run skill X"). Couples tightly to the repo-owned workflow contract (FC-11).

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** all sub-features deferred until FC-11 is P0.

#### 5.7.3. F25 — Notification center

**Owner:** zed UI
**Owning Spec:** [spec-notice-notification](./spec-notice-notification.md)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI �� · zed 🚧
**Depends on:** F23, F24

**Current state:** Designed. A single-active-toast queue (UI-D invariant) plus a persistent log pane. The CLI variant is `caduceus notifications` (TUI list).

**Acceptance criteria pointer:** [spec-notice-notification §Acceptance](./spec-notice-notification.md).

**Deferred sub-features:**
- OS-level notifications (planned for P0; spec covers the contract).
- Per-skill notification routing.

---

### 5.8. Multi-Repo & Multi-Window

This area is driven by Symphony (the sibling project consuming caduceus) and is the source of FC-1 through FC-5, FC-10, FC-11, FC-12.

#### 5.8.1. FC-1 — Multi-repo workspace selector

**Owner:** zed UI
**Owning Spec:** [spec-multi-repo-workspace-model](./spec-multi-repo-workspace-model.md)
**P-Tier:** P1
**Runtime:** VSCode ❌ · CLI 🚧 · zed 🚧
**Depends on:** F1

**Current state:** Designed. A workspace can contain N repos; the selector lets the user scope a session to a subset of repos. Each session carries a `WorkspaceId` (FC-10).

**Acceptance criteria pointer:** [spec-multi-repo-workspace-model §Acceptance](./spec-multi-repo-workspace-model.md).

**Deferred sub-features:**
- Cross-workspace search.

#### 5.8.2. FC-2 — Runs Panel (right dock)

**Owner:** zed UI
**Owning Spec:** [spec-orchestrator-status-snapshot](./spec-orchestrator-status-snapshot.md) (data source); UI surface needs a `spec-runs-panel.md` — see §7
**P-Tier:** P1
**Runtime:** VSCode ❌ · CLI N/A · zed 🚧
**Depends on:** FC-3, FC-10

**Current state:** Designed. Right-dock panel listing all active and recent runs across all sessions, with retry/cascade visualization (FC-4). Symphony-driven.

**Acceptance criteria pointer:** [spec-orchestrator-status-snapshot §Acceptance](./spec-orchestrator-status-snapshot.md).

**Deferred sub-features:**
- Filtering / grouping by skill.
- Timeline view.

#### 5.8.3. FC-3 — Status snapshot subscription

**Owner:** daemon
**Owning Spec:** [spec-orchestrator-status-snapshot](./spec-orchestrator-status-snapshot.md)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F1, FC-10

**Current state:** Designed. A pull-and-subscribe surface returning a snapshot of all known runs + a delta stream. Consumed by FC-2 (zed) and any external consumer.

**Acceptance criteria pointer:** [spec-orchestrator-status-snapshot §Acceptance](./spec-orchestrator-status-snapshot.md).

**Deferred sub-features:**
- Filtered subscriptions (server-side).

#### 5.8.4. FC-4 — Retry / cascade visualization

**Owner:** zed UI
**Owning Spec:** _(none yet)_ — see §7
**P-Tier:** P2
**Runtime:** VSCode ❌ · CLI 🚧 · zed 🚧
**Depends on:** FC-2, FC-3

**Current state:** Sketched. When a run retries (model error, tool error, permission denial leading to profile switch), the runs panel shows a cascade tree of attempts. CLI variant is a textual cascade.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Per-attempt diff view.

#### 5.8.5. FC-10 — Run identity entity

**Owner:** daemon
**Owning Spec:** [spec-orchestrator-status-snapshot](./spec-orchestrator-status-snapshot.md) (run identity tuple)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F1, FC-1

**Current state:** Designed. A run is identified by `(run_id, thread_id, workspace_id, repo_ref)`. This tuple is the join key across the daemon, the runs panel, the audit log, and Symphony.

**Acceptance criteria pointer:** [spec-orchestrator-status-snapshot §Run Identity](./spec-orchestrator-status-snapshot.md).

**Deferred sub-features:**
- Stable run-id format (UUID v7 vs ULID — open).

#### 5.8.6. FC-11 — Repo-owned workflow contract

**Owner:** daemon
**Owning Spec:** [spec-repo-owned-workflow-contract](./spec-repo-owned-workflow-contract.md)
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F21

**Current state:** Designed. A repo can ship a `WORKFLOW.md` (or equivalent) that declares which skills it endorses, default profiles, tenant-policy assertions. The daemon honors it as a higher-priority skill source.

**Acceptance criteria pointer:** [spec-repo-owned-workflow-contract §Acceptance](./spec-repo-owned-workflow-contract.md).

**Deferred sub-features:**
- Multi-repo workflow composition.

#### 5.8.7. FC-12 — Orchestrator dispatch surface

**Owner:** daemon
**Owning Spec:** _(none yet — Symphony-driven; see `symphony-fit-analysis.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** FC-3, FC-10

**Current state:** Designed. A daemon-level surface that lets a higher-layer orchestrator (Symphony) dispatch runs to the caduceus engine while preserving run identity, status snapshot semantics, and audit trail. Recommended in `symphony-fit-analysis.md` as `spec-orchestrator-dispatch-surface.md`.

**Acceptance criteria pointer:** _(deferred — needs spec)_.

**Deferred sub-features:**
- Cross-engine dispatch (multiple caduceus instances).

---

### 5.9. Agent Handoff

The runner can hand a session off to a hosted agent (cloud handoff) or accept a session from another caduceus instance.

#### 5.9.1. FC-7 — Agent handoff to cloud

**Owner:** daemon (originating side); cloud (target side)
**Owning Spec:** _(none yet — needs `spec-m-agent-handoff.md`)_ — see §7
**P-Tier:** P2
**Runtime:** VSCode ❌ · CLI 🚧 · zed 🚧
**Depends on:** F1, F29, FC-10

**Current state:** Designed. A user clicks "send to cloud"; the daemon serializes session state (history, grants, policy fingerprint) and posts to the OpenClaw gateway, which spins up a hosted runner. The local runner enters a `handed-off` state and observes (read-only) the cloud run.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Cloud → local handback.
- Mid-run handoff (currently only at turn boundary).

#### 5.9.2. F6 — Backend pushConfig

**Owner:** daemon
**Owning Spec:** _(none yet)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F30, F32

**Current state:** Designed. The backend channel (engine → model provider) is configured via a `pushConfig` message that the runner sends on attach and on profile change. Lets the same channel multiplex across multiple sessions/profiles.

**Acceptance criteria pointer:** _(deferred — needs spec)_.

**Deferred sub-features:**
- Backend config hot-swap mid-turn.

#### 5.9.3. F7 — Offline queue + replay

**Owner:** daemon
**Owning Spec:** _(none yet)_
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F6, F8

**Current state:** Designed. When the backend channel is unreachable, outbound events queue locally; on reconnect they replay in order using `lastConfirmedEventId` to dedupe.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Disk-backed queue (currently in-memory).

#### 5.9.4. F8 — `lastConfirmedEventId` ack

**Owner:** daemon
**Owning Spec:** _(none yet)_
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F6

**Current state:** Designed. Every backend reply carries a `lastConfirmedEventId`; the runner uses it both as an ack and as a replay anchor.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** none specific.

---

### 5.10. Build/Test Surface

Build and test results are surfaced to the user as part of agent runs (e.g., the agent runs `cargo test` and the user sees pass/fail without leaving the chat).

#### 5.10.1. F44 — File diff & checkpoint UI

**Owner:** zed UI
**Owning Spec:** _(none yet — needs `spec-checkpoint-ui.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F2, F39

**Current state:** Designed. After a turn that modifies files, the UI shows an inline diff with **Accept** / **Reject** / **Open in editor** actions. Each accept creates a checkpoint (commit-like). CLI variant is `caduceus diff <run_id>`.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Per-hunk accept/reject (currently per-file).
- Cross-checkpoint cherry-pick.

#### 5.10.2. F45 — Build/test results surface

**Owner:** zed UI
**Owning Spec:** _(none yet)_ — see §7
**P-Tier:** P2
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F44

**Current state:** Sketched. When the agent runs a build/test command, the UI parses output (or relies on the agent's structured report) and surfaces a pass/fail badge with click-through to logs.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Test-runner-aware parsing (cargo, jest, pytest) — currently text-only.

#### 5.10.3. F46 — Status bar (engine state)

**Owner:** zed UI
**Owning Spec:** _(none yet)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI N/A · zed 🚧
**Depends on:** FC-3

**Current state:** Designed. A persistent status-bar item showing engine state (idle / running / waiting-for-approval / error) and active session count. Click opens the runs panel (FC-2).

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Per-session click-through.

---

### 5.11. Diagnostics & Telemetry

Logger (Spec-F), telemetry pipeline, diagnostics viewer (UI-F), audit log.

#### 5.11.1. F39 — Logger

**Owner:** daemon
**Owning Spec:** _(see Spec-F in `m-spec-analysis.md`; needs a checked-in `spec-m-logging.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** none

**Current state:** Designed. Structured logs with stable field names; per-subsystem log levels; session-scoped log files; rotation.

**Acceptance criteria pointer:** _(deferred — promote Spec-F)_.

**Deferred sub-features:**
- Remote log shipping (separate from F40 telemetry).

#### 5.11.2. F40 — Telemetry pipeline

**Owner:** daemon
**Owning Spec:** _(none yet)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F39, FC-8

**Current state:** Designed. Structured events emitted to a configurable sink (default: none — opt-in only). All events carry a tenant-policy fingerprint and respect the FC-8 opt-out.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- PII scrubbing rules (currently rely on event-author discipline).

#### 5.11.3. F41 — Diagnostics viewer

**Owner:** zed UI
**Owning Spec:** _(see UI-F in `m-spec-analysis-ui.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode ❌ · CLI 🚧 · zed 🚧
**Depends on:** F39, F40

**Current state:** Designed. A panel surfacing per-session structured diagnostics (last 100 events, filterable by subsystem). CLI variant is `caduceus diagnose <session_id>`.

**Acceptance criteria pointer:** _(see UI-F §Acceptance in `m-spec-analysis-ui.md`)_.

**Deferred sub-features:**
- Live trace recording (replay mode).

#### 5.11.4. FC-8 — Telemetry settings & opt-out

**Owner:** daemon
**Owning Spec:** _(none yet)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F40

**Current state:** Designed. Telemetry is opt-in by default. A settings surface (zed: settings.json key; CLI: `caduceus config telemetry`) toggles emission. Tenant policy may force opt-in or opt-out (dual enforcement F15).

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Per-event-class opt-out.

---

### 5.12. Sandbox

Spec-E. Path validator and access auditor are P1; OS-level sandbox primitives are P2 (DESIGN only — no implementation in this milestone).

#### 5.12.1. F26 — Path validator

**Owner:** daemon
**Owning Spec:** _(none yet — needs `spec-m-sandbox.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** FC-1

**Current state:** Designed. Every file-touching tool call passes its target path through a validator that checks (1) within-workspace, (2) not in deny-list, (3) symlink resolution honored.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Per-tool path scope (currently global per-workspace).

#### 5.12.2. F27 — Access auditor

**Owner:** daemon
**Owning Spec:** _(none yet)_
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F26, F39

**Current state:** Designed. Every validated path access is logged to the audit log with `(run_id, tool, path, decision)`.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** none.

#### 5.12.3. F28 — OS sandbox primitives

**Owner:** daemon
**Owning Spec:** _(none yet — DESIGN only this milestone)_
**P-Tier:** P2
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F26

**Current state:** Sketched. Per-OS primitives (macOS sandbox-exec, Linux landlock/seccomp, Windows AppContainer) to enforce the validator's decisions in-kernel. Explicit non-goal for the current milestone: ship the design only.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- All sub-features deferred (P2 design-only).

---

### 5.13. Authentication & Identity

#### 5.13.1. F29 — OAuth login

**Owner:** daemon
**Owning Spec:** _(none yet)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** none

**Current state:** Designed. Per-provider OAuth flow (Anthropic, Azure, GitHub for cloud handoff). Tokens stored in OS keychain.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** none.

#### 5.13.2. F30 — API key management

**Owner:** daemon
**Owning Spec:** _(none yet)_
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** none

**Current state:** Designed. Bring-your-own-key path for users who prefer not to OAuth. Same keychain backing.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** none.

#### 5.13.3. F31 — Identity refresh

**Owner:** daemon
**Owning Spec:** _(none yet)_
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F29, F30

**Current state:** Designed. Background refresh of OAuth tokens before expiry; on failure, surface a re-auth prompt via the notification center.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** none.

---

### 5.14. Horizon (Multi-Provider Routing)

Internal codename for the runner's provider abstraction.

#### 5.14.1. F33 — Horizon: provider routing

**Owner:** runner
**Owning Spec:** _(none yet)_
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F6, F32

**Current state:** Designed. Same model-id may route to different providers based on profile (Anthropic direct vs Bedrock vs Vertex).

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** F35 (fallback chains).

#### 5.14.2. F34 — Horizon: cost/latency telemetry

**Owner:** runner
**Owning Spec:** _(none yet)_
**P-Tier:** P2
**Runtime:** VSCode 🚧 · CLI 🚧 · zed 🚧
**Depends on:** F33, F40

**Current state:** Sketched. Per-call cost/latency counters surfaced via the diagnostics viewer.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** budget alerts.

#### 5.14.3. F35 — Horizon: fallback chains

**Owner:** runner
**Owning Spec:** _(none yet)_
**P-Tier:** P2
**Runtime:** VSCode ❌ · CLI ❌ · zed ❌
**Depends on:** F33

**Current state:** Sketched. On provider error, automatically fall over to the next provider in a configured chain. Out of scope for the current milestone.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** all.

---

### 5.15. Teams Relay

Multi-user / shared session features. Mostly deferred.

#### 5.15.1. F36 — Teams Relay: session sharing

**Owner:** daemon
**Owning Spec:** _(none yet)_
**P-Tier:** P2
**Runtime:** VSCode ❌ · CLI ❌ · zed ❌
**Depends on:** F1, F29

**Current state:** Sketched. Two users observe the same session; one drives, the other watches. Couples to the same session-snapshot mechanism as FC-7 (cloud handoff).

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:**
- Concurrent multi-driver (CRDT-style) — out of scope; if revisited, see [spec-zed-crdt](./spec-zed-crdt.md) for prior art on CRDT integration.

#### 5.15.2. F37 — Teams Relay: presence

**Owner:** daemon
**Owning Spec:** _(none yet)_
**P-Tier:** deferred
**Runtime:** VSCode ❌ · CLI ❌ · zed ❌
**Depends on:** F36

**Current state:** Deferred.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** all.

#### 5.15.3. F38 — Teams Relay: comments

**Owner:** daemon
**Owning Spec:** _(none yet)_
**P-Tier:** deferred
**Runtime:** VSCode ❌ · CLI ❌ · zed ❌
**Depends on:** F36

**Current state:** Deferred.

**Acceptance criteria pointer:** _(deferred)_.

**Deferred sub-features:** all.

---

### 5.16. Thread State & UI Invariants

#### 5.16.1. F43 — Thread-state lock-after-first-message

**Owner:** zed UI
**Owning Spec:** _(see UI-B in `m-spec-analysis-ui.md`)_ — see §7
**P-Tier:** P1
**Runtime:** VSCode 🚧 · CLI N/A · zed 🚧
**Depends on:** F1, F2

**Current state:** Designed. After the first turn is sent, certain settings (model, profile, workspace scope) are *locked* to that thread. Changing them requires forking (F4). The "race-via-current-id" pattern guarantees stale UI events targeting an old thread state are dropped.

**Acceptance criteria pointer:** _(see UI-B §Acceptance in `m-spec-analysis-ui.md`)_.

**Deferred sub-features:**
- Soft-lock with an explicit "I know what I'm doing" override (explicit non-goal).

---

### 5.17. Build & Release Infrastructure

Listed for completeness; out of scope for spec coverage in this catalog.

#### 5.17.1. F47 — Build matrix

**Owner:** —
**Owning Spec:** _(out-of-scope)_
**P-Tier:** P1
**Runtime:** — · — · —
**Depends on:** none

**Current state:** Build infra concern. Tracked in repo CI configuration, not as a product spec.

**Acceptance criteria pointer:** _(out-of-scope)_.

**Deferred sub-features:** N/A.

#### 5.17.2. F48 — Release packaging

**Owner:** —
**Owning Spec:** _(out-of-scope)_
**P-Tier:** P1
**Runtime:** — · — · —
**Depends on:** F47

**Current state:** Release infra concern. Tracked separately.

**Acceptance criteria pointer:** _(out-of-scope)_.

**Deferred sub-features:** N/A.

---

## §6. Cross-Runtime Matrix

This section transposes §4: rows are features, columns are runtimes. Use this view when answering "what does my runtime support?" or "what's missing on CLI?".

### 6.1. Runtime overview

- **VSCode** — external consumer of the engine contract. Caduceus does not own the VSCode UI; it owns the contract surface that the VSCode extension consumes. Symbols in this column reflect what the engine *exposes* to a VSCode-shaped consumer, not what any particular VSCode extension currently ships.
- **CLI** — `caduceus-cli`. Two modes (line, TUI). Some UI features are N/A in line mode but available in TUI mode; the matrix uses the TUI capability where it exists.
- **zed** — `caduceus-zed`. The reference UI runtime. New features land here first.

Symbol legend (repeat for convenience): ✅ ready · 🚧 designed/under-construction · ❌ not planned · N/A meaningless on this runtime · 🌐 remote-engine only.

### 6.2. Composer & chat surface

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F42 Chat composer | ✅ | ✅ | ✅ |
| F20 Skill autocomplete | 🚧 | 🚧 | ✅ |
| FC-9 Profile picker (composer) | 🚧 | 🚧 | 🚧 |
| F32 Horizon: model picker | 🚧 | 🚧 | 🚧 |
| F43 Thread-state lock-after-first-message | 🚧 | N/A | 🚧 |

### 6.3. Sessions

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F1 Create session | ✅ | ✅ | ✅ |
| F2 Send first turn | ✅ | ✅ | ✅ |
| F3 Resume from disk | 🚧 | 🚧 | 🚧 |
| F4 Fork | 🚧 | 🚧 | 🚧 |
| F5 Pause-oldest | 🚧 | 🚧 | 🚧 |
| FC-5 Multi-window | ❌ | N/A | 🚧 |

### 6.4. Permissions

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F9 9-step evaluate() | ✅ | ✅ | ✅ |
| F10 Approval card (4-button) | 🚧 | 🚧 | 🚧 |
| F11 Allow-for-session | 🚧 | 🚧 | 🚧 |
| F12 Always-allow | 🚧 | 🚧 | 🚧 |
| F13 Profile-switch picker | ❌ | 🚧 | 🚧 |
| FC-6 Workspace grants | 🚧 | 🚧 | 🚧 |

### 6.5. Tenant policy

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F14 Managed-source | 🚧 | 🚧 | 🚧 |
| F15 Dual enforcement | 🚧 | 🚧 | 🚧 |
| F16 Banner | 🚧 | N/A | 🚧 |

### 6.6. MCP

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F17 List & toggle | 🚧 | 🚧 | 🚧 |
| F18 Status panel | ❌ | N/A | 🚧 |
| F19 Configure modal | ❌ | 🚧 | 🚧 |

### 6.7. Skills

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F21 Ingest/install | 🚧 | 🚧 | 🚧 |
| F22 Marketplace | ❌ | ❌ | 🚧 |

### 6.8. Background tasks & notifications

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F23 Heartbeat | 🚧 | 🚧 | 🚧 |
| F24 Background automation | 🚧 | 🚧 | 🚧 |
| F25 Notification center | 🚧 | 🚧 | 🚧 |

### 6.9. Multi-repo / runs panel / multi-window

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| FC-1 Multi-repo workspace selector | ❌ | 🚧 | 🚧 |
| FC-2 Runs Panel | ❌ | N/A | 🚧 |
| FC-3 Status snapshot subscription | 🚧 | 🚧 | 🚧 |
| FC-4 Retry/cascade visualization | ❌ | 🚧 | 🚧 |
| FC-10 Run identity entity | 🚧 | 🚧 | 🚧 |
| FC-11 Repo-owned workflow contract | 🚧 | 🚧 | 🚧 |
| FC-12 Orchestrator dispatch surface | 🚧 | 🚧 | 🚧 |

### 6.10. Agent handoff & backend channel

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| FC-7 Agent handoff to cloud | ❌ | 🚧 | 🚧 |
| F6 Backend pushConfig | 🚧 | 🚧 | 🚧 |
| F7 Offline queue + replay | 🚧 | 🚧 | 🚧 |
| F8 lastConfirmedEventId ack | 🚧 | 🚧 | 🚧 |

### 6.11. Build/test surface

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F44 File diff & checkpoint UI | 🚧 | 🚧 | 🚧 |
| F45 Build/test results surface | 🚧 | 🚧 | 🚧 |
| F46 Status bar | 🚧 | N/A | 🚧 |

### 6.12. Diagnostics & telemetry

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F39 Logger | 🚧 | 🚧 | 🚧 |
| F40 Telemetry pipeline | 🚧 | 🚧 | 🚧 |
| F41 Diagnostics viewer | ❌ | 🚧 | 🚧 |
| FC-8 Telemetry settings & opt-out | 🚧 | 🚧 | 🚧 |

### 6.13. Sandbox

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F26 Path validator | 🚧 | 🚧 | 🚧 |
| F27 Access auditor | 🚧 | 🚧 | 🚧 |
| F28 OS sandbox primitives | 🚧 | 🚧 | 🚧 |

### 6.14. Auth

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F29 OAuth | 🚧 | 🚧 | 🚧 |
| F30 API key | 🚧 | 🚧 | 🚧 |
| F31 Identity refresh | 🚧 | 🚧 | 🚧 |

### 6.15. Horizon

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F33 Provider routing | 🚧 | 🚧 | 🚧 |
| F34 Cost/latency telemetry | 🚧 | 🚧 | 🚧 |
| F35 Fallback chains | ❌ | ❌ | ❌ |

### 6.16. Teams Relay

| Feature | VSCode | CLI | zed |
|---------|--------|-----|-----|
| F36 Session sharing | ❌ | ❌ | ❌ |
| F37 Presence | ❌ | ❌ | ❌ |
| F38 Comments | ❌ | ❌ | ❌ |

### 6.17. Per-runtime gap summary

- **VSCode gaps (❌):** F13 profile-switch picker, F18 MCP status panel, F19 MCP configure modal, F22 skill marketplace, F35 fallback chains, F36/F37/F38 Teams Relay, F41 diagnostics viewer, FC-1 multi-repo selector, FC-2 Runs Panel, FC-4 retry visualization, FC-5 multi-window, FC-7 cloud handoff. *Theme:* VSCode lags on UI surfaces that don't fit the VSCode extension UI vocabulary (right-dock panels, modal pickers).
- **CLI gaps (❌ or N/A):** F16 banner (N/A), F18 status panel (N/A), F35 fallback chains (❌), F36/F37/F38 Teams Relay (❌), F43 thread-state lock-after-first-message (N/A — line mode has no persistent state), F46 status bar (N/A), FC-2 Runs Panel (N/A), FC-5 multi-window (N/A). *Theme:* CLI is missing genuinely-meaningless-on-CLI surfaces (correctly marked N/A) and a few cross-cutting deferred features.
- **zed gaps (❌):** F35 fallback chains, F36/F37/F38 Teams Relay. *Theme:* zed is the reference runtime; only deferred-everywhere features are missing.

---

## §7. Maturity Roadmap

This section groups features by P-tier and identifies the missing specs (the canonical "spec-debt" register).

### 7.1. P0 (ready)

| Feature | Owning Spec |
|---------|-------------|
| F1 Create session | spec-m-session-lifecycle |
| F2 Send first turn | spec-m-session-lifecycle |
| F9 9-step permission evaluate() | spec-m-permissions |
| F20 Skill autocomplete | spec-skill-agent-autocomplete |
| F42 Chat composer | spec-skill-agent-autocomplete + spec-hermes-ide |

P0 features have a checked-in spec, runnable acceptance criteria, and shipping (or merge-pending) implementation.

### 7.2. P1 (designed)

P1 features have a written spec or a design doc; implementation may be partial. Many P1 features in this catalog point to `_(none yet)_` — that is the **spec-debt register**.

#### 7.2.1. P1 features WITH a checked-in spec

- F3, F4, F5 → `spec-m-session-lifecycle.md`
- F11, F12 → `spec-m-permissions.md`
- F25 → `spec-notice-notification.md`
- FC-1 → `spec-multi-repo-workspace-model.md`
- FC-3, FC-10 → `spec-orchestrator-status-snapshot.md`
- FC-6 → `spec-m-permissions.md` + `spec-multi-repo-workspace-model.md`
- FC-11 → `spec-repo-owned-workflow-contract.md`

#### 7.2.2. P1 features WITHOUT a checked-in spec (spec-debt)

These are P1 (designed) only because they have a design write-up in `m-spec-analysis.md` / `m-spec-analysis-ui.md` / `symphony-multirepo-ux.md` / `symphony-fit-analysis.md`. To formally promote them to P1 (spec-checked-in) and unblock P0 promotion, the following specs must be authored:

| Missing Spec | Covers Features |
|--------------|-----------------|
| `spec-m-tenant-policy.md` | F14, F15, F16 |
| `spec-m-mcp.md` | F17, F18, F19 |
| `spec-m-skills.md` | F21, F22 |
| `spec-m-logging.md` (promote Spec-F) | F39 |
| `spec-m-telemetry.md` | F40, FC-8 |
| `spec-m-sandbox.md` (promote Spec-E) | F26, F27, F28 |
| `spec-m-auth.md` | F29, F30, F31 |
| `spec-m-backend-channel.md` | F6, F7, F8 |
| `spec-m-horizon.md` | F32, F33, F34, F35 |
| `spec-m-approval-card.md` (promote UI-A) | F10 |
| `spec-m-thread-state.md` (promote UI-B) | F43 |
| `spec-m-tenant-banner.md` (promote UI-E) | F16 (UI side) |
| `spec-m-diagnostics-viewer.md` (promote UI-F) | F41 |
| `spec-m-mcp-status-panel.md` (promote UI-G) | F18 (UI side) |
| `spec-m-heartbeat.md` (promote Spec-G) | F23, F24 |
| `spec-runs-panel.md` | FC-2 |
| `spec-checkpoint-ui.md` | F44 |
| `spec-build-test-surface.md` | F45 |
| `spec-status-bar.md` | F46 |
| `spec-profile-picker.md` | FC-9, F13 (one spec, two surfaces) |
| `spec-orchestrator-dispatch-surface.md` (Symphony-driven) | FC-12 |
| `spec-m-agent-handoff.md` | FC-7 |

**Spec-debt count:** 22 missing specs covering ~30 features. Authoring order should follow the dependency graph: foundational engine specs (logging, sandbox, tenant policy, auth, backend channel, horizon, mcp, skills, heartbeat) before UI specs (approval card, thread state, banner, diagnostics viewer, status panel, runs panel, checkpoint UI, status bar, profile picker), with cross-cutting specs (agent handoff, orchestrator dispatch surface, build/test) last.

### 7.3. P2 (sketched)

| Feature | Notes |
|---------|-------|
| F18 MCP status panel | Optional in original spec set. |
| F19 MCP configure modal | Settings-driven for now. |
| F22 Skill marketplace | Blocked on skill auth/sign. |
| F24 Background automation | Blocked on FC-11 reaching P0. |
| F28 OS sandbox primitives | DESIGN only this milestone. |
| F34 Horizon: cost/latency telemetry | Awaiting telemetry pipeline. |
| F35 Horizon: fallback chains | Out of scope this milestone. |
| F36 Teams Relay: session sharing | Awaiting CRDT decision. |
| F45 Build/test results surface | Heuristic parsing now; structured later. |
| FC-4 Retry/cascade visualization | Awaiting FC-2/FC-3 stable. |
| FC-5 Multi-window/multi-session | Awaiting OQ-1 resolution. |
| FC-7 Agent handoff to cloud | Awaiting OpenClaw gateway contract. |

### 7.4. Deferred

| Feature | Notes |
|---------|-------|
| F37 Teams Relay: presence | Out of scope. |
| F38 Teams Relay: comments | Out of scope. |

### 7.5. Promotion targets for the next milestone

Recommended P1 → P0 promotions (highest ROI first):

1. **F10 Approval card** — promote UI-A to a checked-in spec; the engine side (F9) is already P0, so this is the single biggest UX-correctness win.
2. **F25 Notification center** — spec is checked in; needs implementation across all three runtimes.
3. **F11 + F12 Grants** — already specified in spec-m-permissions; needs persistence + revocation tests.
4. **FC-3 Status snapshot subscription** — unlocks FC-2 (Runs Panel) and FC-12 (orchestrator dispatch).
5. **F39 Logger** — promote Spec-F; many other features (F40, F41, F27) depend on stable log fields.
6. **F26 Path validator** — promote Spec-E sandbox baseline; unlocks F27 and tightens the engine's deny-by-default story.

Recommended P2 → P1 promotions:

1. **F18 MCP status panel** — small surface, high user-value.
2. **F45 Build/test results surface** — UI exists informally; pinning the contract clarifies ownership.
3. **FC-4 Retry/cascade visualization** — unblocks Symphony's UX vision.

---

## §8. Open Questions

This section records design questions whose resolution affects multiple features in this catalog. Each open question is given an `OQ-N` identifier and is referenced from the relevant feature subsections in §5.

### OQ-1. Daemon process boundary

**Question:** Does `caduceusd` ship as a **separate OS process** with multiple UI clients over local IPC, or as an **in-process library** that each UI host links and runs in-process?

**Why it matters:**
- FC-5 (multi-window/multi-session) **requires** a separate daemon. Two zed windows in single-process mode each have their own engine — they cannot see each other's sessions.
- FC-2 (Runs Panel) is more useful with cross-window visibility, which again wants a daemon.
- Symphony's expectations (`symphony-multirepo-ux.md`) lean toward a daemon for cross-IDE consistency.
- **Counter-argument:** in-process is simpler to ship; lifecycle issues (orphaned daemon, version skew) disappear; tests run faster.

**Decision needed by:** start of FC-2 / FC-5 implementation.

**Default if unresolved:** in-process for the current milestone; design FC-5 such that its eventual landing does not break in-process callers (i.e., the engine API is the same in both modes; only the IPC wrapper differs).

### OQ-2. Run-id format

**Question:** UUID v7 or ULID for `run_id` (FC-10)?

**Why it matters:** Both are sortable; UUIDs are more universally parsed; ULIDs are slightly more compact. A wrong choice is hard to undo because run-ids appear in audit logs, file names, IPC, and external consumers (Symphony, OpenClaw).

**Decision needed by:** FC-10 P1 → P0 promotion.

**Default if unresolved:** UUID v7. Reason: standard library support in more downstream consumers.

### OQ-3. Profile picker convergence

**Question:** Are FC-9 (composer profile picker) and F13 (post-denial profile-switch picker) the **same UI** with two entry points, or two distinct UIs?

**Why it matters:** Convergence means one spec (`spec-profile-picker.md`); divergence means two specs, two test sets, two UX patterns to maintain.

**Decision needed by:** before either FC-9 or F13 promotes to P0.

**Default if unresolved:** treat as one spec with two entry points. Reason: identical underlying state (selected profile, list of compatible profiles), divergent only in trigger.

### OQ-4. Cross-runtime feature parity policy

**Question:** When zed gets a new UI surface, is VSCode parity **required** before zed ships, **followed** after some delay, or **never** required?

**Why it matters:** This catalog's runtime matrix would be cleaner if there were a stated policy. Today, the implicit policy is "zed first, others when possible".

**Decision needed by:** before publicly committing to any runtime's feature list.

**Default if unresolved:** zed-first. Document the gap in this catalog (which is what we already do).

### OQ-5. Cloud handoff lifecycle

**Question:** When FC-7 hands a session off to OpenClaw, is the local session **paused** (resumable later) or **closed** (handoff is one-way)?

**Why it matters:** Resumable adds complexity (need to merge cloud-side mutations back into local state on resume); one-way is simpler but loses local context if the user wants to come back.

**Decision needed by:** FC-7 P2 → P1 promotion.

**Default if unresolved:** one-way (closed). Reason: minimum viable handoff. Resume is a separate feature later.

### OQ-6. Tenant policy revocation propagation

**Question:** When a managed-source policy update arrives during an in-flight session, does it apply to **already-granted** allow-for-session and always-allow grants?

**Why it matters:** Strict (apply to existing grants) is more secure; lenient (apply only to future grants in this session) is less surprising to the user.

**Decision needed by:** spec-m-tenant-policy authoring.

**Default if unresolved:** strict. Reason: tenant policy is the strongest enforcement signal; if a tenant says "deny", the catalog says "deny".

### OQ-7. Skill marketplace identity

**Question:** Skills published to F22 — who signs them? Caduceus-team-only? Any-Verified-publisher? Unsigned-with-warning?

**Why it matters:** Determines whether F22 can ever be enabled by default in tenant deployments.

**Decision needed by:** F22 P2 → P1 promotion.

**Default if unresolved:** signed-by-caduceus-team only for v1. Community publishing is a follow-up.

### OQ-8. Heartbeat power management contract

**Question:** Is heartbeat F23 just a logical pulse, or does it acquire OS power-management refcounts (preventing sleep)?

**Why it matters:** A laptop user running a long agent task expects the laptop not to sleep mid-run; but always-acquire-power is hostile on battery.

**Decision needed by:** spec-m-heartbeat authoring.

**Default if unresolved:** opt-in per-session ("keep awake" toggle in composer or settings).

---

## §9. Spec Index

This section lists every spec referenced in the catalog with a one-line description. Use this as the canonical "table of contents" for `docs/specs/`.

### 9.1. Currently checked in

- [`spec-m-session-lifecycle.md`](./spec-m-session-lifecycle.md) — Session create / send-turn / resume / fork / pause-oldest LRU. Covers F1–F5.
- [`spec-m-permissions.md`](./spec-m-permissions.md) — 9-step `evaluate()` pipeline, ApprovalBroker, grants (allow-for-session / always-allow / workspace), audit trail. Covers F9, F11, F12, FC-6.
- [`spec-multi-repo-workspace-model.md`](./spec-multi-repo-workspace-model.md) — Workspace identity, multi-repo composition, repo refs. Covers FC-1, supports FC-6 / FC-10.
- [`spec-orchestrator-status-snapshot.md`](./spec-orchestrator-status-snapshot.md) — Status snapshot pull + delta stream, run identity tuple `(run_id, thread_id, workspace_id, repo_ref)`. Covers FC-3, FC-10.
- [`spec-repo-owned-workflow-contract.md`](./spec-repo-owned-workflow-contract.md) — Repo-shipped `WORKFLOW.md` declaring endorsed skills, default profiles, tenant assertions. Covers FC-11.
- [`spec-skill-agent-autocomplete.md`](./spec-skill-agent-autocomplete.md) — Composer autocomplete for `/skill` and `@skill`. Covers F20, partial F42.
- [`spec-notice-notification.md`](./spec-notice-notification.md) — Single-active-toast queue + persistent log; OS-level routing. Covers F25.
- [`spec-caduceus-agent-runner-contract.md`](./spec-caduceus-agent-runner-contract.md) — Runner ↔ daemon ↔ host contract surface.
- [`spec-caduceus-orchestrator-algorithm.md`](./spec-caduceus-orchestrator-algorithm.md) — Internal algorithm sketch for the orchestrator.
- [`spec-caduceus-collab-patterns.md`](./spec-caduceus-collab-patterns.md) — Multi-agent collaboration patterns.
- [`spec-hermes-ide.md`](./spec-hermes-ide.md) — IDE-side conventions for the chat panel.
- [`spec-hermes-ide-supplement.md`](./spec-hermes-ide-supplement.md) — Supplementary IDE conventions.
- [`spec-zed-crdt.md`](./spec-zed-crdt.md) — Prior art on CRDT integration in zed (relevant if F36 ever revives).
- [`spec-tree-sitter.md`](./spec-tree-sitter.md) — Tree-sitter integration baseline.
- [`spec-qdrant.md`](./spec-qdrant.md) — Qdrant vector store integration baseline.
- [`spec-e2b.md`](./spec-e2b.md) — E2B sandbox integration baseline.
- [`spec-claw-code.md`](./spec-claw-code.md) — Cleanroom Claude-Code rewrite reference.
- [`spec-claurst-full.md`](./spec-claurst-full.md) — Cleanroom full-rewrite reference.
- [`spec-open-multi-agent.md`](./spec-open-multi-agent.md) — Open multi-agent runtime sketch.

### 9.2. Mentioned but not yet checked in (spec-debt)

See §7.2.2 for the full register. Summary by domain:

- **Engine:** `spec-m-tenant-policy`, `spec-m-mcp`, `spec-m-skills`, `spec-m-logging`, `spec-m-telemetry`, `spec-m-sandbox`, `spec-m-auth`, `spec-m-backend-channel`, `spec-m-horizon`, `spec-m-heartbeat`, `spec-m-agent-handoff`.
- **UI:** `spec-m-approval-card`, `spec-m-thread-state`, `spec-m-tenant-banner`, `spec-m-diagnostics-viewer`, `spec-m-mcp-status-panel`, `spec-runs-panel`, `spec-checkpoint-ui`, `spec-build-test-surface`, `spec-status-bar`, `spec-profile-picker`.
- **Cross-cutting:** `spec-orchestrator-dispatch-surface` (Symphony-driven).

### 9.3. Source design documents (not specs)

These live in the cleanroom session-state and are the primary inputs to this catalog. They are NOT specs (they may be stale; they may contradict each other). The catalog distills them; the specs in §9.1/§9.2 supersede them.

- `m-spec-analysis.md` — engine spec set A–G outline.
- `m-spec-analysis-ui.md` — UI spec set UI-A–UI-G outline.
- `symphony-multirepo-ux.md` — multi-repo / multi-window UX proposal.
- `symphony-fit-analysis.md` — Symphony / caduceus boundary analysis.
- `m-e2e-architecture.md` — 48-feature E2E template (primary feature source).

---

## §10. Change Log

| Date | Author | Change |
|------|--------|--------|
| (initial) | (auto-generated from session) | First draft of the feature catalog. P-tier assignments reflect the state of the source design documents at the time of authoring; promotions and demotions belong in subsequent revisions of this file. |

---

## §11. Conventions for Updating This Catalog

1. **Adding a new feature:** allocate a new `FC-N` ID (`FC-13`, `FC-14`, …). Add a row to §4 and a subsection to §5 in the appropriate group. If the group does not exist, add a new §5.X subsection in the appropriate ordering.
2. **Promoting a feature:** change the P-tier in both §4 and §5. If promoting to P0, ensure the owning spec exists (§9.1) and has runnable acceptance criteria; if not, the promotion is invalid.
3. **Adding a runtime:** extend the matrix columns in §4 and §6. Default new symbols to ❌ unless evidence supports a different mark. Add a runtime overview entry to §6.1.
4. **Resolving an open question:** delete the `OQ-N` from §8, fold the decision into the affected feature subsections in §5, and note the resolution in §10 (change log).
5. **Promoting a missing spec to checked-in:** move the entry from §9.2 to §9.1; cross-link from §5 features that pointed to it; clear the `_(none yet)_` markers and instead link to the new spec.
6. **Changing an owner:** prefer to first split the feature (one feature, one owner). If a feature truly has two owners (e.g., a daemon-side surface and a UI-side surface), document both in the **Owner** field separated by `+`.

This catalog is the authority for "is this feature a thing?". It is **not** the authority for "how does this feature behave?" — that is the owning spec.

