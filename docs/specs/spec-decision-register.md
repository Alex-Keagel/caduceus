# Spec: DecisionRegister — locked-decision persistence + restore across context resets

## §0 Header & Attribution

- **Spec ID:** `spec-decision-register`
- **Tier:** P (cross-cutting platform contract)
- **Status:** Draft, scope-locked, post-rubber-duck-iteration-1
- **Audience:** Implementers of `caduceusd` plan-mode reducer, host agent runner engines that emit `AgentEvent`s, `caduceus-zed` plan-panel + workspace-mutation surface, anyone wiring an external agent surface that participates in plan-mode.
- **RFC-2119:** This document uses MUST / MUST NOT / SHOULD / SHOULD NOT / MAY in the senses defined by RFC 2119 / RFC 8174.
- **Z-namespace:** Invariants in this spec are tagged `Z8-D#` ("Decisions"). They are disjoint from `Z6-*` (status snapshot), `Z7-W*` (cross-cutting wiring), and the unsubscripted `Z-*` series (orchestrator algorithm), and may be cited from those specs without renumbering.
- **Attribution / source material:**
  - `spec-caduceus-orchestrator-algorithm.md` — single-authority mutation (I-1), the orchestrator-as-only-mutator invariant.
  - `spec-cross-cutting-wiring.md` §2 — `BootId`, `EventSeq`, `RequestId`, `SessionId` definitions and ordering rules. (`BootId` and `EventSeq` are originally normative there per `Z7-W2..W12`; `spec-orchestrator-status-snapshot.md` §3.1 cites them as `Z6-K1` for snapshot-clock-capture purposes only.)
  - `spec-orchestrator-status-snapshot.md` §3.6 — how `AgentEvent`s flow into a `Snapshot` via the per-session reducer. Compaction interaction: §3.6.5.
  - `spec-multi-repo-workspace-model.md` §3 — the only authoritative path canonicalization rules in the system; this spec inherits them verbatim for `DecisionValue::Path`.
  - `m-e2e-architecture.md` — `~/.caduceus/`-style persistence layout, session-storage path conventions.
  - `crates/caduceus-core/src/lib.rs` (`AgentEvent`, `ModeChanged`, `PlanStepPending`) — the existing event taxonomy this spec extends.
- **Forcing function:** Two failure modes observed in plan-mode threads when a workspace mutation (project root added/removed) occurs mid-session:
  - **Failure A (hard transcript loss):** the agent loop loaded an empty transcript on the turn after the workspace mutation, treating the session as fresh.
  - **Failure B (decision-register loss):** transcript came back, but locked decisions previously checkboxed `✅` in-thread were not surfaced in the engine's working state, leading to re-asking already-answered questions.

  Both failures are products of decisions living **only as free text in the transcript**, never as structured state. **And of session identity being coupled to ephemeral workspace metadata.** This spec makes decisions first-class structured state AND enforces session-identity stability through a separate `ThreadId` storage key.

- **Non-attribution:** Anything tagged `(caduceus-new)` is original to caduceus and has no Symphony/M counterpart. The DecisionRegister concept and the `ThreadId` / `SessionId` separation are `(caduceus-new)`.

This spec defines:
- A `ThreadId` storage-key concept that decouples durable session state from `SessionId` (§3.0).
- New `AgentEvent` variants: `DecisionLocked`, `DecisionAmended`, `DecisionUnlocked`, `DecisionLockDenied`, `DecisionRegisterRestored`, `DecisionRegisterError`, `OpenQuestionUnanswered` (§4.1).
- A per-thread **DecisionRegister** projection (§3.2, §3.3).
- A two-pronged **restore protocol**: structural (open-question elimination) + textual (ReconciliationMessage fallback) (§3.4, §3.5).
- The workspace-mutation invariant, made enforceable through the `ThreadId` keying (§3.6).
- The plan-mode/act-mode interaction rules (§3.7).
- Conflict resolution: single explicit-amend path (§3.8).

It does **not** define UI presentation, persistence-engine choice (sqlite vs. files), or the prompt template the agent uses to *propose* a decision (§7).

---

## §1 Scope

### 1.1 In scope

This spec normatively covers:

1. **`ThreadId` introduction.** A new identifier separate from `SessionId`, owning durable session state. The mechanism that makes the workspace-mutation invariant enforceable. (§3.0, §5 Z8-D40..D43.)
2. **Decision events.** Shape, lifecycle, and equality semantics of `AgentEvent::DecisionLocked`, `DecisionAmended`, `DecisionUnlocked`, `DecisionLockDenied`. (§3.1, §4.1, §5 Z8-D1..D9.)
3. **`DecisionRegister` projection.** A reducer-derived, append-only-with-amendments view keyed by `DecisionId`. (§3.2, §3.3, §5 Z8-D10..D17.)
4. **Persistence.** Where the register lives, fsync discipline, end-of-turn persistence rule, atomic-rename semantics. (§3.3.5, §5 Z8-D18..D20.)
5. **Two-pronged restore protocol.** Structural open-question elimination (mandatory) + ReconciliationMessage (mandatory fallback). (§3.4, §3.5, §5 Z8-D21..D29.)
6. **Workspace-mutation invariant** *with enforcement mechanism*. Workspace-mutation handlers MUST mutate only `WorkspaceContext`, never thread or session identity. (§3.6, §5 Z8-D30..D32.)
7. **Plan→Act transition contract.** (§3.7, §5 Z8-D33..D35.)
8. **Conflict resolution: single amend path.** Re-locking the same id with a different value is an error; agents MUST emit `DecisionAmended` explicitly. (§3.8, §5 Z8-D36..D38.)
9. **Concurrency rules.** Multi-runner same-id ordering, user-vs-agent mid-turn race resolution, permission-denied audit path, false-negative detection. (§3.9, §5 Z8-D44..D49.)
10. **Acceptance, glossary, out-of-scope, open questions.** (§6, §2, §7, §8.)

### 1.2 Out of scope

- The choice of persistence engine (sqlite, JSON-Lines, sled, …). The on-disk shape (§4.3) and durability discipline (§3.3.5) are normative; the engine is not.
- The UI presentation of the decision register. The wire shape is normative; the rendering belongs to `spec-zed-plan-panel` (forward reference; not yet written).
- The prompt template the agent uses to propose decisions. The contract (when to emit, what shape) is normative; the prompt body is not.
- Cross-thread / cross-session decision sharing. Each `DecisionRegister` is scoped to one `ThreadId`. → forward reference `spec-decision-library` (proposed).
- The decision-amendment **policy** (who is allowed to amend which decision class). The mechanism is normative (§3.8); the policy attaches to `caduceus-permissions` and is owned there.
- Notification surfacing (toast, banner, log line) when a decision is locked. The event emission is normative; the surfacing belongs to `spec-notice-notification`.
- Conversation-history compaction algorithm. The interaction (Z8-D14, Z8-D15) is normative; the compaction algorithm itself is in its owning spec.
- Decision import/export bulk operations.
- Telemetry of decision events (attaches to `spec-cross-cutting-wiring.md` §3.5 with a `DecisionLocked` opt-in signal in v2).
- `DecisionValue::Json` (deferred; v1 covers `String`, `Bool`, `I64`, `Path`, `Choice` only — the four shapes that closed Failure B in the canonical thread).

---

## §2 Terms

