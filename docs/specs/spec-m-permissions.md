# Caduceus Behavioral Specification — Permission System (M-derived)

## Provenance

- **Source repository:** internal Microsoft EMU project, codename "Clawpilot" (referred to here as "M")
- **Source repository path at time of analysis:** `/tmp/m-research/`
- **Commit SHA:** `ffd8b054c8ee6c562a690d70f3e97ba287e8ad8c`
- **Branch at analysis time:** `main`
- **Source M docs consulted:**
  - `docs/architecture/06-permissions.md` (primary, 275 lines) — 9-step pipeline, broker/adapter/card-manager split, per-entity permissions, read-only command classification, dialog UX
  - `docs/architecture/14-backend-abstraction.md` (broker/adapter/card-manager separation, `DesktopCapabilities.evaluateLocalPolicy()` and `showApprovalCard()` wiring, conformance rules)
  - `docs/architecture/12-tenant-admin-controls-test-plan.md` (eval-order assertions, sanitization-invariant assertions, precedence test cases)
  - Companion engine wiring view: `m-e2e-architecture.md` §2.3 (F9–F13), §4.3 (sequence diagram), §4.12 (timeout sequence), §6 Q5 (cancellation audit-source)
- **Analysis date:** 2026-04-10
- **Target repository / commit:** `caduceus` @ current main
- **Target modules:** `caduceus-permissions` (envelope, grant, classifier, plus new sub-modules for the approval broker and per-entity permission registry) and `caduceus-orchestrator` (agent harness wiring, pre-tool hook into MCP, automation/background lifecycle)
- **License basis of source:** Internal Microsoft EMU (no public license). This document is authored under an additional cleanroom protocol — see "Cleanroom Statement" below.

## Cleanroom Statement

This specification carries forward only externally observable behaviours, state machines, data contracts, decision-precedence orders, and architectural invariants. It deliberately excludes:

- source code and source-code structure (no copy of any function body, type definition, identifier name, or comment from the source repository)
- proprietary identifiers (internal codenames beyond the disclosed analysis scope, internal service hostnames, app-registration GUIDs, ingestion keys, AAD tenant IDs)
- internal product naming (the source codename "Clawpilot" is referenced solely as the analysis target; the Caduceus implementation does NOT carry forward any source-side branding)
- error-message strings, log-line strings, UI copy, or any other copyrightable expression
- third-party or Microsoft-internal package names and version pins

The behavioural patterns described here are documented for the purpose of independent re-implementation in Rust. Where a behaviour is industry-standard (e.g., glob deny-wins matching, exponential backoff with cap), it is described from first principles, not by reference to the source.

**Microsoft-internal-EMU-specific cleanroom care:** Because the source is internal Microsoft EMU code under no public license, contributors implementing against this spec MUST NOT consult the source repository directly. Any clarification needed must come from this spec or from a peer-reviewed addendum authored under the same cleanroom protocol. Direct quotation from the source is prohibited; paraphrase to behaviour-only language is the only permitted reference path.

Where this document references identifiers like `ApprovalBroker`, `PermissionCardManager`, or `DesktopCapabilities`, these are used as **behavioural concepts** (the role each module plays in the architecture). The Caduceus implementation MUST choose its own Rust-idiomatic names for the corresponding crates, modules, structs, and methods.

> **Terminology:** This spec uses **approval card** as the canonical term for the user-facing prompt surface. Earlier-revision engine docs in this repo used "permission card"; the two terms are interchangeable and refer to the same surface as described in `spec-m-ui-approval-card.md`.

---

## Table of Contents

