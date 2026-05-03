# Caduceus Behavioral Specification — Session Lifecycle

## Provenance

- **Source repository:** internal Microsoft EMU project, codename "Clawpilot" (referred to here as "M")
- **Source repository path at time of analysis:** `/tmp/m-research/`
- **Commit SHA:** `ffd8b054c8ee6c562a690d70f3e97ba287e8ad8c`
- **Branch at analysis time:** `main`
- **Source M docs consulted:**
  - `docs/architecture/04-session-lifecycle.md` (primary)
  - `docs/architecture/14-backend-abstraction.md` (BackendEvent / TurnEvent normalization)
  - `docs/architecture/15-teams-relay.md` (per-session `sendAndCollect` queue, fire-and-forget self-tools)
  - Companion analyses: `m-spec-analysis.md` §4.B, §7; `m-e2e-architecture.md` §2.1, §3.6, §4.2, §4.4
- **Analysis date:** 2026-04-10
- **Target repository / commit:** `caduceus` (target of re-implementation); UI-side companion in `caduceus-zed`
- **License basis of source:** Internal Microsoft EMU (no public license). This document is authored under an additional cleanroom protocol — see "Cleanroom Statement" below.

## Cleanroom Statement

This specification carries forward only externally observable behaviours, state machines, data contracts, decision precedence orders, and architectural invariants. It deliberately excludes:

- source code and source-code structure (no copy of any function body, type definition, identifier name, or comment from the source repository);
- proprietary identifiers (internal codenames beyond the disclosed analysis scope, internal service hostnames, app-registration GUIDs, ingestion keys, AAD tenant IDs);
- internal product naming (the source codename "Clawpilot" is referenced solely as the analysis target; the Caduceus implementation does NOT carry forward any source-side branding);
- error-message strings, log-line strings, UI copy, or any other copyrightable expression;
- third-party or Microsoft-internal package names and version pins.

The behavioural patterns described here are documented for the purpose of independent re-implementation in Rust. Where a behaviour is industry-standard (e.g., LRU eviction, atomic file rename), it is described from first principles, not by reference to the source.

**Microsoft-internal-EMU-specific cleanroom care:** Because the source is internal Microsoft EMU code under no public license, contributors implementing against this spec should NOT consult the source repository directly. Any clarification needed must come from this spec or from a peer-reviewed addendum authored under the same cleanroom protocol. Direct quotation from the source is prohibited; paraphrase to behaviour-only language is the only permitted reference path.

**Companion specs (read together):**
- `spec-m-permissions.md` — the per-entity permission registry consumed by the session manager.
- `spec-m-backend-abstraction.md` (planned) — the `IBackendProvider` contract used to bind sessions to a backend.
- `spec-m-ui-thread-state-invariants.md` (in `caduceus-zed/docs/specs/`) — the UI-side projection of session state.

> **Terminology:** This spec uses **approval card** as the canonical term for the user-facing prompt surface. Earlier-revision engine docs in this repo used "permission card"; the two terms are interchangeable and refer to the same surface as described in `spec-m-ui-approval-card.md`.

---

## Table of Contents