| Term | Definition |
|---|---|
| **DecisionId** | A stable string identifier, lower-kebab path-segments, ASCII, ≤128 bytes, matching `^[a-z0-9][a-z0-9_/-]{0,127}$`. See §3.1.2. |
| **DecisionValue** | The locked value. Closed enum: `String`, `Bool`, `I64`, `Path`, `Choice(Vec<String>, u32)`. See §4.1.1. |
| **DecisionState** | One of `Locked`, `Unlocked`. Stored explicitly per entry; never inferred. See §4.2. |
| **DecisionLocked / DecisionAmended / DecisionUnlocked / DecisionLockDenied** | `AgentEvent` variants. See §4.1. |
| **DecisionRegister** | The reducer-derived projection: `BTreeMap<DecisionId, DecisionEntry>` for the current `ThreadId`. See §3.2. |
| **DecisionEntry** | One row in the register: state, current value, history, locked_at, last_amended_at. See §4.2. |
| **DecisionSource** | Who emitted: `User`, `Agent`. (`System` is **not** a value in this spec — see §3.4 closing.) See §4.1.2. |
| **OpenQuestion** | A question the agent presents to the user with a known `DecisionId` (assigned when the question is shown, not when it is answered). The orchestrator tracks the open-question pool per `ThreadId`. See §3.4.2. |
| **ThreadId** | A `Uuid v7` identifying a logical conversation thread. Persistence, register, and transcript are keyed by `ThreadId`. **Survives workspace mutations and `SessionId` rebinds.** See §3.0. |
| **SessionId** | A `Uuid v7` identifying a user-facing session. Spans many `RunId`s. **MAY change across reattach** (in particular, may be reissued after editor restart) — that is exactly why durable state is keyed by `ThreadId`, not `SessionId`. |
| **WorkspaceContext** | The metadata block holding project roots, mounted folders, current cwd. Mutated by workspace-mutation handlers; orthogonal to transcript and register. |
| **WorkspaceMutationEvent** | Any of `WorkspaceRootAdded`, `WorkspaceRootRemoved`, `WorkspaceRootRenamed`. |
| **PlanModeReducer** | The `caduceusd` per-thread subsystem that owns the `DecisionRegister`. Single-authority writer per I-1. |
| **RestoreProtocol** | The two-pronged sequence in §3.4 (structural + textual). |
| **ReconciliationMessage** | The textual fallback artifact (§3.5); supplements but does NOT replace the structural restore (§3.4.3). |
| **EventSeq** | Per-process monotonic `u64`, resets on `BootId` change. From `spec-cross-cutting-wiring.md` §2. |
| **BootId** | Per-process `Uuid v7` minted at startup. From `spec-cross-cutting-wiring.md` §2. |
| **caduceus-new** | Tag indicating original caduceus content with no Symphony/M parity citation. The DecisionRegister and `ThreadId` are `(caduceus-new)` in entirety. |

---

## §3 Algorithms (normative)

### 3.0 ThreadId — the storage key that survives session rebinds

The single biggest enforcement mechanism in this spec.

#### 3.0.1 Definition

A `ThreadId` is a `Uuid v7` minted by the orchestrator at the **first** non-empty agent turn of a logical conversation thread. It is:

- (Z8-D40.) Stable across `SessionId` rebinds. If the editor reissues `SessionId` (e.g. on reconnect), the orchestrator MUST resolve the same `ThreadId` by matching on `(user_id, transcript_origin_message_id)`.
- (Z8-D41.) Stable across workspace mutations. `WorkspaceMutationEvent`s MUST NOT mint a new `ThreadId`.
- (Z8-D42.) The durable storage key for: `decision_register`, `transcript`, `plan_state`, `mode`, `permission_envelope`. All of these live under `~/.caduceus/threads/<thread_id>/`.
- (Z8-D43.) Mapped from `SessionId` through a small index: `~/.caduceus/sessions/<session_id>/thread_id` is a single-line file containing the `ThreadId`. On session-rebind, the orchestrator reads this index; if the index is absent (truly new session), it mints a new `ThreadId` and writes the index.

#### 3.0.2 Migration

Before this spec lands, durable state lives under `~/.caduceus/sessions/<session_id>/`. Implementations MUST migrate on first boot post-spec:

1. For each existing `<session_id>` directory, mint a `ThreadId` (`Uuid v7` from boot time + session-id hash for determinism).
2. Move the directory to `~/.caduceus/threads/<thread_id>/`.
3. Write the session→thread index file.
4. Emit a one-time `AgentEvent::ThreadIdMigrated { session_id, thread_id }` to the audit log.

Migration is idempotent (re-runnable). Failure aborts boot with a fatal log; the operator must intervene.

### 3.1 Emitting a decision event

#### 3.1.1 Trigger

The agent emits `AgentEvent::DecisionLocked` at the moment it interprets a user reply (or its own derivation) as **closing an open question** that has a known `DecisionId`. Concretely:

- The orchestrator maintains a per-thread **open-question pool** (§3.4.2). When the agent presents a numbered/lettered list of open questions in a turn, it MUST also emit `AgentEvent::OpenQuestionPresented { id, prompt, kind, options? }` for each item, registering its `DecisionId`.
- When the user replies and the agent interprets that reply as closing one of those questions, the agent MUST emit `DecisionLocked { id, value, ... }` where `id` matches the previously-registered `DecisionId`.
- Free text "✅" rendering MUST NOT substitute for the event (Z8-D1). Free text is for humans; events are for the reducer.

The agent MAY also emit `DecisionLocked` for a `DecisionId` that was not previously presented as an open question — these are **agent-derived** decisions (the agent inferred the value rather than asking). The reducer MUST accept them; the entry's `derived_from: Option<DecisionId>` field captures provenance when available (§4.2).

#### 3.1.2 DecisionId discipline

`DecisionId` MUST:

1. (Z8-D2.) Be stable within a thread: re-asking the same open question MUST reuse the prior id.
2. (Z8-D3.) Match `^[a-z0-9][a-z0-9_/-]{0,127}$`. ASCII only.
3. (Z8-D2a.) Be assigned **explicitly by the agent** at the time the open question is presented. There is no auto-derivation from prompt text in v1 (OQ-2 in §8 considers it for v2).

The orchestrator's open-question pool stores `(thread_id, decision_id) → OpenQuestion` and is itself persistent (§3.4.2.b).

#### 3.1.3 Value typing

`DecisionValue` is a closed enum (§4.1.1): `String`, `Bool`, `I64`, `Path`, `Choice`. Adding a variant requires a wire-version bump per `spec-cross-cutting-wiring.md` §3.10.

- `Path` is canonicalized **per `spec-multi-repo-workspace-model.md` §3** (no `..`, no repeated `/`, no `~`, no trailing `/`, case-preserving on macOS/Linux, case-folding on Windows). The reducer rejects non-canonical paths with `DecisionRegisterError { code: "non-canonical-path" }`.
- `Choice` MUST satisfy `selected < options.len()`; reducer rejects otherwise (Z8-D3a).
- Empty `String` is rejected (Z8-D3b). Length cap: 4096 bytes UTF-8.

### 3.2 The DecisionRegister projection

#### 3.2.1 Shape

Per thread (NOT per session):

```rust
struct DecisionRegister {
    thread_id: ThreadId,
    boot_id: BootId,                   // for resync detection
    last_event_seq: u64,
    entries: BTreeMap<DecisionId, DecisionEntry>,
    last_mutation: WallClock,
}

struct DecisionEntry {
    id: DecisionId,
    state: DecisionState,              // Locked | Unlocked
    current: Option<DecisionValue>,    // Some(_) iff state=Locked
    history: Vec<DecisionRecord>,      // append-only, oldest-first
    derived_from: Option<DecisionId>,  // provenance for agent-derived locks
    locked_at: WallClock,
    last_amended_at: Option<WallClock>,
}

struct DecisionRecord {
    op: DecisionOp,                    // Lock | Amend | Unlock | LockDenied
    value: Option<DecisionValue>,      // None for Unlock and LockDenied
    source: DecisionSource,            // User | Agent
    locked_at: WallClock,
    request_id: Option<RequestId>,
    reason: Option<String>,            // required for Amend, Unlock, LockDenied
}
```