1. [System Overview](#1-system-overview)
2. [The 9-Step Evaluate Pipeline](#2-the-9-step-evaluate-pipeline)
3. [Per-Entity Permission Resolution](#3-per-entity-permission-resolution)
4. [User Actions](#4-user-actions)
5. [ApprovalBroker Contract](#5-approvalbroker-contract)
6. [Read-Only Command Classification](#6-read-only-command-classification)
7. [Sanitization Invariants](#7-sanitization-invariants)
8. [Background / Non-UI Sessions](#8-background--non-ui-sessions)
9. [Audit Trail Contract](#9-audit-trail-contract)
10. [State Machine](#10-state-machine)
11. [Cross-Module Wiring](#11-cross-module-wiring)
12. [Open Questions](#12-open-questions)

---

## 1. System Overview

The permission system is the gate through which every tool invocation — shell command, file write, MCP call, URL fetch, custom tool, file upload — MUST pass before the agent harness is allowed to execute it. It is the only place where local policy is consulted, where tenant-administrative blocks are enforced, where the user is prompted for interactive consent, and where a durable audit record of every decision is written.

### 1.1 Position in the engine

Permissions sit between three peer subsystems:

- **Above:** the agent harness (`caduceus-orchestrator::agent_harness`), which receives tool-use intents from a model provider and invokes the permission pipeline before dispatching any tool.
- **Beside:** the MCP manager (`caduceus-mcp`), which surfaces a pre-tool hook on every MCP tool call so external tools cannot bypass the gate.
- **Below:** the audit log sink (`caduceus-telemetry::audit`), which receives one append-only JSONL record per decision and is independent from the diagnostics logger.

Tenant policy (planned `spec-m-tenant-policy.md`) is consulted from inside the pipeline at Steps 1 and 2 but is owned by a separate module so the permission engine itself remains tenant-agnostic and re-usable. Session lifecycle (planned `spec-m-session-lifecycle.md`) owns the `register-per-run` / `unregister-on-finally` hooks that this spec relies on. UI rendering of the approval prompt (planned `spec-m-ui-approval-card.md`) is downstream of the broker contract defined here.

### 1.2 Three-layer architecture

The permission system is split into three layers with strict responsibility boundaries (per `docs/architecture/06-permissions.md` §"Architecture" and `docs/architecture/14-backend-abstraction.md` §"ApprovalBroker is shared infrastructure"):

```
  Backend-specific layer            Shared mechanism layer            UI / surface layer
  ─────────────────────────         ─────────────────────────         ─────────────────────────
  Adapter                           ApprovalBroker                    PermissionCardManager
    (provider-aware)                  (policy-free)                     (presentation-only)
  ───────────────────                ────────────────────              ───────────────────
  evaluate(request) → decision       requestPrompt(req) → Promise      formatDialog(req)
  explainForHuman() hint             auto-deny on timeout              compute tooltips & buttons
  recordInteractiveOutcome()         emit onPending / onResolved /     addCard / respondCard
  logInfraFailure()                    onTimeout                       persistToDisk (where supported)
                                                                       push to renderer
                                                                       optional out-of-band relay
```

- **Adapter** is the only layer that knows which provider/backend is in use. It owns the call to `evaluate()`, optionally pre-computes a human-readable explanation hint, calls the broker, and records the outcome back into policy state.
- **ApprovalBroker** is a pure prompt mechanism. It owns the blocking future, the auto-deny timeout, and event emission. It MUST NOT format anything, run any policy logic, or know what UI surface (if any) is in use.
- **PermissionCardManager** owns all presentation: dialog formatting, tooltip computation, button layout, card state, optional disk persistence of in-flight cards, push-to-renderer events, and any out-of-band relay (e.g., remote notification surface). Both backend variants get identical card behaviour because they share this layer.

This separation is what allows a future "cloud-backend" runtime to be swapped in: the cloud backend supplies its own policy verdict via a `DesktopCapabilities`-equivalent shim and reuses the *same* broker and card manager.

### 1.3 Layered architecture (ASCII)

```
                        ┌─────────────────────────────────────┐
                        │   caduceus-orchestrator             │
                        │   (agent_harness / tool_dispatcher) │
                        └───────────────┬─────────────────────┘
                                        │ tool-call intent
                                        ▼
                        ┌─────────────────────────────────────┐
                        │   Adapter (provider-aware)          │
                        │   evaluate() → approve/deny/prompt  │
                        │   pre-tool hook from caduceus-mcp ──┼──◀── MCP tool calls
                        └───────────────┬─────────────────────┘
                                        │ "prompt" path only
                                        ▼
                        ┌─────────────────────────────────────┐
                        │   ApprovalBroker (mechanism)        │
                        │   blocking future + timeout         │
                        │   onPending / onResolved / onTimeout│
                        └─────┬──────────────────┬────────────┘
                              │                  │
                              ▼                  ▼
              ┌──────────────────────┐   ┌────────────────────┐
              │ PermissionCardManager│   │ optional out-of-   │
              │   format + push      │   │ band relay surface │
              │   to renderer        │   │ (remote consent)   │
              └──────────┬───────────┘   └─────────┬──────────┘
                         │                         │
                         └────────── user response ─┘
                                        │
                                        ▼
                        ┌─────────────────────────────────────┐
                        │   PermissionPolicy state            │
                        │   (entity registry, global config,  │
                        │    pattern whitelist, server map)   │
                        └───────────────┬─────────────────────┘
                                        │ every decision
                                        ▼
                        ┌─────────────────────────────────────┐
                        │   AuditLog sink (JSONL, daily roll) │
                        └─────────────────────────────────────┘
```

### 1.4 Decision shapes

`evaluate()` returns one of three abstract verdicts (per `docs/architecture/06-permissions.md` §"Evaluation Pipeline"):

| Verdict        | Meaning                                                                    | Continues to broker? |
| -------------- | -------------------------------------------------------------------------- | -------------------- |
| `approve`      | The tool call MAY run.                                                     | No                   |
| `deny`         | The tool call MUST NOT run.                                                | No                   |
| `prompt`       | Local policy is silent. The user MUST be asked, or auto-deny if no UI.     | Yes                  |
| `force-prompt` | Tenant policy demands a prompt (Step 2). Skips auto-approve fast paths but still flows through Step 8's deny-prompt gate. See §2.1 carve-out. | Yes (UI sessions only) |

These verdicts are internal. The agent-facing return value is a richer `ApprovalDecision` that distinguishes auto-approve from interactive-approve from infra-failure-fallback (see §9).

### 1.5 Configuration surface

The permission state owned by this module is shaped as:

```
PermissionsConfig {
  autoApproveReadOnly: bool             // gate for shell read-only auto-approve (Step 6)
  allow:               list<pattern>    // shell pattern whitelist (Step 7)
  tools:               map<key, bool>   // structured-tool auto-approve map (Step 7a)
  servers:             map<key, ServerPermission>  // per-server enable + auto-approve (Steps 4, 4b, 5)
}

ServerPermission { enabled: bool, autoApprove: bool }
```

Persisted at the global level in a single user-scoped settings file (caduceus-side filename TBD; the source-side path is implementer-defined and out of scope here). Per-entity copies are persisted alongside the entity definitions themselves (see §3).

A separate `TenantPolicy` shape is supplied by the tenant-policy module and consulted at Steps 1 and 2 only; this spec does not redefine it (see `spec-m-tenant-policy.md` (planned)).

---

## 2. The 9-Step Evaluate Pipeline

`evaluate(request) → verdict` is a strictly ordered precedence ladder. The first step that produces a non-null verdict wins; later steps MUST NOT be consulted, **with one explicit carve-out**: a `force-prompt` sentinel verdict produced by Step 2 (see §2.5) does NOT short-circuit the pipeline at Step 2; it short-circuits *the auto-approve fast paths* (Steps 3, 5, 6, 7a, 7) but still passes through Step 8's deny-prompt gate. This carve-out is what prevents a background (non-UI) session under tenant `forcePrompt: true` from reaching Step 9 with a UI prompt it cannot show — without the carve-out, Step 8 would be skipped and the broker prompt would hang for the full 48h timeout. Each step has a single decision predicate, a single output, and a single audit-source tag.

This section is normative: an implementation that orders the steps differently, merges them, or adds new ones in non-additive positions is non-conformant. New steps MAY be added only as additive, fall-through-when-absent insertions (per `docs/architecture/12-tenant-admin-controls-test-plan.md` §"Backward compatibility").

### 2.1 Step input (common to all steps)

Every step receives the same logical request shape:

| Field         | Type                                          | Description                                                                |
| ------------- | --------------------------------------------- | -------------------------------------------------------------------------- |
| `sessionId`   | string                                        | The session in which the tool call is being attempted.                     |
| `kind`        | enum                                          | One of: `read`, `write`, `shell`, `mcp`, `url`, `custom-tool`, `upload`.   |
| `toolName`    | string                                        | Stable identifier of the tool (e.g., MCP tool name, self-tool name).        |
| `serverName`  | string?                                       | MCP server identifier when applicable.                                     |
| `commandText` | string?                                       | Raw command string for shell tools.                                        |
| `targetPath`  | path?                                         | Filesystem path for write/upload tools.                                    |
| `targetUrl`   | url?                                          | URL for url tools.                                                         |
| `args`        | structured                                    | Tool arguments as supplied by the model.                                   |
| `entityHint`  | enum                                          | Entity context: `user`, `automation`, `background`, `relay`.               |

### 2.2 Step output (common to all steps)

Each step emits either `null` (fall through to next step) or:

| Field           | Description                                                                                 |
| --------------- | ------------------------------------------------------------------------------------------- |
| `verdict`       | `approve` / `deny` / `prompt`.                                                              |
| `auditSource`   | A value from the `AuditDecisionSource` enum (see §9.2).                                     |
| `reasonHint`    | Short free-form tag (non-localised) for telemetry; not a UI string.                          |

### 2.3 Step 0 — Resolve effective `PermissionsConfig`

| Aspect             | Value                                                                                                     |
| ------------------ | --------------------------------------------------------------------------------------------------------- |
| Input              | `sessionId` (and the global registry).                                                                    |
| Decision predicate | `let permissions = sessionPermissions.get(sessionId) ?? globalSettings.permissions ?? FALLBACK_DEFAULTS`. |
| Output             | The `PermissionsConfig` used by Steps 4–7. Step 0 itself emits no verdict.                                |
| Audit-source tag   | none (this step is a setup, not a decision).                                                              |

Invariants for Step 0 (per `docs/architecture/06-permissions.md` §"Per-Entity Permissions"):

1. If a per-entity config is registered for `sessionId`, it FULLY REPLACES global. There is no merging.
2. If neither a per-entity config nor a stored global config is present, a hard-coded conservative fallback is used (no auto-approve, empty allow list, empty tools, empty servers).
3. The sanitization invariants in §7 are applied at this step (or eagerly at write time) so that any downstream step receives a clean config.

### 2.4 Step 1 — Tenant deny

| Aspect             | Value                                                                                                                            |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------------- |
| Input              | request, tenant policy.                                                                                                          |
| Decision predicate | `tenantPolicy.disabledPermissionKinds.contains(request.kind)` OR `tenantPolicy.disabledServers.contains(request.serverName)`.    |
| Output             | `{ verdict: deny, auditSource: "pre-hook-tenant-policy" }`.                                                                      |
| Notes              | `read` MUST be silently stripped from `disabledPermissionKinds` before it reaches this step (see §7).                             |

Tenant deny overrides every user-side allow. It is the highest-precedence rule in the engine and MUST run before any read fast-path.

### 2.5 Step 2 — Tenant force-prompt

| Aspect             | Value                                                                                                                                |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------ |
| Input              | request, tenant policy.                                                                                                              |
| Decision predicate | `tenantPolicy.forcePrompt === true` AND `request.kind !== "read"`.                                                                                                                                       |
| Output             | `{ verdict: force-prompt, auditSource: "user-interactive" (deferred) }` — a sentinel that short-circuits the auto-approve fast paths (Steps 3, 5, 6, 7a, 7) but **passes through Step 8 unchanged**.       |
| Notes              | Reads remain auto-approved even under force-prompt (per `docs/architecture/12-tenant-admin-controls-test-plan.md` §"Read safety"). `force-prompt` is the in-pipeline name; a session reaching Step 9 with this sentinel is treated identically to a normal `prompt` verdict (broker prompt). |

Step 2's effect is implemented by emitting the `force-prompt` sentinel rather than `prompt`. The sentinel still flows through Step 8 (background deny-prompt), where a session flagged `deny-prompt` converts it to `deny` with `auditSource: "policy-auto-deny"` exactly as it would a Step-9 default-prompt. This guarantees that a background runner under tenant `forcePrompt: true` fails closed at Step 8 rather than reaching Step 9 with an unshowable broker prompt and hanging until the 48h timeout. UI-bearing sessions are unaffected — they pass through Step 8 (which only fires for `deny-prompt` sessions, §8.2) and reach Step 9 normally.

### 2.6 Step 3 — Read operations

| Aspect             | Value                                                              |
| ------------------ | ------------------------------------------------------------------ |
| Input              | request.                                                            |
| Decision predicate | `request.kind === "read"`.                                          |
| Output             | `{ verdict: approve, auditSource: "internal-auto-approve" }`.       |
| Notes              | Reads are always safe by definition. Cannot be disabled by tenant.  |

### 2.7 Step 4 — Server disabled

| Aspect             | Value                                                                                            |
| ------------------ | ------------------------------------------------------------------------------------------------ |
| Input              | request, `permissions.servers[request.serverName]`.                                              |
| Decision predicate | Server entry exists AND `enabled === false`.                                                     |
| Output             | `{ verdict: deny, auditSource: "pre-hook-disabled-server" }`.                                    |

### 2.8 Step 4b — Legacy disabled server check

| Aspect             | Value                                                                                                                                       |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------- |
| Input              | request, legacy disabled-server list (kept for back-compat with older settings shapes).                                                     |
| Decision predicate | Legacy list contains `request.serverName`.                                                                                                  |
| Output             | `{ verdict: deny, auditSource: "pre-hook-disabled-server" }`.                                                                               |
| Notes              | Caduceus implementations MAY collapse Step 4 and Step 4b if their settings shape never carried the legacy form, but MUST preserve ordering. |

### 2.9 Step 5 — Server auto-approve

| Aspect             | Value                                                                                            |
| ------------------ | ------------------------------------------------------------------------------------------------ |
| Input              | request, `permissions.servers[request.serverName]`.                                              |
| Decision predicate | Server entry exists AND `enabled === true` AND `autoApprove === true`.                           |
| Output             | `{ verdict: approve, auditSource: "policy-auto-approve" }`.                                      |

### 2.10 Step 6 — Auto-approve read-only commands

| Aspect             | Value                                                                                                              |
| ------------------ | ------------------------------------------------------------------------------------------------------------------ |
| Input              | request, `permissions.autoApproveReadOnly`.                                                                        |
| Decision predicate | `permissions.autoApproveReadOnly === true` AND `isReadOnlyCommand(request)` (see §6).                              |
| Output             | `{ verdict: approve, auditSource: "policy-auto-approve" }`.                                                        |
| Notes              | Only applies to shell-kind tools; non-shell kinds fall through.                                                    |

### 2.11 Step 7a — Structured-tool auto-approve

| Aspect             | Value                                                                                                                  |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------- |
| Input              | request, `permissions.tools`.                                                                                          |
| Decision predicate | `permissions.tools[toolKey(request)] === true`. The tool-key encoding MUST be stable (e.g., `tool:<name>` namespacing).|
| Output             | `{ verdict: approve, auditSource: "policy-auto-approve" }`.                                                            |
| Notes              | Step 7a runs before Step 7 because structured tool keys are more specific than free-form patterns.                     |

### 2.12 Step 7 — Pattern whitelist

| Aspect             | Value                                                                                                              |
| ------------------ | ------------------------------------------------------------------------------------------------------------------ |
| Input              | request, `permissions.allow` (shell-style pattern list, e.g., `git push *`).                                       |
| Decision predicate | At least one pattern in `permissions.allow` matches `request.commandText` under glob/prefix rules.                  |
| Output             | `{ verdict: approve, auditSource: "policy-auto-approve" }`.                                                        |

The pattern matching algorithm MUST be deny-conservative: ambiguous wildcards SHOULD NOT match catastrophic-blast-radius commands (e.g., a pattern of `*` MUST NOT silently match `rm -rf /`). Implementations are free to bound matching with the read-only classifier from §6 as a sanity floor.

### 2.13 Step 8 — Deny-prompt sessions

| Aspect             | Value                                                                                                                          |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------ |
| Input              | session metadata (entity hint).                                                                                                |
| Decision predicate | The session is flagged `deny-prompt` (i.e., it cannot show interactive UI; see §8) AND the running verdict so far is one of `prompt` or `force-prompt`.                                            |
| Output             | `{ verdict: deny, auditSource: "policy-auto-deny" }`.                                                                          |
| Notes              | This step exists so background runs (heartbeat, automations, relay-only sessions) cannot block forever waiting for a prompt. It MUST also catch the `force-prompt` sentinel from Step 2 (per §2.1 carve-out) — without this, a background session under tenant `forcePrompt: true` would reach Step 9 with an unshowable prompt.    |

### 2.14 Step 9 — Default prompt

| Aspect             | Value                                                                                                                                  |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------------------- |
| Input              | request, broker handle.                                                                                                                |
| Decision predicate | None — this is the unconditional fallback when no earlier step matched.                                                                |
| Output             | `{ verdict: prompt, auditSource: "user-interactive" }`. Adapter then calls `broker.requestPrompt()` and awaits the user's response.    |
| Notes              | The audit-source tag is only finalised after the user responds (or the broker times out — see §5.4 and §9).                            |

### 2.15 Precedence summary table

| # | Step                          | Verdict on hit | Audit source tag                  |
| - | ----------------------------- | -------------- | --------------------------------- |
| 0 | Resolve config                | (no verdict)   | —                                 |
| 1 | Tenant deny                   | `deny`         | `pre-hook-tenant-policy`          |
| 2 | Tenant force-prompt           | `force-prompt`*| `user-interactive` (deferred)     |
| 3 | Read operations               | `approve`      | `internal-auto-approve`           |
| 4 | Server disabled               | `deny`         | `pre-hook-disabled-server`        |
| 4b| Legacy disabled server        | `deny`         | `pre-hook-disabled-server`        |
| 5 | Server auto-approve           | `approve`      | `policy-auto-approve`             |
| 6 | Auto-approve read-only shell  | `approve`      | `policy-auto-approve`             |
| 7a| Structured-tool auto-approve  | `approve`      | `policy-auto-approve`             |
| 7 | Pattern whitelist             | `approve`      | `policy-auto-approve`             |
| 8 | Deny-prompt sessions          | `deny`         | `policy-auto-deny`                |
| 9 | Default → prompt              | `prompt`       | `user-interactive` (deferred)     |

*Step 2's `force-prompt` is a sentinel verdict, not a terminal: it skips Steps 3–7a but still flows through Step 8, where it converts to `deny` for `deny-prompt` sessions. UI-bearing sessions reach Step 9 with the sentinel collapsed to ordinary `prompt` semantics. See §2.1 carve-out and §2.5.

### 2.16 Test obligations

Implementations MUST carry forward each of the precedence assertions enumerated in `docs/architecture/12-tenant-admin-controls-test-plan.md` §"Permission evaluation order", including:

1. Tenant deny overrides user "allow-all" patterns.
2. Tenant deny overrides global auto-approve.
3. Force-prompt still permits reads.
4. Per-server auto-approve does not leak across servers.
5. Session-scope grants (Step 7/7a after registration) do not survive session deletion.
6. Background deny-prompt fires *only* for sessions flagged as such, not for ordinary user sessions.
7. Default prompt is the *only* path that creates a broker prompt; all other steps return without engaging the broker.

---

## 3. Per-Entity Permission Resolution

### 3.1 Concept

A per-entity `PermissionsConfig` MAY be registered against a session at run start. While registered, that config FULLY REPLACES the global config inside `evaluate()` for any tool call carrying that `sessionId` (per `docs/architecture/06-permissions.md` §"Per-Entity Permissions"). There is no merge, no overlay, no inherited keys — it is a wholesale substitution.

This is the mechanism that makes automations and background tasks safe: the operator can pre-declare the exact set of approvals an automation may use, run the automation, and be guaranteed that approvals granted during that run cannot widen the global config.

### 3.2 Entity classes

| Entity class                  | Where the config lives                                                | Lifecycle                                                                 |
| ----------------------------- | --------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `automation`                  | Inline in the automation definition (alongside its trigger + prompt). | Registered at execution start, unregistered in the run's `finally` block. |
| `background` (heartbeat-like) | Inline in the background-runner settings.                             | Same as automation — register at start, unregister on finally.            |
| `allow-for-session`           | Created on first user click of "Allow for session" (see §4.2).        | Created lazily by cloning the current global config; cleared on session delete or app restart. |

### 3.3 Resolution order (Step 0 detail)

```
evaluate(request):
    permissions = sessionPermissions.get(request.sessionId)
    if permissions is None:
        permissions = settingsManager.get().permissions
    if permissions is None:
        permissions = FALLBACK_PERMISSIONS    # safe defaults: no auto-approve, no allow list
```

Three levels, fall-through, no merging.

### 3.4 Register-per-run lifecycle

The lifecycle MUST be:

```
run starts
    → registerSessionPermissions(sessionId, permissions, onPersistCallback?)
    →    Steps 0–9 of evaluate() now use entity permissions for this sessionId
    →    Tool calls execute, possibly triggering "Always allow" routing (§4.3)
run ends (success, error, OR timeout)
    → unregisterSessionPermissions(sessionId)   [in finally / equivalent guard]
```

Invariants:

1. Cleanup MUST be guaranteed via a structured guard (Rust `Drop` / scoped guard / `finally` equivalent), even on panic / abort / timeout / cancellation.
2. Between runs of the same automation, the session has NO registered permissions and falls through to global. This means permissions changes between runs are picked up on next run automatically.
3. If a user manually opens an automation's pinned session interactively (outside an automation run), global permissions apply — there is no "sticky" entity scope.
4. If a tool call is in flight when the run is being torn down, the permission decision in flight MUST complete (or the broker MUST timeout) before unregister fires; the registry MUST NOT yank the config out from under an in-flight call.

### 3.5 In-memory updates take effect within the same run

When "Always allow" is clicked during an automation/background run (see §4.3), the new rule MUST be:

1. Persisted to the entity's stored config on disk (so future runs inherit it), AND
2. Mirrored into the in-memory entity-permissions registry for the *current* `sessionId` so the next tool call within the same run consults the updated config without a re-register cycle.

This dual write is what makes "Always allow" feel immediate during a long-running automation.

### 3.6 Background-runner default config derivation

When a background runner has no inline permissions config, the engine MUST derive its effective config as:

```
derived = clone(globalPermissions)
derived.autoApproveReadOnly = true
register derived for the runner's session
flag the session as deny-prompt (see §8)
```

Auto-approve-read-only is forced on for background runs because reads cannot be denied (Step 3) and the alternative would be that every read decision is silently auto-approved with the same outcome but with extra audit churn. Forcing the flag on makes the configuration explicit (per `docs/architecture/06-permissions.md` §"Heartbeat").

### 3.7 Migration of legacy "all-yes" toggles

Where a legacy skip-all-approvals boolean flag exists on a background runner ("yes to everything" mode), it MUST migrate on first load to:

```
permissions = clone(globalPermissions)
permissions.autoApproveReadOnly = true
[other auto-approve fields left at their conservative defaults]
```

This is intentionally more conservative than the legacy flag's behaviour; users who want broader auto-approve are required to opt in via the granular UI (per `docs/architecture/06-permissions.md` §"skip-all-approvals legacy boolean migration").

---

## 4. User Actions

An approval card surfaces four user actions. Each MUST have well-defined persistence semantics. The four canonical actions, named identically to UI-A §2 ("Allow-once / Allow-for-session / Always-allow / Deny"), are:

| Action                | Verdict   | Persistence                                                                                       |
| --------------------- | --------- | ------------------------------------------------------------------------------------------------- |
| Allow-once            | `approve` | None. One-time approval for this specific request.                                                |
| Allow-for-session     | `approve` | Stored in the session's per-entity config; cleared on session delete or app restart (see §4.2).   |
| Always-allow          | `approve` | Persisted to disk; routed per §4.3.                                                               |
| Deny                  | `deny`    | None. One-time denial.                                                                            |

This four-action vocabulary is normative for **grant** prompts (`request_grant` / `PendingGrant` flows). The **profile-switch** surface today is asymmetric: it carries only a 2-action vocabulary (Switch ↔ Allow-once-equivalent / Stay-and-Deny ↔ Deny-equivalent), per `SwitchUserChoice::{Switch, StayAndDeny}` in `acp_thread::AcpThread`. UI-A §6.2 makes this asymmetry normative on the UI side.

### 4.1 Allow-once (one-time)

The simplest action. The request is approved. No state change in any config. The next equivalent request will hit the same prompt.

### 4.2 Allow for session

On click:

1. If the session does not yet have a per-entity config registered, the engine MUST lazily create one by cloning the current global config (lazy session-config creation per session, per `docs/architecture/06-permissions.md` §"Per-Entity Permissions").
2. The approved pattern (or tool key, or server name) MUST be added to the appropriate field of the cloned config:
   - shell pattern → `permissions.allow`
   - structured tool → `permissions.tools[toolKey]`
   - server name → `permissions.servers[name].autoApprove`
3. The cloned config MUST be registered against the current `sessionId` so subsequent tool calls in the same session short-circuit at Step 5/7/7a.
4. The cloned config MUST be cleared on session delete or app restart. It MUST NOT be persisted to disk in the global settings file.

Allow-for-session is the user's "trust this for now, but don't make it permanent" lever.

### 4.3 Always allow — routing rule

This is the most subtle action. The destination of the persisted rule depends on whether the session is running under per-entity permissions or under the global config (per `docs/architecture/06-permissions.md` §"Always Allow Routing"):

| Active config at click time         | Destination of the new rule                                                          | In-memory effect                          |
| ----------------------------------- | ------------------------------------------------------------------------------------ | ----------------------------------------- |
| Entity has custom permissions       | The entity's stored config on disk (automation JSON / background settings / etc.).   | Entity registry updated for current run.  |
| Entity uses global permissions      | The global settings file on disk.                                                    | Global config updated; future sessions inherit. |

This routing rule is what prevents automation-specific approvals from leaking into the global config. The adapter / broker MUST therefore know, at resolution time, *which* config was active for the session, and the registration API MUST accept an optional `onPersist` callback that the adapter calls to perform the entity-side write.

Implementation invariants:

1. The decision of where to route MUST be made from the resolved config in Step 0, not from the request itself.
2. If an `onPersist` callback was provided at register time, the adapter MUST invoke it for "Always allow" actions instead of writing to the global settings file.
3. If no `onPersist` callback was provided (i.e., the session is running under global permissions), the adapter MUST write to the global settings file.
4. The in-memory entity registry MUST be updated synchronously with the disk write so the next tool call within the same run sees the new rule (§3.5).

### 4.4 Deny (one-time)

The request is denied. No state change. The model receives a denial result and may choose to retry, alter its plan, or surface an error to the user. The denial MUST be recorded in the audit log with `userAction: "deny"` and `auditSource: "user-interactive"`.

### 4.5 Cancellation

If the session is cancelled (user clicks Stop, or upstream abort propagates) while a card is awaiting input, the card MUST be auto-removed and the in-flight permission promise MUST be resolved as `deny`. The audit-source tag for cancellation is treated as `user-interactive` with `userAction: "deny"` (the cancellation distinction is recorded via `source` / `sdkResult` semantics rather than a dedicated `userAction` value — see §9.4 and §12 Q5).

### 4.6 Action ↔ ACP option mapping

The canonical four-action vocabulary above maps onto the `acp::PermissionOptionKind` enum exposed by today's ACP grant surface (see `acp_thread::AcpThread::request_tool_authorization` and the resolution path in `acp_thread.rs` ~L2219). Implementation status is called out per row:

| Canonical action     | ACP `PermissionOptionKind`     | Status                                                                                              |
| -------------------- | ------------------------------ | --------------------------------------------------------------------------------------------------- |
| Allow-once           | `AllowOnce`                    | In source today.                                                                                    |
| Always-allow         | `AllowAlways`                  | In source today.                                                                                    |
| Deny                 | `RejectOnce`                   | In source today (also see `RejectAlways`, currently routed as a deny-equivalent — distinction TBD). |
| Allow-for-session    | *(no ACP option)*              | **[PROPOSED — not currently in ACP]**. The ACP surface does not carry a session-scoped affirmative; today's UI MUST hide this action (or render it disabled with a tooltip explaining future-work status). Engine-side Allow-for-session lifecycle (§4.2) is implementable today; the gap is the wire-protocol option. |

**Implementation status:** the four-action vocabulary above is the *target* surface. Today's ACP grant exposes only three of the four; `Allow-for-session` is future work pending an ACP option-kind extension. Profile-switch surfaces (per §4 lead-in) carry only 2 actions today and are out of scope for this mapping table.

---

## 5. ApprovalBroker Contract

### 5.1 Role

`ApprovalBroker` is the policy-free shared mechanism that owns interactive prompt lifecycle. It MUST be reusable across every adapter (provider-aware layers above it). It MUST NOT format anything, run any policy, or know which UI surface (if any) is consuming its events.

### 5.2 Public surface (behavioural)

The broker MUST expose:

| Operation                           | Behaviour                                                                                                                                                                           |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `requestPrompt(brokerRequest)`      | Returns a future that resolves with `{ resolved, decision, userAction }` or `{ timedOut: true }`. Idempotent for a given `requestId` (a duplicate call returns the same future).      |
| `resolve(requestId, decision)`      | Resolves the in-flight future for `requestId`. No-op if already resolved or timed out.                                                                                              |
| `cancel(requestId)`                 | Cancels the in-flight future and emits `onResolved` with a cancellation marker. Used on session cancellation.                                                                       |
| `onPending(handler)`                | Subscription: fired when a new prompt enters the pending state.                                                                                                                      |
| `onResolved(handler)`               | Subscription: fired when a prompt is resolved (by user, by API caller, by cancellation).                                                                                            |
| `onTimeout(handler)`                | Subscription: fired when the auto-deny timer elapses without a resolution.                                                                                                          |

`brokerRequest` carries only **identity** (`permissionRequest`, `requestId`, `sessionId`) plus an optional pre-computed `explanation` hint. The broker MUST NOT derive any UI text from the request itself.

### 5.3 Default timeout

The broker MUST auto-deny any prompt that has not been resolved after a configurable default timeout. The default value is **48 hours** (per `docs/architecture/06-permissions.md` §"Architecture" — `ApprovalBroker.DEFAULT_TIMEOUT_MS`).

Rationale: 48 hours is long enough that a user can return after a weekend break and still find the prompt actionable in the audit log, but short enough that abandoned prompts do not accumulate indefinitely.

The timeout MUST be:

1. Tunable per-deployment (a daemon running headless might want a much shorter timeout).
2. Tunable per-request (reserved for future use; M does not currently exercise this).
3. Reset only on explicit broker re-issue, not on UI-surface re-render.

### 5.4 Auto-deny semantics

When the timeout fires:

1. The broker resolves the in-flight future as `deny`.
2. `onTimeout` is emitted.
3. The card manager removes the card and emits its own `cards-changed` (or equivalent) event.
4. The audit-log entry written for this decision MUST tag `auditSource: "infra/timeout"` (i.e., the **infrastructure** classification — distinct from a user-driven deny).
5. The agent-facing return value MUST be the same as a user-driven interactive deny: `denied-interactively-by-user` (per `docs/architecture/06-permissions.md` §"Permission Flow" — adapter `recordInteractiveOutcome()` vs `logInfraFailure()`).

The dual classification — `infra/timeout` for telemetry, `denied-interactively-by-user` for the agent's view — is intentional. The model MUST NOT be able to distinguish a user clicking Deny from a 48h timeout, or it will learn to wait users out. Operators looking at the audit log MUST be able to tell the two apart so they can investigate UX issues with abandoned prompts.

### 5.5 Other infra-failure denials

Two additional infra-failure paths MUST also produce `deny` with their own audit tags but the same agent-facing classification (`denied-interactively-by-user`):

| Failure                                                                     | Audit-source tag             |
| --------------------------------------------------------------------------- | ---------------------------- |
| No window/UI surface available (agent runs in a context with no UI to show).| `no-window`                  |
| Adapter has no callback installed (configuration error or boot race).       | `no-callback-fallback`       |

These exist so that boot races and headless misconfigurations fail closed rather than blocking forever (per `m-e2e-architecture.md` §2.3 F9 step 9 "Failure modes").

### 5.6 Event ordering invariants

For any single `requestId`, the broker MUST emit events in this order:

```
onPending → (onResolved | onTimeout) [exactly one terminal event]
```

`onPending` MUST fire before the future returned by `requestPrompt` is awaitable from a subscriber's perspective. `onResolved` and `onTimeout` MUST be mutually exclusive: a request that times out MUST NOT also emit `onResolved`, and vice versa.

### 5.7 Multi-resolve (out-of-band)

When an out-of-band UI surface (e.g., a remote relay) wishes to resolve multiple pending prompts at once with the same decision (a "respond to all" gesture), it MUST iterate the broker's pending list and call `resolve(requestId, decision)` on each. The broker itself does NOT expose a bulk resolve operation; bulk semantics are the caller's responsibility (per `m-e2e-architecture.md` §2.3 F13).

### 5.8 Persistence of in-flight cards

In-flight cards SHOULD be persisted by the card manager (not the broker) so that an app restart while a prompt is pending can re-surface the card on next launch. The broker itself is in-memory only; if the process dies, the broker is gone and the agent run that issued the prompt is also gone (sessions are durable, but the in-flight tool call is not). This boundary is intentional: the broker's contract is purely about live in-process consent.

### 5.9 Resolved-Kind Notices

After every terminal transition of `ApprovalBroker.resolve(_)` (whether `Approved`, `Denied`, `TimedOut`, or `Cancelled` per §10.2), the engine MUST emit one `ContextNotice` of kind `grant.resolved`. Symmetrically, after every terminal transition of a profile-switch decision, the engine MUST emit a `ContextNotice` of kind `profile_switch.resolved`. Payload: `{ id, outcome, kind }` where `id` is the `requestId` (or `tool_use_id` for switches), `outcome` ∈ `approved | denied | timed-out | cancelled` (1:1 with §5.10's terminal), and `kind` is the literal string above.

**Purpose.** UI-side picker dismissal and persistent-notice cleanup. The host UI receives both the direct `AcpThreadEvent::*Resolved` (via the bridge) and the resolved-kind `ContextNotice`; rendering layers MAY rely on the notice for idempotent cleanup if they missed the direct event. Cleanup MUST be idempotent — the notice is safe to receive multiple times. Cross-references: UI-A §4.2; sister spec `spec-m-ui-notice-and-notification.md` for the notice channel itself.

**Implementation status:** not yet in source. Today's bridge emits `AcpThreadEvent::*Resolved` but does not yet attach a resolved-kind `ContextNotice`. Wiring this is an engine-side adapter task.

### 5.10 ApprovalDecision Enum

`ApprovalDecision` is the agent-facing terminal value returned to the orchestrator from a permission round-trip. It is referenced from §1.4, §2.15, §10.4, and §11.10. The enum is closed:

| Variant                          | Emitted when                                                                                                       |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `approved-once`                  | User clicked Allow-once (§4.1).                                                                                    |
| `approved-always`                | User clicked Always-allow (§4.3).                                                                                  |
| `approved-for-session`           | User clicked Allow-for-session (§4.2). **[PROPOSED via §4.6 ACP gap — not emitted today.]**                         |
| `denied-by-policy`               | Auto-deny from Step 1, Step 4/4b, Step 8.                                                                           |
| `denied-interactively-by-user`   | User clicked Deny (§4.4) OR session cancelled (§4.5 / §9.4) — agent-indistinguishable from a click.                |
| `denied-by-timeout`              | Broker timeout fired (§5.4). **MUST be agent-indistinguishable from `denied-interactively-by-user`** per §5.4 dual-classification — the model MUST NOT learn to wait users out. The distinction is for telemetry / operator review only; the harness MUST NOT branch on it. |
| `cancelled-by-engine`            | Engine-side cancellation independent of the user (orchestrator aborted parent turn before user responded). Maps to `source: "user-interactive"`, `userAction: "deny"` per §9.4. |

Auto-approve outcomes from Steps 3, 5, 6, 7, 7a do NOT produce an `ApprovalDecision` — they short-circuit before the broker is engaged.

---

## 6. Read-Only Command Classification

Step 6 of the pipeline auto-approves shell commands classified as "read-only" when `permissions.autoApproveReadOnly` is true. The classifier is a small, easily-auditable predicate (per `docs/architecture/06-permissions.md` §"Read-Only Command Detection").

### 6.1 Classifier algorithm

```
isReadOnlyCommand(commandText):
    if commandText contains "|"  → return false
    if commandText contains ";"  → return false
    if commandText contains ">"  → return false
    if commandText.trimStart() starts with any read-only prefix → return true
    return false
```

### 6.2 Read-only prefix list

The classifier MUST recognise at least the following prefixes (the canonical list; implementations MAY add more behind a feature flag, but MUST NOT remove any):

| Group              | Prefixes                                                                                |
| ------------------ | --------------------------------------------------------------------------------------- |
| Git read-only      | `git status`, `git log`, `git diff`, `git branch`, `git show`, `git remote`, `git tag`  |
| File listing/read  | `ls`, `dir`, `cat`, `head`, `tail`, `wc`, `file`, `stat`, `which`, `where`, `type`, `echo` |
| System info        | `pwd`, `whoami`, `hostname`, `uname`, `date`                                            |
| Package query      | `npm list`, `npm ls`, `npm --version`, `npm view`                                       |
| Python package qry | `pip list`, `pip show`, `pip --version`                                                 |
| Interpreter check  | `python --version`, `python3 --version`, `python -c`, `python3 -c`                      |
| Node interpreter   | `node --version`, `node -e`, `node -p`                                                  |
| Curl read-only     | `curl -s` and `curl --silent` (GET-only — implementations MUST reject if `-X POST` etc. are also present) |

### 6.3 Rejection criteria (hard rules)

Even if the prefix matches, the classifier MUST return `false` if any of the following hold:

1. The command contains a pipe character (`|`).
2. The command contains a command separator (`;`).
3. The command contains an output redirect (`>`).

These are blanket blocks rather than case-by-case parsing because the goal is a high-precision (low-false-approve) classifier, not a parser. A command like `cat foo | tee bar` that is technically safe but contains a pipe is forced into the prompt path; this is intentional. Operators who want to allow such commands MUST add them to the pattern whitelist explicitly.

### 6.4 Interaction with Step 7

If a shell command fails the read-only classifier but matches a pattern in `permissions.allow`, Step 7 still approves it. The classifier is a fast-path for commonly-typed read commands; the pattern whitelist is the override mechanism for operator-curated allows.

### 6.5 Security floor

Pattern matching at Step 7 SHOULD treat the read-only classifier as a soft floor: a wildcard pattern that would otherwise match a clearly destructive command MAY be downgraded to a prompt rather than auto-approved, at the implementer's discretion. This is a Caduceus-side hardening hook and is not present in M.

---

## 7. Sanitization Invariants

The permission engine accepts input from three untrusted-ish sources: tenant policy files (deployed by IT admins), user settings files (edited by the user, possibly by hand), and per-entity configs (edited inside automations). Each source MUST be sanitized at read time so that downstream pipeline steps can rely on a well-formed config.

### 7.1 The `read`-stripping invariant

The `read` permission kind MUST be silently stripped from any deny-list at sanitization time (per `docs/architecture/06-permissions.md` §"Per-Entity Permissions" + `docs/architecture/12-tenant-admin-controls-test-plan.md` §"`read` excluded from disableable permission kinds").

Rationale: blocking reads makes the agent non-functional. The model cannot inspect tool definitions, the harness cannot read the workspace, MCP servers cannot enumerate. An admin who attempts to set `disabledPermissionKinds: ["read"]` (in a misconfigured tenant policy or copy-pasted user config) MUST be silently corrected, not have the agent silently break.

The stripping MUST happen at the sanitization layer, BEFORE the config reaches the pipeline. It MUST be observable in startup diagnostics (one structured log line indicating that a `read` entry was stripped, with the source file path) so that operators can detect mis-configurations. It MUST NOT be surfaced as an interactive UI warning to the user (per Q12: the visibility is intentionally low for end-users; operators are expected to read diagnostics).

### 7.2 Unknown-kind drop

Any permission-kind value not in the canonical enum (`read`, `write`, `shell`, `mcp`, `url`, `custom-tool`, `upload`) MUST be silently dropped from any list field. This protects against forward-compat issues where an older version of the engine reads a newer settings file.

### 7.3 Non-string entry filtering

`disabledServers`, `permissions.allow`, and `permissions.tools` keys MUST be filtered so that non-string entries (numbers, nulls, objects) are dropped before pipeline use (per `docs/architecture/12-tenant-admin-controls-test-plan.md` §"Input sanitization").

### 7.4 Whitespace normalisation

Server names and tool keys MUST be trimmed of leading/trailing whitespace at read time. Empty-after-trim entries MUST be dropped.

### 7.5 De-duplication

`permissions.allow` and `permissions.tools` keyspaces MUST be de-duplicated; if a duplicate appears with conflicting boolean values for `tools`, the *last write wins* (mirrors object-merge semantics on the JSON layer).

### 7.6 Unknown server entries

`permissions.servers[k]` entries with unknown shape (missing `enabled` field, or non-boolean `enabled`/`autoApprove`) MUST have their fields defaulted: missing `enabled` → `true` (servers default-on), missing `autoApprove` → `false` (no auto-approve unless explicit).

### 7.7 Sanitization happens once, at boot and on settings reload

Sanitization MUST run:

1. At engine boot when the global settings file is loaded.
2. On every settings reload triggered by a settings-file change watcher.
3. On every per-entity register call (the entity config MUST be sanitized as it enters the registry, not on every `evaluate()`).

Sanitization MUST NOT run inside `evaluate()` itself; the hot path MUST trust its inputs.

### 7.8 Sanitization log line shape

Implementations SHOULD emit a structured diagnostic record per stripped/normalised entry of the shape:

```
{ event: "permissions.sanitized", source: <file-path>, field: <field-name>, dropped: <value-shape> }
```

These records MUST go to the diagnostics logger, not the audit log (the audit log is for *decisions*, not config reads).

---

## 8. Background / Non-UI Sessions

### 8.1 The implicit deny-prompt invariant

A session flagged as `deny-prompt` MUST have any `prompt` verdict from `evaluate()` converted to `deny` at Step 8 (per `docs/architecture/06-permissions.md` §"Heartbeat" + §"Evaluation Pipeline" Step 8).

The conversion rule is:

```
if session.flags.contains("deny-prompt") AND verdict == prompt:
    verdict := deny
    auditSource := "policy-auto-deny"
```

This MUST happen as Step 8 of the pipeline, before the default-prompt fallback, so that no broker future is ever created for a non-UI session.

### 8.2 Which sessions are deny-prompt

A session MUST be flagged `deny-prompt` if any of the following hold:

1. It was created by a background runner (heartbeat, scheduled probe, automation invoked from a trigger).
2. It was explicitly created with no UI surface attached.
3. It was created by a remote-relay-only path with no escalation to a UI surface configured.

Sessions created through normal interactive entry points (user opens a chat, `/resume` from the CLI with a TTY, etc.) MUST NOT be flagged `deny-prompt`.

### 8.3 Why this matters

Without this invariant, a background runner that hits a permission prompt would block waiting for a 48h timeout and then auto-deny. With this invariant, the same runner gets an immediate deny, preserves the run's bounded-time guarantee, and produces a clean `policy-auto-deny` audit record that operators can grep for to identify mis-permissioned runners.

### 8.4 No auto-promote

A `deny-prompt`-flagged session MUST NOT be silently promoted to a UI session if a UI surface later becomes available mid-run. Promotion (if implemented) MUST be an explicit user-triggered action that ends the run and re-issues it as an interactive session.

### 8.5 Combining with auto-approve-read-only

Background runners typically have `autoApproveReadOnly: true` set on their derived permissions (see §3.6) so that the common "read X then summarize" pattern works without prompting. Step 8 only fires if no earlier auto-approve step matched. Operators who want a stricter background runner MAY turn auto-approve-read-only off, accepting that read-only commands not in the prefix list will be denied.

### 8.6 No retry on deny-prompt-deny

When Step 8 denies a tool call, the agent harness MUST NOT auto-retry the same tool call within the same run. Repeated denies of the same call signal a mis-permissioned runner; the run SHOULD surface a single structured error to the operator's notification surface (out of scope for this spec — see `spec-m-background-tasks.md` (planned) once authored).

---

## 9. Audit Trail Contract

### 9.1 Sink behaviour

Every decision produced by `evaluate()` — auto-approve, auto-deny, interactive-resolve, or timeout — MUST produce exactly one append-only JSONL record on a dedicated audit log file (per `docs/architecture/06-permissions.md` and `m-e2e-architecture.md` §2.3 F12 + §4.12).

| Property             | Value                                                                                              |
| -------------------- | -------------------------------------------------------------------------------------------------- |
| Format               | JSONL (one JSON object per line, newline-terminated, UTF-8).                                        |
| Append semantics     | Atomic append. Concurrent writers MUST NOT interleave partial lines.                               |
| Rotation             | Daily, by date stamp in the filename (`permissions-YYYY-MM-DD.jsonl` shape).                       |
| Path                 | A user-scoped data directory; separate from the diagnostics log.                                    |
| Failure handling     | Audit-write failure MUST be logged to the diagnostics logger but MUST NOT block the decision pipeline. |
| Read access          | Read-only surfacing in a diagnostics/admin view; no write-back from the UI.                         |

### 9.2 Record shape

Each audit record carries:

```
{
  timestamp:    string,    // ISO-8601 UTC
  sessionId:    string,
  userLogin:    string,    // primary local identity (e.g., GitHub login)
  subjectId:    string?,   // optional secondary identity; vendor-neutral
  toolCallId:   string?,   // model-side call id when available
  kind:         enum,      // read | write | shell | mcp | url | custom-tool | upload
  toolName:     string,
  serverName:   string?,
  commandText:  string?,   // for shell-kind only
  decision:     enum,      // approve | deny
  userAction:   enum?,     // allow-once | allow-for-session | always-allow | deny | timeout
  source:       AuditDecisionSource,
  sdkResult:    enum?      // model-facing outcome classification, when knowable at write time
}
```

The `subjectId` field is vendor-neutral: implementations MAY populate it from an M365 UPN, an OS user identifier, an IdP-issued subject claim, or any other secondary-identity source available to the host. Format is implementer-defined; consumers of the audit log MUST treat it as an opaque string and SHOULD NOT parse domain-specific structure from it.

The `userAction` enum is exhaustive: `allow-once` and `always-allow` map to the user clicking the corresponding action on the card (§4.1, §4.3); `allow-for-session` maps to §4.2 once `Allow-for-session` is wired through ACP (until then, it never appears in audit records); `deny` covers both interactive deny (§4.4) and cancellation (§4.5, §9.4); `timeout` is emitted when the broker auto-denies on deadline (§5.4). The enum is closed: any other value is non-conformant.

### 9.3 `AuditDecisionSource` enum

The canonical set of source tags (per `m-e2e-architecture.md` §2.3 F12, listing `AuditDecisionSource` enum):

| Tag                              | Emitted from                                                                       |
| -------------------------------- | ---------------------------------------------------------------------------------- |
| `pre-hook-disabled-server`       | Step 4 / 4b.                                                                       |
| `pre-hook-tenant-policy`         | Step 1.                                                                            |
| `pre-hook-file-upload-disabled`  | Tenant-side file-upload kill-switch (when tenant-policy module exposes one).        |
| `internal-auto-approve`          | Step 3 (reads).                                                                     |
| `policy-auto-approve`            | Steps 5, 6, 7, 7a.                                                                  |
| `policy-auto-deny`               | Step 8 (background deny-prompt).                                                    |
| `user-interactive`               | Step 9 → broker → user response (any of the four user actions).                     |
| `infra/timeout`                  | Broker auto-deny on timeout (also see §5.4).                                        |
| `no-window`                      | Adapter detected no UI surface to host the prompt.                                  |
| `no-callback-fallback`           | Adapter has no callback installed (boot/configuration race).                        |

Implementations MAY extend this enum but MUST treat the listed tags as a stable interface for compliance reporting. Adding a new tag is a breaking change for downstream telemetry pipelines.

### 9.4 Cancellation source

Session cancellation while a prompt is awaiting input MUST produce an audit record with:

- `decision: "deny"`
- `userAction: "deny"` (cancellation is recorded as a user-driven deny; the distinction lives in the `source`/`sdkResult` shape, not in `userAction`)
- `source: "user-interactive"`

There is intentionally NO dedicated `cancelled` source in the enum, and no `cancelled` value in the `userAction` enum (per §12 Q5). The cancellation distinction is carried by the broker's `cancel(requestId)` path leaving its own diagnostic trace (§11.11 `permissions.broker.resolved` event), not by audit-record fields.

### 9.5 Non-blocking writes

Audit writes MUST be buffered and flushed asynchronously such that an audit-sink stall does not stall the pipeline. A write failure MUST log a single diagnostic entry and continue; the pipeline MUST NOT enter a deadlock state if the audit sink is full or unmounted.

### 9.6 Separation from diagnostics

The audit log is a compliance artefact and MUST NOT be merged with the general diagnostics/telemetry log. The two have different retention, different rotation, different permissions on disk, and different consumers (compliance review vs. engineering debugging).

### 9.7 Daily rotation rules

Rotation:

1. The active filename MUST be `<prefix>-<YYYY-MM-DD>.jsonl` where the date is the calendar date in UTC at write time.
2. A write at 23:59:59.999 UTC goes into yesterday's file; the next write at 00:00:00.000 goes into today's file.
3. Old files MUST NOT be auto-deleted by the engine; retention is an operator/IT concern.
4. Implementations MUST NOT lock the active file in a way that prevents log shippers from reading the tail.

### 9.8 Read-only diagnostic view

A read-only diagnostic surface MAY render the most recent N audit records to the user (Settings → Diagnostics, etc.), but MUST NOT allow editing, deletion, or replay through the UI. Compliance assumes the audit log is append-only on disk.

---

## 10. State Machine

### 10.1 Pending-approval lifecycle

A single permission request progresses through this state machine:

```
              ┌────────────┐
              │  Created   │ ◀── adapter.evaluate() → prompt verdict
              └──────┬─────┘
                     │ broker.requestPrompt()
                     ▼
              ┌────────────┐
              │  Awaiting  │ ◀── card surfaced; user action pending
              └──┬──┬───┬──┘
                 │  │   │
       resolve() │  │   │ cancel()
                 │  │   │
                 ▼  │   ▼
        ┌───────────┴┐  ┌────────────┐
        │  Resolved  │  │ Cancelled  │
        └────────────┘  └────────────┘
                 ▲
                 │ (user response via any UI surface)
                 │
                 │  timeout fires
                 │       │
                 │       ▼
                 │  ┌────────────┐
                 │  │ TimedOut   │
                 │  └────────────┘
```

### 10.2 States

| State        | Entry condition                                                                                                                | Exit conditions                                          |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------- |
| `Created`    | Pipeline reached Step 9 (or Step 2 forced prompt). Broker has NOT yet been called.                                              | Adapter calls `broker.requestPrompt()`.                  |
| `Awaiting`   | Broker has emitted `onPending`; card manager has surfaced a card; the timeout clock is running.                                  | One of: user resolution, cancellation, timeout.           |
| `Resolved`   | A user response was received (Allow / Allow-for-session / Always-allow / Deny) on any UI surface.                              | Terminal.                                                |
| `Cancelled`  | The session was cancelled or the broker received an explicit `cancel(requestId)` call.                                          | Terminal.                                                |
| `TimedOut`   | The broker's timeout fired without any user response.                                                                          | Terminal.                                                |

### 10.3 Transitions

| From       | To          | Trigger                                                                              |
| ---------- | ----------- | ------------------------------------------------------------------------------------ |
| `Created`  | `Awaiting`  | Adapter invokes `broker.requestPrompt()`; broker registers the future + timeout.     |
| `Awaiting` | `Resolved`  | `broker.resolve(requestId, decision)` is called by adapter from any UI surface.      |
| `Awaiting` | `Cancelled` | `broker.cancel(requestId)` is called (e.g., on session abort).                       |
| `Awaiting` | `TimedOut`  | The timeout (default 48h) elapses without resolution.                                |

### 10.4 Invariants

1. There is exactly one terminal state per request: `Resolved`, `Cancelled`, or `TimedOut`.
2. After a request enters a terminal state, the broker MUST ignore further `resolve()` / `cancel()` calls for that `requestId` (idempotent no-op).
3. The card manager MUST observe the terminal transition (via `onResolved` or `onTimeout`) and remove the card.
4. The audit log MUST receive exactly one record per request, written when the terminal state is entered.
5. The adapter MUST translate the terminal state into the agent-facing `ApprovalDecision`:
   - `Resolved` with `approve` → agent-approve, `auditSource: user-interactive`
   - `Resolved` with `deny` → agent-deny, `auditSource: user-interactive`
   - `Cancelled` → agent-deny, `auditSource: user-interactive`, `userAction: deny` (cancellation distinction is not carried by `userAction`; see §9.4)
   - `TimedOut` → agent-deny, `auditSource: infra/timeout`, but agent-facing classification is still `denied-interactively-by-user` (see §5.4)
6. State transitions MUST be logged to the diagnostics logger at debug-level; they MUST NOT be logged to the audit log (which only carries terminal records).

### 10.5 Multiple concurrent prompts

A single session MAY have multiple in-flight prompts (e.g., the model issues a parallel-tool-call batch). Each prompt has its own state machine; they are independent. Cancellation of the session terminates all of them.

The broker MAY accept and track multiple prompts concurrently. **The broker MUST NOT assume a UI rendering policy.** Specifically:

- a UI surface MAY choose to render concurrent prompts serially (head-of-FIFO visible, others pending — see UI-A §3 for the two-slot model the caduceus-zed surface uses today);
- a UI surface MAY choose to render concurrent prompts simultaneously (one card per prompt);
- a UI surface MAY mix strategies by kind (e.g., serial within grants, parallel across grant + profile-switch).

The engine's contract is purely "I will broker N prompts; resolve each with a per-`requestId` decision." The order in which user resolutions arrive is **not** constrained by the engine — out-of-order resolution is supported (resolving prompt B before prompt A is conformant). Engine-side audit records and `ApprovalDecision` returns are per-prompt, not per-batch.

Cross-link: UI-A §3 (two-slot rendering model) and UI-A §10.5 (broker-boundary). UI-A's serial rendering is one valid policy among several; the engine MUST NOT bake it in.

### 10.6 Worked sequence — interactive approve

The following ASCII sequence diagram tracks one prompt from model intent to audit write. It is normative for the ordering of cross-module messages.

```
 model    harness     adapter     broker     cardMgr     UI       auditSink
  │ tool_use │           │           │           │         │           │
  ├─────────►│           │           │           │         │           │
  │          │ build req │           │           │         │           │
  │          ├──────────►│ evaluate  │           │         │           │
  │          │           │ Step 0..7 │           │         │           │
  │          │           │ Step 9    │           │         │           │
  │          │           │ requestPrompt(req)    │         │           │
  │          │           ├──────────►│           │         │           │
  │          │           │           │ onPending │         │           │
  │          │           │           ├──────────►│ format  │           │
  │          │           │           │           │ surface │           │
  │          │           │           │           ├────────►│           │
  │          │           │           │  ... user thinks ...           │
  │          │           │           │           │ click   │           │
  │          │           │           │           │◄────────┤           │
  │          │           │           │ resolve   │         │           │
  │          │           │           │◄──────────┤         │           │
  │          │           │ verdict   │ onResolved│         │           │
  │          │           │◄──────────┤──────────►│ remove  │           │
  │          │           │ recordOutcome  │      │         │           │
  │          │           ├─────────────────────────────────────────────►│ append
  │          │ approve   │           │           │         │           │
  │          │◄──────────┤           │           │         │           │
  │          │ dispatch tool                                            │
  │ result   │                                                          │
  │◄─────────┤                                                          │
```

The corresponding timeout sequence (per `m-e2e-architecture.md` §4.12) is identical through `onPending`; the divergence is that the user never clicks, the timeout fires, the broker emits `onTimeout`, the adapter calls `logInfraFailure("timeout")`, and the audit record carries `source: "infra/timeout"` while the agent-facing return value is still `denied-interactively-by-user`.

### 10.7 Restart behaviour

If the engine is restarted while a request is in `Awaiting` (this corresponds to §10.6's pending state lasting across a process boundary), the in-memory state is lost. The card manager MAY have persisted a placeholder card to disk (§5.8); on next start, the card MAY be re-surfaced as a record-of-what-was-pending, but it CANNOT be resolved into an answer for the original tool call (the original request is gone). Any UI re-surfacing MUST make this distinction clear to the user (e.g., a label of "Stale — original request expired").

---

## 11. Cross-Module Wiring

### 11.1 Wiring overview

The permission engine is consumed by, and consumes, several peer subsystems:

```
                       ┌──────────────────────────┐
                       │   tenant policy module   │ (planned)
                       └────────────┬─────────────┘
                                    │ TenantPolicy snapshot
                                    │ (consumed at Steps 1, 2)
                                    ▼
        ┌──────────────────────────────────────────────────┐
        │            caduceus-permissions                  │
        │       (this spec — pipeline + broker)            │
        └─┬───────────────┬─────────────────┬──────────┬──┘
          │ evaluate()    │ register/        │ pre-tool  │ audit-write
          │               │ unregister       │ hook      │
          ▼               ▼                  ▼           ▼
    ┌──────────┐   ┌──────────────┐   ┌──────────┐  ┌────────────┐
    │  agent_  │   │  session-    │   │  caduceus│  │  caduceus- │
    │  harness │   │  lifecycle   │   │  -mcp    │  │  telemetry │
    │ (orches- │   │  (planned)   │   │          │  │  (audit)   │
    │  trator) │   │              │   │          │  │            │
    └──────────┘   └──────────────┘   └──────────┘  └────────────┘
```

### 11.2 Hook into agent harness

The agent harness (`caduceus-orchestrator::agent_harness`) MUST call the permission engine at the boundary where a model-issued tool-call intent is converted into an actual tool dispatch. Specifically:

1. On receiving a `tool_use` block from the model provider, the harness MUST construct a `permission request` (§2.1 shape) and call `evaluate()`.
2. If the verdict is `approve`, the harness dispatches the tool and continues.
3. If the verdict is `deny`, the harness MUST emit a `tool_result` with a denial classification back to the model and continue the turn (the model is allowed to react to the denial).
4. If the verdict is `prompt`, the harness MUST suspend the tool dispatch, await the broker's future, and then proceed as `approve` / `deny` once the future resolves.

The harness MUST NOT bypass the permission engine for any tool kind, including self-tools (skills, memory, settings). Self-tools that should be free are exposed through the `internal-auto-approve` audit source (§9.3) by being classified as `kind: read`.

### 11.3 MCP pre-tool hook

The MCP manager (`caduceus-mcp`) MUST install a pre-tool hook so that every MCP tool invocation passes through `evaluate()`. The hook receives the tool call from the MCP client layer, builds a permission request, and calls into this module.

This is a defence-in-depth layer: even if the agent harness path is somehow bypassed (e.g., an MCP server triggers an internal tool call), the hook ensures permissions are still consulted. (Per `docs/architecture/12-tenant-admin-controls-test-plan.md` §"Dual enforcement", which calls out unifying the pre-hook + policy-callback paths into a single `onPreToolUse` mechanism. Caduceus implementations SHOULD adopt the unified-hook design from the start rather than carrying forward M's transitional dual-path architecture.)

### 11.4 Tenant-policy boundary

The tenant-policy module is consulted from inside Steps 1 and 2 of the pipeline. The contract is:

| Aspect                           | Owner                                                         |
| -------------------------------- | ------------------------------------------------------------- |
| Tenant policy schema             | `spec-m-tenant-policy.md` (planned)                            |
| Tenant policy resolution / load  | Tenant-policy module                                           |
| Sanitization (`read` strip etc.) | Tenant-policy module (per §7.1)                                |
| Snapshot consumed at Steps 1, 2  | Tenant-policy module exposes a read-only snapshot              |
| Refresh strategy                 | Tenant-policy module (cache + revalidation)                    |

The permission engine MUST treat the tenant-policy snapshot as immutable for the duration of one `evaluate()` call. Mid-call snapshot rotation is forbidden (it would create non-deterministic precedence).

### 11.5 Envelope module integration

`caduceus-permissions` already exposes a `PermissionEnvelope` for path/network/exec sandboxing. The new pipeline defined here is *complementary* to the envelope:

| Concern                      | Owner                                  |
| ---------------------------- | -------------------------------------- |
| Tenant + entity + user policy| `evaluate()` pipeline (this spec).     |
| Path glob deny-wins matching | Existing `PermissionEnvelope`.         |
| Sensitive-path grants        | Existing `PermissionEnvelope`.         |
| Network / exec policy        | Existing `PermissionEnvelope`.         |
| OS-level sandbox enforcement | `spec-m-sandbox-enforcement.md` (planned). |

Order of consultation: `evaluate()` runs first; if it approves, the envelope still has the right to refuse on path/network/exec grounds. Conversely, if `evaluate()` denies, the envelope is not consulted at all (the pipeline result is final).

### 11.6 Session-lifecycle integration

Per-entity register/unregister MUST be driven by the session-lifecycle module:

1. On automation/background run start, the lifecycle module MUST call `registerSessionPermissions(sessionId, permissions, onPersist?)`.
2. The lifecycle module MUST hold a scope guard (Rust `Drop` / equivalent) that calls `unregisterSessionPermissions(sessionId)` when the run ends.
3. The lifecycle module MUST surface the session's `deny-prompt` flag to the permission engine (read-only, set at session-creation time).

Details of the lifecycle state machine are out of scope for this spec; see `spec-m-session-lifecycle.md` (planned).

### 11.7 UI / approval-card boundary

The UI surface that renders an approval card is *downstream* of the broker. The broker emits `onPending` with a `BrokerRequest` carrying only identity. The card manager consumes that event, formats the dialog, and pushes to the renderer. See `spec-m-ui-approval-card.md` (planned) for the format/tooltip/button contract.

This spec defines only the broker-to-card-manager handshake (§5). The dialog text, button layout, and any localisation are entirely a UI concern.

### 11.8 Telemetry boundary

Audit writes go to a dedicated audit sink (`caduceus-telemetry::audit`). The permission engine MUST NOT write to the general-purpose telemetry/diagnostics logger for decision records, and the audit sink MUST NOT receive non-decision events. This separation is normative (§9.6).

### 11.9 Settings-store boundary

The global `PermissionsConfig` is owned by the settings store. The permission engine consumes a read-only snapshot at boot and on settings reload events; it MUST NOT read the on-disk file directly inside the hot path. Writes to global config (Always-allow targeting global) MUST go through the settings store API so its file-watcher debouncer and atomic-write guarantees apply.

### 11.10 Failure-mode contract with the harness

The agent harness MUST treat the following failure modes from this module as recoverable and continue the turn:

| Failure                                              | Harness behaviour                                                                                                       |
| ---------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| Audit-write failure                                  | Continue. Decision pipeline is authoritative; audit miss MUST log a diagnostic but MUST NOT roll back the decision.      |
| Broker timeout (`infra/timeout`)                     | Continue with `tool_result: denied`. Same agent-facing classification as user-deny.                                      |
| `no-window` / `no-callback-fallback`                 | Continue with `tool_result: denied`. Surface a single diagnostic record so the operator can fix the configuration.       |
| Sanitization-stripped settings entry                 | Continue. The pipeline runs against the sanitized config.                                                                |
| Per-entity register-time error (e.g., disk read fail)| The lifecycle module MUST treat this as a run-start failure and MUST NOT proceed with the run.                            |

Failures the harness MUST NOT mask:

| Failure                                              | Harness behaviour                                                                                                       |
| ---------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `evaluate()` panics or returns a malformed verdict    | Crash the turn with a diagnostic. The model MUST NOT be told the call was approved or denied if the engine itself failed. |
| Broker's terminal-event invariant violated (§5.6)     | Crash the turn. State-machine corruption is non-recoverable.                                                              |
| Audit-log path unmounted / unwritable on boot         | Refuse to start the engine. The audit log MUST exist before any decision is made.                                         |

### 11.11 Telemetry events emitted

In addition to audit records (§9), the engine SHOULD emit the following structured events to the diagnostics logger for observability (these are NOT audit records):

| Event                              | When emitted                                                            | Fields                                                  |
| ---------------------------------- | ----------------------------------------------------------------------- | ------------------------------------------------------- |
| `permissions.evaluate.start`       | Top of `evaluate()`.                                                    | `sessionId`, `kind`, `toolName`.                        |
| `permissions.evaluate.end`         | Bottom of `evaluate()`.                                                 | + `verdict`, `auditSource`, `step`, `durationUs`.       |
| `permissions.entity.register`      | Per-entity config registered.                                           | `sessionId`, `source` (automation/background).          |
| `permissions.entity.unregister`    | Per-entity config unregistered.                                         | `sessionId`.                                            |
| `permissions.broker.pending`       | `onPending` fired.                                                      | `requestId`, `sessionId`.                               |
| `permissions.broker.resolved`      | `onResolved` fired.                                                     | + `userAction`.                                         |
| `permissions.broker.timeout`       | `onTimeout` fired.                                                      | `requestId`, `sessionId`, `elapsedMs`.                  |
| `permissions.sanitized`            | A config entry was stripped/normalised.                                  | `source`, `field`, `dropped`.                            |

These events are operational telemetry; their schema is not part of the compliance contract and MAY evolve.

### 11.12 Backend abstraction (cloud variant)

When a future cloud-backend is in use, the adapter layer is replaced by a thin shim that delegates `evaluate()` to a remote policy service. The broker and card manager remain in-process and unchanged (per `docs/architecture/14-backend-abstraction.md` §"Implementation note"). The conformance contract is:

| Aspect                                                  | Local backend                          | Cloud backend (future)                                   |
| ------------------------------------------------------- | -------------------------------------- | -------------------------------------------------------- |
| Where `evaluate()` runs                                 | In-process pipeline (this spec).        | Remote service; result carried in a backend event.        |
| Where the broker runs                                   | In-process (this spec).                 | Same in-process broker.                                   |
| Where the card manager runs                             | In-process.                             | Same in-process card manager.                             |
| Audit log location                                      | Local disk (this spec).                 | Local disk, mirrored into cloud audit on next sync.       |
| `DesktopCapabilities.evaluateLocalPolicy` equivalent    | n/a                                     | Cloud calls back to ask local policy for local tool calls.|
| `DesktopCapabilities.showApprovalCard` equivalent       | n/a                                     | Cloud calls back to ask local UI to surface a prompt.     |

Caduceus implementations SHOULD design their broker / card manager interfaces to be transport-neutral so a future remote-policy variant does not require a structural rewrite.

---

## 12. Open Questions

This section enumerates places where M's source documentation is silent, ambiguous, or self-contradictory and where Caduceus implementers MUST make a deliberate choice rather than guess. References use the question numbering from `m-e2e-architecture.md` §6 where applicable.

### 12.1 Cancellation audit-source (Q5)

**Issue.** `AuditDecisionSource` enumerates 9 explicit values; none is `cancelled`. The cancellation flow (§4.5, §10) is described as producing an audit record, but no source tag for cancellation is documented.

**This spec's choice.** Cancellation surfaces as `source: "user-interactive"` with `userAction: "deny"` (per §9.4). This treats cancellation as a user-driven deny without expanding the `userAction` enum; the distinction is recorded only via the broker's `cancel(requestId)` diagnostic event (§11.11) and not in the audit record's structured fields.

**Open work.** A future revision MAY introduce a dedicated `cancelled` source tag if compliance reviewers find the current shape ambiguous. Adding it is a breaking change for downstream telemetry pipelines and SHOULD be co-ordinated with the audit-log schema version.

### 12.2 `read`-strip observability

**Issue.** The `read` permission kind is silently stripped from any deny-list at sanitization time (§7.1). M docs note this is observable only as a startup log line. There is no UI surface that warns an admin that their tenant policy contained a stripped `read` entry.

**Risk.** An IT admin who copy-pastes a deny-list including `"read"` will silently get a policy that accepts everything else from the list but quietly removes the read block. They may not notice unless they read diagnostics carefully.

**This spec's stance.** The sanitization invariant is normative (§7.1); the silent behaviour is preserved. A future tenant-policy spec MAY introduce an admin-side validation tool that surfaces this at policy-deploy time. Caduceus implementers SHOULD log a structured diagnostic record per stripped entry (§7.8) so downstream tooling can detect the case.

### 12.3 Unified pre-tool hook vs. dual-path

**Issue.** M's permission policy is consulted via two entry paths (pre-tool MCP hook + SDK permission callback); M docs flag the unification of these into `onPreToolUse` as future work (per `docs/architecture/12-tenant-admin-controls-test-plan.md` §"Dual enforcement").

**Caduceus stance.** Caduceus SHOULD implement the unified hook from the start. There is no compatibility burden for a greenfield implementation, and the dual-path design is a transitional artefact of M's history.

### 12.4 Pattern-whitelist matching algorithm

**Issue.** M's pattern syntax for `permissions.allow` is described as shell-style (e.g., `git push *`) but the exact matching rules (greedy vs. lazy, anchoring, escape rules, character classes) are not formally specified.

**This spec's stance.** Implementations MUST document the matching algorithm explicitly. Recommended starting point: glob-style matching anchored to the start of the trimmed `commandText`, with `*` matching any character run *not including* shell metacharacters (`;`, `|`, `>`, backticks, `$(`). This keeps the classifier conservative (§6) consistent with the whitelist.

### 12.5 Server-key encoding

**Issue.** `permissions.servers[k]` keys are not formally specified. M uses MCP server identifiers; whether these are the user-facing display name, the registered server-id, or an opaque hash is not stated.

**Caduceus stance.** Use the registered server-id (the same identifier that appears in the MCP server registry). Keys MUST be canonicalised (case-folded, trimmed) at sanitization time (§7.4).

### 12.6 Tool-key encoding

**Issue.** `permissions.tools[k]` keys for structured-tool auto-approve are documented as namespaced (e.g., `tool:m365_send_email`) but the namespace contract is not formal.

**Caduceus stance.** Adopt a `<source>:<tool-name>` convention where `<source>` is one of `tool`, `mcp:<server-id>`, `self`. Key collisions MUST resolve as last-write-wins per §7.5.

### 12.7 Cross-session prompt routing

**Issue.** When the same tool is requested concurrently from multiple sessions, M surfaces a card per session. Some out-of-band UI surfaces (e.g., Teams-relay-style) implement a "respond to all" gesture (per `m-e2e-architecture.md` §2.3 F13). The semantics of "all" when the pending prompts span different users / different identity contexts are not formally specified.

**Caduceus stance.** "Respond to all" SHOULD be scoped per-user-identity. A bulk-resolve gesture from user A MUST NOT resolve user B's pending prompts even if both are visible in the same out-of-band surface.

### 12.8 Persistence of in-flight cards across restart

**Issue.** §5.8 states that the card manager SHOULD persist in-flight cards so a restart can re-surface them, but the original tool call is lost on restart (§10.6). Whether a re-surfaced card is actionable or informational is a UX decision.

**Caduceus stance.** Re-surfaced cards on restart MUST be informational (i.e., a record-of-what-was-pending), with their action buttons disabled. The audit log MUST carry an `infra/timeout` (or new tag, e.g., `infra/restart`) record to close the loop on the original request.

### 12.9 Audit-log file locking on read

**Issue.** Daily-rotated JSONL files are commonly tailed by log shippers. M does not specify whether the active file is exclusively locked.

**This spec's stance.** Implementations MUST NOT exclusive-lock the active audit file. Append writes MUST use append-mode writes that do not block readers (§9.7).

### 12.10 Timeout configurability per tenant

**Issue.** §5.3 specifies a 48h default but says it is per-deployment configurable. Whether tenant policy can override the default is not specified.

**Caduceus stance.** Tenant policy MAY declare a maximum timeout (a tenant SHOULD NOT be able to extend the timeout beyond a deployment-wide ceiling, but MAY shorten it). The local user setting MUST be clipped to the tenant maximum at config-load time, with a sanitization log record per §7.

### 12.11 Cancellation propagation order

**Issue.** When a session is cancelled while several prompts are awaiting (§10.5), the order in which `cancel(requestId)` is called across pending prompts is not specified.

**Caduceus stance.** Cancellation MUST iterate pending prompts in *most-recent-first* order, but this is for telemetry tidiness only; correctness MUST NOT depend on the order.

### 12.12 Resurrection of denied calls

**Issue.** When the harness receives a `deny` (whether interactive or from §8 background-deny-prompt), the model may re-issue the same tool call on its next turn. M does not formally treat this as a separate case; each call is independently evaluated.

**Caduceus stance.** Each call is independently evaluated, per M. However, implementations MAY add a soft per-session rate-limit on identical denied tool calls to prevent denial loops. The rate-limit MUST be observability-only at this stage (a diagnostic log line, not a hard cap).

---

*End of specification.*
