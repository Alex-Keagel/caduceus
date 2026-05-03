# caduceus: Orchestrator Status Snapshot

**Spec ID:** #4
**Status:** Draft, scope-locked.
**Audience:** `caduceusd` implementors, engine and zed client implementors,
spec authors of #7 (run-identity) and #8 (runs-panel).

> **⚠️ Known residual issues — iter-28 backlog (2026-04-29).**
> The following items were surfaced by `gpt-5.4` standalone review at iter-27
> with verbatim replacement text saved in
> `private/reviews/iter27-spec4-gpt.md`. They were not blocking for the
> iter-27 ship — the spec converged on `claude-opus-4.6` + `gpt-5.3-codex`
> at min 9 / 8 respectively (SHIP under the original tie-break rule).
> Resolve in iter-28+.
>
> 1. **§1.2 trust/redaction + §4.1/§4.4/§4.5 wire shapes** — non-local
>    redaction currently mutates v1 wire shapes (nulls required
>    `workspace_path`, substitutes undeclared replacement objects for
>    `last_event` / `event_log_tail` / `hook_log`). That is a hidden
>    second API. Replace with: v1 surface is local-only — non-local
>    transports MUST reject with `SnapshotError::Unavailable` or
>    terminate at a separately-versioned API; field presence/type/meaning
>    MUST NOT change as redaction.
> 2. **§4.1 `RunStatus` + §4.5 `RunDetail`** — finished-run `RunDetail`
>    is structurally underdefined: `RunStatus` lacks `Finished`;
>    `RunDetail` lacks `exit_reason`. Add `RunStatus::Finished` and
>    `RunDetail.exit_reason: Option<ExitReason>` (`Some` iff `Finished`).
> 3. **§3.4 clause-(d′) detection vs Z-29 boot edge** — detection requires
>    replay-index hit at `s_c`, but at boot the replay index is empty.
>    Define `fp_at(s_c)`: returns `current_fingerprint` when `s_c ==
>    current_stream_seq`, else replay-index entry; if absent, clause (a)
>    or (P) fires instead. Clause (d′) fires exactly when
>    `fp_at(s_c) != last_known_fingerprint`.
> 4. **§3.4 vacuous-`UpToDate` / clause-(d) duplication** — same rule
>    restated in 5+ near-identical wordings. Collapse to one **subscribe
>    outcome algorithm** (clause priority `(a) > (P) > (b) > (c) > (d) >
>    (d′)`; vacuous `UpToDate` only when none match; clause (d) is the
>    sole non-vacuous `UpToDate`). All other prose cites, never restates.
> 5. **§3.4 replay-cancel recovery** — subscriber MUST treat
>    `ReplayCancelled` as terminal and re-subscribe with
>    `since_fingerprint = None`, `since_stream_seq = None`,
>    `since_boot_id = Some(last_ack.boot_id)` (`None` only for pre-v1
>    clients) — deterministically routes through clause (c).
> 6. **T-3** — currently MUST-asserts on `RunRow.tokens.absolute_total`
>    which §4.7 declares MAY-only. Rewrite against mandatory token fields
>    (`input_tokens`, `output_tokens`); also assert
>    `tokens_aggregate.{input,output}_tokens` agreement with per-Run totals.

---

## 0. Header & Attribution

This document is the normative specification for the **status snapshot**
surface exposed by `caduceusd` — the read-only projection of orchestrator
state that drives every UI surface (CLI, web, zed runs panel, third-party
clients). It defines the snapshot data shape, the RPC entry points, the
PubSub delta channel, and the consistency invariants between the snapshot
and the orchestrator state defined in spec #1. RFC-2119 keywords are used
in their RFC-2119 sense.

Reference implementation (Symphony, commit `58cf97d`): `presenter.ex`
(canonical projection); `orchestrator.ex:1178–1195` and `:1362–1403`
(token-delta accounting); `observability_pubsub.ex` (broadcast pattern);
`SPEC.md` §13.3 (snapshot interface), §13.5 (token accounting), §13.7.2
(JSON shape), §4.1.8 (orchestrator state). Caduceus divergences are
tagged **(caduceus-new)**; consequential ones: `repo_coordinate` on every
row (§4.1, I-5), `Disconnected` bucket (§4.3, I-6), `SnapshotFingerprint`
(§4.6, I-7), `subscribe(channel, since_fingerprint, since_stream_seq, since_boot_id)` resync (§3.4). Apache-2.0;
verbatim Symphony quotations carry `(file:lines, SPEC §)` attribution.

---

## 1. Scope

### 1.1 In scope

- Shape of `AggregateSnapshot`, `RunRow`, `RetryRow` (§4).
- Snapshot RPC interface: request/response/error envelopes, the
  `Aggregate | Run(run_id)` scope discriminator (§3.2).
- PubSub delta channel: `SnapshotDelta` shape, emission rules,
  reconciliation (§3.3, §3.4); `subscribe(channel, since_fingerprint, since_stream_seq, since_boot_id)` resync.
- Token-accounting projection rule inherited from spec #2 §4.3 /
  Symphony SPEC §13.5 (cited verbatim; §3.1 step 3, I-3).
- `RunDetail` per-Run detail endpoint (§4.5).
- Snapshot/event-log consistency invariant (I-4): replay reconstructs
  the snapshot.

### 1.2 Out of scope

- Orchestrator decision logic → spec #1. Agent runner internals,
  JSONL schema, token reconciliation algorithm → spec #2. Workspace
  filesystem layout → spec #3. Event taxonomy → spec #5. UI
  presentation → spec #7, spec #8.
- Specific RPC transport, wire encoding, authentication, IPC socket
  location → future ADR. This spec is transport-neutral.
- **Transport trust requirement (normative).** The snapshot channel
  MUST run over a transport that is locally-trusted (Unix domain
  socket with `SO_PEERCRED` UID match, named pipe with
  platform-equivalent peer auth) OR mutually-authenticated (mTLS with
  daemon-issued client certs).

  **Redaction on non-locally-trusted transports (normative — Z6-N1).**
  On any transport NOT classified as locally-trusted per the above,
  the daemon MUST redact the following fields before serialization:

  - `pid` — replace with `null` or omit.
  - `workspace_path` — replace with `null` or omit.
  - `parent_path_redacted_unless_trusted` — replace with `None`
    (see §4.5 `WorkspaceMeta`).
  - `error_message` — replace with sanitized `error_kind` (enum-only,
    no free-form text).
  - `last_event` — replace with `{ kind, timestamp }` only (drop
    `text` and `truncated`; see §4.4 for the full `LastEventSummary`
    shape).
  - `event_log_tail` — replace with `{ count: usize }` only.
  - `hook_log` — replace with `{ count: usize }` only.
  - `rate_limit.raw` — replace with coarse-bucketed values only. The
    standard typed fields of `RateLimitBlob` MAY remain, but any numeric
    quota, retry, or duration values exposed on non-locally-trusted
    transports MUST be serialized as coarse buckets rather than raw values.
  - `session_id` — replace with `HMAC(session_id, daemon_startup_secret)`
    so subscribers can correlate without recovering the raw id. The
    HMAC key is the per-daemon-startup secret (regenerated on every
    daemon boot — fresh `boot_id` ⇒ fresh key).
  - `repo_coordinate.remote_url` — replace with `None` on
    non-locally-trusted transports.
  - `stage` (free-form agent text) — replace with `"<redacted>"` on
    non-locally-trusted transports.
  - `WorkspaceMeta.slug` — replace with `None` on non-locally-trusted
    transports.
  - `tokens` per-row + `tokens_aggregate` — bucketed ranges (or null)
    on non-locally-trusted transports.

  Redaction is applied at the serialization boundary (post-`build_snapshot`,
  pre-wire). Locally-trusted callers receive the unredacted form. This
  list is normative-exhaustive; any field not enumerated here passes
  through unredacted, and adding a leak vector requires a spec bump.