#### 3.2.2 Reducer rules

The reducer is a pure function `(state, event) → state`. Single-authority writer per I-1: only the orchestrator's per-thread plan-mode reducer mutates the register. IPC handlers (§4.5) **enqueue events onto the orchestrator's event stream**; they do not mutate state directly. (Z8-D45.)

Rules:

1. **`DecisionLocked { id, value, source, request_id }`** →
   - If `id` not in `entries`: create `DecisionEntry { state: Locked, current: Some(value), history: [Lock-record], ... }`.
   - If `id` in `entries` AND `state == Unlocked`: re-lock. Append `Lock` record, set `state = Locked`, `current = Some(value)`. (Z8-D5b.)
   - If `id` in `entries` AND `state == Locked` AND `current == Some(value)`: **idempotent re-lock**. Reducer treats as no-op; emits no audit record. (Z8-D5.)
   - If `id` in `entries` AND `state == Locked` AND `current != Some(value)`: **error**. Reducer surfaces `DecisionRegisterError { code: "implicit-amend-forbidden", id }` and rejects the event. The agent MUST emit `DecisionAmended` explicitly. (Z8-D6, **revised**: no implicit amend.)
2. **`DecisionAmended { id, value, prior_value, reason, source, request_id }`** →
   - If `id` not in `entries` OR `state == Unlocked`: error `DecisionRegisterError { code: "amend-of-unlocked" }`. Reducer rejects.
   - If `entries[id].current != Some(prior_value)`: error `DecisionRegisterError { code: "stale-amend" }`. Reducer rejects. (Catches concurrent-amend races; see §3.9.)
   - Else: append `Amend` record, set `current = Some(value)`, set `last_amended_at`.
   - `reason` MUST be non-empty (Z8-D9).
3. **`DecisionUnlocked { id, prior_value, reason, source, request_id }`** →
   - If `id` not in `entries` OR `state == Unlocked`: error `DecisionRegisterError { code: "unlock-of-unlocked" }`. Reducer rejects.
   - Else: append `Unlock` record, set `state = Unlocked`, set `current = None`. History preserved.
   - `reason` MUST be non-empty.
4. **`DecisionLockDenied { id, attempted_value, denied_by, reason, request_id }`** →
   - Append `LockDenied` record to the entry (creating entry with `state=Unlocked` if absent). State unchanged. Audit-only; observable by §4.5 IPC. (Z8-D47.)
5. Any non-decision event passes through unchanged. The reducer is sparse.

#### 3.2.3 Ordering

Reducer reads events in `(BootId, EventSeq)` order. Out-of-order events within a single `BootId` are a fatal reducer error (Z8-D11). Cross-process ordering (multi-runner, §3.9.1) uses lexicographic `(producer_id, BootId, EventSeq)` with deterministic tie-break by `producer_id` ASCII order.

### 3.3 Persistence

#### 3.3.1 What persists

The register persists in entirety: every `DecisionEntry`, every `DecisionRecord` in every history. Plus `(thread_id, boot_id, last_event_seq, last_mutation)` metadata.

#### 3.3.2 When it persists

Three triggers, in priority order:

1. **End-of-turn rule** (Z8-D18a, **new from rubber-duck**): the reducer MUST persist at the end of any agent turn that emitted at least one decision event. This closes the window where the canonical aletheia turn (5 locks in one user reply) could otherwise be lost in memory if the daemon crashed before `N`/`M` thresholds.
2. **Pre-WorkspaceMutationEvent rule**: synchronous persist before the orchestrator emits any `WorkspaceMutationEvent`, so durable register exists BEFORE the mutation that historically destroyed context.
3. **Throttled batch rule**: every N=10 reducer mutations OR every M=2 wall-clock seconds, whichever first, for steady-state non-turn-boundary mutations (rare; mainly user-driven IPC).

Synchronous persist also occurs before any IPC reply that the user-facing surface treats as a hard durability point (`/list_decisions`, `/export_register`).

#### 3.3.3 Where it persists

Under `~/.caduceus/threads/<thread_id>/decision_register.<format>`. Format and engine are out of scope (§1.2).

#### 3.3.4 Compaction immunity

Conversation-history compaction MUST NOT touch the DecisionRegister. (Z8-D14, Z8-D15.) Specifically: if a `ContextGroupsEvicted` or `ContextCompacted` event names messages that *contained* `DecisionLocked` events, the reducer MUST NOT undo those mutations. The register survives even when the entire transcript window has been replaced with a compaction summary.

#### 3.3.5 Durability discipline (fsync, atomic rename)

Atomic-rename alone does not guarantee durability of the latest write. Implementations MUST:

1. (Z8-D19a.) Write to `<thread_id>/decision_register.<format>.tmp.<rand8>` in the SAME directory as the target (so rename is atomic on the same filesystem).
2. (Z8-D19b.) `fsync` the temp file before rename.
3. (Z8-D19c.) `rename` to the target path.
4. (Z8-D19d.) `fsync` the parent directory after rename. (On macOS, `F_FULLFSYNC` SHOULD be used per Apple guidance for true durability against power loss.)
5. (Z8-D19e.) On crash recovery: if both `decision_register.<format>` and `decision_register.<format>.tmp.*` exist, prefer the non-temp file (the rename never completed). Sweep stale `.tmp.*` files older than 1 hour.

#### 3.3.6 Boot-crossing

On daemon restart: the plan-mode reducer reads the on-disk register before processing any new event. The first reducer state-change after restore MUST emit `AgentEvent::SessionResumed { thread_id, prior_boot_id, new_boot_id, register_size, transcript_size }`. (Z8-D20.)

### 3.4 Restore Protocol — two-pronged

#### 3.4.1 Triggers

- **T1: Agent runner attach.** A host agent runner attaches to an existing thread for the first time in this boot.
- **T2: WorkspaceMutationEvent.** Any of `WorkspaceRootAdded/Removed/Renamed`. Restore runs *after* the mutation lands and *before* the next agent turn. (Z8-D21.)
- **T3: BootId change observed at the agent runner.** (Z8-D22.)
- **T4: SessionId rebind detected.** Editor reconnect with a different `SessionId` mapping to the same `ThreadId` via the index file (§3.0.1.Z8-D43).

#### 3.4.2 Open-question pool — the structural prong

The orchestrator maintains a per-thread `BTreeMap<DecisionId, OpenQuestion>` of currently-unanswered questions. This is **persistent** (under `~/.caduceus/threads/<thread_id>/open_questions.<format>`) so it survives restart.

`OpenQuestion`:

```rust
struct OpenQuestion {
    id: DecisionId,
    prompt: String,
    kind: DecisionValueKind,         // String | Bool | I64 | Path | Choice
    options: Option<Vec<String>>,    // for Choice
    presented_at: WallClock,
    presented_by_run_id: RunId,
}
```

Lifecycle:

- (a) Agent emits `OpenQuestionPresented { id, prompt, kind, options? }`. Reducer inserts into the pool.
- (b) Reducer applies `DecisionLocked { id, ... }`. The orchestrator removes the entry from the pool (Z8-D24).
- (c) On RestoreProtocol step 3 (below), the orchestrator constructs the next agent input by **subtracting** the register's `Locked` ids from the open-question pool. This means: the agent CANNOT see a stale open question for an already-locked id, even if its working transcript is incomplete. **This is the structural enforcement that closes Failure B at the orchestrator layer, not the model layer.** (Z8-D33a, **new from rubber-duck**.)