1. [System Overview](#1-system-overview)
2. [Session State Machine](#2-session-state-machine)
3. [Persistence Model](#3-persistence-model)
4. [Session Pool Management](#4-session-pool-management)
5. [Internal & Hidden Sessions](#5-internal--hidden-sessions)
6. [Per-Session Event Serialization](#6-per-session-event-serialization)
7. [Resume Semantics](#7-resume-semantics)
8. [First-Message Lock Invariants](#8-first-message-lock-invariants)
9. [BackendEvent → TurnEvent Normalization](#9-backendevent--turnevent-normalization)
10. [Cross-Backend Visibility](#10-cross-backend-visibility)
11. [Cancellation & Cleanup](#11-cancellation--cleanup)
12. [Cross-Module Wiring](#12-cross-module-wiring)
13. [Open Questions](#13-open-questions)

---

## 1. System Overview

A **session** is the unit of conversation in the system. It is the durable, addressable container for one continuous dialogue between a user (or an internal automation) and the model. Every turn — every user message, every assistant reply, every tool invocation, every permission grant — is scoped to exactly one session. Sessions are independent of one another: they do not share message history, do not share locked-in model selection, and do not share active backend handles.

### 1.1 Session classes

The system distinguishes three classes of session by visibility and origin:

| Class | Visibility | Examples | Sidebar listing |
|---|---|---|---|
| **User-facing** | Always visible to the human operator | Normal chat sessions started by the user; resumed past sessions | Yes |
| **Internal / automation** | Visible to the operator when meaningful, hidden when noisy | Automation runs that produce visible output for the user | Visible only when carrying a referenceable identity (e.g., a known automation) |
| **Hidden / system** | Never surfaced in the sidebar; reachable only programmatically | Condition-check probes, system bot singleton, heartbeat probes | Filtered out of all session list and search surfaces |

Visibility is enforced at the listing layer (see §5). The on-disk representation of a hidden session is identical in shape to a user-facing one; only the `source` and presence/absence of a discriminator field decide which surface lists it.

### 1.2 Session pool & persistence (ASCII)

```
                     ┌────────────────────────────────────────────────┐
                     │           Session Manager (process)            │
                     │                                                │
                     │  ┌─────────────── Active Pool ─────────────┐  │
                     │  │  bounded by MAX_ACTIVE (LRU pause)      │  │
                     │  │                                         │  │
                     │  │  ┌──────┐  ┌──────┐  ┌──────┐  ...      │  │
                     │  │  │ S_a  │  │ S_b  │  │ S_c  │           │  │
                     │  │  │handle│  │handle│  │handle│           │  │
                     │  │  └───┬──┘  └───┬──┘  └───┬──┘           │  │
                     │  └──────┼─────────┼─────────┼──────────────┘  │
                     │         │         │         │                  │
                     │   per-session sendAndCollect queues            │
                     │         │         │         │                  │
                     │  ┌──────▼─────────▼─────────▼──────────────┐  │
                     │  │   Backend Provider (one bound backend)  │  │
                     │  └─────────────────────────────────────────┘  │
                     │                                                │
                     │  ┌─────────────── Paused Set ──────────────┐  │
                     │  │  no provider handle; on-disk only       │  │
                     │  │                                         │  │
                     │  │  S_d (paused, ts=…)  S_e (paused, ts=…) │  │
                     │  └─────────────────────────────────────────┘  │
                     └─────────────────┬──────────────────────────────┘
                                       │
                ┌──────────────────────┴───────────────────────┐
                │                  Persistence                 │
                │                                              │
                │  index.json   ── newest-first metadata only  │
                │  {sid}.json   ── full snapshot bundle/session│
                │  ...                                         │
                └──────────────────────────────────────────────┘
```

The active pool holds in-memory provider handles for the most recently used sessions, bounded by a configured maximum. The paused set is the on-disk projection only — a paused session has no live provider handle but its full state is durable.

### 1.3 Reading-order summary

| Section | Tells you |
|---|---|
| §2 | What a session can be doing at any moment, and which inputs are accepted in each state |
| §3 | Where session state lives on disk and which file is authoritative |
| §4 | How the active pool is bounded and how paused sessions are chosen |
| §5 | Which sessions appear in the sidebar and which are hidden |
| §6 | Why send-and-collect is serialized per session, and where the serialization is bypassed |
| §7 | What "resume" really does, including the resume-on-paused fallback and the rebuild-from-history fallback |
| §8 | Which session metadata is locked once the first user message commits |
| §9 | How heterogeneous backend events are normalized into one renderer-facing event stream |
| §10 | How sessions from different backends are kept distinct without forking call paths |
| §11 | How turn cancellation propagates and how cleanup is ordered |
| §12 | How the session manager surfaces to permissions, MCP, telemetry, tenant policy |
| §13 | Items where doc evidence and observable evidence diverge or are silent |

---

## 2. Session State Machine

### 2.1 States

```
       ┌─────────┐
       │ Created │   identifier allocated, no backend handle yet
       └────┬────┘
            │
            ▼
     ┌──────────────┐
     │ Initializing │  backend handle being established (create or resume)
     └──────┬───────┘
            │
            ▼
        ┌──────┐         turn in flight
        │ Idle │ ───────────────────────► ┌──────┐
        │      │ ◄─────────────────────── │ Busy │
        └──┬───┘    turn complete /       └──┬───┘
           │        cancelled                │
           │                                 │
           │   permission needed             │
           │   (Busy → NeedsInput)           │
           │   user resolved                 │
           │   (NeedsInput → Busy)           │
           │                                 │
           ▼                                 ▼
     ┌────────────┐                    ┌─────────┐
     │ NeedsInput │                    │  Error  │  recoverable? → Idle
     └─────┬──────┘                    └────┬────┘  unrecoverable? → Error (terminal)
           │                                │
           └──────────────┬─────────────────┘
                          │
                          ▼
                    ┌───────────┐
                    │  Paused   │  no handle, durable on disk; resumable
                    └─────┬─────┘
                          │   resume
                          ▼
                    (back to Initializing)

                    ┌───────────┐
                    │ Destroyed │  terminal — handle gone, on-disk state deleted
                    └───────────┘
```

| State | Meaning | Has backend handle? |
|---|---|---|
| Created | Session record allocated; identifier assigned; no backend interaction yet | No |
| Initializing | Backend handle being established (fresh create, resume, or rebuild-from-history) | Pending |
| Idle | Handle live; no turn in flight; ready to accept input | Yes |
| Busy | A turn is being processed end-to-end (model generation, tool calls, streaming) | Yes |
| NeedsInput | A turn is paused waiting for user resolution of an approval card or an explicit ask-user prompt | Yes |
| Error | Turn ended with a failure; recoverable errors return to Idle, unrecoverable errors stay Error until destroyed | Yes (until destroyed) |
| Paused | Session is durable on disk but has no live handle (either LRU-evicted from the active pool or never resumed since startup) | No |
| Destroyed | Terminal. Handle is released and the on-disk record has been removed | No |

### 2.2 Accepted-input matrix

Inputs that reach the session manager from the renderer, the relay, or internal callers must be checked against the session's current state. The matrix below is the contract: an input that is not accepted in a given state MUST be rejected without side effect (no partial work, no orphaned events).

| Input ↓ \ State → | Created | Initializing | Idle | Busy | NeedsInput | Error | Paused | Destroyed |
|---|---|---|---|---|---|---|---|---|
| `send(turn)` | no | no | yes | no | no | no | auto-resume then yes | no |
| `cancel(turn)` | no | no | no-op | yes | yes | no-op | no | no |
| `resolvePermission(cardId)` | no | no | no | no | yes | no | no | no |
| `resume()` | no | no-op (already initializing) | no-op (already live) | no | no | no | yes | no |
| `pause()` | no | no | yes | reject (must wait for idle) | reject | no-op | no-op | no |
| `destroy()` | yes | yes (cancels init) | yes | yes (also cancels turn) | yes (also cancels turn) | yes | yes | no-op |
| `setMetadata(title|notes)` | no | no | yes | yes | yes | yes | yes (writes through to disk) | no |
| `changeModel/personality` | yes (until first user message) | yes (until first user message) | yes only if no user message yet | no | no | no | yes only if no user message yet | no |

"auto-resume then yes" for `send` on a Paused session is the resume-on-paused fallback (see §7) — the send call MUST NOT throw a not-active-style error before attempting resume.

### 2.3 Transition table

| From | To | Trigger | Side effects |
|---|---|---|---|
| Created | Initializing | first `send`, explicit `resume`, or pool admission | begin backend handle establishment |
| Initializing | Idle | backend handle ready | persist initial metadata; emit state-changed |
| Initializing | Error | backend create/resume failed unrecoverably | persist error stamp; emit state-changed |
| Idle | Busy | accepted `send` | enqueue serialized listener (§6); persist optimistic user message |
| Busy | NeedsInput | tool requires permission OR explicit ask-user prompt | render approval card / question; do not advance turn |
| NeedsInput | Busy | permission resolved or question answered | resume turn execution |
| Busy | Idle | turn-complete event observed | persist committed message and token-usage; emit complete |
| NeedsInput | Idle | turn-complete (e.g., user denial caused tool to return failure that the model accepts) | same as Busy → Idle |
| Busy | Error | recoverable error mid-turn | mark turn errored; on `recoverable=true` allow next `send` to return state to Idle |
| Error | Idle | next accepted `send` succeeds for a recoverable error | clear error stamp |
| Idle/Busy/NeedsInput/Error | Paused | LRU eviction OR explicit pause when Idle | release backend handle; preserve on-disk state |
| Paused | Initializing | accepted `send`, explicit `resume`, or visibility change | begin resume or rebuild-from-history |
| any non-terminal | Destroyed | explicit delete | abort in-flight work; release handle; remove on-disk files; remove from index |

A session MUST NOT transition directly from Created to Idle: every Idle state is preceded by a successful Initializing.

---

## 3. Persistence Model

### 3.1 Files on disk

The session manager owns a directory of session state. The shape (paths illustrative; no real keys, no internal codenames) is:

```
~/<app-data-root>/sessions/
├── index.json                   # newest-first metadata-only listing
├── {sessionId}.json             # full snapshot bundle for one session
├── {sessionId}.json             # ...
└── ...
```

The on-disk file layout has two distinct artifacts:

1. **The index** — `index.json` is an ordered list of metadata records (one per session, newest first). It does NOT contain message history or pending grants. Its purpose is fast enumeration of the sidebar and search surfaces without loading every session bundle.
2. **The snapshot bundle** — `{sessionId}.json` is a single self-contained record holding the session's metadata, full message history, any pending permission grants, any committed-but-unrendered context summary, and any backend-origin discriminator (see §10). The bundle is the authoritative record for a session.

Settings unrelated to sessions (preferences, model defaults, disabled servers) live in a separate file outside this directory.

### 3.2 Source-of-truth invariant

**Disk wins on conflict.** When the in-memory representation of a session disagrees with the on-disk snapshot bundle, the on-disk version is authoritative. Specifically:

- On startup, the session manager seeds the active pool from disk; in-memory state begins empty.
- On resume, the session manager re-reads the snapshot bundle and uses it to rebuild in-memory state; any cached values from the previous run that were not flushed to disk are discarded.
- On every message append, the bundle is rewritten before the renderer is told the turn is complete. A turn is not "complete" from the persistence layer's perspective until the disk write succeeds.

The index is a derived view: if the index disagrees with the union of bundles in the directory, the bundles are correct and the index is regenerated.

### 3.3 Atomic snapshot bundle write

A snapshot bundle MUST be written atomically. The behaviour is: write to a sibling temporary path, fsync, then rename over the live path. A failure mid-write MUST NOT leave a partially-written bundle at the live path. The index is written with the same protocol.

Atomicity is per-bundle: writing one session's bundle does not require holding a lock across other bundles. The index write is independent of any single bundle write.

### 3.4 Index update is prepended (newest first)

When a new session is created or an existing session receives a new turn, the index entry for that session MUST be moved to (or inserted at) the head of the list. Sidebar ordering is therefore most-recent-activity-first without a separate sort step.

When a session is destroyed, its index entry is removed; the relative order of the remaining entries is preserved.

### 3.5 Pre-spec-row migration rule

A session bundle may pre-date a given schema field. Loaders MUST tolerate missing optional fields and apply a deterministic default rather than rejecting the bundle. Specifically:

- A bundle missing a `backendOrigin` field (see §10) is treated as belonging to the legacy default backend.
- A bundle missing a `source` field is treated as a user-facing session.
- A bundle missing pending-grant fields is treated as having no pending grants.
- A bundle whose schema version is older than the current shipping version is upgraded in memory on load and re-persisted on the next write.

Migration is forward-only: a newer-shipping-version bundle MUST NOT be downgraded by an older code path. If the loader encounters a bundle whose schema version is strictly newer than it understands, it MUST refuse to load that session and surface a clear, non-destructive error.

### 3.6 Auto-save cadence

The bundle is auto-saved on:

- session creation (first write);
- every committed message append (user message accepted, assistant message complete, tool result committed, permission grant added or expired);
- metadata mutation (title, model, personality before lock — see §8 — and any backend-origin assignment);
- explicit `pause` and `destroy` (final write, then file removal in the destroy case).

Streaming deltas are NOT individually persisted; only the committed message at turn-complete is durable.

---

## 4. Session Pool Management

### 4.1 Pool bound

The active pool — the set of sessions currently holding live backend handles — is bounded by a configured maximum (`MAX_ACTIVE`). When admitting a new session would exceed the bound, the pool MUST evict (pause) the least-recently-used eligible session before admitting the new one.

`MAX_ACTIVE` is a single configurable number; the spec does not normatively prescribe a value. The reference behaviour treats a low double-digit count as a sensible default: large enough to keep typical multi-tab usage live, small enough to bound provider memory.

### 4.2 Eviction algorithm: pause-oldest LRU

```
admit(newSessionId):
    if pool.size < MAX_ACTIVE:
        bind(newSessionId)
        return
    candidates = pool \ excludeSet(newSessionId)
    if candidates is empty:
        bind(newSessionId)               # bound exceeded by design (see §4.4)
        return
    victim = argmin(candidate.lastActivityTs for candidate in candidates)
    pause(victim)
    bind(newSessionId)
```

The eviction unit is one session (not one turn, not one message). Eviction happens transactionally at admission time, not on a background timer.

### 4.3 The `excludeId` set

The eligible-for-eviction set is the active pool minus an exclusion set. The exclusion set MUST contain at minimum:

1. **The current foreground session** — the session the user is actively viewing. Evicting the foreground session would visibly break the UI and is forbidden.
2. **The system bot session** (see §5) — a singleton internal session that is the target for relay traffic. Pausing it under load would silently drop incoming messages.
3. **Any session marked by the caller via an explicit exclude argument** — for example, the session that is being admitted itself (a self-eviction would be nonsensical), or a session the caller has already chosen to keep live for the duration of a multi-step operation.

If every eligible-for-eviction candidate is in the exclusion set, the pool MAY temporarily exceed the bound rather than evict an excluded session. This is a deliberate design choice: bounded-pool overflow is recoverable; evicting the foreground or the bot session is not.

### 4.4 Pause vs. destroy

Pause and destroy have orthogonal effects:

| Effect | Pause | Destroy |
|---|---|---|
| Releases backend handle | Yes | Yes |
| Removes from active pool map | Yes | Yes |
| Removes from index | No (index now records `paused` plus pause timestamp) | Yes |
| Removes snapshot bundle from disk | No | Yes |
| Cancels in-flight turn | Refused while Busy/NeedsInput; pause is an Idle-only operation OR is the consequence of an LRU eviction at admission | Yes (forced) |
| Reversible | Yes (resume) | No |

A paused session is, by construction, recoverable. A destroyed session is gone.

### 4.5 Pause failure mode

If the backend's pause RPC fails, the session manager logs the failure and proceeds with the admission of the new session. The failed-to-pause session remains in the active pool; the active count temporarily exceeds the bound. The intent is that a backend hiccup MUST NOT block the user from creating or resuming a new session.

---

## 5. Internal & Hidden Sessions

The system uses internal sessions to back automation, relay traffic, and probing. They share the session-state on-disk shape (§3) and the same state machine (§2) but differ in visibility and creation pathway.

### 5.1 Source taxonomy

Each session carries a `source` discriminator. The taxonomy and visibility rules:

| `source` | Purpose | Sidebar listing | Search | Notes |
|---|---|---|---|---|
| user-facing (default) | Normal user conversations | Yes | Yes | Created by explicit user action |
| automation-execution | An automation run that the user explicitly cares about | Yes — when carrying an associated automation identity (so the user can navigate to it) | Yes | Treated like a user-facing session for permissions purposes |
| automation-probe | Condition-check probes evaluated by automations | No | No | Hidden because they are noise; one probe per evaluation |
| system-bot | The singleton session that backs external-relay traffic and proactive notifications | No | No | Singleton — exactly one per process instance |
| heartbeat-probe | Periodic system-health interactions | No | No | Hidden because they are routine |
| workspace-attached | A user-facing session tied to a particular workspace | Yes | Yes | Carries a workspace discriminator field |
| feedback | A session created to capture an explicit user feedback round-trip | Yes | Yes | Source-tagged for telemetry |

Future sources MAY be added; loaders treat unknown `source` values as user-facing for safety (least-restrictive listing) — the spec deliberately avoids the "unknown source means hidden" rule because it would silently swallow new visible sources after a downgrade.

### 5.2 Discriminator-vs-identity rule for automation

An `automation`-sourced session has two sub-cases distinguished by whether it carries an automation identity:

- `source = automation-execution` AND has an automation identity → user-facing for listing.
- `source = automation-probe` (no identity, by definition) → hidden.

The presence/absence of the identity is the discriminator. There is no separate `hidden: true` flag.

### 5.3 The system bot session is a singleton

There is exactly one system bot session per process instance. It is created lazily on first use (the first incoming relay message, the first proactive-notification request, the first heartbeat tick that needs to deliver output via the relay). It is shared across all relay-facing traffic. It is hidden from the sidebar.

If the user requests "delete all sessions," the bot session IS deleted as part of that operation, and its singleton handle is reset so that the next use lazily recreates it. Deleting all sessions therefore does NOT leave a phantom bot session behind.

The bot session is in the §4.3 default exclusion set: LRU eviction MUST NOT pause it.

### 5.4 Listing and search filters per surface

The session-listing API (sidebar, search, recent-sessions UI, command-palette session pickers) MUST apply the visibility filter. The filter is computed solely from `source` and the discriminator-presence rule in §5.2.

The same filter is applied to any user-visible aggregate (badge counts, "you have N unread sessions"). Background processes (e.g., automation executors) bypass the filter when they have a direct identity-keyed lookup.

### 5.5 Hidden sessions still receive turn events

Hidden does not mean silent. A hidden session generates the same turn events (start, deltas, tool start/result, complete) as a visible session. Internal subscribers (automation orchestrator, heartbeat monitor, relay forwarder) consume these events directly. The renderer simply does not subscribe to events for sessions filtered out of its view.

---

## 6. Per-Session Event Serialization

### 6.1 The send-and-collect contract

The system exposes a synchronous-feeling helper used by interactive paths and by relay traffic that needs a single end-to-end turn round-trip:

```
sendAndCollect(sessionId, content, timeoutMs) → committedTurn
```

The helper sends a turn, registers a listener for that session, and resolves with the committed turn when the session next emits `turn-complete` (or rejects on timeout / unrecoverable error).

### 6.2 Per-session serialization queue

Concurrent calls to `sendAndCollect` against the same session MUST be serialized. The queue is per session: calls against different sessions run independently and concurrently.

```
session S queue:
    head → call_1 (running) → call_2 (queued) → call_3 (queued) → tail
```

Without serialization, the second call's listener could observe events from the first call's turn (a listener race), corrupting the second caller's view of the conversation. The queue is the contract that makes the listener registration safe.

### 6.3 Ordering invariants

For a single session:

- **FIFO.** Calls are dispatched in the order they were enqueued.
- **No interleaving.** A queued call does not begin sending until the running call has resolved (with success, error, or timeout).
- **Cancellation cascades.** If a call is cancelled, the queue advances to the next pending call; cancellation does not skip ahead.
- **Timeout closes the slot.** A timed-out call vacates the head of the queue exactly as a successful one would.

Across sessions there is no ordering guarantee: the order in which two different sessions' queues drain is not specified and MUST NOT be relied upon.

### 6.4 Fire-and-forget escape hatch for self-tools

Some tools are invoked by the model on a session and themselves need to drive that same session (or another internal session that may be queued behind the caller). If those tools were to call `sendAndCollect` synchronously on the same queue, the tool would await its own queue slot and deadlock:

```
            session-S sendAndCollect (running)
                              │
              model invokes a tool whose handler calls
                  sendAndCollect on session-S or
                  on another session whose queue
                  is entered behind the caller
                              │
                              ▼
                  queue position = behind self
                              │
                              ▼
                          deadlock
```

The escape hatch is **fire-and-forget**: the affected self-tools MUST kick off their work without awaiting a result. Specifically:

- A tool that runs a heartbeat-probe-style action does its work asynchronously and returns to the model immediately with an acknowledgement, not the result.
- A tool that triggers a long-running automation returns immediately with a started/queued indicator; the result is delivered out-of-band (via the relay, via a follow-up notification, or by surfacing the automation-execution session in the sidebar).

This rule is normative for any self-tool that could re-enter the session-event-serialization queue. If a new tool is added that requires a synchronous result on the same session, it MUST NOT use `sendAndCollect`; it MUST instead use a path that does not enqueue.

### 6.5 Renderer-driven sends are also serialized

The renderer's normal "user typed a message" path goes through the same per-session queue. If the user types two messages in quick succession, the second one queues behind the first; the UI MUST reflect the queued state and MUST NOT begin streaming the second turn before the first completes.

---

## 7. Resume Semantics

### 7.1 Resume on a Paused session

A session in the Paused state has no live backend handle. Resume re-establishes the handle and rehydrates in-memory state from the snapshot bundle.

```
resume(sessionId):
    bundle = readBundle(sessionId)           # disk wins (§3.2)
    rehydratePendingGrants(bundle)            # §7.3
    handle = backend.resumeSession(bundle.backendSessionRef, bundle.config)
    if handle is unavailable (failed):
        handle = backend.createSession(bundle.config)
        replayHistory(handle, bundle.messages)   # §7.4
    bind(sessionId, handle)
    state = Idle
    emit state-changed
```

### 7.2 Resume-on-paused fallback (no "session not active")

A `send` against a Paused session MUST trigger an automatic resume rather than throwing a "session not active" error. The order is:

1. Caller calls `send(sessionId, content)`.
2. If the session is in Paused, the manager performs `resume(sessionId)` first, transparently.
3. After resume succeeds (Idle), the send proceeds.
4. If resume fails, the send rejects with a resume-error, NOT a not-active error. The error MUST distinguish resume-failure from not-existent-session (the former is potentially retriable; the latter is permanent).

**Why this is normative.** A prior implementation that threw "session not active" before attempting resume caused a class of bugs in which the system bot session — having been paused by LRU pressure, by a backend watchdog restart, or by a session-expiry from the backend — would silently stop responding to all incoming relay messages until the application was restarted. Auto-resume is the contract that prevents this.

### 7.3 Pending-grant rehydration

The snapshot bundle includes any per-session pending permission grants (the per-entity registry of "approved tools for this run" maintained by the permissions module — see `spec-m-permissions.md`). On resume:

- Pending grants are rehydrated into the in-memory per-session permission registry.
- Any grant that has expired (per its expiry stamp) is **silently reaped** — it is not rehydrated, the rehydration step does not log a user-visible warning, and the bundle is rewritten with the expired entry removed at the next save.
- A grant is "expired" if its expiry timestamp is strictly less than the current wall-clock time at resume, OR if the grant was scoped to a single turn that completed before pause.

The silent-reap rule prevents stale grants from leaking permission across resume boundaries (a grant intended for a short window must not become effectively unbounded just because the session was paused for longer than the window).

### 7.4 Resume-with-fallback (rebuild-from-history)

If the backend's `resumeSession` call fails — backend doesn't recognize the session reference, backend session has expired its internal cache, backend has been restarted and lost ephemeral state — the manager MUST fall back to `createSession` followed by replaying the bundle's committed message history into the fresh handle.

Fallback semantics:

- The session's identifier and on-disk bundle are unchanged.
- The model and personality lock (§8) is preserved — the rebuild MUST use the same model and personality the original session locked in.
- All previously committed user-and-assistant messages are replayed in order. Pending streaming-deltas (which are never committed; see §3.6) are not replayed.
- Tool results that were committed are replayed; abandoned tool results (tool started but never committed; see §11.3) are NOT replayed — their absence is handled by the model regenerating the tool call if it still wants the data.
- After replay, the session is Idle and accepts the next `send` normally.

The user MUST NOT see a different session identifier, a different sidebar entry, or a "session was rebuilt" warning unless explicitly opted in. Resume-with-fallback is a transparency contract.

### 7.5 Triggers that pause an active session

A session can transition from Idle (or Busy at turn-complete) to Paused without explicit user action under any of:

- **LRU eviction** — admission of a newer session displaced this one (§4).
- **Backend watchdog revival** — the backend transport was restarted; all active sessions are dropped from the pool and re-resumed lazily on next access.
- **Token refresh / auth restart** — the backend transport restarted following a token refresh; same effect as watchdog.
- **Backend-side session expiry** — the backend signaled that its server-side handle expired; the session manager moves the session to Paused and the next access triggers resume-with-fallback.

In every case the snapshot bundle is intact and the session remains discoverable from the index.

---

## 8. First-Message Lock Invariants

### 8.1 What gets locked

The first user message committed to a session locks two pieces of metadata:

1. **Model.** The model selection chosen at session creation (or, if not chosen, the system default at that moment) is frozen.
2. **Personality** (or whatever the system's persona/system-prompt-style discriminator is). The personality chosen at session creation is frozen.

After the first user message is committed, neither value can be changed in place.

### 8.2 Why this is normative

A model or personality change mid-conversation produces a discontinuity that confuses both the user and the model. Specifically:

- The model's prior turns were generated under one set of conditioning; later turns under different conditioning would be inconsistent with the visible history.
- A personality switch mid-conversation produces a tonal jolt that the user perceives as a different agent.
- Permissions and tool-set decisions made earlier in the conversation may have been predicated on the original model's capabilities.

The lock makes the conversation a single coherent transcript end-to-end.

### 8.3 Subsequent change requires destroy + recreate

The user-facing affordance for "I want a different model on this conversation" is therefore destroy-and-recreate, NOT in-place mutation. The flow is:

1. The user requests a model/personality change.
2. The UI surfaces a confirmation that this will start a new session.
3. On confirm, the current session is destroyed (or kept and a new session is created — implementation-defined; the spec requires only that the old session's lock is preserved).
4. A new session is created with the desired model/personality. It has no first-user-message yet, so its lock is still open.

Pre-first-user-message edits to model/personality on the existing session ARE permitted. The lock is set when the first user message is *committed* (i.e., persisted in the bundle), not when the session is created.

### 8.4 Auto-save invariants around the lock

The save protocol around the first message is:

1. Accept the user message (state Idle → Busy).
2. Persist the locked model and personality into the bundle metadata.
3. Persist the user message into the message history.
4. Begin the turn with the backend.
5. Move the session's index entry to the head of the index (§3.4).

If step 2 or 3 fails, the turn MUST NOT proceed: the session is rolled back to its prior state and the user is told the message was not accepted. The lock is conceptually atomic with the first message.

---

## 9. BackendEvent → TurnEvent Normalization

### 9.1 Two stream shapes from backends

Backends differ in how they stream model output. Two shapes are observed:

- **Delta-streaming backends** emit incremental fragments: each event carries the *new* characters since the last event.
- **Snapshot-streaming backends** emit cumulative snapshots: each event carries the full accumulated text-so-far. Snapshots are a superset of all preceding deltas.

### 9.2 The normalization rule

The session manager normalizes both shapes into a single renderer-facing event stream. The renderer does not know — and MUST NOT need to know — which shape the backend produced. The normalization rule:

- A delta event passes through unchanged.
- A snapshot event is diffed against the last-observed snapshot for that turn; the new tail is emitted as a delta. The first snapshot in a turn is emitted as a delta containing its full text.
- A snapshot that is shorter than (or not a prefix-extending) the last snapshot indicates a backend-side correction; the normalizer MAY emit a reset marker (a zero-length delta paired with a status change) to signal the renderer to re-render. The behaviour for non-monotonic snapshots is documented as a known fragile point and is allowed to be conservative (drop or warn).

### 9.3 Conformance: one shape for the renderer

Regardless of backend, the renderer receives the same canonical sequence per turn:

```
turn-start
( turn-text-delta | turn-reasoning-delta | turn-tool-start | turn-tool-result )*
turn-complete | turn-error
```

Approval-card creation is NOT a turn event in this normalization — approval cards are emitted on the permissions channel (see `spec-m-permissions.md`). The session-lifecycle contract here is solely about turn-shaped events.

### 9.4 Token-usage and metadata side-events

Each backend may emit out-of-band events alongside the streaming turn:

- A `token-usage` event carrying `currentTokens` and `tokenLimit`.
- A `metadata-updated` event carrying any model-fallback or provider change visible mid-conversation.
- A `status` event carrying an opaque status string (e.g., a backend-side compaction notice).

These events are NOT part of the turn-streaming sequence and are emitted on their own channels; the renderer subscribes to them independently.

### 9.5 Where the normalization runs

Normalization runs inside the session manager, between the backend adapter and the renderer-facing event bus. Its placement is normative for two reasons:

1. The normalizer is the only component that has a per-turn accumulator; it is the only sensible place to diff snapshots against prior snapshots.
2. Placing the normalizer in the session manager guarantees that internal subscribers (automation, heartbeat, relay) and the renderer all observe the same event shape — there is no two-flavour event stream where one consumer sees deltas and another sees snapshots.

A backend that wants to ship a new event variant MUST add the variant to the backend-event taxonomy and the normalizer MUST be updated; renderer code MUST NOT be patched directly.

---

## 10. Cross-Backend Visibility

### 10.1 The `backendOrigin` field

Sessions carry a `backendOrigin` field — a small enumeration naming the backend that owns the live transport for this session. The default backend is implicit: a bundle on disk that lacks this field is treated as belonging to the legacy default backend (see §3.5).

### 10.2 The "rows missing it" rule

A session bundle saved before `backendOrigin` was introduced has no such field. Loaders MUST treat a missing `backendOrigin` as the default-backend value. This is a forward-compatibility hinge: it allows the field to be added without rewriting every existing bundle on first launch.

The corollary: the default-backend value MUST be stable. Renaming it would silently re-bind every legacy bundle.

### 10.3 The "gateway rows MUST always carry it" rule

Sessions belonging to any non-default backend (a remote gateway, a future cloud backend, etc.) MUST always have `backendOrigin` set explicitly. A gateway-origin session that fails to record the field would, on next load, be misclassified as a default-backend session and bound to the wrong transport.

This is a write-side normative rule: every code path that creates a non-default-backend session MUST set `backendOrigin` before the first save.

### 10.4 Listing across backends

When the active backend differs from a session's `backendOrigin`, the session manager MUST NOT fork the listing call path. The rule is:

- The sidebar lists only sessions whose `backendOrigin` matches the active backend.
- Sessions belonging to other backends remain on disk and are NOT surfaced.
- Switching backends is a coarse operation: it re-filters the listing, it does NOT migrate sessions.

The intent is full isolation between backends. A user who switches backend sees a different conversation list; sessions from the prior backend are intact and reappear if the user switches back.

### 10.5 No fork-on-origin in call sites

Code paths that operate on a session (send, resume, cancel, destroy) MUST NOT branch on `backendOrigin` directly. Branching, where it is necessary, lives inside the backend abstraction (see `spec-m-backend-abstraction.md`). The session manager treats all sessions uniformly and dispatches polymorphically through the bound backend provider.

This rule prevents the most common cross-backend bug class: a feature that works on backend A but silently no-ops on backend B because some call site special-cased the origin.

---

## 11. Cancellation & Cleanup

### 11.1 Turn cancellation propagation

Cancellation of an in-flight turn flows from the surface that initiated it down through the stack:

```
renderer "Stop" button   ─ or ─   relay "cancel" intercept   ─ or ─   internal cancel
            │                                │                                │
            └──────────────► session manager: cancel(sessionId)
                                            │
                                            ▼
                              backend abstraction: cancel(handle)
                                            │
                                            ▼
                            backend transport: SDK abort or RPC cancel frame
                                            │
                                            ▼
                                    in-flight turn aborts
                                            │
                                            ▼
                              turn-complete emitted with cancelled=true
                                            │
                                            ▼
                              state Busy/NeedsInput → Idle
```

A cancel against an Idle, Paused, Error, Created, or Destroyed session is a no-op. A cancel against a Busy or NeedsInput session is dispatched and MUST result in a `turn-complete` event with a cancelled marker (or, equivalently, a `turn-error` with a cancellation code).

### 11.2 In-flight permission and ask-user prompts on cancel

When a turn is cancelled while the session is in NeedsInput:

- Any pending approval cards belonging to that turn MUST be auto-removed from the approval card surface; they are NOT silently left to time out.
- Any pending ask-user prompt MUST be dismissed; it does NOT block the cancellation.
- The cancellation is recorded for audit (see `spec-m-permissions.md`).

### 11.3 Abandoned tool results

When a tool call is aborted before its result is committed (cancellation, backend disconnect, application crash), the result is **abandoned**. Abandoned results MUST NOT be replayed on resume (§7.4). Specifically:

- An abandoned tool's `tool-start` event was already emitted to the renderer; its absence of a paired `tool-result` is reconciled at turn-complete by marking the tool as cancelled in the committed message.
- The on-disk bundle does NOT carry a half-completed tool; only committed tool calls are persisted.
- If, on backend reconnect, the backend later delivers the result of a tool call whose owning turn has already been marked complete-cancelled, the late result MUST be dropped silently. The session manager MUST NOT inject it into a different turn.

### 11.4 Cleanup ordering on destroy

Destroying a session MUST follow this order:

1. **Mark Destroyed in memory** so subsequent inputs are rejected (§2.2).
2. **Cancel any in-flight turn** (§11.1) and wait for the cancellation to settle (with a bounded timeout — a hung backend cancel MUST NOT block destroy).
3. **Release the backend handle** (release transport-side state).
4. **Remove the session from the active pool map.**
5. **Remove the session from the index.**
6. **Delete the on-disk snapshot bundle.**

Steps 5 and 6 MUST NOT be reordered: removing from the index before deleting the bundle is acceptable (the bundle becomes orphaned and is reaped on next startup). Deleting the bundle before removing the index entry is NOT acceptable (the index would point at a missing bundle, and the listing surface would briefly show a broken row).

If the session was the foreground session, the renderer MUST be told to switch focus to a fallback session (typically the next-most-recent in the index) before destroy completes.

### 11.5 Idempotence

Cancel and destroy MUST be idempotent. A second cancel on an already-cancelled turn is a no-op. A second destroy on an already-destroyed session is a no-op (no error). This rule holds across crash recovery: a destroy that was interrupted MUST be safely re-driveable on next start.

---

## 12. Cross-Module Wiring

The session manager is the integration hub for several other modules. The contracts below describe what the session manager surfaces to each consumer.

### 12.1 Permissions (per-entity)

The permissions module (see `spec-m-permissions.md`) maintains a per-entity registry of grants. The session is one such entity. The session manager surfaces:

- **Session identity** for the permissions module to key its registry by.
- **Session source** (§5.1) so the permissions module can apply the correct entity scope (a user-facing session uses the interactive registry; an automation-execution session uses that automation's registry; a hidden background session uses an implicit-deny-prompt registry).
- **Session lifecycle hooks** so the permissions module can:
  - **register-per-run**: open a fresh per-entity registry slot when the session begins a turn; close it with `finally` cleanup whether the turn succeeded or failed;
  - **rehydrate** the registry from the snapshot bundle on resume (§7.3);
  - **reap** expired entries silently on resume.

The session manager MUST NOT itself decide permission outcomes; it MUST forward all permission decisions to the permissions module.

### 12.2 MCP

External tool servers (MCP servers) are bound at session creation via the backend abstraction (the backend reads a server configuration and asks each server to register its tools). The session manager's responsibilities:

- The bound MCP server set is captured into the session bundle's config snapshot at creation, so resume rebinds the same servers.
- Toggling an MCP server (enable/disable) does NOT affect already-running sessions in flight; the next admit/resume picks up the new toggle state.
- An MCP server failure is surfaced as a tool-result error in the affected turn, NOT as a session error.

### 12.3 Telemetry

Telemetry sees session events at well-defined emission points. The session manager:

- Emits a session-created event tagged with `source` and `backendOrigin`.
- Emits a turn-started and turn-completed pair tagged with the session identifier, the model, the personality, and the success/cancelled/error outcome — but NOT the message content.
- Emits a session-destroyed event with the lifetime stats (turn count, last activity).
- Emits an LRU-eviction event when a pause was caused by admission pressure.

Telemetry MUST be a fire-and-forget concern relative to the session lifecycle — a telemetry sink failure MUST NOT block session operations.

### 12.4 Tenant policy

Tenant policy (see the planned `spec-m-tenant-policy.md`) is consulted at multiple session lifecycle points:

- **At session creation**, to decide whether the chosen model is allowed for the current tenant; if not, creation is refused before a backend handle is bound.
- **Per turn**, the permissions module consults tenant policy as one step in its evaluation precedence; the session manager does not consult policy directly per turn — it surfaces session metadata to the permissions module which then consults policy.
- **At resume**, the locked model and personality are re-checked against current tenant policy. If the locked model is now disallowed for the tenant, resume MUST fail with a policy-disallow error rather than silently switching the model (which would violate §8).

### 12.5 Power management and keep-awake

Long-running background activity (relay, heartbeat) keeps the session manager active. The session manager does NOT itself decide when the host should be prevented from sleeping; it cooperates with a separate power-management module that uses ref-counted holds. The lifecycle hooks:

- A session entering Busy MAY acquire a power hold; entering Idle releases it.
- The bot session and any active automation-execution session MAY hold a power lock for their duration.
- Hold/release is idempotent and ref-counted; the session manager MUST balance every acquire with a release on every exit path including error and cancellation.

The exact policy (which sessions hold power locks, how long) is not normative here; the wiring contract is.

---

## 13. Open Questions

These items reflect places where doc evidence and observable evidence diverge, where the source is silent, or where a decision is roadmap. They are flagged for resolution by the implementer and SHOULD be closed before the corresponding section is treated as final. Cross-referenced to the e2e-architecture analysis (`m-e2e-architecture.md` §6) where applicable.

| # | Topic | Status | Notes |
|---|---|---|---|
| Q1 | Cancellation audit source | Open (E2E §6 Q5) | Cancellation produces an audit entry but the audit-source enum has no explicit `cancelled` value. Likely surfaces as "user-interactive" with a cancellation sub-action; spec-permissions should normalize. |
| Q2 | `MAX_ACTIVE` default value | Open | Spec is value-agnostic; pick a default and write it into the project's settings spec. Avoid coupling it to backend-specific limits. |
| Q3 | Snapshot non-monotonic correction (§9.2) | Open | When a snapshot-streaming backend emits a snapshot shorter than its predecessor, the safest renderer behaviour is unspecified. Reference behaviour: emit a reset marker; require renderer to re-render the turn from the latest snapshot. |
| Q4 | Cross-backend session listing UX | Open | When the user switches backend, do we surface "you have N sessions on the other backend" as an affordance? Spec currently says full isolation; UX may want a discovery hint. |
| Q5 | Resume-with-fallback observability | Open | Spec says the user MUST NOT see a "session was rebuilt" warning unless opted in. Determine whether telemetry fires unconditionally, and whether a developer-mode log surfaces the fallback. |
| Q6 | Bot-session deletion on "delete all" | Confirmed | The bot session is deleted on user-requested delete-all; its singleton is reset for lazy recreation. |
| Q7 | Bundle schema versioning | Open | A schema-version field is implied (see §3.5 forward-only migration). Define the version stamp shape and the policy for declining strictly-newer bundles. |
| Q8 | Pending-grant rehydration scope | Open (cross-cuts spec-permissions) | Confirm that "pending grants" rehydrated on resume covers per-session grants only, not global rules. Global rules live in the permissions module's own persistence. |
| Q9 | Heartbeat-probe sessions vs. heartbeat ring durability | Open (E2E §6 Q15) | Heartbeat ring is in-memory only; a heartbeat-probe session may still persist a bundle. Confirm whether a probe session is bundle-less by design (live-only) or persists for audit. |
| Q10 | Listing rule for unknown `source` (§5.1) | Open | Spec defaults unknown sources to user-facing for safety. Confirm this is the desired downgrade behaviour, or invert. |
| Q11 | LRU exclusion set composition | Confirmed | Default exclusion = {foreground, system-bot} ∪ caller-supplied. Active automation-execution session is NOT in the default exclusion set; the implementer SHOULD confirm whether long-running automation should be added. |
| Q12 | Cancel timeout on destroy (§11.4 step 2) | Open | The bounded timeout for a hung backend cancel during destroy is unspecified. Pick a low single-digit-seconds value; document it. |

---

*End of Caduceus session-lifecycle behavioral specification.*