- Snapshot persistence across daemon restart. Snapshot is in-memory;
  on restart the daemon re-derives from `WorkSource` (spec #1 I-6)
  plus reattaching agents.
- Multi-daemon federation (multirepo-ux Q8) — deferred. Token-budget
  *enforcement* — spec #1 §8.5; snapshot only *exposes*.

---

## 2. Terms

| Term | Definition |
|---|---|
| **Snapshot** | A point-in-time, read-only projection of `OrchestratorState` (spec #1 §4) into a wire-stable shape suitable for UI consumption. Synonym for the result of `build_snapshot` (§3.1). |
| **AggregateSnapshot** | The full snapshot variant: all running runs + all retrying runs + all disconnected runs + aggregate counters. Shape in §4.3. |
| **RunRow** | One row of the snapshot's `running` (or `disconnected`) bucket — one live `RunAttempt`. Shape in §4.1. |
| **RetryRow** | One row of the snapshot's `retrying` bucket — one Run waiting for a backoff timer. Shape in §4.2. |
| **TokenTotals** | Cumulative `{input_tokens, output_tokens, cache_read, cache_write, seconds_running}` for one Run *or* aggregated across all Runs. Field shape in §4.7; reconciliation rule inherited from spec #2 §4.3. |
| **RateLimitBlob** | Opaque, agent-supplied rate-limit posture for the headline ("primary 12345/20000 reset 30s …"). Shape in §4.8; passes through unparsed. |
| **LastEventSummary** | A bounded, human-readable tail of the agent's event stream rendered as one row's "what is it doing right now" cell. Shape in §4.4. |
| **RunDetail** | The `Run(run_id)` scope variant: `RunRow` extended with full event-log tail, full token-history, prompt-hash trail, hook-execution log. Shape in §4.5. |
| **SnapshotChannel** | The broadcast channel name the daemon publishes deltas on. PubSub topic in §3.3. |
| **SnapshotRequestId** | Caller-allocated opaque identifier echoed in the response; lets clients correlate concurrent requests. Type: `u64` (or wire-equivalent). |
| **SnapshotFingerprint** | Deterministic 128-bit hash of the snapshot's identity-bearing fields (§4.6). Used by `subscribe` to decide stale vs current (§3.4). |
| **SnapshotDelta** | One of `RunStarted`, `RunUpdated`, `RunFinished`, `RetryScheduled`, `RetryUpdated`, `RetryCleared`, `AggregateUpdated` (§3.3). Carries a small payload describing what changed; subscribers apply locally. |
| **PostAckFrame** | Post-ack subscription wire frame (§3.4): `Delta(SnapshotDelta)` for live deltas or `ReplayCancelled { stream_seq, reason }` as a terminal cancellation under clause-(d) replay. Carries everything emitted on a subscribe stream *after* the initial `SubscribeAck`. |
| **SubscribeFrame** | Outer subscribe-stream envelope (§3.4): `Ack(SubscribeAck)` (exactly one, first frame) followed by zero or more `PostAck(PostAckFrame)` frames. The full subscriber-bound stream type is `Stream<SubscribeFrame>`. |
| **ReplayCancelReason** | Discriminator on `PostAckFrame::ReplayCancelled`; one of `LockStepTrimRace` (pre-commit detection — T-31b), `RingEviction` (post-commit discovery — T-31), `BufferEviction` (non-trim payload fault — T-31a). Partition is normatively exhaustive over replay-fault detection sites; see §3.4 *`ReplayCancelReason` variants*. All three terminate the subscription stream; subscriber recovers by re-subscribing into clause (c). |
| **Disconnected** | A `RunRow` whose engine-side agent connection has dropped while the daemon still believes the Run is alive. Transient state, bounded by `disconnect_timeout_ms` (§5 I-6). **(caduceus-new)** |
| **Engine** | The `caduceus` engine process that hosts the agent runner (spec #2). Connects to `caduceusd` over RPC; one engine MAY host one or many concurrent runners. |
| **Daemon** | `caduceusd`, the orchestrator process (spec #1). Owns `OrchestratorState` and is the sole producer of snapshots. |

---

## 3. Normative algorithms

### 3.1 `build_snapshot(state, options) → AggregateSnapshot`

**Purpose.** Project the live `OrchestratorState` (spec #1 §4) into an
`AggregateSnapshot` (§4.3). Pure projection: MUST NOT mutate `state`
(I-1).

**Inputs:** `state: &OrchestratorState` (borrowed read-only); `options:
SnapshotOptions` = `{ snapshot_timeout_ms: u32 = 200,
last_event_max_bytes: u16 = 240, runtime_now: SystemTime }`.

> **Bound semantics (normative).** `last_event_max_bytes` is a bound on
> the **UTF-8 byte length** of the projected `LastEventSummary.text`,
> NOT on character/code-point count. Truncation at the multi-byte
> boundary MUST yield a valid UTF-8 string: implementations MUST
> truncate to the last complete codepoint at or before the byte limit
> (i.e., never split a multi-byte sequence). See §4.4.

**Output:** `Result<AggregateSnapshot, SnapshotError>`.

**Clock-capture invariant (normative — Z6-K1).** The `build_snapshot`
fixpoint MUST capture `mono_now: Instant` and `wall_now: SystemTime`
exactly ONCE at function entry (step 1) and reuse those values
throughout the body of the fixpoint. Re-sampling either clock at any
later step is **FORBIDDEN**; downstream steps MUST receive `mono_now`
and `wall_now` as inputs, not re-derive them. Calls to
`Instant::now()` or `SystemTime::now()` after the entry capture are a
spec violation regardless of how cheap they are; even monotonic
re-sampling within a single fixpoint can desynchronise `runtime_total`
from per-row `started_at` projections and produce a self-inconsistent
snapshot.

**Clock-domain projection rule (normative).** `OrchestratorState`
stores all live time fields in the daemon's monotonic clock domain
(`Instant`) per spec #1 I-7 — `RunningEntry.started_at`,
`RetryEntry.scheduled_at`, and `state.next_poll_scheduled` are all
`Instant`. Wall-clock projection happens **at the snapshot boundary,
in this function**, by computing:

```text
wall_now      = options.runtime_now           // SystemTime
mono_now      = Instant::now()                // captured ONCE per build_snapshot invocation
project(t_mono: Instant) -> SystemTime:
    delta = mono_now.saturating_duration_since(t_mono)            // Duration
    return wall_now.checked_sub(delta).unwrap_or(SystemTime::UNIX_EPOCH)
```

The daemon MUST capture a **single** `mono_now` at the start of
`build_snapshot` and reuse it for ALL projections within that snapshot
(and likewise reuse a single `mono_now` for all projections within a
single delta construction). Underflow of `wall_now − delta` (e.g.
`wall_now` reset backwards by NTP step relative to `mono_now`) MUST
clamp to `SystemTime::UNIX_EPOCH` rather than panic on signed-time
overflow.

Every `SystemTime`-typed field on a row (`RunRow.started_at`,
`RetryRow.next_retry_at`, `AggregateSnapshot.next_poll_at`) MUST be
the result of `project(...)` applied to the corresponding `Instant` in
`OrchestratorState`. The projection is recomputed afresh on every
`build_snapshot` invocation; cross-snapshot drift of ≤ 1 ms per second
of elapsed wall time is expected (it is the difference between the
host's monotonic and wall clocks under NTP slew) and is not a bug.
Implementations MUST NOT cache projected `SystemTime` values across
snapshots.

Cite: Symphony's `Orchestrator.snapshot/2`
(`orchestrator.ex:1362–1403`, SPEC §13.3) and the per-row builders in
`presenter.ex:99–158` (`running_entry_payload/1`,
`retrying_entry_payload/1`).

**Steps.**

1. **Project running map.** For each `(run_id, RunningEntry)` in
   `state.running`, build a `RunRow` (§4.1) reading exclusively from
   `RunningEntry`, the WorkSource record cached in
   `RunningEntry.run_snapshot`, and `state.last_reported_tokens[run_id]`
   — the per-Run `TokenTotals` watermark owned by spec #1 §4 (X-5).
   No scalar projection is performed at this step: every component
   field of `TokenTotals` (`input_tokens`, `output_tokens`,
   `cache_read`, `cache_write`, `seconds_running`) is copied through
   verbatim, and `RunRow.tokens` is the resulting `TokenTotals`. Set
   `status = Running` if the engine RPC connection is live;
   `Disconnected` if the daemon has marked it dropped (I-6 governs
   transitions). `repo_coordinate` is REQUIRED (I-5) and read from the
   cached `Run`. `last_event` is the truncated event tail (§4.4).
   `runner_seq` is the per-Run monotonic cursor defined in spec #2 §4.4.

2. **Project retry map.** For each `(run_id, RetryEntry)` in
   `state.retry_attempts`, build a `RetryRow` (§4.2). The `retry_token`
   field MUST be the live `RetryEntry.token` for spec #1 I-4 freshness.

3. **Compute aggregate token totals.** Sum
   `state.last_reported_tokens[run_id]` (spec #1 §4; X-5) across all
   `run_id` in `running ∪ disconnected` (NOT `retrying` — a backed-off
   Run has no live agent contributing tokens *now*; its prior
   contribution was absorbed when its prior attempt exited). Per-Run
   reconciliation is owned by spec #2 §4.3; this spec MUST NOT
   re-derive totals from raw events.

   **Token reconciliation rule:** see spec #2 §4.3 (X-15). That
   section is the single normative copy of the absolute-preferred,
   delta-fallback rule; `build_snapshot` is a strict consumer of the
   already-reconciled `state.last_reported_tokens` watermark.

4. **Compose `RateLimitBlob`.** Pass through the most recent rate-limit
   posture from any active runner; caduceus does NOT merge or parse it
   (Symphony: `presenter.ex:160–198`).

5. **Compute `runtime_total`** as
   `mono_now.saturating_duration_since(min(started_at across
   running ∪ disconnected entries))` using the daemon's monotonic
   clock; **reuse the `mono_now` captured in step 1** (per the
   Clock-capture invariant above) — MUST NOT call `Instant::now()` or
   `SystemTime::now()` here. MUST use saturating subtraction so a
   corrupt or future-dated `started_at` cannot underflow. Result is a
   `Duration`. Zero if both buckets are empty (Symphony:
   `status_dashboard.ex:351–377`).

6. **`next_poll_at` = `state.next_poll_scheduled`** (spec #1 §4); MAY be `None`.

7. **Compute `SnapshotFingerprint`** per §4.6.

8. **Sort.** `running` and `disconnected` MUST be sorted by the
   following total comparator (every pair of rows compares strictly
   to `Less` or `Greater`, never `Equal` — `run_id` is the final
   tiebreaker, and `RunId` is totally ordered):

   1. **Group A first, Group B second.** A row is in *Group A* if
      `issue_ref.is_some()`; otherwise in *Group B*. Group A rows
      sort ahead of Group B rows.
   2. *Within Group A*: ascending by `issue_ref.identifier`. Z-20:
      comparison MUST be **lexicographic over the UTF-8 byte sequence**
      of `identifier` (i.e., a `memcmp` / unsigned-byte ordering, not a
      Unicode collation). Implementations MUST NOT apply locale-aware
      collation, case-folding, or normalisation; identifiers are opaque
      tracker IDs and bit-identical sort order across implementations
      is required for keyboard-nav parity (spec #8). For implementers:
      this matches Rust `str::cmp` / `<[u8]>::cmp`, Go
      `bytes.Compare`/`strings.Compare`, and JS `<` on strings of
      well-formed UTF-8 — but NOT `String.localeCompare` or any
      `LC_COLLATE`-sensitive C `strcoll`.
   3. *Within Group B*: ascending by `started_at` (the projected
      `SystemTime`).
   4. *Final tiebreaker (both groups)*: ascending by `run_id`.

   `retrying` is sorted ascending by `next_retry_at`, with `run_id`
   as final tiebreaker. (Symphony parity: `status_dashboard.ex:584`
   for running, `:653` for retrying — extended here to a strict
   total order so cross-implementation row-position is bit-identical
   for keyboard nav in spec #8.)

9. **Assemble** the `AggregateSnapshot` (§4.3).

10. **Bound.** Total wall time of steps 1–9 MUST NOT exceed
    `options.snapshot_timeout_ms`; on exhaustion, return
    `SnapshotError::Timeout` — no partial snapshot, no side effects
    (I-1, I-2). Default 200ms.

**Side effects:** none. `build_snapshot` does NOT publish, log, or touch
the filesystem. (Diagnostic counters tracking timeout-rate live outside
`OrchestratorState`.)

### 3.2 `dispatch_snapshot_request(request_id, scope) → SnapshotResponse`

RPC entry point for clients; wraps `build_snapshot` in a
request/response envelope.

**Inputs:** `request_id: SnapshotRequestId` (caller-allocated); `scope:
SnapshotScope` = `Aggregate` | `Run { run_id: RunId, options: RunDetailOptions }`.
The `RunDetailOptions` payload (§4.5) is REQUIRED on `Run` requests so
the per-request `event_log_max` is coupled with the §4.5 DoS clamp
(`event_log_max_cap`); a missing or oversized `event_log_max` MUST
be silently clamped to `event_log_max_cap` rather than rejected.

**Output:** `SnapshotResponse { request_id, payload: SnapshotPayload }`
where `SnapshotPayload = Aggregate(AggregateSnapshot) | Run(RunDetail)
| Error(SnapshotError)`.

**Steps.**

1. **Bind to orchestrator main task.** The snapshot MUST be built on
   the orchestrator's owning task (spec #1 I-1; the read MUST observe
   a coherent state). Implementations MAY satisfy this by
   `Cmd::Snapshot { reply: oneshot }` over the command channel, or by
   borrowing `&OrchestratorState` under the main task's `select!` arm.
2. **Branch on scope.** `Aggregate`: call `build_snapshot`; wrap result.
   `Run(run_id)`: look up in `state.running`, `state.retry_attempts`,
   and the bounded `recent_history_ring` (§4.5). If absent everywhere,
   `SnapshotError::RunNotFound`. Otherwise build `RunDetail` (§4.5).
3. **Echo `request_id`** unchanged so clients demultiplex concurrent
   requests over a multiplexed RPC connection.

**Errors** (`SnapshotError`):

| Variant | Condition |
|---|---|
| `Timeout` | `build_snapshot` exceeded `snapshot_timeout_ms`. |
| `Unavailable` | Daemon main task is unhealthy / not currently servicing requests (e.g. mid-shutdown, mid-restart). Symphony parity: `presenter.ex:26–31` returns `:unavailable` when the orchestrator process is down. |
| `RunNotFound { run_id }` | `Run(run_id)` scope, no record. |
| `BudgetTooSmall { requested_ms, minimum_ms }` | Caller-supplied `snapshot_timeout_ms < minimum_ms` (the daemon MAY enforce a floor of 50ms to prevent pathological clients). |
| `PreV1Unsupported` | Subscriber sent a pre-v1 subscribe (`since_boot_id == None`) and the daemon's deployment policy refuses pre-v1 callers (closes the subscribe stream; client must upgrade to v1 — see §3.4 "Pre-v1 cross-incarnation collision risk"). |

The error envelope is part of `SnapshotPayload`; it does NOT terminate
the RPC connection. A client that asks for `Run(unknown_run_id)` MUST
receive `RunNotFound` and remain connected.

### 3.3 `publish_snapshot_delta(channel, delta)`

**Purpose.** Push small, typed deltas describing every observable
`OrchestratorState` change so subscribers keep a local copy in sync
without polling. Cite Symphony's `observability_pubsub.ex` (~25 lines);
the broadcast primitive is `Phoenix.PubSub.broadcast(@pubsub, @topic,
msg)` on topic `"observability:snapshot"`.

**When the daemon MUST publish.** (1) After every state-mutating event
processed by the orchestrator main task — spec #1 §3 enumerates
`poll_tick`, `dispatch_run`, `on_worker_exit`, `on_retry_timer`,
reconcile cascades; after the handler returns and `OrchestratorState`
is at a fixpoint, publish the corresponding delta(s). (2) At least
once per `@minimum_idle_rerender_ms` (default 1000ms; Symphony parity
`status_dashboard.ex:13–16`) even if no event fired — refreshes
`runtime_total`, `age`, `next_poll_at` so clients render advancing
clocks without polling. Idle rerender MUST be `AggregateUpdated` only,
never a per-Run delta.

**Delta variants.**

Every variant carries a daemon-scoped `stream_seq: u64` that is
monotonic across all deltas the daemon emits on this `SnapshotChannel`
(X-2). `stream_seq` is the **transport-level** gap-detection cursor:
subscribers detect any dropped delta — including dropped
`RunStarted`, `RunFinished`, and `AggregateUpdated` events that carry
no `runner_seq` — by observing a non-`+1` jump in `stream_seq` and
trigger `subscribe(channel, since_fingerprint, since_stream_seq, since_boot_id)` (§3.4). Per-Run `runner_seq`
(spec #2 §4.4) is preserved on `RunUpdated` for per-Run ordering.
`stream_seq` resets to `0` on daemon boot (the `boot_id` mixed into
`SnapshotFingerprint` makes cross-incarnation sequences distinguishable).

```rust
enum SnapshotDelta {
    RunStarted   { stream_seq: u64,
                   run_id: RunId, row: RunRow },
    RunUpdated   { stream_seq: u64,
                   run_id: RunId, runner_seq: u64,
                   fields_changed: Map<String, JsonValue> },
    RunFinished  { stream_seq: u64,
                   run_id: RunId, exit_reason: ExitReason,        // spec #1 §4
                   final_tokens: TokenTotals },
    RetryScheduled { stream_seq: u64,
                     run_id: RunId, row: RetryRow },
    RetryUpdated   { stream_seq: u64,
                     run_id: RunId,
                     fields_changed: Map<String, JsonValue> },
    RetryCleared   { stream_seq: u64,
                     run_id: RunId, reason: RetryClearReason },
    AggregateUpdated { stream_seq: u64,
                       tokens_aggregate: TokenTotals,
                       runtime_total: Duration,
                       rate_limit: RateLimitBlob,
                       agents_used: u32,
                       next_poll_at: Option<SystemTime>,
                       snapshot_fingerprint: SnapshotFingerprint },
}

enum RetryClearReason {
    Dispatched,   // RetryEntry consumed by a successful dispatch_run (spec #1 §3.3).
    Cancelled,    // Run cancelled / WorkSource Terminal cleared the retry (spec #1 §3.2 5b).
    Abandoned,    // Run left the WorkSource query (Neither / orphan); claim released without redispatch.
}

// Post-ack frame envelope (§3.4). The post-ack subscription channel
// carries `PostAckFrame` values, not bare `SnapshotDelta`s, so that the
// terminal `ReplayCancelled` frame (clause (d) replay-mode escape hatch)
// is representable on the wire without overloading `SubscribeAck`.
enum PostAckFrame {
    Delta(SnapshotDelta),
    ReplayCancelled { stream_seq: u64, reason: ReplayCancelReason },
}

enum ReplayCancelReason {
    RingEviction,
    BufferEviction,
    LockStepTrimRace,
}

// Outer subscriber-bound stream envelope (§3.4). The full subscribe
// stream carries exactly one initial `SubscribeAck` (handshake reply)
// followed by zero or more `PostAckFrame`s (live deltas and the optional
// terminal `ReplayCancelled`). Modelling the wire as a single
// `Stream<SubscribeFrame>` lets implementers decode the first frame
// (which is NOT a `PostAckFrame`) without an out-of-band channel.
enum SubscribeFrame {
    Ack(SubscribeAck),     // exactly one, first frame on the stream.
    PostAck(PostAckFrame), // zero or more, after the Ack.
}
```

The retry-bucket variants (`RetryScheduled`, `RetryUpdated`,
`RetryCleared`) project `state.retry_attempts` mutations onto the
delta channel symmetrically with the run-bucket variants. They MUST
be emitted under the same orchestrator-owning-task discipline as the
run-bucket variants: every mutation enumerated in spec #1 §3.5
(`on_worker_exit` schedules a retry → `RetryScheduled`; `on_retry_timer`
re-queue with a new `reason` → `RetryUpdated`; successful dispatch /
classification cascade → `RetryCleared`) corresponds to exactly one
emission after the handler reaches its fixpoint.

`fields_changed` carries the wire-named fields that changed since the
prior delta for this Run, e.g. `{"turn_number": 8,
"tokens.absolute_total": 120_450, "stage": "review"}`. Subscribers
apply patches to their local row.

**Delivery semantics.** At-most-once (I-8); the daemon does NOT track
per-subscriber acknowledgements. Per-Run order is preserved via
`runner_seq`; subscribers MUST detect gaps and trigger
`subscribe(channel, since_fingerprint, since_stream_seq, since_boot_id)` (§3.4) to resync. No back-pressure on
the orchestrator: implementations MUST use bounded broadcast channels
and drop on full; dropped subscribers reconcile via resync.

### 3.4 `subscribe(channel, since_fingerprint, since_stream_seq, since_boot_id) → Stream<SubscribeFrame>`

Client connects (or reconnects) and supplies its current
`SnapshotFingerprint`; the daemon decides stale-vs-current and either
ships a fresh baseline or hands the client into the live delta stream.
This is the **C-hybrid reconnect contract**: an engine that crashes and
reattaches, or a zed window that reopens, gets to a coherent view in
one round-trip without waterfalling `Aggregate` requests.

**Inputs:** `since_fingerprint: Option<SnapshotFingerprint>` — `None`
means "I have no state, give me a baseline"; `since_stream_seq:
Option<u64>` — the highest `stream_seq` the client has applied
locally, supplied iff the client has been driving its local copy
from delta application. `None` means the client has no prior delta
context (fresh subscribe). `since_boot_id: Option<Uuid>` — the
daemon `boot_id` the subscriber last observed (echoed from a prior
`SubscribeAck.boot_id`, §3.4.1). Optional for backward
compatibility with pre-v1 subscribers; when present, drives clause
(b) detection directly (see clause (b) *Detection* below). Y-11.

**Input validity rule (normative).** `since_fingerprint = Some(_)` with `since_stream_seq = None` is treated as non-replayable state and MUST return `SubscribeAck::Resync { ... }` (same recovery path as clause (c)); the daemon MUST NOT emit `UpToDate` for this tuple.

**Stream protocol.** The subscriber-bound stream is `Stream<SubscribeFrame>`
(envelope defined alongside `PostAckFrame` above). The first frame is
ALWAYS `SubscribeFrame::Ack(SubscribeAck)`, carrying either
`SubscribeAck::UpToDate { fingerprint, stream_seq, boot_id }`
(`UpToDate` has two normative shapes — see the "Normative definition
of `UpToDate`" paragraph below for the full definition; the vacuous
shape matches the daemon's `current_fingerprint`, the clause-(d)
replay-anchor shape matches `fingerprint_at(s_c)`) or
`SubscribeAck::Resync { fingerprint, stream_seq, boot_id, snapshot:
AggregateSnapshot }` (subscriber MUST replace its local copy).
Thereafter, `SubscribeFrame::PostAck(PostAckFrame)` frames — typically `PostAckFrame::Delta(SnapshotDelta)` carrying live deltas, with a possible terminal `PostAckFrame::ReplayCancelled { … }` (see "Replay cancellation frame" below) — until the subscriber disconnects.

**Normative definition of `UpToDate` (single source of truth).**
`UpToDate` has two normative shapes: (i) **vacuous** — no force-resync
clause and no clause (d) fires; `ack.stream_seq = current_stream_seq`;
no post-ack replay frames; the subscriber's `since_fingerprint`
already equals the daemon's current fingerprint. (ii) **clause-(d)
replay anchor** — `ack.stream_seq = s_c` (the subscriber's cursor),
followed by post-ack delta frames `(s_c, current]` on the subscription
stream; the subscriber's view at `s_c` is provably consistent with
daemon history and converges to `current_fingerprint` after consuming
the replay. Both shapes preserve the invariant that the subscriber's
view converges to the daemon's `current_fingerprint` after consumption.
In both shapes `UpToDate` means **no full resync is needed on
fingerprint-included fields**: the subscriber's local snapshot is (or
becomes, after replay in shape (ii)) consistent with the daemon's
state at the post-consumption cursor **on the subset of fields
included in the fingerprint** (per I-7 — `TokenTotals` and other
explicitly excluded fields are NOT covered by this equivalence and
may have pending updates not yet applied to the subscriber). The
subscriber adopts `ack.stream_seq` as its new cursor, and the next
delivered delta MUST satisfy `delta.stream_seq > ack.stream_seq`
(per "First-delta ordering" below). **Subscribers MUST consume any
subsequent deltas to receive updates to fingerprint-excluded fields**
(e.g., token totals); `UpToDate` does NOT certify those fields as
up-to-date. Clause (d) of §3.4 and the worked example in §3.4.1 both
reduce to this rule.

`boot_id` (the daemon's current incarnation identifier — spec #1 §4
owns this field, see Z-6; it is also fingerprint hash input #1, §4.6)
is echoed on the ack as a `Uuid` (16-byte raw representation per §4.6.1's
UUID encoding rule, Z-9) so subscribers have an explicit epoch-reset
signal independent of fingerprint comparison. A change in `boot_id`
between two `SubscribeAck`s — or between a previously cached ack and a
new one — MUST be treated by the subscriber as a hard reset: all
locally cached `stream_seq` and per-Run `runner_seq` cursors are
invalidated, and the subscriber MUST adopt the new daemon's cursors
from this ack.

**Atomicity (normative).** `subscribe` MUST execute on the
orchestrator's owning task (the same task that owns
`OrchestratorState` per spec #1 I-1). Within that critical section,
the daemon MUST, in this order:

1. Register the new subscriber with the broadcast channel (so any
   subsequent delta emitted by the orchestrator is fanned out to it).
2. Read the current `(fingerprint, stream_seq)` and — if a baseline
   is required — build the `AggregateSnapshot` (§3.1). The
   `SubscribeAck` decision (`UpToDate` vs `Resync`) and any
   `Resync.snapshot` payload MUST be derived from the **same coherent
   state observation** as the `(fingerprint, stream_seq)` returned in
   the ack.
3. Release the critical section and emit the `SubscribeAck`.

This guarantees no orchestrator state mutation can occur in the
window between "subscriber attached" and "baseline captured": every
mutation either is reflected in the baseline (because it preceded the
read) or is delivered as a delta to the new subscriber (because the
subscriber was already attached).

**First-delta ordering.** `SubscribeAck` MUST carry a `stream_seq`
whose semantics depend on the ack shape — see §3.4.1 (struct comment
on `stream_seq`, L779-782) for the full normative definition (vacuous
`UpToDate` and `Resync`: daemon's current `stream_seq` at the moment
of the coherent read; clause-(d) `UpToDate`+replay: `s_c`, the
subscriber's cursor / replay anchor). The first
`SnapshotDelta` delivered to that subscriber MUST satisfy
`delta.stream_seq > ack.stream_seq`; if a delta with
`stream_seq <= ack.stream_seq` would be the next delivery (e.g.
because the subscriber was attached mid-broadcast and saw a delta
already represented in the baseline), the daemon MUST instead send
`Resync` rather than risk replay-as-novel.

**First-delta rules (strict split, normative summary).** The first
message on every subscribe stream is `SubscribeAck`, whose body is
exactly one of:

- `subscribe(channel, since_fingerprint = Some(_), since_stream_seq = Some(s_c), since_boot_id)`
  where `since_fingerprint` matches the daemon's current fingerprint
  AND `s_c == current_stream_seq` (no gap) AND `boot_id` is unchanged
  AND no force-resync clause fires
  ⇒ **vacuous `UpToDate`**: subscriber retains its local snapshot;
  because fingerprints match AND there is no gap, the local snapshot is
  already consistent with the daemon's state at `current_stream_seq`
  on **every** field (including fingerprint-excluded fields such as
  `TokenTotals` and `AggregateUpdated`, since no delta has been
  emitted since `s_c`). The subscriber adopts
  `ack.stream_seq = current_stream_seq` as its cursor and receives only
  live deltas with `stream_seq > ack.stream_seq` — no replay is
  required or performed in this **vacuous shape**. **Note:** if
  `s_c < current_stream_seq` and `fp_at(s_c) == last_known_fingerprint`
  (even when `last_known_fingerprint == current_fingerprint`, i.e. the
  gap contains only fingerprint-excluded deltas such as
  `AggregateUpdated` / `TokenTotals` updates), this bullet does NOT
  apply — clause (d) below fires instead, so the subscriber receives
  the deltas it needs to update fingerprint-excluded fields. The
  clause-(d) replay-anchor shape follows different rules —
  `ack.stream_seq = s_c` and the first delta after the ack MUST be
  `s_c + 1` per the first-delta ordering invariant; the daemon
  delivers `(s_c, current]` in strict ascending `stream_seq` order.
- `subscribe(channel, since_fingerprint = Some(_), since_stream_seq = Some(s_c), since_boot_id = Some(daemon.boot_id))`
  where `s_c` is in-window (clause (a) does not fire) AND
  `last_known_fingerprint == daemon.fingerprint_at(s_c)` AND
  `s_c < current_stream_seq` (any gap, regardless of whether the
  daemon's `current_fingerprint` differs from `last_known_fingerprint`
  — fingerprint-excluded deltas in the gap still require replay)
  ⇒ **clause-(d) `UpToDate { stream_seq: s_c }` + post-ack delta
  replay `(s_c, current]`**: subscriber retains its local snapshot at
  `s_c`, applies the buffered deltas in strict ascending `stream_seq`
  order to advance to `current_stream_seq`, then resumes live deltas.
  No full baseline transfer.
- `subscribe(channel, since_fingerprint, since_stream_seq = Some(s_c), since_boot_id)`
  where the gap exceeds the ring OR `boot_id` differs OR the
  force-resync clauses below fire
  ⇒ **`Resync { snapshot, stream_seq }`**: subscriber discards local
  state; live-only deltas after that, no replay window.
- `subscribe(channel, since_fingerprint = None, since_stream_seq, since_boot_id)`
  ⇒ **`Resync { snapshot, stream_seq }`** unconditionally (clause (c)
  below); live deltas thereafter.

The clauses (a), (b), (c), (d), (d′), (P) below formalise the Resync
(and clause-(d) UpToDate+replay) side of this split and are
exhaustive (plus the §3.4 input-validity rule, which routes through
clause (c)).

**Force-resync triggers (closed set, normative — Z-4 / Z-6 / V-7).**
The six-clause closed set (a),(b),(c),(d),(d′),(P) below is
**exhaustive** over all `subscribe` outcomes. Of these, clauses
**(a),(b),(c),(d′),(P)** force `SubscribeAck::Resync`; clause **(d)**
emits `SubscribeAck::UpToDate + post-ack delta replay (s_c, current]`
on the subscription stream. The daemon MUST emit exactly one of these
acks per `subscribe`; iff none of (a),(b),(c),(d),(d′),(P) fires the
daemon emits a vacuous `UpToDate` (no replay needed). **Vacuous
`UpToDate` fires iff** `since_fingerprint == current_fingerprint`
AND `s_c == current_stream_seq` AND boot_id matches AND no other
clause fires; equivalently, vacuous fires only when the subscriber's
cursor has zero gap to `current_stream_seq`. Any nonzero gap
(`s_c < current_stream_seq`) with `fp_at(s_c) == last_known_fingerprint`
routes to clause (d) — including the case where
`last_known_fingerprint == current_fingerprint` (gap contains only
fingerprint-excluded deltas like `AggregateUpdated` / token-total
updates), which still requires replay so the subscriber can update
its fingerprint-excluded fields. Implementations
MUST evaluate (a),(b),(c),(d),(d′),(P) and emit at most one of those
clause-driven acks (the daemon ALWAYS emits exactly one ack per
`subscribe` — vacuous or clause-driven). The daemon MUST NOT
introduce additional implementation-specific resync triggers beyond
this list. The §3.4 input-validity rule (`since_fingerprint = Some,
since_stream_seq = None`) is evaluated before clauses (a)–(d),(d′),(P)
and routes through clause (c)'s recovery path.

**Clause (a) — `stream_seq` gap or future cursor.**

- *Trigger.* The client supplied `since_stream_seq = Some(s_c)` and
  EITHER `current_stream_seq < s_c` (the daemon's cursor is strictly
  behind the client's last-observed cursor — e.g. the client outlived
  a daemon-boot reset of `stream_seq` to 0) OR `current_stream_seq −
  s_c > recent_history_ring_size` (the gap from the client's cursor
  to the daemon's current cursor exceeds the bounded history the
  daemon has retained; Z-5 default 32). The bound is a *count of
  deltas*, not the byte-size of the recent-history ring's payload;
  the two share a name because they share a budget.
- *Detection.* On `subscribe` arrival, on the orchestrator owning
  task, the daemon reads `current_stream_seq` from the **same
  coherent state observation** as `current_fingerprint` (per the
  Atomicity rule above) and computes the two arithmetic comparisons.
- *Daemon action.* Build `AggregateSnapshot` (§3.1) and emit
  `SubscribeAck::Resync { version: 1, boot_id, fingerprint,
  stream_seq: current_stream_seq, body: Resync { snapshot } }`
  (wire format §3.4.1).
- *Subscriber action.* Discard the local `AggregateSnapshot` and ALL
  locally cached `stream_seq` and per-Run `runner_seq` cursors;
  install `Resync.snapshot` as the new local copy; adopt
  `ack.stream_seq` as its new transport cursor; resume applying live
  deltas under the constraint `delta.stream_seq > ack.stream_seq`.

(See §3.4 *First-delta ordering* for the daemon-side reduction to clause (a).)

**Clause (b) — `boot_id` mismatch (cross-incarnation epoch reset).**

- *Trigger.* The client supplied state that was computed under a different daemon incarnation (`boot_id`) than `OrchestratorState.boot_id` (detected via `since_boot_id` when present; otherwise by cross-incarnation fingerprint inequality per Detection). Equivalently: any subscriber that
  previously cached an `ack.boot_id` for this daemon channel and now
  observes a different `boot_id` MUST treat the change as a hard
  reset. Note: any nonzero `since_stream_seq` carried alongside a
  cross-incarnation fingerprint will ALSO trip clause (a) by virtue
  of the daemon's `stream_seq` having reset to 0; the two clauses
  agree on the outcome and the daemon MUST emit exactly one `Resync`.
- *Detection.* The subscribe envelope SHOULD carry an explicit
  `since_boot_id: Option<Uuid>` field (added in v1; subscribers from
  older clients MAY omit it). When present and `current_boot_id ≠
  since_boot_id`, clause (b) fires. When `since_boot_id` is omitted,
  the daemon falls back to fingerprint inequality as a
  **sufficient-but-not-necessary** signal: because `boot_id` is
  fingerprint hash input #1 (§4.6.1), any cross-incarnation
  fingerprint comparison MUST fail except in the cryptographically
  negligible collision case bounded by the "Pre-v1 cross-incarnation
  collision risk" paragraph below, so a cross-incarnation subscribe
  whose explicit `since_boot_id` is absent will still be caught — and
  in the residual case where a same-boot in-window fingerprint
  mismatch occurs at `s_c`, clause (d′) catches it; deltas-missed
  at-cursor-equal cases are handled by clause (d). The daemon MUST NOT compute "would the bodies match if
  we ignored boot_id" — there is no such mode. (Pre-v1 callers with
  `since_boot_id = None` are subject to clause (a) routing on any
  boot mismatch — which the daemon cannot detect by `boot_id` —
  so detection relies on fingerprint inequality alone. The residual
  cross-incarnation collision risk is bounded as documented in the
  "Pre-v1 cross-incarnation collision risk" paragraph below
  (`boot_id` is fp hash input #1; cryptographically negligible at
  ≥128-bit digest sizes). Daemons MAY refuse pre-v1 subscriptions
  where this residual is unacceptable, closing the subscribe stream
  with `SnapshotError::PreV1Unsupported` (per §3.4.1 wire format,
  `SubscribeAck::Resync` carries only `{ snapshot }` and has no
  free-form `reason` field; refusal therefore travels via the
  `SnapshotError` channel — see §4.5 errors table).)
- *Independence from clause (d′).* Clauses (b) and (d′) are logically independent: clause (b) is the **cross-incarnation** trigger (subscriber state predates the current daemon `boot_id`); clause (d′) is the **same-boot in-window fingerprint-divergence** trigger (cursor recoverable but fingerprint at that cursor disagrees). Implementations MUST preserve both predicates as distinct checks. Ack selection still follows the §3.4 closure rule; when explicit `since_boot_id != current_boot_id`, an implementation MAY short-circuit before evaluating clause (d′)'s same-boot-only predicate.
- *Daemon action.* Build `AggregateSnapshot` and emit
  `SubscribeAck::Resync { version: 1, boot_id: <current>,
  fingerprint: <current>, stream_seq: <current>, body:
  Resync { snapshot } }`. The fresh `boot_id` is REQUIRED on the ack
  so the subscriber can compare with its previously cached value.
- *Subscriber action.* Mandatory cache flush — discard local
  snapshot, ALL cached `stream_seq` and per-Run `runner_seq` cursors
  (these are scoped to the prior daemon incarnation and have no
  meaning under the new `boot_id`), install `Resync.snapshot`, adopt
  the ack's `(stream_seq, boot_id)` as the new epoch.

**Pre-v1 boot_id absence (normative).** Subscribers using the pre-v1
schema MAY omit `since_boot_id` (i.e., send `since_boot_id = None`).
When `since_boot_id == None`:

- Clause (a) fires under its normal `stream_seq` arithmetic trigger.
- Clause (b) cannot fire on the explicit `since_boot_id` inequality
  path (no boot_id to compare); it falls back to the
  fingerprint-inequality detection path (sufficient-but-not-necessary,
  per T-27).
- Clause (c) fires under its normal trigger
  (`since_fingerprint = None`).
- Clauses (d) and (d′) MAY fire if `s_c` is in-window AND the
  fingerprint predicate resolves per their normal triggers; the
  daemon treats `since_boot_id == None` as "boot match assumed" for
  same-boot evaluation. Tests T-30b-pre-v1 and T-10-pre-v1 cover
  these cases.
- Clause (P) fires under its normal trigger.

This closes the synthetic collision hole where pre-v1 clients
would otherwise be forced to clause (c) snapshot resubscribe on
every reconnect even when `s_c` is in-window and `fp_at(s_c)`
matches `last_known_fingerprint`. v1+ callers SHOULD always supply
`since_boot_id` (so clause (b) detection is exact rather than
fingerprint-inequality fallback); the relaxation here exists solely
for backward compatibility (Y-11).

**Pre-v1 cross-incarnation collision risk (documented).** Pre-v1
clients (those subscribing with `since_boot_id = None`) MAY hit a
low-probability collision in clause (a) when fp-hash-collision
occurs across daemon incarnations with identical replay-relevant
state. The risk is bounded by:

1. `boot_id` is the first hash input to fp (per §3.3 fp definition),
   so a true cross-incarnation collision requires hashing two
   distinct `boot_id`s to identical fp output — at the fp-hash digest
   size (≥128 bits), this is cryptographically negligible.
2. Pre-v1 clients SHOULD upgrade to v1 (`since_boot_id = Some(b)`) at
   the earliest opportunity; v1 forces `Resync` on any boot mismatch,
   eliminating this hole.
3. Daemons MAY refuse pre-v1 subscriptions in deployments where this
   residual is unacceptable, closing the subscribe stream with
   `SnapshotError::PreV1Unsupported` (§3.4.1 `SubscribeAck::Resync`
   has no `reason` field; refusal travels via the `SnapshotError`
   channel — see §4.5).

Cross-reference: T-27 covers fingerprint-inequality fallback for
pre-v1 clause (b) detection.

**Clause (c) — explicit subscriber resync request.**

- *Trigger.* The subscriber supplied `since_fingerprint = None` (the
  canonical "fresh subscribe, I have no state, give me a baseline"
  request — §3.4 inputs). Includes the first-ever subscribe of a
  fresh client and any subscribe issued after the subscriber has
  voluntarily discarded its local state (UI window reopen,
  panic-recover, manual reload, phantom-row gap recovery per I-8).
  Clause (c) is evaluated in closure-rule priority order
  (a > P > b > c > d > d′). Higher-priority clauses subsume (c)
  when they match — (c) does NOT need to assert mutual exclusivity
  with (a), (P), or (b); the closure rule handles it.
- *Detection.* `since_fingerprint.is_none()` on the request envelope.
- *Daemon action.* Build `AggregateSnapshot` and emit
  `SubscribeAck::Resync { … }` unconditionally; the daemon MUST NOT
  serve `UpToDate` for a `None` fingerprint, even on a quiescent
  channel.
- *Subscriber action.* Install the baseline, adopt the ack's
  `(stream_seq, boot_id)`, resume live deltas with
  `delta.stream_seq > ack.stream_seq`.

**Clause (d) — Same-boot, in-window stale subscriber (Z6-J1).**

- *Trigger.* Subscriber's `since_boot_id` matches the daemon's
  `boot_id` (or is `None` per the pre-v1 boot_id absence rule below,
  in which case same-boot is assumed for clause-(d)/(d′) evaluation),
  `since_stream_seq = Some(s_c)` is within the
  `recent_history_ring` window (i.e., clause (a) does NOT fire),
  `last_known_fingerprint == daemon.fingerprint_at(s_c)` (the
  subscriber's view at `s_c` was historically consistent — no
  divergence, no ring rebuild) AND
  `s_c < current_stream_seq` (there is at least one unreplayed delta
  in the gap `(s_c, current_stream_seq]`, regardless of whether that
  delta affected the fingerprint). This is the canonical
  "subscriber missed one or more deltas" case: the cursor is at a
  known-good past point and the gap is replayable from the bounded
  delta buffer. **Note:** clause (d) fires even when
  `last_known_fingerprint == current_fingerprint` — i.e. the gap
  contains only fingerprint-excluded deltas (`AggregateUpdated`,
  `TokenTotals` updates per I-7). Skipping replay in that case
  (emitting a vacuous `UpToDate`) would leave the subscriber's
  fingerprint-excluded fields silently stale; the protocol MUST NOT
  do so. Vacuous `UpToDate` is reserved exclusively for the
  zero-gap case (`s_c == current_stream_seq`).
- *Detection.* Detection uses the **delta replay index** — a bounded
  map of `(stream_seq → snapshot_fingerprint)`, owned by the daemon,
  populated on each delta emission and trimmed when entries fall
  before `ring_oldest_seq`. This is distinct from `recent_history_ring`
  (which holds finished-run summaries).

  **Delta payload buffer (normative).** Distinct from both
  `recent_history_ring` (finished-run summaries) and the replay index
  (fingerprint map): the daemon MUST retain the serialized
  `SnapshotDelta` payloads for every `stream_seq` retained in the
  replay index, owned by the orchestrator main task, populated on
  `publish_snapshot_delta` (§3.3) immediately before broadcast, and
  trimmed in lock-step with the replay index at `ring_oldest_seq`
  (warm-up-aware, per Z-29 below). The replay index span invariant
  (Z-29 below) extends to this buffer: payload retention span MUST
  equal index span, so that clause (d)'s `(s_c, current]` replay
  can never hit a hole. If either the replay index OR the delta
  payload buffer is missing any `stream_seq` in `(s_c, current]`,
  the daemon MUST emit `Resync` (clause (a) trigger), NOT a partial
  replay.

  If `since_stream_seq` is
  present in the replay index, the indexed fingerprint **equals**
  `last_known_fingerprint`, AND `s_c < current_stream_seq`, clause (d)
  fires. This INCLUDES the case `last_known_fingerprint ==
  current_fingerprint` — fingerprint match does NOT exclude
  clause (d), because the fingerprint excludes `AggregateUpdated` and
  token-counter fields (per §3.3 fp definition), so a stream gap with
  matching fp can still carry unreplayed deltas in the excluded field
  set. Only when `s_c == current_stream_seq` AND fp matches AND boot
  matches is `UpToDate` "vacuous" (truly no replay needed). (If the
  indexed fingerprint *disagrees* with `last_known_fingerprint`, that
  is a divergence / ring-rebuild signal handled by clause (d′); see
  §3.4 for the in-window divergence trigger and full-resync action.)
- *Daemon action.* Emit `SubscribeAck::UpToDate { fingerprint:
  last_known_fingerprint, boot_id, stream_seq: s_c }` (NOT Resync —
  subscriber's snapshot at `s_c` is consistent with daemon history);
  THEN emit the buffered delta frames `(s_c, current_stream_seq]` on
  the subscription stream as normal post-ack delta frames. The ack
  carries `ack.stream_seq = s_c` so first-delta ordering
  (`delta.stream_seq > ack.stream_seq`) drives the replay; live
  deltas continue thereafter. Subscriber consumes deltas to advance
  from `s_c` to `current_stream_seq` without a full baseline
  transfer. (`SubscribeAck::UpToDate` carries `stream_seq` via the
  outer `SubscribeAck` envelope — see §3.4.1 wire format.)
- *Subscriber action.* Retain local snapshot at `s_c`, apply the
  replayed deltas in order, then resume live deltas. No discard, no
  full baseline. Verifies T-10 (same-boot, in-window, fp-equal,
  fp-changed-since case; see §6).

**Replay-mode subscriber buffering (normative).** When the daemon
serves clause (d) (UpToDate + `(s_c, current_stream_seq_at_ack]`
replay), the per-subscriber send queue MUST enter "replay mode"
between ack emission and the last replay frame's send-completion.
While in replay mode:

1. The daemon emits `UpToDate { …, stream_seq: s_c }` first.
2. Then it emits each delta frame in
   `(s_c, current_stream_seq_at_ack]` in strict ascending
   `stream_seq` order, exactly as captured at ack-evaluation time.
3. Any live delta with `stream_seq > current_stream_seq_at_ack`
   produced during this window MUST be buffered per-subscriber
   and only released to the wire after step 2 completes (queue
   tail order preserved).
4. If, while in replay mode, a delta required by step 2 cannot
   be materialised — per the `ReplayCancelReason` partition
   defined below (exhaustive over pre-commit trim, post-commit
   trim discovery, and non-trim payload fault) — the daemon
   MUST cancel the replay and emit a `ReplayCancelled` frame
   (defined below) to the affected subscriber, then close that
   subscriber's send queue (terminating replay mode).

This rule makes Z-29's strict-ascending replay ordering provable
end-to-end: live deltas cannot overtake replay frames on the wire.

**Replay cancellation frame (normative).** When the daemon's
replay-mode buffering must cancel a replay due to eviction (per the
escape hatch in step 4 above), it MUST emit a single
`PostAckFrame::ReplayCancelled { stream_seq: current_stream_seq_at_cancel,
reason: ReplayCancelReason::RingEviction | ReplayCancelReason::BufferEviction
| ReplayCancelReason::LockStepTrimRace }` frame to the affected
subscriber, then close the per-subscriber send queue. The subscriber
MUST treat receipt of `ReplayCancelled` as a signal to re-subscribe;
the new `SubscribeAck` will take clause (c) (snapshot RPC re-anchor
with `since_fingerprint = None`, since the in-flight replay's
fingerprint context is lost when the daemon trimmed the required
entries).

`PostAckFrame::ReplayCancelled` is a `PostAckFrame` variant distinct
from `SubscribeAck::Resync`. `SubscribeAck::Resync` is the
initial-handshake-only signal; `ReplayCancelled` is a terminal
post-ack frame indicating the daemon could not complete a
previously-requested replay. The daemon MUST NOT emit
`SubscribeAck::Resync` mid-stream; `PostAckFrame::ReplayCancelled` is
the only mid-stream cancellation frame. `ReplayCancelled` MUST be the
final frame on a subscription stream — no further frames (including
live deltas) follow it before the close.

**`ReplayCancelReason` variants (normative partition).** The three
reasons are disjoint by the daemon-side detection site:

- `LockStepTrimRace` — emitted on the **pre-commit** path: the
  owning task is about to commit a Z-29 sub-invariant 3 lock-step
  trim AND observes at least one open replay subscriber whose
  required window overlaps the trim. The cancel frame is enqueued
  on the subscriber's terminal-control path **before** the trim
  commits (T-31b). Index integrity preserved end-to-end.
- `RingEviction` — emitted on the **post-commit discovery** path:
  a lock-step trim has already committed without enqueuing a
  `LockStepTrimRace` cancel for this subscriber (because the
  subscriber's replay setup had not yet registered with the trim
  observation set when the trim was evaluated). Replay subsequently
  reads the replay index/payload buffer and discovers the required
  `stream_seq` is no longer present (T-31).
- `BufferEviction` — emitted on a **non-trim payload fault**: the
  replay index entry is intact (Z-29 invariant holds) but the
  delta payload buffer cannot materialise the entry — payload
  checksum mismatch, in-memory corruption, or buffer-rebuild
  operation not yet complete for the requested window (T-31a).
  No trim is involved; the fault is purely a payload-read
  failure.

These three are exhaustive: any cancel-replay condition reduces
to exactly one of (pre-commit-trim, post-commit-trim-discovery,
non-trim-payload-fault).

**Clause (d′) — Same-boot in-window fingerprint divergence.**

- *Trigger.* `since_boot_id == Some(daemon.boot_id) || since_boot_id ==
  None` (the disjunction admits pre-v1 clients per the boot_id-absence
  rule above; for v1 clients carrying `Some(_)`, strict equality
  applies — strict inequality routes to (b) instead), `since_stream_seq
  = Some(s_c)`, clause (a) does NOT fire (i.e. `s_c` is within the
  replay-index window), AND `last_known_fingerprint ≠
  daemon.fingerprint_at(s_c)`. The relationship between
  `last_known_fingerprint` and `current_fingerprint` is irrelevant to
  (d′); mismatch at the cursor is sufficient. (d) and (d′) are
  mutually exclusive by the `fp_subscriber == fp_at(s_c)` predicate.
  *Independent of (b):* (d′) requires same-boot evaluation
  (`since_boot_id ∈ {Some(daemon.boot_id), None}`), so v1
  cross-incarnation cases (`Some(other)`) route to (b) before reaching
  (d′); the two clauses are disjoint by construction.
- *Detection.* Replay-index lookup at `s_c` yields a fingerprint, and
  that fingerprint ≠ `last_known_fingerprint`. (If `s_c` is absent
  from the index, clause (a) or (P) fires; (d′) is reached only when
  `s_c` IS indexed.) **Invariant Z-29 (replay-index dense retention).** Define
  `ring_oldest_seq := min(stream_seq) over all entries currently in
  the replay index`. The daemon MUST satisfy:

  1. **Span bound (warm-up-aware).** Define `span :=
     current_stream_seq − ring_oldest_seq + 1`. The daemon MUST
     satisfy `span == min(current_stream_seq + 1,
     recent_history_ring_size)` **only when the replay index is non-empty**.
     Equivalently:
     - During warm-up (`0 < current_stream_seq < recent_history_ring_size`):
       `ring_oldest_seq == 1`; the index/buffer retain every emitted
       `stream_seq` from `1` through `current_stream_seq`.
     - At steady state (`current_stream_seq ≥ recent_history_ring_size`):
       `ring_oldest_seq == current_stream_seq − recent_history_ring_size + 1`;
       the index/buffer retain exactly the last `recent_history_ring_size`
       entries.
     - **Z-29 boot edge case.** When `current_stream_seq == 0`, the replay
       index and delta payload buffer are empty. In this regime, the reachable
       routing outcomes are:
       - (b) — boot mismatch (`since_boot_id != Some(daemon.boot_id)`).
       - (a) — future cursor (`since_stream_seq = Some(s_c)` with `s_c > 0`).
       - (c) — `since_fingerprint == None`.
       - (d′) — `s_c == 0 && last_known_fingerprint != current_fingerprint && boot matches`.
       - vacuous-`UpToDate` — `s_c == 0 && fp matches && boot matches`.

       Clauses (P) and (d) cannot fire at boot because the replay index is
       empty; clause (d′) at boot reduces to same-boot fingerprint mismatch
       against the seq-0 baseline (`current_fingerprint`).
  2. **Dense retention.** Every `stream_seq ∈ [ring_oldest_seq,
     current_stream_seq]` MUST be present in BOTH the replay index
     AND the delta payload buffer (§3.4 above). Implementations
     MAY only trim from the oldest end (FIFO).
  3. **Lock-step trim.** Trim of replay index and delta payload
     buffer MUST happen atomically — at no point may one contain
     a `stream_seq` the other does not.

  These three sub-invariants make clause (d)'s replay
  contract `(s_c, current]` provable: if `s_c ≥ ring_oldest_seq`,
  the daemon can ALWAYS produce a hole-free replay; otherwise
  clause (a) fires and `Resync` is emitted.
- *Daemon action.* Build `AggregateSnapshot` and emit
  `SubscribeAck::Resync { snapshot, stream_seq: current_stream_seq,
  boot_id, fingerprint: current_fingerprint }` — full baseline, NOT
  delta replay (subscriber's snapshot at `s_c` is provably
  inconsistent with daemon history; applying buffered deltas would
  not converge).
- *Subscriber action.* Discard local snapshot and per-Run cursors;
  install `Resync.snapshot`; adopt
  `(ack.boot_id, ack.stream_seq, ack.fingerprint)`.
- *Verifies.* T-30, T-30b, T-30b-pre-v1, and (negatively) T-29.

**Clause (P) — Stale-since-without-fingerprint (Z6-P1).**

- *Trigger.* `since_stream_seq = Some(s_c)` **and the replay index is
  non-empty** and `s_c < ring_oldest_seq`, regardless of whether
  `last_known_fingerprint` was supplied (`(Some(stale_seq), None)` is
  the canonical case this clause closes). If the replay index is
  empty (`current_stream_seq == 0`), clause (P) is inapplicable.
- *Daemon action.* The daemon MUST deliver `Resync { snapshot,
  stream_seq }` (full snapshot). It MUST NOT deliver `UpToDate`: the
  replay window cannot reach `since_stream_seq`, so the subscriber's
  view is unrecoverable from incremental deltas. Note: when
  `since_fingerprint` is also `Some(_)`, this clause overlaps with
  clause (a); the daemon MUST emit exactly one `Resync` regardless of
  how many force-resync clauses fire.
- *Subscriber action.* Discard local state and install the `Resync`
  snapshot.

**Force-resync clause set (Z6-J1 / Z6-P1 update).** With clauses (d),
(d′) and (P) added, the daemon MUST emit `Resync` iff at least one of
clauses (a), (b), (c), (d′), (P) fires; clause (d) emits `UpToDate +
delta replay`, not `Resync`. The six clauses are exhaustive but NOT
mutually exclusive — multiple clauses can match a given subscription
tuple (e.g., (a)+(b) when both stream-seq is out-of-window and boot
mismatches; (a)+(P) when `since_stream_seq < ring_oldest_seq` also
trips the count-comparison trigger). The daemon emits exactly one ack
per subscription per the **closure rule**: clauses are evaluated in
priority order (a) > (P) > (b) > (c) > (d) > (d′), and the first
matching clause produces the ack. The remaining matches are subsumed.
The §3.4 input-validity rule
(`since_fingerprint = Some, since_stream_seq = None`) is evaluated
before clauses (a)–(d),(d′),(P) and routes through clause (c)'s
recovery path.

**Non-clauses (deliberately excluded — Z-4 closure).** The daemon
MUST NOT emit `Resync` solely because: (i) `since_fingerprint`
matches but the daemon is "uncomfortable" or "cannot prove it has
every intervening delta buffered" — that informal rationale was
eliminated in Z-4 and replaced by the deterministic clause (a);
(ii) per-Run `runner_seq` non-monotonicity observed by the daemon —
`runner_seq` is the subscriber-side gap signal (I-8), not a
daemon-side resync trigger; (iii) backlog TTL or other implementation
internals — they MUST be expressed as the count comparison in clause
(a) or not enforced at all.

#### 3.4.1 `SubscribeAck` wire format (Z-6, normative)

`SubscribeAck` is the first message on every `subscribe` stream. The
protocol version is **`v1` (= `1`)**; subscribers MUST reject any ack
whose `version` field is unequal to `1` and MUST close the subscribe
stream with `SnapshotError::Unavailable` rather than silently coerce.

```rust
struct SubscribeAck {
    version:     u16,                  // protocol version; v1 == 1.
    boot_id:     Uuid,                 // 16 raw bytes per §4.6.1 UUID rule.
    fingerprint: SnapshotFingerprint,  // 16 bytes per §4.6.
    stream_seq:  u64,                  // For Resync and vacuous UpToDate: daemon's current_stream_seq.
                                       // For clause (d) UpToDate+replay: s_c (the subscriber's cursor,
                                       // which becomes the replay anchor — first-delta ordering then
                                       // drives delivery of (s_c, current]).
    body:        SubscribeAckBody,
}

enum SubscribeAckBody {
    UpToDate,
    Resync { snapshot: AggregateSnapshot },
}
```

**Field order on the wire (normative).** Under any concrete encoding
bound by §7, the fields MUST be serialised in the order
`version, boot_id, fingerprint, stream_seq, body` — fixed-width prefix
first so a subscriber parsing a corrupt or future-version message can
fail fast on `version` before allocating for the body.

**Subscriber behavior on `boot_id` mismatch (normative).** If
`ack.boot_id` differs from any `boot_id` the subscriber previously
cached for this daemon channel, the subscriber MUST: (1) immediately
**discard local state** — the local `AggregateSnapshot`, ALL per-Run
cursors, and the prior `stream_seq`; (2) **if `ack.body = UpToDate`**
(a daemon-side conformance bug, since `UpToDate` MUST NOT carry a new
`boot_id`), defensively close the subscribe stream and re-subscribe
with `since_fingerprint = None`, `since_stream_seq = None`, and
`since_boot_id = Some(<last_known_boot_id>)` so the daemon
deterministically routes the new subscribe to clause (c)
(`since_fingerprint = None` always falls through to clause (c)
per its Trigger above; `since_boot_id` is supplied for diagnostics
and to allow the daemon to log the cross-boot transition);
v1+ callers MUST supply the last-known `boot_id` here (consistent
with the SHOULD at clause (b) Detection above) — `since_boot_id =
None` is permitted only for pre-v1 callers, which fall back to
fingerprint inequality per T-27; (2′) **if `ack.body = Resync {
snapshot }`** (the daemon proactively detected the cross-boot
mismatch — this is the normal, conforming path for clause (b)), the
subscriber MUST accept the embedded snapshot and update its tracked
`boot_id` in-place without a close/re-subscribe round-trip — the
`Resync` frame itself constitutes the boot_id re-handshake; (3)
install the new `boot_id` as the cached epoch only after step (2) or
(2′) completes successfully. This is the canonical "discard local
state, full resync" response to clause (b). The close+re-subscribe
path (2) applies only when the client detects mismatch via its own
logic (e.g., snapshot-fetch RPC mismatch, or the conformance-bug case
of an `UpToDate` ack carrying a different `boot_id`).

**Worked example — daemon restart between subscribes.** A zed window
reopens and reconnects to a daemon that restarted while the window
was closed:

1. Subscriber sends `subscribe(channel, since_fingerprint = Some(0xCAFE…),
   since_stream_seq = Some(7421), since_boot_id = None)`. The fingerprint
   and stream_seq are from the prior daemon incarnation
   (`boot_id_old = 0xA0…`); `since_boot_id` is omitted in this example
   to exercise the pre-v1 / T-27 path.
2. Daemon, on the orchestrator owning task, reads
   `(boot_id_new = 0xB0…, fingerprint_new = 0x1234…,
   stream_seq_new = 12)` as a single coherent observation.
   `since_fingerprint = 0xCAFE…` was computed under
   `boot_id_old = 0xA0…`. Because `boot_id` is fingerprint hash input
   #1 (§4.6.1) and `boot_id_new ≠ boot_id_old`,
   `fingerprint_new ≠ 0xCAFE…` is guaranteed — clause (b) fires via
   the fingerprint-inequality fallback (since `since_boot_id` was not
   sent in this example, exercising the pre-v1 / T-27 path).
   (Clause (a) also fires because `12 < 7421`; the two clauses agree
   on the outcome.)
3. Daemon emits `SubscribeAck { version: 1, boot_id: 0xB0…,
   fingerprint: 0x1234…, stream_seq: 12, body: Resync { snapshot:
   <AggregateSnapshot at boot_id=0xB0…> } }`.
4. Subscriber observes `ack.boot_id == 0xB0…` ≠ cached `0xA0…`,
   executes the boot_id-mismatch hard-reset procedure: discards local
   snapshot and all cursors; installs `ack.body.Resync.snapshot` as the new local
   copy; caches `(boot_id = 0xB0…, stream_seq = 12)`; awaits live
   deltas with `stream_seq > 12`.

**Worked example — same daemon, brief socket interruption (clause (d)
replay over a fingerprint-excluded gap).** The same window reconnects
after a brief socket drop. During the drop the daemon emitted two
deltas (`stream_seq = 10` and `11`) that affected only
fingerprint-excluded fields (e.g., `TokenTotals` updates,
`AggregateUpdated` timestamps per I-7), so `current_fingerprint`
still equals `0xCAFE…`:

1. Subscriber sends `subscribe(channel, since_fingerprint = Some(0xCAFE…),
   since_stream_seq = Some(9), since_boot_id = Some(0xA0…))` (same
   incarnation, `boot_id_old = 0xA0…`).
2. Daemon reads `(boot_id = 0xA0…, fingerprint = 0xCAFE…,
   stream_seq = 11)`. Clause (a): `11 - 9 = 2 ≤ 32` (no out-of-window
   gap). Clause (b): `boot_id` matches (no epoch reset). Clause (c):
   `since_fingerprint` is `Some` (not an explicit baseline request).
   Clause (d): `since_stream_seq = 9` is in the replay-index window;
   the indexed fingerprint at `9` equals `since_fingerprint = 0xCAFE…`
   (so `fp_subscriber == fp_at(s_c)` holds) AND
   `s_c = 9 < current_stream_seq = 11` (nonzero gap) — clause (d)
   FIRES, even though `last_known_fingerprint == current_fingerprint`,
   because the gap `(9, 11]` contains fingerprint-excluded deltas the
   subscriber has not yet applied; emitting a vacuous `UpToDate` here
   would silently strand the subscriber's `TokenTotals` /
   `AggregateUpdated` fields. Clause (d′): `fp_subscriber == fp_at(9)
   = 0xCAFE…`, so the divergence predicate does NOT hold; (d′) does
   NOT fire. Clause (P): `since_stream_seq = 9 ≥ ring_oldest_seq`;
   does not fire.
3. Daemon emits `SubscribeAck { version: 1, boot_id: 0xA0…,
   fingerprint: 0xCAFE…, stream_seq: 9, body: UpToDate }` (note
   `ack.stream_seq = s_c = 9`, the replay anchor — NOT
   `current_stream_seq`).
4. Daemon then emits the buffered delta payloads at `stream_seq = 10`
   and `stream_seq = 11` on the subscription stream in strict
   ascending order (per the replay-mode buffering rule); any live
   delta with `stream_seq > 11` produced during the replay is
   buffered per-subscriber and released only after frame `11`
   completes.
5. Subscriber retains its local snapshot at `s_c = 9`, applies deltas
   `10` and `11` in order (which update `TokenTotals` /
   `AggregateUpdated`), then resumes live deltas with `stream_seq >
   11`. No baseline transfer; no discard.

**Worked example — same daemon, zero-gap reconnect (vacuous
`UpToDate`).** The same window reconnects after a brief socket drop
during which the daemon emitted no deltas at all:

1. Subscriber sends `subscribe(channel, since_fingerprint = Some(0xCAFE…),
   since_stream_seq = Some(11), since_boot_id = Some(0xA0…))`.
2. Daemon reads `(boot_id = 0xA0…, fingerprint = 0xCAFE…,
   stream_seq = 11)`. Clauses (a),(b),(c) do not fire as in the
   previous example. Clause (d): `s_c = 11 == current_stream_seq =
   11`, so the `s_c < current_stream_seq` conjunct fails; (d) does
   NOT fire. Clause (d′): `fp_subscriber == fp_at(11) = 0xCAFE…`;
   does not fire. Clause (P): does not fire. None of (a),(b),(c),
   (d),(d′),(P) fires — the closed six-clause set is exhaustive and
   demonstrably so for this case.
3. Daemon emits `SubscribeAck { version: 1, boot_id: 0xA0…,
   fingerprint: 0xCAFE…, stream_seq: 11, body: UpToDate }` —
   **vacuous** shape; `ack.stream_seq = current_stream_seq = 11`,
   no replay frames follow.
4. Subscriber proceeds with its existing local copy and awaits
   deltas with `stream_seq > 11` (i.e., `delta 12` and onward).
   Because `since_fingerprint` matched the daemon's current
   fingerprint AND there is zero gap, the subscriber's local state
   is already consistent with the daemon's state at `stream_seq =
   11` on **every** field (fingerprint-included and -excluded);
   no replay is required or performed.

**Worked example — same daemon, in-window divergence (clause (d′)).**
The same window reconnects with a `since_fingerprint` that disagrees
with the daemon's history at the supplied cursor (e.g.,
subscriber-side state corruption, partial-write recovery, or a ring
rebuild on the daemon side that altered `fp_at(s_c)`):

1. Subscriber sends `subscribe(channel, since_fingerprint =
   Some(0xDEAD…), since_stream_seq = Some(9), since_boot_id =
   Some(0xA0…))` — same incarnation, but `0xDEAD…` is NOT the
   fingerprint the daemon recorded at `stream_seq = 9`.
2. Daemon reads `(boot_id = 0xA0…, fingerprint = 0xCAFE…,
   stream_seq = 11)`. Clause (a): `11 - 9 = 2 ≤ 32` (no gap).
   Clause (b): `boot_id` matches (no epoch reset). Clause (c):
   `since_fingerprint` is `Some` (not an explicit baseline request).
   Clause (d): replay-index lookup at `9` yields fingerprint
   `0xBEEF…` (the daemon's recorded fingerprint at that cursor);
   `since_fingerprint = 0xDEAD… ≠ 0xBEEF…`, so the
   `fp_subscriber == fp_at(s_c)` predicate fails; clause (d) does
   NOT fire. Clause (d′): the divergence predicate
   (`last_known_fingerprint ≠ fp_at(s_c)`, i.e.
   `0xDEAD… ≠ 0xBEEF…`) holds; clause (d′) fires. Clause (P):
   `since_stream_seq = 9 ≥ ring_oldest_seq`; does not fire.
3. Daemon emits `SubscribeAck { version: 1, boot_id: 0xA0…,
   fingerprint: 0xCAFE…, stream_seq: 11, body: Resync { snapshot:
   <AggregateSnapshot at stream_seq=11> } }` — full baseline, not
   delta replay (the subscriber's view at `s_c` is provably
   inconsistent with daemon history; applying deltas in `(9, 11]`
   would not converge).
4. Subscriber discards its local snapshot and per-Run cursors,
   installs `ack.body.Resync.snapshot`, adopts
   `(boot_id = 0xA0…, stream_seq = 11, fingerprint = 0xCAFE…)`,
   and awaits live deltas with `stream_seq > 11`.

### 3.5 Latency bounds and backpressure (normative)

#### 3.5.1 `subscribe` → `SubscribeAck` latency

The daemon MUST emit `SubscribeAck` within `subscribe_ack_max_ms`
(default `500` ms) of the `subscribe` request landing on the
orchestrator command channel. Wall-clock measurement; on exhaustion
the daemon MUST close the subscribe stream with
`SnapshotError::Unavailable` rather than leave the subscriber blocked.
Because the bound includes `build_snapshot` time when the ack body is
`Resync`, conforming implementations MUST configure
`subscribe_ack_max_ms ≥ snapshot_timeout_ms` (default 500 ≥ 200,
satisfied).

**Subscribe-storm coalescing (normative-MAY — Z6-O1).** When ≥3
concurrent `subscribe(_)` requests with identical
`since_stream_seq = None` (and `since_fingerprint = None`) arrive
within 100 ms, the daemon MAY coalesce them onto a single
`build_snapshot` invocation and broadcast the resulting
`SubscribeAck` (and any associated `Resync`) to all of them —
`SubscribeAck`/`Resync` produce identical bytes for identical
`since_stream_seq = None` requests, so coalescing is byte-safe.

**Coalescing MUST NOT extend any single subscribe's
`subscribe_ack_max_ms` budget.** If coalescing would cause any
participant to exceed its budget, the late-arriving subscriber
receives its own (parallel) `build_snapshot` invocation rather than
joining the coalesced batch. Implementations MAY use a small worker
pool with budget-aware dispatch. Coalescing is OPTIONAL; a conforming
daemon MAY always serve each subscribe with a dedicated
`build_snapshot` and remain spec-compliant.

#### 3.5.2 Delta flush latency

Once the orchestrator main task reaches a fixpoint after a
state-mutating event (§3.3), the corresponding `SnapshotDelta` MUST
be enqueued onto the broadcast channel within `delta_flush_max_ms`
(default `50` ms). The daemon MUST NOT batch, coalesce, or hold
deltas for downstream consumer convenience beyond this bound. Idle
re-render (`AggregateUpdated` once per `@minimum_idle_rerender_ms`,
§3.3) is governed by its own cadence and is NOT subject to this
bound.

#### 3.5.3 Backpressure (closed set, normative — answer is NOT unbounded buffering)

The daemon's broadcast channel is bounded with capacity
`broadcast_channel_capacity` (default `512` slots **per subscriber**;
sized to absorb burst > `recent_history_ring_size`). When a
subscriber's slot is full and a new delta would overflow, the daemon
MUST take exactly one of the following actions, in this order of
precedence:

1. **Drop the delta for that subscriber only** (other subscribers'
   slots are unaffected) and increment a per-subscriber
   `dropped_deltas` counter exposed via diagnostic telemetry. The
   dropped delta is observable to the subscriber as a `stream_seq`
   discontinuity on the next successful delivery, which trips clause
   (a) of §3.4; the subscriber issues
   `subscribe(channel, since_fingerprint, since_stream_seq, since_boot_id)`
   (where `since_boot_id` is the `boot_id` from the last cached
   `SubscribeAck`, or `None` if no prior incarnation was observed)
   and recovers via `Resync`.
2. **If the same subscriber's slot remains full for ≥
   `slow_subscriber_disconnect_ms`** (default `30_000` ms — i.e. the
   subscriber is not draining its broadcast queue at all), the daemon
   MUST disconnect the subscriber's subscribe stream with
   `SnapshotError::Unavailable`. This is the **only** path under
   which the daemon disconnects a subscriber for backpressure
   reasons.

The daemon MUST NOT, under any circumstances:

- (a) Buffer deltas in an **unbounded queue** waiting for the slow
  subscriber. Backpressure resolution is bounded-buffer-then-drop,
  never grow-the-buffer.
- (b) Apply backpressure to the orchestrator main task — the
  orchestrator MUST never block on snapshot publication (spec #1
  I-1 forbids it; the broadcast channel send path MUST be
  non-blocking from the orchestrator's perspective).
- (c) Silently drop deltas without bumping the subscriber's
  `stream_seq` gap signal. Every dropped delta MUST be observable as
  a `stream_seq` discontinuity within at most `delta_flush_max_ms`
  of the next successful delivery to that subscriber.

Tests T-18..T-22 cover the latency bounds and forbidden behaviors
enumerated above; tests T-23..T-25 cover §3.4.1, I-6, and §4.5.
T-10 covers clause (d) (UpToDate + `(s_c, current]` replay), and
T-10-pre-v1 covers the same path under `since_boot_id = None`.
T-26..T-29 cover clause (b) (cross-incarnation / out-of-window
Resync). T-30 and T-30b cover clause (d′) (same-boot in-window
fingerprint divergence), with T-30b-pre-v1 covering (d′) under
`since_boot_id = None`. T-31 covers post-commit replay cancellation with
`ReplayCancelReason::RingEviction`. T-31a covers non-trim payload faults
with `ReplayCancelReason::BufferEviction`. T-31b covers pre-commit trim
races with `ReplayCancelReason::LockStepTrimRace`.

---

## 4. Data shapes

These shapes are normative on field names and semantics. Field-level
types (`u32` vs `u64`, `String` vs `Arc<str>`) are
implementation-defined. Optional fields are explicit (`Option<T>`); a
field present but null on the wire MUST round-trip as `None` not absent.

**Wire-shape primitives.** This spec is transport-neutral (§7). Where
a field needs a free-form structured payload, an associative map, or
opaque bytes, the abstract wire primitives are:

| Abstract | Meaning |
|---|---|
| `JsonValue` | Self-describing structured value (object / array / string / number / bool / null). |
| `Map<K, V>` | Unordered key→value association. Keys are `String` unless otherwise stated. |
| `Bytes` | Opaque byte string of declared bit-width (e.g. `Bytes(256)` for a 256-bit BLAKE3 hash). |

Source-language types (Rust `&'static str`, `Cow<'static, str>`,
`serde_json::Value`, `Blake3Hash`, etc.) MUST NOT appear in
conforming implementations as part of the public wire surface. The
future RPC ADR (§7) binds these primitives to a concrete encoding
(Cap'n Proto / gRPC / framed CBOR). Implementations MAY use any
in-process representation provided the on-wire shape conforms.

Type names are imported from companion specs by reference (this spec
does NOT redefine them): `RunId`, `Run`, `RunningEntry`, `RetryEntry`,
`RetryToken`, `ExitReason`, `OrchestratorState` (spec #1 §4);
`RepoCoordinate`, `RepoSlug`, `Workspace`, `WorkspaceId`,
`AbsolutePath` (spec #3 §4); `SessionId`, `runner_seq`, agent
capability bits (spec #2 §4); `IssueRef`, `ThreadId` (spec #7 forward
reference).

### 4.1 `RunRow`

```rust
struct RunRow {
    run_id:               RunId,
    parent_run_id:        Option<RunId>,           // reserved for spec #5 Pattern 3 cross-run handoff (v1 always None).
    issue_ref:            Option<IssueRef>,        // None for ad-hoc threads (multirepo-ux Q7).
    thread_id:            Option<ThreadId>,        // engine-side join key (C-hybrid bridge).
    repo_coordinate:      RepoCoordinate,          // REQUIRED (I-5). caduceus-new vs Symphony.
    status:               RunStatus,               // §4.1.1
    stage:                String,                  // free-form ("In Progress", "review", …)
    pid:                  Option<u32>,             // host PID; see note below.
    started_at:           SystemTime,
    age:                  Duration,                // runtime_now − started_at, computed at projection.
    turn_number:          u32,
    tokens:               TokenTotals,             // §4.7
    workspace_path:       AbsolutePath,            // for "open workspace" affordance.
    last_event:           Option<LastEventSummary>,// §4.4
    restart_count:        u32,                     // attempts.restart_count (presenter.ex:72–75).
    current_retry_attempt: u32,                    // attempts.current_retry_attempt (presenter.ex:90–92).
    session_id:           Option<SessionId>,
    runner_seq:           u64,                     // monotonic per-Run from spec #2.
}
```

Field origins: `run_id`, `issue_ref`, `thread_id` (spec #7);
`parent_run_id` (caduceus-new, **reserved** for spec #5 Pattern 3
cross-run handoff; in v1 the daemon MUST set this to `None` on every
emitted `RunRow` — non-`None` values are reserved for a future
spec-bump and MUST be rejected by v1 consumers as a protocol error);
`repo_coordinate` (spec #3, embedded by value so the snapshot is
self-contained); `status`, `stage`, `pid`, `started_at`,
`turn_number`, `tokens`, `restart_count`, `current_retry_attempt`,
`session_id` (Symphony parity, `presenter.ex:99–158`);
`workspace_path` (caduceus-new — Symphony does not surface it on
rows); `last_event` (Symphony's `EVENT` column,
`status_dashboard.ex:111`); `runner_seq` (caduceus-new, gap detection
per I-8).

**Note on `pid`.** This field exposes a daemon-host process
identifier to every subscriber. In single-user v1 deployments this
is acceptable. The `pid` field MAY be elided by transport-layer
policy (replaced with `None` before broadcast) and a future
authn/authz ADR will gate its visibility once federation /
multi-tenant deployments land. Implementations MUST treat absence
of `pid` as semantically equivalent to "not disclosed", not "no
process".

#### 4.1.1 `RunStatus`

```rust
enum RunStatus {
    Running,                  // live engine connection; agent producing turns.
    Retrying,                 // present in snapshot.retrying; not in snapshot.running.
    Disconnected,             // caduceus-new: engine RPC dropped, daemon retains row.
}
```

`Retrying` only appears on `RetryRow` in practice; `RunRow` carries
`Running` or `Disconnected`. Spec #8 renders all three buckets.

### 4.2 `RetryRow`

Mirrors Symphony's retry projection (`presenter.ex:120–158`); plus
`repo_coordinate` and `retry_token`.

> **Source of `error_message` (Y-5).** The string surfaced here is a
> direct projection of `RetryEntry.error_message: Option<String>`
> defined in spec #1 §4 and populated in spec #1 §3.5 `on_worker_exit`
> from `ExitReason::Abnormal { error }.to_string()`. The orchestrator
> is the sole owner of the captured text; spec #4 MUST NOT re-derive
> it from any other source. `None` on the orchestrator side
> (continuation retries / dispatch-deferred / slot-pressure retries)
> projects to the empty string `""` on the wire. Truncation /
> line-collapse / byte budget per §4.2.1 below applies regardless of
> source.

```rust
struct RetryRow {
    run_id:          RunId,
    issue_ref:       Option<IssueRef>,
    repo_coordinate: RepoCoordinate,                // REQUIRED (I-5).
    attempt:         u32,                            // attempt number (1, 2, 3, …).
    next_retry_at:   SystemTime,                     // wall-clock; clients render `due_in_ms`.
    error_message:   String,                         // projected from spec #1 RetryEntry.error_message; line-collapsed AND truncated to retry_error_max_bytes (§4.2.1). Empty string when source is None.
    retry_token:     RetryToken,                     // for spec #1 I-4 freshness checks.
    reason:          String,                         // "continuation" | "failure" | …
}
```

The daemon does NOT truncate `error_message` for *display* — UI
clients truncate per their column width (Symphony's CLI truncates to
96 chars, `status_dashboard.ex:659–700` — that is a UI rule, not a
snapshot rule). The daemon DOES enforce a hard byte budget at
ingestion, per §4.2.1 below.

#### 4.2.1 Error message budget

> **Truncation ownership (SRP).** Spec #1 owns the truncation of
> `recent_history_ring` (single writer, FIFO eviction at insertion
> per §4.5). Spec #4 (this spec) is **read-only** over the ring; it
> MUST NOT mutate the ring during snapshot or delta construction.
> The `error_message` byte-budget enforced below is a projection-time
> bound applied when copying out into `RetryRow` / `SnapshotDelta`,
> not a mutation of the underlying ring entry.

A misbehaving agent (or a runner crash dump) can produce arbitrarily
large `error_message` payloads. Holding such payloads in
`RetryEntry`, hashing them, and broadcasting them to every subscriber
on every idle re-render is a denial-of-service vector. Therefore:

- The daemon MUST truncate `error_message` to `retry_error_max_bytes`
  (default `4096` bytes) **at a UTF-8 char boundary** before storing
  it in `RetryEntry` and before any inclusion in a `SnapshotDelta`,
  `RetryRow`, or fingerprint input.
- Truncation MUST happen **after** the line-collapse step (`\r?\n` →
  `↵`, U+21B5) defined in §4.4 so that newline replacements cannot
  push a previously-fitting message over budget at egress.
- For v1, a trailing `↵…` (U+21B5 + horizontal ellipsis U+2026)
  suffix on truncated messages is sufficient signalling.
  Implementations MAY additionally surface a `truncated: bool` field
  on `RetryRow`; if present, it MUST be `true` iff truncation
  occurred. Absence of the field MUST NOT be interpreted as
  "definitely not truncated".
- `retry_error_max_bytes` is a **daemon-side** limit. UI clients
  remain free to truncate further for display.

### 4.3 `AggregateSnapshot`

```rust
struct AggregateSnapshot {
    running:               Vec<RunRow>,             // status == Running.
    retrying:              Vec<RetryRow>,
    disconnected:          Vec<RunRow>,             // status == Disconnected. caduceus-new.
    tokens_aggregate:      TokenTotals,             // sum across running ∪ disconnected.
    runtime_total:         Duration,
    rate_limit:            RateLimitBlob,
    agents_used:           u32,                      // == running.len() + disconnected.len().
    agents_max:            u32,                      // from Config.max_concurrency.
    next_poll_at:          Option<SystemTime>,
    snapshot_fingerprint:  SnapshotFingerprint,
    taken_at:              SystemTime,
}
```

Bucket invariants: a `RunId` MUST appear in exactly one of `running`,
`retrying`, `disconnected` (I-9); `agents_used <= agents_max` MUST
hold in steady state, with one exception: during a workflow hot-reload
that lowers `max_concurrency` from `N` to `M < N`, `agents_used` MAY
temporarily exceed `agents_max` until natural drain reduces
`running.len()` to `M`. This drain interval is governed by spec #1
I-8 (X-12); the daemon MUST NOT terminate live attempts to meet a
lowered ceiling. `tokens_aggregate` is the sum across
`running ∪ disconnected` only (`retrying` Runs have no live agent
contributing tokens *now*; their prior contribution is already
absorbed into `OrchestratorState.last_reported_tokens` via the
per-Run watermark — see spec #1 §4 (X-5)).

### 4.4 `LastEventSummary`

```rust
struct LastEventSummary {
    kind:        EventKind,                // spec #5; e.g. TurnCompleted, Exec, TokenCount, …
    text:        String,                    // truncated to options.last_event_max_bytes (default 240).
    timestamp:   SystemTime,
    truncated:   bool,                      // true iff text was shortened.
}
```

The daemon truncates by **bytes** at a UTF-8 char boundary, not by
code points or columns. The bound is on **UTF-8 byte length**, NOT
character count. Multi-byte boundary truncation MUST yield a valid
UTF-8 string: truncate to the last complete codepoint at or before
the byte limit (i.e., never split a multi-byte sequence). Multi-line
events MUST be collapsed to a single line: replace `\r?\n` with `↵`
(U+21B5) before truncation.

(Open question §8: 240 chars vs 1 line vs structured. Decision today:
240 bytes, single-line, with `kind` so clients can re-render.)

### 4.5 `RunDetail`

> **Ring ownership (Y-4).** The bounded `recent_history_ring` referenced
> throughout this section is a field on `OrchestratorState` defined in
> spec #1 §4 (`recent_history_ring: BoundedRingBuffer<FinishedRunSummary>`).
> Insertion and eviction are owned exclusively by the orchestrator main
> task in spec #1 §3.5 `on_worker_exit` (terminal path). Spec #4 reads
> the ring read-only on the snapshot path; spec #4 MUST NOT mutate it.
>
> **Bound (normative).** The ring's capacity is
> `recent_history_ring_size` (default `32`, X-14, owned by spec #1 §8.7).
> Entries are evicted in FIFO order on capacity overflow.
> Subscribers MUST NOT assume that entries older than the most recent
> `recent_history_ring_size` finished Runs are present. A subscriber
> wanting durable post-finish observability MUST capture `RunDetail`
> proactively on receiving the corresponding `RunFinished` delta
> (per the ring-before-delta ordering rule below); a `RunDetail`
> request issued an unbounded time later MAY return
> `SnapshotError::RunNotFound`, and that response is correct, not a bug.

Returned by `dispatch_snapshot_request(_, Run(run_id))`. Extends
`RunRow` with detail fields not carried in the aggregate (so
`AggregateSnapshot` stays small).

```rust
struct RunDetail {
    row:                RunRow,                                  // §4.1
    event_log_tail:     Vec<EventRecord>,                        // spec #5; default last 100.
    token_history:      Vec<(turn: u32, totals: TokenTotals)>,
    prompt_hash_trail:  Vec<(turn: u32, hash: Bytes)>,        // 256-bit BLAKE3; spec #2 §4.
    hook_log:           Vec<HookExecutionRecord>,                // spec #3 §3.5.
    workspace:          WorkspaceMeta,                            // see WorkspaceMeta below (Z6-M1).
}

// Z6-M1: concrete fields (no `…`); WorkspaceStatus aliases spec #3 Status.
pub struct WorkspaceMeta {
    pub workspace_id:                          WorkspaceId,
    pub slug:                                  String,
    pub parent_path_redacted_unless_trusted:   Option<PathBuf>,   // see §1.2 trust rules
    pub status:                                WorkspaceStatus,   // alias of spec #3 Status
    pub created_at_wall:                       SystemTime,
    pub last_heartbeat_at:                     Option<SystemTime>, // mirrors spec #3 §4.2
    pub run_id:                                Option<RunId>,      // None if workspace not yet bound
}
```

**Per-field DoS clamps (normative — Z6-L1).** `RunDetail` carries four
variable-length fields (`event_log_tail`, `token_history`,
`prompt_hash_trail`, `hook_log`); each has a per-request maximum and a
hard cap that bounds the maximum returned length. The daemon MUST
clamp each field to `min(option_value, hard_cap)` before serialization.
The hard cap is enforced silently — there is no rejection path.
`Error::OptionExceedsFloor` is REMOVED; numeric option values that
exceed the hard cap are clamped, not errored.

| Field | Default max | Hard cap (max) | `RunDetailOptions` field |
|---|---|---|---|
| `event_log_tail`    | 100 entries  | 1000 entries | `event_log_max` (existing)              |
| `token_history`     | 500 entries  | 500 entries  | `token_history_max` (NEW; default 500)  |
| `prompt_hash_trail` | 200 entries  | 200 entries  | `prompt_hash_trail_max` (NEW; default 200) |
| `hook_log`          | 100 entries  | 100 entries  | `hook_log_max` (NEW; default 100)       |

```rust
struct RunDetailOptions {
    event_log_max:        u32,   // default 100;  hard cap (max) 1000
    token_history_max:    u32,   // default 500;  hard cap (max)  500  (Z6-L1)
    prompt_hash_trail_max:u32,   // default 200;  hard cap (max)  200  (Z6-L1)
    hook_log_max:         u32,   // default 100;  hard cap (max)  100  (Z6-L1)
}
```

`RunDetail` MAY be requested for *finished* Runs while they are in
the daemon's bounded `recent_history_ring` (default size 32, X-14 —
the same ring serves both `RunDetail` lookup and disconnect-retention
visibility per spec #1 §8.7); after eviction, response is
`SnapshotError::RunNotFound`.

**Ring-before-delta ordering (normative).** When a Run finishes, the
daemon MUST, atomically on the orchestrator owning task:

1. Insert the finished Run into `recent_history_ring`. If the ring
   is at capacity (`recent_history_ring_size`, default `32`),
   eviction of the least-recently-finished entry MUST occur at this
   insertion step.
2. *Then* emit the corresponding `RunFinished { run_id, … }` delta
   on the SnapshotChannel.

The orchestrator MUST NOT emit `RunFinished` before the row is
queryable in the ring. Consequence: a subscriber observing
`RunFinished{run_id}` and immediately issuing
`dispatch_snapshot_request(_, Run(run_id))` is guaranteed to receive
a `RunDetail` (not `RunNotFound`) **unless at least
`recent_history_ring_size` other Runs have finished between the
delta arrival and the lookup**. Subscribers wanting a stronger
guarantee MUST capture the `RunDetail` proactively on receiving the
`RunFinished` delta.

### 4.6 `SnapshotFingerprint`

```rust
type SnapshotFingerprint = [u8; 16];   // 128-bit; algorithm choice in §8.
```

Deterministic hash over the snapshot's identity-bearing fields.
Stable: identical inputs MUST produce the same value (I-7).

**Hash inputs, in this exact order:**

1. Daemon `boot_id` — sourced exclusively from
   **`OrchestratorState.boot_id`** (spec #1 §4 / Z-6). The orchestrator
   is the single owner of this field; spec #4 reads it via the
   snapshot construction path on the orchestrator's owning task (no
   independent re-derivation, no caching across boots). Random per-
   process, fixed for the daemon's lifetime; ensures cross-
   incarnation fingerprints never collide.
2. For each `RunRow` in `running ∪ disconnected`, sorted by `run_id`:
   `(run_id, status, restart_count, turn_number)`.
3. For each `RetryRow` in `retrying`, sorted by `run_id`:
   `(run_id, attempt, retry_token)`. Z-17: `next_retry_at_ms` is
   **explicitly excluded** from this tuple — it is a projected wall
   clock that ticks every poll interval and would invalidate the
   idle-rerender contract; the retry's *identity* is fully captured
   by `(run_id, attempt, retry_token)`. The wall-clock arrival time
   is observable via `RetryUpdated` deltas.
4. `agents_used`, `agents_max`.

**Inputs explicitly excluded** (so `runtime_now`-driven re-renders do
not change the fingerprint, AND so token motion under active workload
does not invalidate the idle-stable contract): `runtime_total`,
`age`, `taken_at`, `next_poll_at`, `last_event.{text,timestamp}`
(high-frequency chatter would invalidate idle-rerender deltas),
`rate_limit` blob contents, **`tokens.absolute_total` and every
other field of `TokenTotals`** — the fingerprint reflects
*structural* state (which Runs exist, which bucket each is in, retry
posture) only. Token motion is observable through `RunUpdated`
(per-Run) and `AggregateUpdated.tokens_aggregate` (aggregate)
directly; subscribers needing token-level change detection MUST use
those signals, NOT the fingerprint. An idle subscriber whose only
updates are `age`, `runtime_total`, and token tickers MAY observe
the same fingerprint across many `AggregateUpdated` deltas — that is
correct.

#### 4.6.1 Canonical encoding (Y-13, normative)

Two implementations producing the same `OrchestratorState`-derived
inputs MUST produce byte-identical hash inputs and therefore identical
fingerprints. The encoding is fixed-width little-endian throughout —
NOT JSON, NOT bincode, NOT serde — to remove any library-dependent
ambiguity:

| Type                          | Wire form                                                                |
|-------------------------------|--------------------------------------------------------------------------|
| `u8`, `u16`, `u32`, `u64`     | Little-endian fixed-width (1, 2, 4, 8 bytes)                             |
| `i32`, `i64`                  | Little-endian two's complement (4, 8 bytes)                              |
| `bool`                        | `0x00` (false) or `0x01` (true), 1 byte                                  |
| Enum variant tag              | `u8` in the variant's declaration order (top-to-bottom in the spec); panic if > 255 variants |
| `RunId`, `RetryToken`, `boot_id` | UUID rendered as the 16 raw bytes of `Uuid::as_bytes` (BE network order, RFC 4122 §4.1.2 — Uuid's canonical byte form). Z-9: this row covers ONLY UUID-typed identifiers; do NOT route string-typed identifiers through this rule. |
| `WorkspaceId`                 | Z-9: encoded under the **`String`** rule below — `u32` LE byte length followed by UTF-8 bytes — because `WorkspaceId` is a `String` (spec #3 §3.4 sanitised slug + run-id form, NOT a UUID). Implementations that internally represent `WorkspaceId` as a hashed UUID for storage MUST first project to its canonical string form before encoding. |
| `String`                      | `u32` LE byte length, then UTF-8 bytes                                   |
| `Option<T>`                   | `0x00` (None, no payload) or `0x01` followed by `T`'s encoding           |
| Tuple `(A, B, …, N)`          | Z-7: concatenation of each field's encoding, in declaration order. NO length-prefix is emitted — tuple arity is fixed by the spec at every call site, so the prior `u32 LE field count (sanity)` byte was a redundant constant that produced spurious cross-implementation drift if any side miscounted. Vec / sorted-iteration retains its `u32 LE` element count because that count is dynamic. |
| `Vec<T>` / sorted iteration   | `u32` LE element count, then concatenation                               |
| `SystemTime`                  | NEVER fingerprinted — these are projected (§3.1 clock-domain rule); attempting to encode triggers a programmer-error panic |
| `Duration`                    | NEVER fingerprinted (excluded above)                                     |

The hash is **BLAKE3-256** over the concatenated byte stream; the
fingerprint is the first 16 bytes of the digest (truncated;
collision-resistance still ≥ 2^64 by birthday bound, sufficient for
the daemon-lifetime-scoped namespace because `boot_id` is mixed in
first).

The hash input is constructed as:

```text
H = BLAKE3-256(
        encode(boot_id)                                  // 16 B (Uuid raw)
     || u32_LE(running_plus_disconnected.len())
     || for each row in sort_by_run_id(running ∪ disconnected):
            encode( (run_id, status_tag_u8, restart_count_u32,
                     turn_number_u32) )                  // Z-7: no tuple length-prefix
     || u32_LE(retrying.len())
     || for each row in sort_by_run_id(retrying):
            encode( (run_id, attempt_u32, retry_token) )  // Z-17: next_retry_at_ms removed
     || u32_LE(agents_used) || u32_LE(agents_max)
    )[0..16]
```

`status_tag_u8` is the `u8` encoding of `RunStatus` per the enum-tag
rule above (declaration order in §4.1). Z-17 drops `next_retry_at_ms`
from the retry tuple — a retry's identity is `(run_id, attempt,
retry_token)`; the projected wall-clock arrival is observable via
`RetryUpdated` and would otherwise tick the fingerprint every poll.
The `u32_LE(.len())` length prefixes are present even when the
collection is empty so an empty `running` does not collide with an
empty `retrying`.

Cross-implementation fingerprint divergence is a P0 conformance bug,
not a stylistic difference — the C-hybrid resync contract (§3.4)
collapses into permanent thrash if two implementations never agree
on idle-state fingerprints.

**Sort comparator rule (Z-20 cross-cutting, normative).** Every
"sort by X" reference in this spec uses lexicographic comparison
over the **canonically-encoded byte sequence** of the sort key (per
the §4.6.1 table for that key's type), interpreted as unsigned bytes
(`memcmp` semantics). Concrete sites:

- `sort_by_run_id(running ∪ disconnected)` and `sort_by_run_id(retrying)`
  in §4.6.1 (fingerprint input ordering): `RunId` is a `Uuid`, encoded
  as 16 raw bytes per the §4.6.1 UUID rule; comparison is unsigned
  16-byte memcmp over those bytes (RFC 4122 network-order). This
  matches Rust `Uuid::as_bytes().cmp(...)`.
- `RunId` final tiebreaker in §3.1 step 8.4 and the
  `sort_by(next_retry_at, run_id)` in §3.1 step 8 (retrying bucket):
  same 16-byte unsigned-byte ordering.
- `sort_by(issue_ref.identifier)` in §3.1 step 8.2: UTF-8 byte-order
  (already specified in step 8.2).

Implementations MUST NOT use locale-aware collation, case-folding,
Unicode normalisation, or any sort key derived from a
non-canonical encoding. The single bit-identical comparator across
implementations is required for keyboard-nav parity (spec #8) and
for fingerprint determinism (a difference in tuple iteration order
across implementations would diverge the BLAKE3 input bytes).

### 4.7 `TokenTotals`

```rust
struct TokenTotals {
    input_tokens:    u64,
    output_tokens:   u64,
    cache_read:      u64,        // optional in spec #2; zero if agent does not report.
    cache_write:     u64,
    seconds_running: u64,        // wall-clock seconds the agent has been alive.
}
```

`TokenTotals` is the authoritative per-Run watermark. Each component
field is independently watermarked by spec #2 §4.3's per-component
rule (`new = max(stored, payload)` under absolute mode; per-component
addition under delta mode). The `input_tokens` and `output_tokens`
fields ARE the watermark — there is no separate scalar high-water.

**`absolute_total` (derived view, backward-compat).** For consumers
that historically read a single scalar "absolute total", the daemon
MAY surface a derived field
`absolute_total = input_tokens + output_tokens` on the wire. It is a
projection, not a stored field, and is recomputed at each snapshot
boundary; it MUST NOT be used as the input to the §4.6 fingerprint
(see §4.6's exclusion list) and MUST NOT be conflated with
`last_reported_tokens`. `cache_read` is informational and is not
included in `absolute_total`.

Spec #2 §4.3 owns the watermark movement rule; this spec is a strict
consumer.

### 4.8 `RateLimitBlob`

```rust
struct RateLimitBlob {
    model:                Option<String>,            // "gpt-5", "claude-opus-4.7", …
    primary_used:         Option<u64>,
    primary_limit:        Option<u64>,
    primary_reset_after:  Option<Duration>,
    secondary_used:       Option<u64>,
    secondary_limit:      Option<u64>,
    secondary_reset_after: Option<Duration>,
    credits_ok:           Option<bool>,
    raw:                  Option<JsonValue>,         // pass-through for non-standard fields.
}
```

The daemon does NOT compute or interpret rate-limit posture. It stores
the most recent blob from any active runner and surfaces it. Spec #2
owns the wire shape from the agent.

### 4.9 `SnapshotResponse` envelope

```rust
struct SnapshotResponse {
    request_id: SnapshotRequestId,
    payload:    SnapshotPayload,
}

enum SnapshotPayload {
    Aggregate(AggregateSnapshot),
    Run(RunDetail),
    Error(SnapshotError),
}

enum SnapshotError {
    Timeout,
    Unavailable,
    RunNotFound { run_id: RunId },
    BudgetTooSmall { requested_ms: u32, minimum_ms: u32 },
    PreV1Unsupported,
}
```

---

## 5. Invariants (MUST)

Each invariant is testable in §6.

### I-1: Snapshot is a pure projection

`build_snapshot(state, options)` MUST NOT mutate any field of
`OrchestratorState`, MUST NOT enqueue commands on the orchestrator
command channel, MUST NOT emit observability events. Diagnostic
counters tracking timeout-rate are permitted because they live outside
`OrchestratorState`. Test: T-1.

### I-2: Bounded snapshot completion

Wall-clock latency of `build_snapshot` MUST NOT exceed
`options.snapshot_timeout_ms`. On exhaustion, the result MUST be
`Err(SnapshotError::Timeout)`; no partial snapshot. Default 200ms
(multirepo-ux A.4). Test: T-2.

### I-3: Token reconciliation absolute-preferred

Every `RunRow.tokens`, every `AggregateSnapshot.tokens_aggregate`, and
every `RunFinished.final_tokens` delta MUST follow spec #2 §4.3
(absolute-preferred, delta-fallback). The snapshot layer is a
*consumer* of the already-reconciled `last_reported_tokens` watermark;
it MUST NOT re-derive totals from raw events. Verbatim source:
Symphony SPEC §13.5 lines 1304–1328 (cited in §3.1 step 3). Tests:
T-3, T-9.

### I-4: Snapshot/event-log consistency

The snapshot and the event log are two projections of the same
underlying state and MUST be mutually consistent. Formally: let `E`
be the daemon's event log over `[t0, t1]` and `S(t)` the snapshot at
`t ∈ [t0, t1]`. For every `run_id`:

- If `run_id ∈ S(t).running ∪ S(t).disconnected`, `E[t0..t]` MUST
  contain a `RunStarted{run_id}` and MUST NOT contain a subsequent
  `RunFinished{run_id}`.
- If `run_id ∉ S(t).running ∪ S(t).disconnected ∪ S(t).retrying`,
  either `E[t0..t]` contains no `RunStarted{run_id}`, or the most
  recent `RunStarted{run_id}` is followed by a `RunFinished{run_id}`.

Replay of `E` from boot deterministically reconstructs `S(t)`. (Spec
#1 invariant set: orchestrator state and event log MUST be consistent
at every observable moment.) Test: T-10.

### I-5: `repo_coordinate` is REQUIRED

Every `RunRow` and `RetryRow` MUST carry a non-`Option`
`repo_coordinate`. Symphony has none; caduceus's row identity is
`(repo_coordinate, run_id)`, not `run_id` alone (spec #3 §4). The
daemon MUST refuse to construct a `RunRow` whose `repo_coordinate` is
unknown — the only path into `RunningEntry` is through `dispatch_run`
(spec #1 §3.3), which carries the coordinate by construction.

### I-6: `Disconnected` is transient

A `RunRow` MAY occupy the `Disconnected` bucket only for a bounded
interval governed by **two distinct timers** (X-4) defined and owned
by spec #1 §8.7:

- `disconnect_timeout_ms` (default `60_000` ms) — on expiry the
  daemon FIRST calls `stop_cascade(reason =
  "disconnect_timeout_exceeded")` on the live worker (spec #1 Z-2 /
  §3.5; spec #2 §3.3) and THEN routes the run through
  `on_worker_exit`'s `Abnormal` arm with `error =
  "disconnect_timeout_exceeded"` (Y-5 failure backoff +
  `RetryEntry.error_message` populated). The exit reason that lands
  in the snapshot is `ExitReason::Abnormal { error:
  "disconnect_timeout_exceeded" }`, NOT
  `ExitReason::DaemonTerminated { cause: EngineDisconnected }` —
  `TerminationCause::EngineDisconnected` is reserved by spec #1 §4's
  enum but is not used on this path (the disconnect-timeout exit is
  Abnormal-with-retry, not DaemonTerminated). A reattach RPC that
  arrives after the cascade fires no longer finds a `RunningEntry`
  to bind; the row is in `recent_history_ring`.
- `disconnect_retention_ms` (default `3_600_000` ms / 1 hour) —
  controls how long the row remains visible in the
  `recent_history_ring` after the cascade has fired so a late
  reattach can render the outcome.

On engine RPC disconnect: flag `RunningEntry` as disconnected
(retained intact so a reattaching engine can re-bind); start
`disconnect_timer(run_id, disconnect_timeout_ms)`. The reattaching
engine's first message uses the **reattach control frame defined in
spec #2 §4.5** (`Reattach { run_id, runner_seq, session_id }`; X-3) —
the daemon validates the supplied `runner_seq` against its high-water
and on success clears the timer and transitions the row back to
`Running`. If `disconnect_timeout_ms` fires first, the daemon runs
the spec #1 exit cascade and the row enters the
`recent_history_ring`, where it remains until
`disconnect_retention_ms` elapses or the ring evicts it. A snapshot
taken before reattach or timeout MUST report `status: Disconnected`.
Test: T-8.

### I-7: SnapshotFingerprint stability

For any two snapshots `S1, S2`: matching of every input enumerated in
§4.6 implies `S1.snapshot_fingerprint == S2.snapshot_fingerprint`.
Conversely, a change in `restart_count`, `turn_number`, bucket
membership, or `agents_used`/`agents_max` MUST change the
fingerprint. Token motion (`tokens.absolute_total` and other
`TokenTotals` fields) is **excluded** from fingerprint inputs by
design (§4.6) — token-level change detection MUST use `RunUpdated` /
`AggregateUpdated.tokens_aggregate` instead. Tests: T-4, T-5.

### I-8: At-most-once delta delivery; gap-driven resync

Subscribers MUST NOT assume exactly-once delivery. The daemon's
broadcast channel is bounded; on saturation, the daemon drops, NOT
the orchestrator. Subscribers MUST track **two independent cursors**
(X-2) and MUST trigger `subscribe(channel, since_fingerprint, since_stream_seq, since_boot_id)` (§3.4)
on a gap on either:

1. **Per-Run `runner_seq`** — the per-Run monotonic cursor defined in
   spec #2 §4.4. Carried on `RunUpdated`. A non-monotonic value (or a
   `RunUpdated` for a Run whose latest seen `runner_seq` is already
   greater) is a per-Run gap signal.
2. **Daemon-scoped `stream_seq`** — the transport-level monotonic
   cursor carried on every `SnapshotDelta` variant. Any non-`+1` jump
   between successive deltas observed by a subscriber is a
   transport-level gap signal, regardless of which Runs are affected.
   This is what catches dropped `RunStarted`, `RunFinished`, and
   `AggregateUpdated` events that carry no `runner_seq`.
3. **Phantom row reference** — any `RunUpdated`, `RunFinished`,
   `RetryUpdated`, or `RetryCleared` delta referencing a `run_id` the
   subscriber has never seen (no prior `RunStarted` or
   `RetryScheduled` in this subscription) MUST be treated as a gap
   signal and trigger `subscribe(channel, since_fingerprint, since_stream_seq, since_boot_id)`. This
   covers both run-bucket and retry-bucket gaps symmetrically: a
   missed `RunStarted` followed by `RunUpdated` for the same `run_id`,
   AND a missed `RetryScheduled` followed by `RetryUpdated` /
   `RetryCleared` for the same `run_id`, both trip the same phantom
   detector. The daemon MUST NOT rely on subscribers tolerating
   phantom-row patches (silent merge of unknown `run_id`s is
   forbidden because it would mask a dropped `RunStarted` or
   `RetryScheduled`).

The daemon's broadcast channel MUST be bounded; on saturation the
daemon drops, NOT the orchestrator. Test: T-10.

### I-9: Bucket disjointness

For every `run_id` and every snapshot `S`, treating set membership as a
`{0,1}` indicator:

```
[run_id ∈ S.running] + [run_id ∈ S.retrying] + [run_id ∈ S.disconnected] == 1
```

Equivalently: the three buckets MUST be pairwise disjoint AND every
`run_id` referenced anywhere in `S` MUST appear in at least one bucket
(no orphans, no double-bucketing). Symphony parity:
`presenter.ex:99–158` keeps `running` and `retrying` disjoint at
projection time; caduceus extends to include `disconnected`. Test: T-11.

Note: an earlier draft expressed this with XOR (`A ⊕ B ⊕ C`); that
formulation is wrong because XOR over three booleans is `true` for
`(1,1,1)`. Implementations MUST use the indicator-sum form above.

---

## 6. Test contract

Normative obligations on any conforming implementation. Driver:
`OrchestratorState`; observer: `AggregateSnapshot` and the delta stream.

### T-1: `build_snapshot` purity

Build snapshot 1000× against a frozen `OrchestratorState`. State hash
MUST be identical before and after; no `Cmd` enqueued. Verifies I-1.

### T-2: Snapshot timeout returns `Err`, not partial

Inject a 500-Run state and `snapshot_timeout_ms = 1`. Result MUST be
`Err(SnapshotError::Timeout)`. No partial snapshot observable; no
delta emitted as a side effect. Verifies I-1, I-2.

### T-3: Token reconciliation: absolute beats delta

Drive a runner that emits `delta {input: 100, output: 50}` then
`absolute {input: 1000, output: 500}` within one turn. Resulting
`RunRow.tokens.absolute_total` MUST be 1500 (NOT 1650);
`tokens_aggregate` MUST agree. Verifies I-3.

### T-4: Fingerprint stability under identical state

Two back-to-back snapshots with no state mutation between them.
`fingerprint_1 == fingerprint_2`. Verifies I-7 (stability).

### T-5: Fingerprint changes when `restart_count` bumps

Snapshot, increment `RunningEntry.restart_count` for one Run via
`on_worker_exit` → restart, snapshot. `fingerprint_1 !=
fingerprint_2`. Verifies I-7 (sensitivity).

### T-6: `subscribe(stale)` triggers `Resync`

Client subscribes with `since_fingerprint = Some(stale)`,
`since_stream_seq = Some(stale_seq)` where `stale_seq <
ring_oldest_seq`, `since_boot_id = Some(daemon.boot_id)`. First
message MUST be `SubscribeAck::Resync { snapshot }` with the current
`AggregateSnapshot` (clause (P) — stale-since-without-fingerprint /
out-of-window). Verifies §3.4. (Inputs trip both clauses (a) and (P);
per §3.4 closure rule, the daemon emits exactly one `Resync` ack.)

### T-7: `subscribe(current)` skips `Resync`

Client subscribes with `since_fingerprint = Some(current)`,
`since_stream_seq = Some(current_stream_seq)`, `since_boot_id =
Some(daemon.boot_id)` and the daemon has lost no deltas. First
message MUST be `SubscribeAck { version: 1, boot_id, fingerprint, stream_seq: current_stream_seq, body: UpToDate }`. No baseline
payload follows. Verifies §3.4. (Note: `since_stream_seq` is
required — per §3.4 input-validity rule, `since_fingerprint = Some`
with `since_stream_seq = None` deterministically routes to `Resync`.)

### T-8: `Disconnected` timeout

Run a Run; sever the engine RPC connection. Snapshot — row appears in
`disconnected`. Wait `disconnect_timeout_ms + ε`; snapshot — row is
absent (progressed to `retrying` or `RunFinished`). In a parallel
case, sever and reattach within the timeout — row returns to
`running`, `runner_seq` resumes monotonically. Verifies I-6.

### T-9: Concurrent `build_snapshot` under load

Pin the daemon under a 10-Run, ~100-token-events/s workload. Issue
100 concurrent `dispatch_snapshot_request` calls. Sum
`tokens_aggregate.input_tokens + tokens_aggregate.output_tokens` of
the last response — MUST equal the
sum of every per-Run high-water value at that observation point. No
double-counting. Verifies I-3 under concurrency.

### T-10: PubSub gap → next snapshot self-corrects

Subscriber connects, observes deltas. Harness drops one
`RunUpdated{run_id=X, runner_seq=k+1}` from the subscriber's view
(simulated saturation). Subscriber's local snapshot is now stale.
Next `RunUpdated` arrives with `runner_seq=k+2`. Subscriber detects
the gap, issues `subscribe(channel, since_fingerprint=local, since_stream_seq=local_seq, since_boot_id=local_boot_id)`.
Because the subscriber's `local_seq` is in-window and the
fingerprint the daemon recorded at `local_seq` equals the
subscriber's `since_fingerprint` (the drop happened in the
*delivery path* after the daemon committed the delta to its replay
index — daemon-side history is unchanged), this falls into clause
(d): daemon responds with `SubscribeAck::UpToDate { fingerprint:
local, boot_id, stream_seq: local_seq }` followed by the buffered
delta frames `(local_seq, current]` replayed on the subscription
stream as normal post-ack deltas. Subscriber consumes the replayed
deltas to advance to `current`. Local snapshot now matches daemon.
(If the scenario instead forces fall-through to (a)/(d′)/(P) —
e.g., the gap exceeds the ring, or fingerprint at cursor disagrees
— the daemon emits `Resync` per the firing clause; T-10 covers the
clause (d) path explicitly, and for Resync paths see T-20 (clause a),
T-6 (clause P), T-30 (clause d′), T-30b (clause d′ collision case).)
Verifies I-4 (consistency holds after replay), I-8.

### T-11: Bucket disjointness

Property test: for every reachable `OrchestratorState` (randomized
spec #1 command sequencing), the snapshot partitions every `run_id`
into exactly one of `running`, `retrying`, `disconnected`. Verifies I-9.

### T-12: `repo_coordinate` round-trips

Construct a `Run` with `RepoCoordinate { slug: "acme_app", remote_url:
Some(_), default_branch: Some("main") }`, dispatch, snapshot.
`RunRow.repo_coordinate` MUST equal the input by structural equality.
Verifies I-5.

### T-13: Atomic subscribe baseline ↔ live stream

Drive the orchestrator under a steady stream of mutations
(dispatch / on_worker_exit / token-delta), then issue
`subscribe(channel, since_fingerprint=None, since_stream_seq=None, since_boot_id=None)`. The harness MUST assert: (a) the
`SubscribeAck::Resync.snapshot.snapshot_fingerprint` equals
`SubscribeAck::Resync.fingerprint`; (b) every subsequent
`SnapshotDelta` carries `stream_seq > ack.stream_seq`; (c) the union
of (baseline state) ∪ (applied deltas) equals an independently-built
oracle snapshot at the moment of subscriber attach (no mutation lost,
no mutation double-applied). Verifies §3.4 atomicity. Verifies I-4.

### T-14: `RunFinished` → `RunDetail` lookup never races

Spawn a Run, let it finish. Subscriber receives `RunFinished{run_id}`
and immediately issues `dispatch_snapshot_request(_, Run(run_id))`.
With `recent_history_ring_size = 32` and **strictly fewer than 32**
other Runs finishing between the `RunFinished` delta arrival and the
`RunDetail` lookup (i.e. at most 31 intervening finishes), the
response MUST be `RunDetail`, never `RunNotFound`. Boundary: at
exactly 32 intervening finishes, the original entry MUST have been
evicted by FIFO (§4.5 ring-before-delta) and `RunNotFound` is the
correct response — the test asserts both directions of the boundary.
Property variant: randomize finish ordering and ring fill; assert
the bound holds at exactly the inclusive/exclusive thresholds above.
Verifies §4.5 ring-before-delta ordering.

### T-15: Phantom `RunUpdated` triggers gap-driven resync

Subscriber connects, observes deltas. Harness drops
`RunStarted{run_id=X}` from the subscriber's view (simulated channel
drop) but allows the subsequent `RunUpdated{run_id=X, runner_seq=k}`
to land. Subscriber MUST detect the unknown `run_id`, issue
`subscribe(channel, since_fingerprint=local, since_stream_seq=local_seq, since_boot_id=local_boot_id)`, and receive `Resync`. Verifies
I-8 phantom-row trigger.

### T-16: Phantom retry-bucket delta triggers gap-driven resync

Subscriber connects, observes deltas. Harness drops
`RetryScheduled{run_id=X}` from the subscriber's view (simulated
channel drop) but allows the subsequent
`RetryUpdated{run_id=X, ...}` (or `RetryCleared{run_id=X, ...}`) to
land. Subscriber MUST detect the unknown `run_id` in the retry
bucket, issue `subscribe(channel, since_fingerprint=local, since_stream_seq=local_seq, since_boot_id=local_boot_id)`,
and receive `Resync`. Verifies I-8 phantom-row trigger applied
symmetrically to the retry bucket (Y-10).

### T-17: `error_message` truncation budget

Inject a retry whose `error_message` is 50 MiB. Snapshot and any
emitted delta MUST carry `error_message` of length ≤
`retry_error_max_bytes` (default 4096), terminated at a UTF-8 char
boundary, with `↵…` suffix. Verifies §4.2.1.

### T-18: Latency: SubscribeAck deadline

Block `build_snapshot` with a 600 ms sleep injected on the
orchestrator task; issue `subscribe(channel, None, None, None)`. Assert the subscriber
receives `SnapshotError::Unavailable` (stream closed) within
`subscribe_ack_max_ms + ε`, NOT a delayed `Resync`. Verifies §3.5.1
(`subscribe_ack_max_ms = 500`).

### T-19: Latency: delta flush

Drive a `dispatch_run` mutation; timestamp the orchestrator fixpoint
and the broadcast-enqueue moment; assert delta latency
`≤ delta_flush_max_ms`. Verifies §3.5.2 (`delta_flush_max_ms = 50`).

### T-20: Backpressure drop + resync

Subscribe; stop draining the subscriber's slot; emit
`> broadcast_channel_capacity` deltas; assert `dropped_deltas`
increments, the next successful delivery shows a `stream_seq`
discontinuity, the subscriber issues `subscribe(channel, since_fingerprint, since_stream_seq, since_boot_id)`
and receives `Resync` via §3.4 clause (a). Other subscribers'
`stream_seq` is uninterrupted (per-subscriber slot isolation).
Verifies §3.5.3 clause (1) and the "no silent drop" rule (c).

### T-21: Slow-subscriber disconnect

Same setup as T-20; hold the slot full ≥
`slow_subscriber_disconnect_ms = 30_000`; assert subscribe stream
closes with `SnapshotError::Unavailable`. Verifies §3.5.3 clause (2).

### T-22: Orchestrator non-blocking under backpressure

Property test — fill every subscriber slot; assert orchestrator
main-task tick latency is unaffected (no observable blocking on
`publish_snapshot_delta`). Verifies §3.5.3 prohibition (b).

### T-23: SubscribeAck version reject

Inject an ack with `version = 2`; assert subscriber closes with
`SnapshotError::Unavailable` and does NOT install the body. Verifies
§3.4.1 version-rejection rule.

### T-24: `disconnect_retention_ms` retention

Sever engine; let `disconnect_timeout_ms` fire (row enters
`recent_history_ring`); issue
`dispatch_snapshot_request(_, Run(run_id))`. At
`t < disconnect_retention_ms` ⇒ `RunDetail`; at
`t > disconnect_retention_ms` ⇒ `RunNotFound`. Verifies I-6 retention
timer (the second of X-4's "two distinct timers").

### T-25: `event_log_max_cap` DoS clamp

Request `RunDetailOptions { event_log_max: 10_000 }`; assert response
`event_log_tail.len() ≤ event_log_max_cap` (= 1000). Verifies §4.5
DoS bound.

### T-26: `since_boot_id` explicit-mismatch path

Subscriber sends `since_fingerprint = Some(matching_current_fp)`,
`since_stream_seq = Some(current_stream_seq)`, `since_boot_id =
Some(prior_boot_id)` where `prior_boot_id ≠
daemon.boot_id` (i.e. the fingerprint happens to collide with the
current daemon's fingerprint, but the explicit `since_boot_id` field
proves cross-incarnation provenance). Assert `SubscribeAck::Resync`.
Verifies §3.4 clause (b) *Detection* — explicit `since_boot_id`
inequality fires clause (b) directly without relying on fingerprint
inequality.

### T-27: `since_boot_id` fingerprint-fallback path

Subscriber omits `since_boot_id` (sends `None`), supplies
`since_fingerprint` from a prior daemon incarnation. Assert
`SubscribeAck::Resync`. Verifies §3.4 clause (b) *Detection*
fingerprint-inequality fallback when explicit `since_boot_id` is
absent (sufficient-but-not-necessary signal via fingerprint hash
input #1, §4.6.1).

### T-28: Pre-v1 compat — fresh-boot fingerprint match

Subscriber that never sends `since_boot_id` (pre-v1 client) but
whose `since_fingerprint` matches the current daemon's fingerprint
under the same boot incarnation, with `since_stream_seq =
Some(current_stream_seq)`. Assert `SubscribeAck::UpToDate`.
Verifies §3.4 clause (b) does NOT fire when fingerprint matches and
`since_boot_id` is absent (pre-v1 backward compatibility — Y-11).
(Note: `since_stream_seq` is required to satisfy the §3.4
input-validity rule.)

### T-29: Clause (b) fires without clause (d′)

Cross-incarnation subscribe with `since_stream_seq` outside the ring
window (so clauses (a) and (b) both fire by construction). Assert
exactly one `SubscribeAck::Resync` is emitted, that clause (d′)
detection path is NOT entered (the same-boot fingerprint-mismatch
arm is unreachable when `boot_id` already differs), and that the
`Resync` is attributable to (a)+(b). Assert that observability/log
shows clauses (a) and (b) BOTH evaluated true, AND that exactly ONE
`Resync` frame was written to the wire (NOT two — the closure rule
at §3.4 requires "at most one ack per subscribe"). Verifies §3.4 clause (b)
*Independence from clause (d′)* — clause (b) is the cross-incarnation
trigger and stands on its own.

### T-30: Clause (d′) fires without clause (b)

Same-boot subscribe (`since_boot_id == daemon.boot_id`) with
`since_stream_seq` inside the ring window (so clause (a) does not
fire) but with a synthetically corrupted `last_known_fingerprint`
(subscriber's `since_fingerprint` disagrees with the daemon's
fingerprint at that cursor, simulating subscriber-side divergence
or ring rebuild). Assert `SubscribeAck::Resync` driven by clause
(d′) only (full-resync; not delta replay) — clause (b) does NOT
fire, and clause (d) is excluded by the
`fp_subscriber == fp_at(s_c)` predicate. Verifies §3.4 clause (d′)
trigger predicate (independent firing without (b)).

### T-30b: Clause (d′) fires under fp_subscriber == current_fp collision

Same scenario as T-30 with `fp_subscriber == current_fp ∧
fp_subscriber ≠ fp_at(s_c)` (collision case where subscriber's
claimed view at `s_c` happens to equal `current_fp` by coincidence —
e.g., a fingerprint cycle, a synthetic test construction, or an
extremely unlikely hash coincidence). Assert
`SubscribeAck::Resync` driven by clause (d′) — collision does NOT
make (d′) skip; mismatch at `s_c` is sufficient. Verifies §3.4
clause (d′) trigger predicate (`last_known_fingerprint ≠
fp_at(s_c)`) is evaluated independently of any equality between
`last_known_fingerprint` and `current_fingerprint`.

### T-10-pre-v1: Clause (d) fires when `since_boot_id = None`

Same scenario as T-10 (same-boot, in-window, `fp_at(s_c) ==
last_known_fingerprint`, `s_c < current_stream_seq`) but the
subscriber sends `since_boot_id = None` (pre-v1 client). Assert
`SubscribeAck::UpToDate { stream_seq: s_c }` followed by post-ack
delta replay of `(s_c, current_stream_seq]`. Verifies §3.4 "Pre-v1
boot_id absence" rule — clauses (d)/(d′) MAY fire under
`since_boot_id = None` with same-boot assumed; the synthetic
collision hole is closed (the subscriber is NOT forced to clause
(c) snapshot resubscribe).

### T-30b-pre-v1: Clause (d′) fires when `since_boot_id = None`

Same scenario as T-30b (in-window, `fp_subscriber ≠ fp_at(s_c)`,
`fp_subscriber == current_fp` collision) but the subscriber sends
`since_boot_id = None`. Assert `SubscribeAck::Resync` driven by
clause (d′). Verifies §3.4 "Pre-v1 boot_id absence" rule — clause
(d′) evaluates and fires under `since_boot_id = None` exactly as
under explicit same-boot.

### T-31: Replay discovers a previously-evicted window (post-commit RingEviction)

Subscriber attaches in clause (d) replay mode at
`s_c = current_stream_seq − 1`. The `SubscribeAck::UpToDate` is
emitted, but before the per-subscriber send queue enters replay
mode (and is therefore visible to T-31b's pre-commit enqueue
scan), the owning task processes a backlog burst and commits a
lock-step trim per Z-29 sub-invariant 3.

Because this subscriber's replay was not yet in the trim's
observed-set at evaluation time, the pre-commit path does NOT
enqueue a `LockStepTrimRace` cancel for it; the trim commits
cleanly. When the per-subscriber task next reads from the
replay index/payload buffer to begin streaming
`(s_c, current_stream_seq_at_ack]`, it discovers the required
window is gone from both structures.
**Expected:** daemon emits exactly one `PostAckFrame::ReplayCancelled
{ stream_seq, reason: ReplayCancelReason::RingEviction, .. }` frame
to the affected subscriber, then closes its send queue; no further
frames (including live deltas) are written to that stream. Other
subscribers in non-replay mode receive the new deltas normally. The
cancelled subscriber's re-subscribe carries `since_fingerprint =
None` and takes clause (c) (snapshot RPC re-anchor). Verifies the
post-eviction discovery path for `RingEviction` (distinct from
T-31b's pre-commit `LockStepTrimRace` path) and the `ReplayCancelled`
frame contract (§3.4 clause (d), replay-mode buffering rule, replay
cancellation frame paragraph) — and explicitly that the daemon does
NOT emit `SubscribeAck::Resync` mid-stream.

### T-31a: ReplayCancelled with reason=BufferEviction (non-trim payload-buffer fault)

A subscriber requests replay of `[s_c+1 .. current]` where the ring's
index entries are intact (Z-29 invariant holds), but the delta payload
buffer encounters a non-trim fault: payload checksum mismatch on disk,
payload-buffer corruption detected during read, or a buffer-rebuild
operation that has not yet completed for the requested window.
(BufferEviction is NOT caused by any trim — index integrity is preserved.
Lock-step trim coordinated with an in-progress replay is `LockStepTrimRace`
(T-31b); a trim that committed without enqueuing a cancel for a
racing subscriber is discovered downstream as `RingEviction` (T-31).)

Daemon MUST emit `PostAckFrame::ReplayCancelled { stream_seq, reason:
ReplayCancelReason::BufferEviction }` and terminate the post-ack stream.
The subscriber MUST re-subscribe with no retained local state
(`since_fingerprint = None`, `since_stream_seq = None`; `since_boot_id`
MAY carry the last-known boot_id when available). The daemon then routes
via clause (c) and returns a fresh `SubscribeAck::Resync`.

Z-29's lock-step trim invariant is unaffected because no trim occurs in
this scenario; the fault is a payload-read/materialization failure, not a
replay-index / payload-buffer trim divergence.

### T-31b: ReplayCancelled with reason=LockStepTrimRace (atomic trim+cancel ordering)

A subscriber's replay is in progress when a lock-step ring trim races
and evicts the under-replay window. Daemon MUST detect the race, emit
`PostAckFrame::ReplayCancelled { stream_seq, reason: ReplayCancelReason::LockStepTrimRace }`,
and terminate the post-ack stream.

*Atomicity contract (write-before-trim, owning-task-local).* On the
orchestrator's owning task, the trim operation MUST, for each affected
replay subscriber whose stream is still open, enqueue exactly one
`PostAckFrame::ReplayCancelled { stream_seq: current_stream_seq_at_cancel,
reason: ReplayCancelReason::LockStepTrimRace }` on a terminal-control
path before the trim commits. Ordinary data-buffer backpressure MUST
NOT suppress this control frame; implementations MUST reserve capacity
(or equivalent) for one terminal control frame per replay subscriber.
Streams already closed before trim begins are excluded from the affected
set. Only after every affected open subscriber has the cancel frame
enqueued MAY the trim commit. The daemon MUST NOT wait for
subscriber-side receipt or ACKs (per §3.4 delivery semantics).

This is a strictly local atomicity guarantee on the owning task:
`WRITE-cancel-frame-to-all-buffers` happens-before `TRIM`. Subscribers'
**receive** ordering is NOT a daemon obligation. A stuck subscriber
CANNOT freeze daemon-wide trim.

---

## 7. Out of scope

- **Specific RPC transport** — Cap'n Proto, gRPC, JSON-RPC, framed
  CBOR over Unix socket are all candidates. Future ADR; this spec is
  transport-neutral so the choice can swap without touching the data
  model.
- **Snapshot persistence / durable replay.** Today snapshot is
  in-memory; on daemon restart, state is re-derived from `WorkSource`
  (spec #1 I-6) plus reattaching agents. Daemon restart implies
  `since_fingerprint` mismatch and a `Resync` for every subscriber.
- **Multi-daemon federation.** multirepo-ux Q8 deferred. Shape
  generalises (add a daemon-id discriminator to `RunRow`) but
  cross-daemon `run_id` collision rules and partial-availability
  fan-in are not specified here.
- **UI rendering rules.** Sort, color, column widths, click-through,
  keyboard bindings — spec #7, spec #8. The snapshot guarantees only
  stable, deterministic ordering of the three buckets (§3.1 step 8).
- **Per-subscriber filtering.** PubSub channel is whole-snapshot;
  clients filter locally. Future ADR.
- **Authentication and authorization.** Layered by the RPC transport.

---

## 8. Open questions

### 8.1 Push vs pull cadence (multirepo-ux Q2 — settled, alternatives recorded)

**Decision:** push for live updates (§3.3), pull on subscriber mount or
after gap (§3.4). Alternatives rejected: *pure pull* (quadratic per-tick
bandwidth at scale); *pure push, no pull* (unbounded delta retention to
bootstrap fresh subscribers); *push + periodic full-snapshot anchor*
(higher peak bandwidth than on-demand `subscribe(channel, since_fingerprint, since_stream_seq, since_boot_id)`).

### 8.2 Token-budget enforcement (multirepo-ux Q6)

The snapshot *exposes* totals; it does NOT enforce a budget. Per-run /
per-mission / per-day caps and where the cutoff happens (dispatch
admission? agent mid-turn?) belong to spec #1 §8.5.

### 8.3 Ad-hoc thread row title (multirepo-ux Q7)

When `issue_ref == None`, the row title fallback is **proposed** as the
thread title (first non-empty user prompt, truncated to 64 chars).
Final decision deferred to spec #7.

### 8.4 `LastEventSummary` truncation policy

(a) 240 bytes single-line (current recommendation, §4.4); (b) full
event line, no length cap; (c) structured payload pass-through.
Recommendation: stay at (a) for v1; revisit when spec #5 lands.

### 8.5 SnapshotFingerprint algorithm choice (CLOSED — Z-18)

**Decision (normative).** The fingerprint is computed via **unkeyed
`BLAKE3-256`** over the canonically-encoded byte stream from §4.6.1,
truncated to the first 16 bytes (`digest[0..16]`). This matches the
formula given in §4.6 and is the only conformant algorithm.

**Implementations MUST NOT use a keyed BLAKE3 ("BLAKE3 keyed with
`boot_id`")** for the fingerprint. The keyed-BLAKE3 candidate from
earlier drafts has been **rejected** for the following substantive
reasons:

- **Reproducibility without key persistence.** The fingerprint MUST
  be reproducible by external test harnesses, replay tools, and
  conformance suites that derive it solely from the canonically
  encoded byte stream of §4.6.1. An unkeyed BLAKE3 has zero hidden
  state — any conformant implementation given the same
  `OrchestratorState`-derived inputs produces the same 16 bytes.
  A keyed BLAKE3 would force every harness to (a) acquire the
  daemon's runtime keying material, (b) survive a daemon restart
  (which discards the in-memory key), or (c) standardise a key
  derivation ceremony across daemon, harness, and replay tooling.
  None of these has a defensible bootstrap path; conformance testing
  becomes intractable.
- `boot_id` is already mixed into the hash input list as input #1
  (§4.6) under the canonical encoding rule for UUIDs (§4.6.1, Z-9).
  Routing it through the BLAKE3 key parameter as well would (a)
  double-count the same value across two distinct collision-resistance
  domains and (b) couple the fingerprint algorithm to a key-derivation
  ceremony that has no downstream use case (the daemon is the only
  producer; subscribers compare-only).
- `WorkspaceId` (spec #3 §3.4) does NOT use BLAKE3-keyed-with-boot_id —
  it uses an unkeyed `BLAKE3_128` over a sanitised slug + run-id
  composition (Z-9 confirms `WorkspaceId` is a `String`, not a UUID).
  The earlier "matches spec #3" rationale was incorrect.
- xxhash3-128 was rejected because non-cryptographic compression
  reuses 32-bit lanes that re-amplify boot_id reuse on the (unlikely
  but possible) restart-with-same-pid corner case; the cost of
  switching to BLAKE3-256 is ~5 ns/byte at 1-2 MB inputs, which is
  irrelevant on the snapshot path (rebuilt at most every poll
  interval).
- SHA-256 truncated was rejected on speed alone.

Collision-resistance under truncation: 16-byte BLAKE3 output gives ≥
2^64 birthday-bound resistance over the daemon-lifetime-scoped
namespace, which is sufficient because `boot_id` (input #1) re-randomises
the namespace on every restart.

### 8.6 Recent-history ring size

`RunDetail` for finished Runs requires a bounded `recent_history_ring`
(§4.5). Default 32; operator-tunability deferred to spec #8.

---

## 9. Cross-references

- **Spec #1 (`spec-caduceus-orchestrator-algorithm.md`).** Owns state
  mutation: §3.2 reconcile, §3.3 dispatch_run, §3.5 on_worker_exit,
  §4 OrchestratorState shape (which this spec projects). This spec
  inherits its invariant set and extends with I-4, I-7, I-8, I-9.
- **Spec #2 (`spec-caduceus-agent-runner-contract.md`).** Owns token
  reconciliation (§4.3, cited verbatim in §3.1 step 3 and I-3); owns
  `runner_seq` monotonicity and the reattach handshake for I-6.
- **Spec #3 (`spec-multi-repo-workspace-model.md`).** Owns
  `RepoCoordinate` and `Workspace` (§4) — embedded in `RunRow`; owns
  the workspace path surfaced in `RunRow.workspace_path`.
- **Spec #5 (event taxonomy, future).** Owns `EventKind` used in
  `LastEventSummary` and `EventRecord` in `RunDetail.event_log_tail`.
- **Spec #7 (run-identity, upcoming).** Will define `IssueRef` and
  `ThreadId` (forward-referenced in §4.1).
- **Spec #8 (runs-panel, upcoming).** Renders this snapshot; owns
  sort-within-client, color, click-through.

This spec is **read-only** w.r.t. `OrchestratorState`. Mutation rules
belong in spec #1; this document follows.