#### 3.4.3 Protocol steps

1. **Read the register and open-question pool** from durable storage. If missing or unreadable, treat as empty and log a warning. The protocol MUST NOT abort.
2. **Validate** every `DecisionEntry.id` and `current` value against §3.1.2 + §3.1.3. Invalid entries are quarantined: moved to `~/.caduceus/threads/<thread_id>/_quarantine/<id>.<wall_time>.json` (schema = full `DecisionEntry` JSON). Quarantined entries are NOT re-applied. A `DecisionRegisterError { code: "quarantine-on-restore", id }` is emitted per quarantined entry.
3. **Eliminate** `Locked` ids from the open-question pool. (Structural prong.) Emit `OpenQuestionEliminated { id, reason: "already-locked" }` per elimination.
4. **Construct ReconciliationMessage** (§3.5). (Textual prong — fallback / belt-and-suspenders.)
5. **Inject the ReconciliationMessage** as the first `system`-role message of the next agent turn (before any user input on that turn).
6. **Emit `AgentEvent::DecisionRegisterRestored { thread_id, count, since_event_seq, truncated }`** so observers see restore activity.

#### 3.4.4 Idempotence

RestoreProtocol MUST be idempotent within a boot: running it twice with no intervening reducer mutation MUST produce the same eliminations and same ReconciliationMessage, and emit a single `DecisionRegisterRestored` (cached). (Z8-D23.)

#### 3.4.5 Note on `DecisionSource`

The system itself never creates decisions. RestoreProtocol does NOT emit `DecisionLocked` events; it only injects context and eliminates stale open questions. `DecisionSource` is therefore a closed enum of `User | Agent` only. (Removed `System` per rubber-duck #10.)

### 3.5 Reconciliation Message — textual prong

#### 3.5.1 Role

The ReconciliationMessage is the **secondary** restore mechanism. The structural prong (§3.4.2) is primary; this message exists to preserve the audit trail and to surface state to the agent in case the structural prong is missed by a buggy runner. (Z8-D29: agents MUST treat it as authoritative context, not as user input.)

#### 3.5.2 Shape

```
[caduceus DecisionRegister — restored from thread <thread_id_short>]

Locked decisions in this thread (do NOT re-ask):

- <id_1>: <rendered_value_1>
  (locked <wall_time_1>, source=<source_1>)
- <id_2>: <rendered_value_2>
  (locked <wall_time_2>, source=<source_2>)
- ... (N more)

If a user message contradicts a locked decision, treat it as
DecisionAmended { id, value, reason } — emit the event explicitly.
Do NOT silently overwrite.
```

#### 3.5.3 Rendering rules

- Boolean: `true` / `false` literally.
- Path: verbatim, no truncation.
- Choice: `<selected_label> (option <i> of <n>)`.
- Wall-time: ISO-8601 UTC.
- (Z8-D28.) Sorted lexicographically by `DecisionId` for determinism.

#### 3.5.4 Truncation

Byte budget: ≤ 6000 bytes UTF-8 (model-agnostic; chosen so all current GitHub Models tokenizers fit ≤ ~1500 tokens). When the rendered message exceeds 6000 bytes:

1. **Selection**: pick the K most-recently-amended entries that fit (priority by `last_amended_at desc, then locked_at desc, ties broken by `id` lex asc`).
2. **Display sort**: within the selected K, sort lexicographically by `id` (Z8-D28 retained for the displayed subset).
3. **Marker line**: append `... and <Total - K> earlier locked decisions; query via /list_decisions`.
4. **Event**: emit `DecisionRegisterRestored { count: Total, truncated: true, displayed_k: K }`.

This resolves the prior contradiction between Z8-D27 (recency selection) and Z8-D28 (lex sort): **selection** is recency-first; **display** within the selected subset is lex-sorted. (Z8-D27a, **revised** from rubber-duck #6.)

### 3.6 Workspace-Mutation Invariant — *with enforcement mechanism*

Workspace mutations are metadata-only with respect to the chat transcript and decision register.

#### 3.6.1 Enforcement

- (Z8-D30.) The orchestrator MUST route all `WorkspaceMutationEvent`s through a single dedicated handler that mutates ONLY the in-memory `WorkspaceContext` block AND its on-disk projection at `~/.caduceus/threads/<thread_id>/workspace_context.<format>`. The handler is forbidden — by code-review-enforced static check — from invoking session-creation, transcript-rebind, or `ThreadId`-mint paths. (See §6.4 acceptance test 19a for the static-check assertion.)
- (Z8-D31.) The agent runner's view of the transcript MUST be identical before and after mutation, modulo a single appended `WorkspaceMutationEvent` record.
- (Z8-D32.) `ThreadId` MUST NOT change on `WorkspaceMutationEvent`. `SessionId` change rules are owned by the editor surface; if it changes, the orchestrator resolves the same `ThreadId` via §3.0.1.Z8-D43.

#### 3.6.2 Why this works (vs the original spec which the rubber-duck rightly attacked)

The original draft asserted "MUST NOT re-key the session" without specifying how. This revised version makes it enforceable because:

1. Durable state is keyed by `ThreadId`, not `SessionId`. Even if the editor reissues `SessionId`, the index file at `~/.caduceus/sessions/<session_id>/thread_id` resolves to the same `ThreadId`.
2. The workspace-mutation handler is a single named module/function; a static check (lint or build-time test) asserts it has no edges to session/transcript-creation paths.
3. The pre-mutation persist rule (§3.3.2 trigger 2) ensures durable state is committed BEFORE the mutation, so even a crash mid-mutation cannot lose decisions.

### 3.7 Plan→Act transition contract

When the user moves from plan to act mode:

1. (Z8-D33.) The act-mode entry handler MUST read the current `DecisionRegister`.
2. (Z8-D33a.) The handler MUST consume the structural open-question elimination (§3.4.2.c). No `Locked` `DecisionId` may appear in the next agent turn's open-question pool.
3. (Z8-D34.) Auto-generated scaffolding prompts MUST surface register-derived locked values verbatim (e.g. "Scaffold per locked decisions: stack=Python, name=Aletheia, path=/Users/.../aletheia").
4. (Z8-D35.) The mode-transition emits `ModeChanged { from: "plan", to: "act", carried_decisions: <count> }`.

### 3.8 Conflict resolution — single explicit-amend path

Per the rubber-duck critique, the implicit-amend path is removed.

- (Z8-D6, **revised**.) Re-locking the same `DecisionId` with a different value is a reducer error (`code: "implicit-amend-forbidden"`). The agent MUST emit `DecisionAmended` explicitly with a non-empty `reason` AND the correct `prior_value`.
- The `prior_value` field on `DecisionAmended` is required (§4.1). It is checked against `entries[id].current`; mismatch is `code: "stale-amend"`, which makes concurrent-amend races first-class detectable.
- (Z8-D37.) `DecisionUnlocked` preserves history. A subsequent re-lock of the same id starts fresh state-wise but inherits visible history.

The user-facing surface (zed plan panel, CLI) emits `lock_decision`/`amend_decision`/`unlock_decision` IPC calls. The IPC handler enqueues the corresponding event on the orchestrator's event stream (per I-1; see Z8-D45). It does not mutate state directly.

### 3.9 Concurrency and edge cases

#### 3.9.1 Multi-runner same-id ordering

Two agent runners attached to the same thread MAY both emit decision events for the same `DecisionId` within the same wall-clock millisecond.

- (Z8-D44.) The reducer applies events in `(producer_id, BootId, EventSeq)` lexicographic order — **`producer_id` first**, with deterministic tie-break. This makes two-runner runs deterministic and replayable.
- (Z8-D44a.) The first event to be applied "wins" per §3.2.2 rule 1 / 2; subsequent same-id events are evaluated against the post-first-event state. A second-arriving `DecisionLocked` with a different value triggers `code: "implicit-amend-forbidden"` per Z8-D6; the second runner MUST observe that error and re-emit as `DecisionAmended` if it really intends to change the value.

#### 3.9.2 User amend vs agent emit mid-turn

The orchestrator's event stream is single-threaded per thread (per I-1). Both user IPC `amend_decision` and agent-emitted `DecisionAmended` enqueue events on this stream and are serialized in arrival order. The `prior_value` check (§3.8 / `code: "stale-amend"`) catches lost-update races: whichever event arrives second sees a mismatched `prior_value` and is rejected. (Z8-D45.)

#### 3.9.3 Permission-denied locks

The agent or user MAY attempt to lock a decision whose value would violate the active permission envelope (per `caduceus-permissions`). Currently the policy attaches in `caduceus-permissions`; this spec only defines the audit path:

- (Z8-D47.) Permission denial is surfaced as `AgentEvent::DecisionLockDenied { id, attempted_value, denied_by, reason, request_id }`. It is recorded in the entry's history with `op = LockDenied`, but does NOT change `state` or `current`. (§3.2.2 rule 4.)
- The user-facing surface MAY display denials in the plan panel; `caduceus-notice-notification` SHOULD surface a notice.
- IPC `lock_decision` returns `Err(PermissionDenied { reason })` synchronously when the envelope rejects.

#### 3.9.4 False negatives: open question with no lock event

If an `OpenQuestionPresented { id }` is followed by a user reply that the agent does not interpret as a lock (no `DecisionLocked { id }` emitted within N=2 subsequent agent turns), the orchestrator MUST emit `AgentEvent::OpenQuestionUnanswered { id, turns_elapsed, prompt }`. (Z8-D48.)

This is observable to the user surface (which can prompt the user) and to telemetry. It does NOT auto-create a decision; the spec deliberately avoids guessing. It surfaces a known omission so it can be addressed.

#### 3.9.5 Agent locking a User-sourced decision

(Z8-D38.) If an agent emits `DecisionLocked { id, source: Agent, value: V_new }` where `entries[id].history.last().source == User` AND `current != V_new`, the reducer rejects with `code: "agent-overrode-user"`. The agent MUST emit `DecisionAmended` explicitly, with a `reason` that names the user's prior answer.

---

## §4 Wire & storage shapes

### 4.1 AgentEvent variants

Add the following to the existing `AgentEvent` enum at `caduceus-core/src/lib.rs:822`:

```rust
/// A decision moves from "open question" to "locked answer".
/// `spec-decision-register` Z8-D1..D9.
DecisionLocked {
    id: DecisionId,
    value: DecisionValue,
    source: DecisionSource,                 // User | Agent (no System)
    derived_from: Option<DecisionId>,        // provenance for agent-derived locks
    reason: Option<String>,                  // optional for initial Lock
    request_id: Option<RequestId>,
    producer_id: ProducerId,                 // for multi-runner ordering
},

/// A previously-locked decision changes value. prior_value MUST match
/// current state; mismatch is rejected as stale-amend.
DecisionAmended {
    id: DecisionId,
    value: DecisionValue,
    prior_value: DecisionValue,
    source: DecisionSource,
    reason: String,                          // non-empty
    request_id: Option<RequestId>,
    producer_id: ProducerId,
},

/// A previously-locked decision is retracted; history preserved.
DecisionUnlocked {
    id: DecisionId,
    prior_value: DecisionValue,
    source: DecisionSource,
    reason: String,                          // non-empty
    request_id: Option<RequestId>,
    producer_id: ProducerId,
},

/// A lock attempt was rejected by the permission envelope. Audit-only.
DecisionLockDenied {
    id: DecisionId,
    attempted_value: DecisionValue,
    denied_by: String,                       // envelope rule id
    reason: String,                          // non-empty
    source: DecisionSource,
    request_id: Option<RequestId>,
},

/// Emitted by the orchestrator after RestoreProtocol completes.
DecisionRegisterRestored {
    thread_id: ThreadId,
    count: u32,
    since_event_seq: u64,
    truncated: bool,
    displayed_k: u32,
},

/// Reducer-internal error surfaced as a first-class event.
DecisionRegisterError {
    code: String,                            // closed enum: see §4.4
    id: Option<DecisionId>,
    detail: String,
},

/// Agent registered an open question with a known DecisionId. Drives the
/// open-question pool in §3.4.2.
OpenQuestionPresented {
    id: DecisionId,
    prompt: String,
    kind: DecisionValueKind,                 // String | Bool | I64 | Path | Choice
    options: Option<Vec<String>>,            // for Choice
},

/// Open-question pool entry was eliminated (DecisionLocked landed for it,
/// or restore-time elimination per Z8-D33a).
OpenQuestionEliminated {
    id: DecisionId,
    reason: String,                          // "locked" | "already-locked" | "user-skipped"
},

/// N=2 turns elapsed after an open question with no DecisionLocked.
/// Z8-D48.
OpenQuestionUnanswered {
    id: DecisionId,
    turns_elapsed: u32,
    prompt: String,
},

/// One-time event emitted when a pre-spec session_id directory was
/// migrated to the thread_id-keyed layout. §3.0.2.
ThreadIdMigrated {
    session_id: SessionId,
    thread_id: ThreadId,
},
```

#### 4.1.1 DecisionValue (closed)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
#[non_exhaustive]
pub enum DecisionValue {
    String(String),                          // non-empty, ≤4096 bytes UTF-8
    Bool(bool),
    I64(i64),
    Path(String),                            // canonicalized per spec-multi-repo-workspace-model.md §3
    Choice { options: Vec<String>, selected: u32 },
}
```

`DecisionValue::Json` is **NOT** in v1 (deferred per §1.2).

#### 4.1.2 DecisionSource (closed)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionSource {
    User,
    Agent,
}
```

(`System` removed per rubber-duck #10.)

#### 4.1.3 DecisionState (closed)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionState { Locked, Unlocked }
```

#### 4.1.4 ProducerId

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProducerId(pub String); // ASCII; e.g. "agent-runner-claude", "ipc-handler", "zed-cli"
```

### 4.2 DecisionEntry on-the-wire

Already shown in §3.2.1. Constraints:

- `state == Unlocked` ⇔ `current == None`. (Z8-D17.)
- `history` is non-empty whenever the entry exists. (Z8-D16.)
- `history` is append-only.

### 4.3 On-disk schema

```json
{
  "schema_version": 1,
  "thread_id": "<uuid-v7>",
  "boot_id": "<uuid-v7>",
  "last_event_seq": 0,
  "last_mutation": "2026-05-03T18:00:00Z",
  "entries": {
    "naming/framework": { /* DecisionEntry */ },
    "...": { /* ... */ }
  }
}
```

Schema version bumps follow `spec-cross-cutting-wiring.md` §3.10. Migration policy: §3.0.2.

### 4.4 DecisionRegisterError codes (closed enum)

| Code | When | Recovery |
|---|---|---|
| `implicit-amend-forbidden` | `DecisionLocked` for an existing Locked id with different value. | Agent retries as `DecisionAmended`. |
| `amend-of-unlocked` | `DecisionAmended` for an unknown or `Unlocked` id. | Agent emits `DecisionLocked` instead. |
| `unlock-of-unlocked` | `DecisionUnlocked` for an unknown or `Unlocked` id. | No-op; agent ignores. |
| `stale-amend` | `DecisionAmended.prior_value != entries[id].current`. | Caller re-reads state, re-issues with correct prior_value. |
| `agent-overrode-user` | Agent attempted to silently override a User-sourced decision. | Agent emits `DecisionAmended` with reason naming the override. |
| `amend-without-reason` | `DecisionAmended` / `Unlocked` with empty reason. | Caller adds reason. |
| `invalid-id` | `DecisionId` violates §3.1.2. | Producer fixes id. |
| `invalid-value-shape` | `Choice.selected >= options.len()`, empty `String`, etc. | Producer fixes. |
| `non-canonical-path` | `Path` violates `spec-multi-repo-workspace-model.md` §3. | Producer canonicalizes. |
| `out-of-order` | `EventSeq` regression within same `BootId`. | Fatal. |
| `quarantine-on-restore` | An on-disk entry failed validation during RestoreProtocol step 2. | Logged, entry quarantined. |

New codes MUST be added by spec amendment.

### 4.5 IPC surfaces

Four IPC methods on the orchestrator. **All four enqueue events on the orchestrator's event stream; they do NOT mutate reducer state directly.** (Z8-D45.) IPC return values are computed AFTER the enqueued event is applied (synchronous from the caller's POV, async at the wire level).

- `list_decisions(thread_id) -> Vec<DecisionEntry>` — read-only enumeration. Locally-trusted callers only.
- `lock_decision(thread_id, id, value, reason?) -> Result<DecisionEntry, DecisionRegisterError>`
- `amend_decision(thread_id, id, value, prior_value, reason) -> Result<DecisionEntry, DecisionRegisterError>`
- `unlock_decision(thread_id, id, reason) -> Result<DecisionEntry, DecisionRegisterError>` — added per rubber-duck #4.

---

## §5 Invariants (Z8-D series)

| Tag | Invariant |
|---|---|
| **Z8-D1** | Free text "✅" rendering MUST NOT substitute for a decision event. Agent MUST emit the event whenever it interprets a user reply as closing an open question. |
| **Z8-D2** | `DecisionId` is stable within a thread; re-asking reuses the prior id. |
| **Z8-D2a** | `DecisionId` is assigned explicitly by the agent at open-question presentation. No auto-derivation in v1. |
| **Z8-D3** | `DecisionId` matches `^[a-z0-9][a-z0-9_/-]{0,127}$`, ASCII. |
| **Z8-D3a** | `DecisionValue::Choice.selected < options.len()`. |
| **Z8-D3b** | `DecisionValue::String` is non-empty, ≤4096 bytes UTF-8. |
| **Z8-D4** | `DecisionValue::Path` canonicalized per `spec-multi-repo-workspace-model.md` §3. |
| **Z8-D5** | Re-emitting `DecisionLocked` with same `(id, current_value)` is a no-op. |
| **Z8-D5b** | Re-emitting `DecisionLocked` for an `Unlocked` id is a re-lock; new history record appended. |
| **Z8-D6** | Re-emitting `DecisionLocked` for a `Locked` id with a different value is a reducer error (`implicit-amend-forbidden`). Agent MUST use `DecisionAmended`. |
| **Z8-D7** | `DecisionAmended` for an unknown id or `Unlocked` id is rejected. |
| **Z8-D8** | `DecisionUnlocked` for an unknown id or `Unlocked` id is rejected. |
| **Z8-D9** | `DecisionAmended` / `DecisionUnlocked` MUST carry a non-empty `reason`. |
| **Z8-D9a** | `DecisionAmended` MUST carry the correct `prior_value`; mismatch is `stale-amend`. |
| **Z8-D10** | The reducer is a pure function `(state, event) → state`; no side effects in the reducer. |
| **Z8-D11** | Out-of-order events within a single `BootId` are a fatal reducer error. |
| **Z8-D12** | The reducer reads events in `(BootId, EventSeq)` order; cross-process: `(producer_id, BootId, EventSeq)`. |
| **Z8-D13** | The reducer is sparse: non-decision events pass through unchanged. |
| **Z8-D14** | Conversation-history compaction MUST NOT undo decision-register mutations. |
| **Z8-D15** | The DecisionRegister survives transcript-wide compaction. |
| **Z8-D16** | `DecisionEntry.history` is append-only and non-empty whenever the entry exists. |
| **Z8-D17** | `state == Unlocked` ⇔ `current == None`. |
| **Z8-D18** | Persistence captures full history of every entry, not just `current`. |
| **Z8-D18a** | The reducer MUST persist at the end of any agent turn that emitted at least one decision event (end-of-turn rule). |
| **Z8-D19a** | Atomic-rename pattern: temp file in same dir, fsync temp, rename, fsync parent dir. |
| **Z8-D19e** | Crash recovery: prefer the non-temp file when both exist. |
| **Z8-D20** | Daemon restart emits `SessionResumed { thread_id, prior_boot_id, new_boot_id, register_size, transcript_size }` before any reducer state change. |
| **Z8-D21** | `WorkspaceMutationEvent` MUST trigger RestoreProtocol after the mutation lands and before the next agent turn. |
| **Z8-D22** | `BootId` change observed at the agent runner MUST trigger RestoreProtocol. |
| **Z8-D23** | RestoreProtocol is idempotent within a boot. |
| **Z8-D24** | `DecisionLocked { id }` removes `id` from the open-question pool. |
| **Z8-D25** | RestoreProtocol MUST emit `DecisionRegisterRestored` even when register is empty. |
| **Z8-D26** | RestoreProtocol MUST run before the orchestrator forwards the next user message to the agent. |
| **Z8-D27a** | When ReconciliationMessage exceeds 6000 bytes UTF-8, selection is recency-first; display order within selected subset is lexicographic by `id`. |
| **Z8-D28** | ReconciliationMessage entries displayed sorted by `DecisionId` lex. |
| **Z8-D29** | ReconciliationMessage is `system`-role; agent treats as authoritative context. |
| **Z8-D30** | Workspace-mutation handler mutates ONLY `WorkspaceContext`; static check forbids edges to session/transcript/thread-mint paths. |
| **Z8-D31** | Agent runner's view of transcript identical pre/post mutation modulo the appended event. |
| **Z8-D32** | `ThreadId` MUST NOT change on `WorkspaceMutationEvent`. |
| **Z8-D33** | Plan→Act transition MUST NOT re-ask any `DecisionId` with `state == Locked`. |
| **Z8-D33a** | Structural enforcement: orchestrator subtracts `Locked` ids from the open-question pool when constructing the next agent input. (Primary mechanism.) |
| **Z8-D34** | Plan→Act transition surfaces register-derived locked values in auto-generated scaffolding prompts. |
| **Z8-D35** | `ModeChanged` at plan→act includes `carried_decisions: u32`. |
| **Z8-D36** | `DecisionAmended` reason field MUST be non-empty. |
| **Z8-D37** | `DecisionUnlocked` preserves history; subsequent re-lock starts fresh state, inherits visible history. |
| **Z8-D38** | Agent MUST NOT silently override a User-sourced decision; rejected as `agent-overrode-user`. |
| **Z8-D40** | `ThreadId` is stable across `SessionId` rebinds; resolved via `~/.caduceus/sessions/<session_id>/thread_id` index. |
| **Z8-D41** | `WorkspaceMutationEvent` MUST NOT mint a new `ThreadId`. |
| **Z8-D42** | Durable session state (register, transcript, mode, envelope) is keyed by `ThreadId`, not `SessionId`. |
| **Z8-D43** | Session→thread index file is single-line UTF-8, atomic-rename written. |
| **Z8-D44** | Cross-process event ordering: lex `(producer_id, BootId, EventSeq)`. |
| **Z8-D44a** | Two-runner same-id second arrival sees post-first-event state; mismatched value triggers `implicit-amend-forbidden`. |
| **Z8-D45** | IPC handlers enqueue events on the orchestrator's event stream; they do NOT mutate reducer state directly (preserves I-1). |
| **Z8-D46** | `lock_decision` IPC's `Result` reflects the post-apply state; synchronous from caller's POV. |
| **Z8-D47** | Permission denial surfaces as `DecisionLockDenied`; recorded in entry history with `op = LockDenied`; state unchanged. |
| **Z8-D48** | Open question with no `DecisionLocked` after N=2 turns surfaces `OpenQuestionUnanswered`; does NOT auto-create a decision. |
| **Z8-D49** | All decision events carry `producer_id` for cross-process ordering and audit traceability. |

---

## §6 Acceptance (test contract)

The following tests are normative. Implementations MUST pass all of them; failure of any is a spec violation.

### 6.1 Reducer unit tests (`caduceus-orchestrator/tests/decision_register_reducer.rs`)

1. `test_z8_d1_lock_creates_entry` — single `DecisionLocked` produces one entry; `state=Locked`, `current=Some(value)`, history len 1.
2. `test_z8_d5_idempotent_relock` — re-locking same `(id, value)` is no-op; no history append; no audit event.
3. `test_z8_d6_implicit_amend_forbidden` — re-locking `(id, different_value)` produces `DecisionRegisterError { code: "implicit-amend-forbidden" }`; reducer state unchanged. (Per rubber-duck #3.)
4. `test_z8_d7_amend_unknown_id_is_error` — `DecisionAmended` for unknown id → `code: "amend-of-unlocked"`.
5. `test_z8_d9_amend_without_reason_is_error` — empty reason → `code: "amend-without-reason"`.
6. `test_z8_d9a_stale_amend_is_error` — `prior_value` mismatch → `code: "stale-amend"`.
7. `test_z8_d11_out_of_order_is_fatal` — same-boot `EventSeq` regression returns fatal Err.
8. `test_z8_d14_compaction_does_not_undo_register` — emit lock, then `ContextGroupsEvicted` referencing the message; assert register unchanged.
9. `test_z8_d16_history_append_only` — three amends, history len == 4, current == last; oldest untouched.
10. `test_z8_d17_unlocked_implies_no_current` — invariant check after unlock.
11. `test_z8_d38_agent_override_user_is_error` — agent emits Lock with different value than User-sourced existing → `agent-overrode-user`.

### 6.2 Persistence tests (`caduceus-orchestrator/tests/decision_register_persist.rs`)

12. `test_z8_d18_persists_full_history` — lock + amend + amend; restart reducer; full history recovered.
13. `test_z8_d18a_end_of_turn_persist` — turn that emits 5 locks → register on disk after turn end, even with `N`/`M` thresholds not reached.
14. `test_z8_d19a_atomic_rename_pattern` — assert temp-file exists with `.tmp.` infix mid-write; never the target file.
15. `test_z8_d19e_crash_recovery_prefers_target` — synthesize state with both target and temp present; assert target loaded.
16. `test_persist_fsync_called` — assert `libc::fsync` (or platform analogue) called for both temp file and parent dir; via mock.

### 6.3 Restore protocol tests (`caduceus-orchestrator/tests/decision_register_restore.rs`)

17. `test_z8_d21_workspace_mutation_triggers_restore` — emit `WorkspaceRootAdded`; assert RestoreProtocol ran exactly once before next agent forward.
18. `test_z8_d23_restore_idempotent` — run RestoreProtocol twice; same eliminations + ReconciliationMessage; single `DecisionRegisterRestored` (cached).
19. `test_z8_d24_lock_eliminates_open_question` — emit `OpenQuestionPresented` then `DecisionLocked`; assert pool empty.
20. `test_z8_d25_empty_register_still_emits_restored` — empty register → `DecisionRegisterRestored { count: 0 }`.
21. `test_z8_d27a_truncation_above_6000_bytes` — synthesize 200 entries; assert truncated message + `truncated: true`; selection is recency-first; displayed entries are lex-sorted.
22. `test_z8_d28_message_displayed_lex_sorted` — locks emitted in time order A-C-B; ReconciliationMessage lists A-B-C.
23. `test_z8_d29_message_role_is_system` — restore message has `role: "system"`.
24. `test_z8_d33a_structural_elimination_overrides_text` — even with no ReconciliationMessage, structural elimination prevents re-ask. (The structural prong is primary.)

### 6.4 Workspace-mutation invariant tests (`caduceus-orchestrator/tests/workspace_mutation_invariant.rs`)

25. `test_z8_d30_mutation_does_not_truncate_transcript` — append 5 messages; emit `WorkspaceRootAdded`; transcript length still 5 + 1.
26. `test_z8_d31_agent_view_identical` — agent runner snapshot pre-mutation == post-mutation, modulo the event.
27. `test_z8_d32_thread_id_stable` — mutation does not change `ThreadId`; `BootId` unchanged unless daemon restarts.
28. `test_z8_d30_static_check_handler_isolation` — *build-time* static check (a `#[test]` that uses `cargo-public-api` or hand-rolled symbol introspection) asserts the workspace-mutation handler module has no `use` edges to session-creation, transcript-rebind, or `ThreadId`-mint paths. (This is the enforcement mechanism for Z8-D30.)

### 6.5 ThreadId tests (`caduceus-orchestrator/tests/thread_id.rs`)

29. `test_z8_d40_session_rebind_resolves_same_thread_id` — write index file; re-resolve with new `SessionId`; same `ThreadId` returned.
30. `test_z8_d41_workspace_mutation_does_not_mint_thread_id` — emit mutation; `ThreadId` unchanged.
31. `test_z8_d42_durable_state_under_thread_id_path` — assert files at `~/.caduceus/threads/<thread_id>/`, not `~/.caduceus/sessions/<session_id>/`.
32. `test_z8_d43_index_file_atomic_rename` — write index; assert temp-file pattern.
33. `test_thread_id_migration_idempotent` — pre-spec layout; run migration twice; second run is no-op; `ThreadIdMigrated` emitted only on first run.

### 6.6 End-to-end thread replay (`caduceus-orchestrator/tests/aletheia_thread_replay.rs`)

This test is **the** regression-guard for the original failure mode.

#### 6.6.1 Replay fixture format

The test fixture is a directory at `caduceus-orchestrator/tests/fixtures/aletheia_thread/`:

```
fixtures/aletheia_thread/
  events.jsonl         # one AgentEvent per line, chronological
  ipc_calls.jsonl      # one IPC call per line, chronological
  workspace_events.jsonl  # WorkspaceMutationEvents
  scheduling.jsonl     # daemon-restart / runner-attach / detach boundaries
  oracle.json          # expected post-replay state
```

`events.jsonl` schema:
```json
{"t_mono_us": 0, "boot_id": "...", "event_seq": 0, "event": { /* AgentEvent JSON */ }}
```

`scheduling.jsonl` schema:
```json
{"t_mono_us": 1234567, "kind": "daemon_restart" | "runner_attach" | "runner_detach" | "workspace_mutation_processed", "detail": "..."}
```

`oracle.json` schema:
```json
{
  "expected_register": { /* DecisionRegister at end of replay */ },
  "expected_open_questions": [],
  "expected_no_re_ask_ids": [
    "student/substrate", "teacher/provider", "naming",
    "path/scaffold-root", "capabilities/scope"
  ],
  "expected_session_resumed_count": 1,
  "expected_decision_register_restored_count": 1
}
```

#### 6.6.2 The test

34. `test_aletheia_thread_no_failure_a_b` — replay the fixture deterministically:
    - Apply events in `(BootId, EventSeq)` order.
    - At each `scheduling.jsonl` boundary, simulate the daemon/runner action.
    - At end of replay, assert:
      - The reducer's final state matches `oracle.expected_register` exactly.
      - The orchestrator's open-question pool is `oracle.expected_open_questions` exactly.
      - For each `id` in `oracle.expected_no_re_ask_ids`, the agent runner has NOT received an `OpenQuestionPresented` event for that id after the workspace mutation. (Direct assertion of "no re-ask".)
      - `SessionResumed` count == `oracle.expected_session_resumed_count`.
      - `DecisionRegisterRestored` count == `oracle.expected_decision_register_restored_count`.

The fixture is constructed by hand from the original aletheia thread (paste-1777834122173.txt), translating each turn into the appropriate event and scheduling boundary. The test is a literal regression: pre-spec, the assertion on `expected_no_re_ask_ids` fails (the engine re-asks); post-spec, it passes.

### 6.7 Concurrency tests (`caduceus-orchestrator/tests/decision_register_concurrency.rs`)

35. `test_z8_d44_two_runners_same_id` — two `producer_id`s emit `DecisionLocked` for same id, same value: deterministic single applied event.
36. `test_z8_d44a_two_runners_different_value` — two `producer_id`s emit `DecisionLocked` for same id with different values: first applied, second triggers `implicit-amend-forbidden`.
37. `test_z8_d45_user_amend_vs_agent_amend_race` — IPC `amend_decision` and agent `DecisionAmended` enqueued in arrival order; both succeed sequentially via `prior_value` discipline; whichever is second sees `stale-amend`.
38. `test_z8_d47_permission_denied_surfaces_event` — agent emits `DecisionLocked` against an envelope that denies; reducer applies `DecisionLockDenied` with `op = LockDenied` in history; state unchanged.
39. `test_z8_d48_open_question_unanswered` — emit `OpenQuestionPresented`, then 2 agent turns with no matching lock; assert `OpenQuestionUnanswered { id, turns_elapsed: 2 }`.

### 6.8 Plan→Act transition tests (`caduceus-orchestrator/tests/plan_act_transition.rs`)

40. `test_z8_d33_act_does_not_reask_locked` — lock 3 in plan; switch to act; assert prompt builder consumes register, doesn't ask again.
41. `test_z8_d33a_structural_elimination_at_transition` — open-question pool is empty for all locked ids after transition.
42. `test_z8_d34_act_prompt_includes_locked_values` — auto-scaffold prompt contains all locked values verbatim.
43. `test_z8_d35_mode_changed_carries_count` — `ModeChanged.carried_decisions == 3`.

### 6.9 IPC tests (`caduceus-orchestrator/tests/decision_register_ipc.rs`)

44. `test_lock_decision_via_ipc_enqueues_event` — IPC call observed as a `DecisionLocked` event on the orchestrator's stream; not as a direct state mutation. (Z8-D45 enforcement.)
45. `test_amend_via_ipc_returns_post_apply_state` — IPC `amend_decision` returns `Ok(DecisionEntry)` with the new value; synchronous from caller POV.
46. `test_unlock_via_ipc_marks_unlocked` — IPC `unlock_decision`; entry's `state == Unlocked`, `current == None`, history grows by 1.

### 6.10 Documentation tests

47. `test_doc_examples_compile` — every Rust snippet in §3 and §4 of this spec MUST be a `///` doctest in `caduceus-core` and pass `cargo test --doc -p caduceus-core`. Catches spec drift.

---

## §7 Out of scope / deferred

- Decision UI presentation. → `spec-zed-plan-panel` (forward).
- Cross-thread / cross-session decision libraries. → `spec-decision-library` (proposed).
- Decision amendment **policy** (who may amend what). Mechanism here; policy in `caduceus-permissions`.
- Notice/banner surfacing. → `spec-notice-notification`.
- Conversation-history compaction algorithm. Interaction normative; algorithm in its owning spec.
- Decision import/export bulk operations. v1 IPC is four primitives only.
- Telemetry. v2 opt-in signal in `spec-cross-cutting-wiring.md` §3.5.
- The agent's prompt template. Contract here; body owned by agent runner spec.
- `DecisionValue::Json` (deferred to v2 if a real shape demands it).
- `lock_count` field (cut per rubber-duck #7).
- `DecisionSource::System` (cut per rubber-duck #10).

---

## §8 Open questions

- **OQ-1.** Default values for the throttle thresholds (`N=10`, `M=2`s) and the false-negative window (`N=2` turns). Adaptive vs. static? Current default: static, configurable. Reconsider after telemetry.
- **OQ-2.** Auto-derived `DecisionId` from prompt text via stable hash (BLAKE3-128 of NFKC-normalized whitespace-collapsed lowercased prompt). Pro: removes "agent forgot to specify id" bugs. Con: paraphrase collisions. Recommended: defer; v1 requires explicit ids.
- **OQ-3.** ~~Add `unlock_decision` IPC in v1?~~ **Resolved (yes)** per rubber-duck #4. v1 surface is four IPC methods.
- **OQ-4.** When `derived_from: Some(_)` is set, should ReconciliationMessage label it differently (e.g. `<value> (derived from <basis>)`)? Defer; render same as User-sourced for now to keep message clean.
- **OQ-5.** Cross-process ordering tie-break: deterministic ASCII order on `producer_id` is currently specified (Z8-D44). Verify with a chaos test (test 35 in §6.7) before declaring stable.
- **OQ-6.** `read_my_decisions` AgentTool for self-introspection. Defer to v2.
- **OQ-7.** Schema migration policy beyond v1 → v2. Codify in `spec-cross-cutting-wiring.md` versioning addendum when v2 is proposed.
- **OQ-8.** `AgentEvent::OpenQuestionUnanswered` is observable but not consumed automatically. Should the user surface auto-prompt the user when it fires? Defer; UI policy.

---

## §9 Acknowledgments

This spec exists because a plan-mode thread on framework `Aletheia` (SLM-from-LLM distillation harness, Karpathy-stack) lost context twice in succession when `/Users/alexkeagel/Dev/aletheia` was added as a project root mid-thread:

- **Failure A:** hard transcript loss for two turns; the engine treated the session as fresh.
- **Failure B:** transcript came back, but the locked decisions were not surfaced as structured state, so the engine re-asked questions whose answers it had already checkboxed.

Both were latent in the architecture: free-text "✅" rendering was the only place locked decisions lived, AND durable state was keyed by ephemeral `SessionId`. This spec makes decisions first-class structured state AND introduces `ThreadId` as a separate, durable storage key, AND enforces non-re-ask structurally at the orchestrator (open-question pool elimination) rather than only textually at the agent (ReconciliationMessage).

The thread, replayed deterministically, is acceptance test 34 in §6.6. If the spec is implemented correctly, that thread runs to scaffold-completion without re-asking any of the five locked decisions.

The first draft of this spec was attacked by the rubber-duck reviewer; this version (post-iteration-1) addresses all five blocking issues, the four coverage gaps, and the precision/cross-spec consistency findings the review surfaced. The cut list (`Json`, `lock_count`, `derived` flag rebranded as `derived_from`, `DecisionSource::System`) reflects the review's verdict that they were speculation rather than responses to observed failures.
