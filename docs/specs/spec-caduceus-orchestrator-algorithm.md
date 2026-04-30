# spec-caduceus-orchestrator-algorithm

> **Attribution.** © 2025 OpenAI, derivative work under Apache-2.0;
> `openai/symphony` @ `58cf97d`. This spec ports Symphony's orchestrator
> algorithm into caduceus's process model. Verbatim citations to
> `orchestrator.ex`, `agent_runner.ex`, `app_server.ex`, and `SPEC.md` are
> kept so the derivation is auditable. A copy of the Apache-2.0 license is
> at <http://www.apache.org/licenses/LICENSE-2.0>.

- **Status:** Draft
- **Author:** caduceus core
- **Last-updated:** 2026-04-28
- **Priority:** P0 (keystone — specs #2–#8 reference this).
- **Scope-locked:** This spec assumes the **C-hybrid topology** decision: a
  separate `caduceusd` daemon owns orchestrator state (run dispatch, retry
  maps, snapshots, multi-repo workspace registry); the caduceus engine
  (per-zed-process) owns per-thread chat state; they join on `run_id`. The
  algorithm specified here is what `caduceusd` runs. Alternative topologies
  (orchestrator co-resident with the engine, ACP-protocol-embedded
  orchestrator, multi-host orchestrator) are explicitly out of scope and
  are tracked as open questions in §8.

> **⚠️ Known residual issues — iter-28 backlog (2026-04-29).**
> The following items were surfaced by `gpt-5.4` standalone review at iter-27
> with verbatim replacement text saved in
> `private/reviews/iter27-spec1-gpt.md`. They were not blocking for the
> iter-27 ship — the spec converged on `claude-opus-4.6` + `gpt-5.3-codex`
> at min 8 / 7 respectively. Resolve in iter-28+.
>
> 1. **§0 Trust boundary** — `EngineDisconnected` is daemon-observed (a
>    daemon-owned subsystem event), not authored by an authenticated engine
>    session; reclassify producer class. Capability-scope `Cmd` senders per
>    producer class so no producer can emit another class's variants.
> 2. **Glossary `RunAttempt`** — monotonicity claim contradicts the bounded
>    `recent_history_ring`. Clarify that attempt numbering is monotonic
>    only while the Run is represented in active state or retained history;
>    a fully-drained Run MAY restart numbering at `1` after eviction.
> 3. **Glossary `RetryToken`** — specified two ways (per-Run vs
>    daemon-process-wide). Canonicalize as a per-daemon-process monotonic
>    counter; `on_retry_timer` MUST require exact equality with current
>    `RetryEntry.token`.
> 4. **§3.2 step 4 / helper section** — `eligible_for_dispatch(run, state)`
>    is called but never normatively defined. Insert a pure helper that
>    delegates to `state.work_source.classify(run) == TrackerClass::Active`.
> 5. **§4 `Config`** — `recent_history_ring_size` is referenced and
>    validated (§3, ring invariant #2) but missing from the `Config` struct.
>    Add the field with default `32` and `>= 1` constraint.
> 6. **§3.5 `Cmd::Reattach`** — has normative side effects across §3.5/§4
>    but no `on_reattach` handler body. Add the handler immediately after
>    `on_disconnect_timer_expired`: clear `disconnected_since` /
>    `disconnect_deadline`, advance `runner_seq_high_water` (drop stale
>    reattaches), set `session_id`; MUST NOT mutate `attempt` or
>    `disconnect_generation`.

---

## 0. Header

This document is the normative specification for the orchestration algorithm
executed by `caduceusd`. Its purpose is to fix the dispatch decision, the
retry scheduler, the reconcile loop, and the terminal-state cascade with
enough precision that two independent implementations agree on observable
behaviour. RFC-2119 keywords (MUST, MUST NOT, SHOULD, SHOULD NOT, MAY) are
used in their RFC-2119 sense.

**Trust boundary (normative).** Every `Cmd::*` message consumed by this
algorithm MUST come from an allowed producer for that specific variant.
The allowed producers are: (1) daemon-owned timers (`Tick`, `RetryRun`,
`DisconnectTimerExpired`), (2) daemon-owned subsystems and workers
(`WorkerExit`, `WorkflowReloaded`, supervisor-issued `Shutdown`),
(3) snapshot clients for `SnapshotRequest`, and (4) an authenticated
engine session established by spec #8 for `EngineDisconnected` and
`Reattach`. A producer MUST NOT be allowed to submit any other `Cmd`
variant. Any unauthenticated, cross-user, replayed, or wrong-producer
message MUST be rejected before it reaches the main loop. WorkSource
credentials/secrets are owned by spec #6, and workspace path validation /
symlink-escape rejection are owned by spec #3; implementations of this
spec MUST rely on those contracts and MUST NOT bypass them.

The reference implementation that this spec derives from is Symphony's
Elixir orchestrator (`elixir/lib/symphony_elixir/orchestrator.ex`,
`elixir/lib/symphony_elixir/agent_runner.ex`,
`elixir/lib/symphony_elixir/codex/app_server.ex`) at commit `58cf97d`. Each
algorithm below carries a *Cite* block of the form `(file:lines, SPEC §)`
pointing into that tree. Where caduceus diverges from Symphony, the
divergence is called out as **(adaptation)** with a one-line rationale.

---

## 1. Scope

### 1.1 In scope

- The `caduceusd` main poll loop (the single-authority decision loop).
- The dispatch decision (which `Run`s to start this tick).
- The retry scheduler (continuation retries on normal exit, exponential
  backoff on abnormal exit, stale-token rejection).
- The reconcile loop (re-derive ground truth from the `WorkSource` each
  tick before dispatching).
- The terminal-state cascade (WorkSource terminal → kill running attempt →
  cleanup workspace).
- Workspace cleanup-at-boot (sweep workspaces whose `WorkSource` item is
  already terminal at daemon start).
- Reconciliation across daemon restart (re-derive `running` set from
  `WorkSource`; reap orphans on first tick).

### 1.2 Out of scope

- **Agent runner internals** (`run_attempt`'s inner turn loop, prompt
  shape, session handshake, JSONL transport, three-stage stop) — see
  `spec-caduceus-agent-runner-contract.md` (#2).
- **Workspace filesystem layout** (path sanitisation, symlink-escape
  rejection, lock-file scheme, per-repo subdirs) — see
  `spec-multi-repo-workspace-model.md` (#3).
- **Snapshot shape** (the `RunSnapshot` PubSub channel that replaces
  Symphony's `:tick`/`:run_poll_cycle` 20 ms render delay) — see
  `spec-orchestrator-status-snapshot.md` (#4).
- **Collab patterns 3+** (shared-context multi-agent). v1 caduceus
  supports Patterns 1 (sequential handoff via WorkSource) and 2
  (concurrent isolated) only — see `spec-caduceus-collab-patterns.md`
  (#5) and §7 below.
- **Workflow YAML contract** (workflow schema, hot-reload semantics,
  prompt-shape contract for `build_turn_prompt`) — see
  `spec-caduceus-workflow-contract.md` (#6).
- **UI surfaces** (runs panel, reattach UX, dashboard) — see
  `spec-caduceus-runs-panel.md` (#7) and
  `spec-caduceus-engine-daemon-protocol.md` (#8).
- **SSH worker hosts / multi-host orchestration.** v1 caduceus is
  single-host. (`orchestrator.ex:660–743` `spawn_issue_on_worker_host`
  has multi-host plumbing; caduceus drops it.)
- **Persistent scheduler database.** `caduceusd` MUST be able to lose all
  in-memory state and reconcile from `WorkSource` on restart (see I-6).

---

## 2. Terms

- **WorkSource** — the analog of Symphony's *tracker*. The single
  source-of-truth for *what work exists* and *what state it is in*. It is
  pluggable: caduceus ships three reference adapters — Linear, local-file
  (a checked-in TODO file), GitHub Issues. The trait surface is defined
  in `spec-caduceus-workflow-contract.md` (#6); this spec depends only on
  the abstract contract: `fetch_candidates`, `fetch_by_ids`,
  `revalidate`, `classify`. **Tracker** is retained as a synonym for
  parity with Symphony source citations.
- **Run** — one logical unit of work, identified by `RunId`. A `RunId` is
  derived from a stable `WorkSource` identifier (e.g. Linear issue
  identifier, GitHub issue number, local-file slug). A Run's identity
  outlives any individual attempt. Runs map 1:1 to `Issue` in Symphony.
- **RunAttempt** — a single end-to-end execution of an agent for a Run,
  spanning many turns within a single agent session. A Run may have many
  RunAttempts over its life (one per dispatch). Attempt numbering is
  monotonic per Run within a daemon's lifetime; on daemon restart,
  attempt numbering MAY restart at 1 (the WorkSource is the durable
  identity, not the attempt counter).
- **Workspace** — the per-Run filesystem root used as the agent
  process's `cwd`. The unique handle for a Run is the canonical path
  returned by spec #3's `create_workspace(repo_coordinate, run_id, workflow)`,
  whose layout is `<workspace_root>/<repo_slug>/<run_id>/`. The daemon
  MUST treat this path as registry-owned (spec #3); it MUST NOT
  re-derive the path from `run.identifier`. See I-2.
- **RunningEntry** — the in-memory record for a live RunAttempt. Fields
  in §4.
- **RetryEntry** — the in-memory record for a scheduled retry. Fields in
  §4.
- **ReconcileResult** — the per-tick verdict for each Run currently in
  `running`: one of `Active`, `Terminal`, `Neither` (paused / reassigned /
  out of query). Drives the cascade in `reconcile_running_runs`.
- **Tick** — one iteration of the daemon's main poll loop.
- **ClaimedSet** — the set of `RunId`s for which a dispatch is in flight
  *within a single tick*. Prevents double-dispatch in the window between
  candidate sort and the spawn returning a handle. Cleared as part of
  exit/reconcile.
- **RunningMap** — `RunId → RunningEntry`. Live RunAttempts.
- **RetryMap** — `RunId → RetryEntry`. Pending retries (continuation or
  failure backoff).
- **RetryToken** — a monotonic per-Run generation counter. Each fresh
  dispatch increments the Run's RetryToken; a scheduled retry message
  carries the token under which it was scheduled, and is dropped if the
  current `RetryEntry.token` does not match. This is the freshness check
  for stale retry timers (`orchestrator.ex:1456` generation counter; see
  I-4). A monotonic counter is sufficient — no clock dependence.
- **Tracker** — synonym for **WorkSource**. Used in citations.

---

## 3. Normative algorithms

This section ports the five algorithms from Symphony's Part A.1 into
Rust-flavoured pseudocode. The pseudocode is normative for control flow,
ordering, and state mutations; it is not required to compile. Each
algorithm is followed by the invariants it enforces and the failure modes
it tolerates.

Three Symphony-to-caduceus adaptations apply throughout:

1. **Mailbox.** Symphony's BEAM GenServer mailbox becomes a
   `tokio::sync::mpsc::Receiver<Cmd>` owned by the daemon's main task.
   The main task is the **single consumer** of all state-mutating
   commands; this preserves the GenServer's single-authority property
   under tokio. `select!` over the mpsc receiver, the tick interval,
   exit notifications, and retry timers replaces the GenServer
   `handle_info` dispatch. (Adaptation of `orchestrator.ex:74–273`.)
2. **Render delay dropped.** Symphony's
   `@poll_transition_render_delay_ms = 20` split (`orchestrator.ex:74–117`)
   exists so the dashboard can render "checking now…" before the work
   begins. caduceus replaces this with the snapshot-PubSub channel
   defined in spec #4: snapshots are emitted on state transitions, not
   on a tick boundary. The render delay MUST NOT be ported.
3. **Multi-host dropped.** Symphony's `spawn_issue_on_worker_host`
   (`orchestrator.ex:660–743`) accepts a `worker_host` selector; v1
   caduceus has one host (the daemon's host). The selector is dropped;
   `spawn_worker` is unconditional local spawn.

The three state collections — `claimed`, `running`, `retry_attempts` —
are kept verbatim with the same semantics (`claimed` prevents
double-dispatch within a tick; `running` tracks live attempts;
`retry_attempts` tracks backoff). The `RetryToken` generation counter is
preserved verbatim.

### 3.1 `start_service` — bring the daemon up cleanly

> **Cite.** SPEC §16.1 lines 1681–1706; `orchestrator.ex:52–71` `init/1`.

```rust
async fn start_service(config: Config) -> OrchestratorState {
    // Boot ordering (normative): validation runs against the raw `&Config`
    // BEFORE any `OrchestratorState` is allocated. This guarantees that
    // ring invariant #2 (`recent_history_ring_size >= 1`) and every other
    // dispatch-config precondition is enforced pre-allocation; a bad
    // config can never produce a partially-constructed state.
    //
    // On rejection we MUST surface a structured diagnostic that cites the
    // exact invariant the operator violated (e.g. `max_dispatch_defer_-
    // attempts: must be >= 1`) and then abort the process. T-Z9's
    // config-validation variant pins this contract: an unstructured
    // `.expect()` panic would not satisfy the "diagnostic citing the
    // `>= 1` invariant" assertion.
    match validate_dispatch_config(&config) {
        Ok(()) => {}
        Err(reason) => {
            // `reason` is the human-readable invariant citation produced
            // by `validate_dispatch_config`, e.g.
            //   "max_dispatch_defer_attempts: must be >= 1"
            //   "recent_history_ring_size: must be >= 1"
            log_diagnostic(
                "config_validation_failed",
                reason = &reason,
            );
            abort_process();   // non-zero exit; no OrchestratorState ever allocated
        }
    }

    configure_logging(&config);
    start_workflow_watch(&config.workflow_path);          // hot-reload; spec #6

    let mut state = OrchestratorState {
        config:                config.clone(),
        boot_id:               Uuid::new_v4(),               // Z-6: random per-process,
                                                             // stable for daemon lifetime;
                                                             // mixed into spec #4 §4.6 fingerprint
                                                             // and echoed on §3.4 SubscribeAck.
        running:               HashMap::new(),
        claimed:               HashSet::new(),
        retry_attempts:        HashMap::new(),
        dispatch_defer_attempts: HashMap::new(),               // Z-9 livelock guard
        last_poll_at:          None,
        next_poll_scheduled:   None,
        token_totals:          HashMap::new(),
        last_reported_tokens:  HashMap::new(),
        work_source:           build_work_source(&config),  // spec #6
        events_tx:             snapshot_bus_sender(&config), // spec #4
        recent_history_ring:   BoundedRingBuffer::with_capacity(
                                   config.recent_history_ring_size), // Y-4
    };

    startup_terminal_workspace_cleanup(&mut state).await;   // SPEC §9.6
    schedule_tick(after_ms = 0);                            // first poll immediate
    state
}
```

**Invariants enforced.**

- `validate_dispatch_config` MUST run as the **first line** of
  `start_service`, **before** `OrchestratorState` is allocated. It
  operates on `&Config` (the raw config), not on `&OrchestratorState`,
  so a misconfigured workflow (missing `agent.command`, unsanitisable
  workspace root, undeclared WorkSource adapter,
  `recent_history_ring_size == 0` — see ring invariant #2;
  `max_dispatch_defer_attempts < 1`) is a
  startup-time failure that fails the daemon **before any state
  allocation occurs**, not a run-time failure. `validate_dispatch_config`
  MUST validate `Config.max_dispatch_defer_attempts >= 1` and reject the
  Config with a startup diagnostic if violated (Z-9 livelock guard
  requires at least one attempt budget). There is no path on
  which a partially-constructed `OrchestratorState` is observable to
  any other call site. (`orchestrator.ex:67`.)
- `startup_terminal_workspace_cleanup` MUST run exactly once per
  daemon process. After this, the reconcile path (§3.2) is the sole
  authority for cleanup. This is the cold-start half of I-5 and
  closes the window where a Run was marked terminal in the WorkSource
  while the daemon was down. (SPEC §9.6.)
- `schedule_tick(0)` rather than waiting for `poll_interval_ms`: the
  first poll is on the critical path of "daemon started" and MUST NOT
  be delayed.
- `OrchestratorState` is owned exclusively by the main task (I-1).

**Failure modes.**

- **Workflow file missing or unparseable.** `validate_dispatch_config`
  fails; the daemon exits with a non-zero code. Service supervisor
  (launchd / systemd / `caduceusd run`'s parent) decides whether to
  retry. The daemon MUST NOT silently start without a valid workflow.
- **Workspace root missing.** `startup_terminal_workspace_cleanup`
  treats a missing root as empty (no Runs to clean). The first
  dispatch creates the root.
- **WorkSource unreachable at boot.** The first tick will fail
  `fetch_candidates`; the loop logs and re-arms via §3.2 step 5. The
  daemon MUST NOT crash on transient WorkSource failure.

```rust
/// Z-3: Common terminal-path helper. Routes a run through the full daemon-
/// terminated exit sequence so every cascade point lands in
/// `recent_history_ring` (spec #4 §4.5) and clears claimed/retry state.
///
/// All daemon-driven cascade paths — the §3.2 stall sweep, the
/// `WorkSourceTerminal` arm, the `WorkSourceLeftQuery` (Neither) arm, and
/// the orphan reaper — MUST call this helper rather than open-coding the
/// removal sequence; pre-Z-3 paths bypassed `recent_history_ring` because
/// they did not call `push_finished`. The helper is the single
/// normative writer of `ExitReason::DaemonTerminated { cause }` for these
/// cascades; `on_worker_exit` continues to own the agent-driven exit
/// reasons (`Normal`, `Abnormal`).
async fn terminate_and_finish(
    state: &mut OrchestratorState,
    run_id: RunId,
    terminate_reason: &'static str,
    cause: TerminationCause,
    cleanup: bool,
) {
    let entry = match state.running.remove(&run_id) {
        Some(e) => e,
        None => return,                                     // already gone
    };
    stop_cascade(&entry, reason = terminate_reason).await;  // spec #2 §3.3
    if cleanup {
        cleanup_workspace(&run_id, state).await;            // spec #3 §3.6
    }
    state.retry_attempts.remove(&run_id);
    state.claimed.remove(&run_id);
    let reason = ExitReason::DaemonTerminated { cause };
    push_finished(state, &entry, &reason, /*final*/ true);  // Y-4 ring write
    // Ring invariant #5 (post-finalize cleanup; canonical declaration in §4
    // `OrchestratorState.recent_history_ring` docstring).
    state.token_totals.remove(&run_id);
    state.last_reported_tokens.remove(&run_id);
}

fn drain_per_run_state(state: &mut OrchestratorState, run_id: &RunId) {
    state.running.remove(run_id);              // defensive
    state.claimed.remove(run_id);
    state.retry_attempts.remove(run_id);
    state.dispatch_defer_attempts.remove(run_id);
    state.token_totals.remove(run_id);
    state.last_reported_tokens.remove(run_id);
}
```

**Postconditions of `terminate_and_finish` (normative).** On return, all
of the following MUST hold for the supplied `run_id`:

1. `state.running.contains_key(&run_id) == false`.
2. `state.claimed.contains(&run_id) == false`.
3. `state.retry_attempts.contains_key(&run_id) == false`.
4. **Conditional ring write.**
   - If `state.running.contains_key(&run_id)` was `true` at entry,
     `state.recent_history_ring` has had **exactly one** new
     `FinishedRunSummary` appended whose `run_id` equals the
     argument and whose
     `exit_reason == ExitReason::DaemonTerminated { cause }` for
     the `cause` argument supplied (eviction MAY have occurred per
     §4 ring invariants, but the new entry is at the tail).
   - If `state.running.contains_key(&run_id)` was `false` at entry,
     the helper short-circuits and `state.recent_history_ring` is
     unchanged.
5. `stop_cascade` has been invoked exactly once on the removed worker
   handle (spec #2 §3.3); this helper removes `state.running[run_id]`
   BEFORE invoking `stop_cascade`.
6. If `cleanup == true`, `cleanup_workspace` has been invoked exactly
   once for the run (spec #3 §3.6); if `cleanup == false`, no workspace
   mutation has occurred on this path.
7. No retry timer has been scheduled for the run on this path
   (`DaemonTerminated` is cascade-terminal — X-11). Any *prior* in-flight
   `Cmd::RetryRun` for this run is either already drained or will be
   dropped by `on_retry_timer`'s freshness check (I-4) since
   `state.retry_attempts` no longer contains the run.
8. `state.token_totals.contains_key(&run_id) == false` and
   `state.last_reported_tokens.contains_key(&run_id) == false`
   (ring invariant #5; see §4).

If the helper is invoked on a `run_id` that is not in `state.running`
(early-return at line 309), postconditions 1, 2, 3, 5, 6, 7 are trivially
satisfied (no entry to act on), postcondition 4 is satisfied via its second
arm (no ring write), and postcondition 8 is satisfied because the prior
call that removed `run_id` from `state.running` is the canonical
token-drain site. The drain sites are:

  1. this helper (`terminate_and_finish`) — removes from `running` and
     drains tokens together;
  2. the "WorkSource vanished" branch in `on_retry_timer` at L943
     (drain L980–985) — `running` was already drained upstream by
     `on_worker_exit` at L739; this branch verifies absence (defensive
     `state.running.remove` at L980, B23 FIX 5) and drains the
     remaining 5 maps (B22 FIX 2 also writes the cascade ring entry
     from this branch via `push_finished_from_retry` at L964);
  3. the defer-cap abandon branch in `on_retry_timer` at L1140
     (drain L1194–1198) — same upstream-drain shape as (2): `running`
     was drained by `on_worker_exit`, this branch drains the remaining
     maps without ring write (no truthful `TerminationCause` exists
     for this path: the run was abandoned by the dispatch-defer
     livelock guard, not by a WorkSource transition; documented in
     place at L1176–1193);
  4. `on_worker_exit`'s `DaemonTerminated` defensive arm at L763–794
     (full Z-3-equivalent finalize via `push_finished` at L787); and
  5. the `TrackerClass::{Terminal, Neither}` arms in `on_retry_timer`
     at L999 (drain L1045–1050) and L1057 (drain L1082–1087) (B22
     FIX 1) — `running` was already drained upstream by
     `on_worker_exit`; each arm verifies absence from `running`
     defensively, writes the cascade ring entry via
     `push_finished_from_retry` (L1036 / L1074), then drains the
     remaining 5 maps. The Terminal arm additionally calls
     `cleanup_workspace` at L1035 (B23 FIX 1) for symmetry with the
     canonical live `Terminal` path at §3.2 step 5b (L495–502); the
     Neither arm does NOT (symmetric with the canonical live
     `Neither` path's `cleanup=false` at L510–517).

NOTE on `on_worker_exit`'s `Normal` / `Abnormal` arms: those remove from
`running` but defer token drain to the subsequent `on_retry_timer`
Terminal/Neither/vanished arms (or to `terminate_and_finish` if the
daemon terminates first); postcondition 8 is therefore *eventually*
satisfied for runs in the retry-pending window. The four cascade call
sites (§3.2 stall sweep, `WorkSourceTerminal`, `WorkSourceLeftQuery`/
Neither, orphan reaper) MUST invoke this helper and MUST NOT open-code
the removal sequence.

### 3.2 `poll_tick` — the single decision loop

> **Cite.** SPEC §16.2 lines 1711–1738; `orchestrator.ex:74–117` (`:tick`,
> `:run_poll_cycle`) + `:224–273` (`maybe_dispatch`).

```rust
async fn poll_tick(state: &mut OrchestratorState) {
    state.last_poll_at = Some(Instant::now());

    // 1. Reconcile what we already think is running against ground truth.
    reconcile_running_runs(state).await;

    // 2. Re-validate dispatch config (workflow may have hot-reloaded).
    //    Operates on `&Config` (signature hoisted in §3.1); `state.config`
    //    is the current snapshot after any hot-reload swap.
    if validate_dispatch_config(&state.config).is_err() {
        log_and_skip_dispatch_this_tick(state);
        schedule_tick(after_ms = state.config.poll_interval_ms);
        return;
    }

    // 3. Pull candidate Runs from the WorkSource.
    let mut candidates = match state.work_source.fetch_candidates(
        &state.config.work_source_query
    ).await {
        Ok(c) => c,
        Err(e) => {
            log_work_source_error(e);
            schedule_tick(after_ms = state.config.poll_interval_ms);
            return;
        }
    };
    sort_candidates(&mut candidates,
        by = (priority_desc, updated_at_desc, identifier_asc));

    // 4. Dispatch into open slots.
    for run in candidates {
        if state.running.len() >= state.config.max_concurrency { break; }
        if state.running.contains_key(&run.id)              { continue; }
        if state.claimed.contains(&run.id)                  { continue; }
        if !eligible_for_dispatch(&run, state)              { continue; }
        // Discard `DispatchResult` here: a `Deferred` outcome on the main
        // loop simply means "try again next tick"; there is no retry entry
        // to keep alive in this path. The retry-timer path (§3.5) is the
        // only caller that MUST act on `Deferred`.
        let _ = dispatch_run(state, run).await;
    }

    // 5. Re-arm. Unconditional.
    schedule_tick(after_ms = state.config.poll_interval_ms);
}
```

```rust
async fn reconcile_running_runs(state: &mut OrchestratorState) {
    // 5a. Stall sweep — agents with no telemetry within stall_timeout_ms.
    // Z-3: routes through terminate_and_finish so the run lands in
    // `recent_history_ring` (Y-4) with cause = Stall.
    let stall = state.config.agent.stall_timeout_ms;
    let now = Instant::now();
    let stalled: Vec<RunId> = state.running.iter()
        .filter(|(_, e)| now.duration_since(e.last_activity_at).as_millis() as u64 > stall)
        .map(|(id, _)| id.clone())
        .collect();
    for run_id in stalled {
        terminate_and_finish(
            state, run_id,
            /*terminate_reason*/ "stall_timeout",
            TerminationCause::Stall,
            /*cleanup*/ true,
        ).await;
    }

    // 5b. WorkSource refresh — every running Run is re-fetched.
    // Transient WorkSource failure MUST NOT cause healthy running attempts
    // to be reaped as `orphan_after_restart`. On Err we log and return
    // *without* running step 5c; classification and orphan reaping resume
    // on a future tick once the WorkSource is reachable again. This is the
    // failure-mode contract documented below ("Running attempts are not
    // terminated on a transient WorkSource error").
    let ids: Vec<RunId> = state.running.keys().cloned().collect();
    let refreshed = match state.work_source.fetch_by_ids(&ids).await {
        Ok(r) => r,
        Err(e) => {
            log_work_source_error(e);
            return;                                         // skip classification + 5c this tick
        }
    };
    let observed_ids: HashSet<RunId> = refreshed.iter().map(|r| r.id.clone()).collect();

    for run in refreshed {
        match state.work_source.classify(&run) {
            TrackerClass::Terminal => {
                // Z-3: cleanup workspace on Terminal classification.
                terminate_and_finish(
                    state, run.id.clone(),
                    /*terminate_reason*/ "work_source_terminal",
                    TerminationCause::WorkSourceTerminal,
                    /*cleanup*/ true,
                ).await;
            }
            TrackerClass::Active => {
                // dashboard input — refresh the cached snapshot in place.
                if let Some(entry) = state.running.get_mut(&run.id) {
                    entry.run_snapshot = run;
                }
            }
            TrackerClass::Neither => {                       // paused / reassigned
                // Z-3: do NOT cleanup workspace — Run may return.
                terminate_and_finish(
                    state, run.id.clone(),
                    /*terminate_reason*/ "work_source_left_query",
                    TerminationCause::WorkSourceLeftQuery,
                    /*cleanup*/ false,
                ).await;
            }
        }
    }

    // 5c. Daemon-restart reaper: anything in `running` that the WorkSource
    // does not return is an in-flight attempt with no live worker (the
    // worker died with the previous daemon). Treat as Neither.
    //
    // Step 5c MUST run only after a *successful* `fetch_by_ids` (the early
    // `return` above guarantees this). Running it on an empty `refreshed`
    // produced by a transient error would orphan-reap every healthy run.
    // Z-3: route through terminate_and_finish so orphan-reaped rows land
    // in `recent_history_ring` with cause = WorkSourceMissing.
    let orphans: Vec<RunId> = state.running.keys()
        .filter(|id| !observed_ids.contains(id))
        .cloned().collect();
    for run_id in orphans {
        terminate_and_finish(
            state, run_id,
            /*terminate_reason*/ "orphan_after_restart",
            TerminationCause::WorkSourceMissing,
            /*cleanup*/ false,
        ).await;
    }
}
```

**Invariants enforced.**

- The orchestrator MUST NOT use its own in-memory state to decide what
  *should* run. The set of *what should run* is re-derived from
  `WorkSource` every tick; in-memory `running` / `claimed` exist only
  to suppress duplicate spawns. (SPEC §7.4 — "tracker is the queue".)
- Reconciliation MUST run before dispatch in the same tick. If two
  conflicting truths exist (e.g. a worker that crashed but we don't yet
  know vs. a WorkSource state that says "done"), WorkSource wins. (I-3.)
- `schedule_tick` MUST be unconditional at the bottom. There MUST NOT
  be a path through the loop that fails to re-arm. A panic in dispatch
  MUST NOT stop the timer (use a `tokio::select!`-level panic boundary
  or supervised task to enforce this).
- The orphan reaper (5c) MUST run on every tick, not only the first
  one. This is the steady-state guarantee for I-6.

**Failure modes.**

- **WorkSource transient failure.** `fetch_candidates` and `fetch_by_ids`
  can fail; the loop logs and re-arms. Running attempts are not
  terminated on a transient WorkSource error — only on a positive
  `Terminal` or `Neither` classification.
- **`max_concurrency` saturation.** Dispatch loop breaks early; surplus
  candidates wait for the next tick. There is no queue.
- **Stall sweep false positive.** A long-running tool (e.g. a multi-minute
  test runner) that emits no events can be killed. Mitigation: agents
  SHOULD emit progress events at least every `stall_timeout_ms / 2`. See
  open question §8.4.
- **Workflow hot-reload makes config invalid mid-tick.** `validate_dispatch_config`
  fails on step 2; this tick skips dispatch but reconcile already ran,
  so terminal-state cascade still applies. Re-arm proceeds.

### 3.3 `dispatch_run` — spawn one worker, atomically claim it

> **Cite.** SPEC §16.4 lines 1769–1803; `orchestrator.ex:660–743`
> (`do_dispatch_issue`, `spawn_issue_on_worker_host`),
> `:745–763` (`revalidate_issue_for_dispatch`).

```rust
/// Return value of `dispatch_run`. Callers MUST act on this:
/// - Main-loop dispatch (§3.2 step 4) discards the value (see comment there).
/// - Retry-timer dispatch (§3.5 `on_retry_timer`) MUST keep the
///   `RetryEntry` alive and reschedule on `Deferred`, and MUST remove it
///   on `Spawned`. Removing the retry entry on `Deferred` strands the run
///   in `claimed` with no live worker and no scheduled retry.
enum DispatchResult {
    /// A worker was spawned and inserted into `running`; any prior
    /// `RetryEntry` for the run has been cleared by this function.
    Spawned,
    /// The dispatch did not spawn a worker (revalidation race, workspace
    /// creation failure, spawn failure). `running`, `claimed`, and
    /// `retry_attempts` were not mutated by this call.
    Deferred { reason: &'static str },
}

async fn dispatch_run(state: &mut OrchestratorState, run: Run) -> DispatchResult {
    // Re-validate immediately before spawn — WorkSource may have changed
    // since the candidate sort.
    let run = match state.work_source.revalidate(run).await {
        Ok(r) if r.dispatchable => r,
        _ => return DispatchResult::Deferred { reason: "revalidate_raced" },
    };

    let workspace = match caduceusd
        .create_workspace(run.repo_coordinate.clone(), run.id.clone(), workflow)
        .await                                              // spec #3 §3.5
    {
        Ok(ws) => ws,
        Err(_) => return DispatchResult::Deferred { reason: "workspace_unavailable" },
    };
    let workspace_path = workspace.path.clone();          // canonical, registry-owned
    let workspace_root = workspace.root.clone();          // Y-9: daemon-wide root from spec #3 §4

    let attempt = next_attempt_number(state, &run.id);
    let retry_token = RetryToken::fresh();                 // monotonic; §4

    let (handle, exit_rx) = match spawn_worker(
        run_attempt,                                       // spec #2 owns body
        args = (run.clone(), workspace_path.clone(), workspace_root.clone(),
                attempt, state.config.clone()),            // Y-9: workspace_root threaded through
        on_event = state.events_tx.clone(),
    ) {
        Ok(pair) => pair,
        Err(_) => return DispatchResult::Deferred { reason: "spawn_failed" },
    };

    state.running.insert(run.id.clone(), RunningEntry {
        handle,
        exit_rx,
        run_snapshot:           run.clone(),
        workspace_path,
        attempt,
        retry_token,
        started_at:             Instant::now(),
        last_activity_at:       Instant::now(),
        pid:                    handle_pid(&handle),       // populated when worker is an OS child (§8.1)
        session_id:             None,                       // filled on agent-runner session_started; spec #2
        restart_count:          prior_restart_count(state, &run.id), // Source: state.recent_history_ring.
                                                                     // Find the last `FinishedRunSummary` for `run_id`;
                                                                     // if its `exit_reason == DaemonTerminated{..}`, return
                                                                     // `restart_count + 1`; otherwise return its
                                                                     // `restart_count`. Default 0 when no entry is found
                                                                     // (cold-start or evicted). The ring is the single
                                                                     // source of truth for restart_count across attempts;
                                                                     // spec #4 RunRow projects this field.
        current_retry_attempt:  prior_retry_attempt(state, &run.id), // last RetryEntry.attempt before clear
        runner_seq_high_water:  0,                           // bumped on each turn_event; spec #2 §4.4
        disconnected_since:     None,                        // armed by Cmd::EngineDisconnected (§8.7)
        disconnect_deadline:    None,                        // disconnect_timeout_ms expiry (§8.7)
        disconnect_generation:  0,                           // Z-1: freshness key paired with `attempt`;
                                                             // bumped on every None→Some transition in
                                                             // `on_engine_disconnected`; preserved across
                                                             // Cmd::Reattach (which clears disconnected_since
                                                             // / disconnect_deadline only).
    });
    state.claimed.insert(run.id.clone());
    state.retry_attempts.remove(&run.id);                  // spawn clears prior retry tag
    state.dispatch_defer_attempts.remove(&run.id);         // Z-9: clear any prior defer streak
    DispatchResult::Spawned
}
```

**Invariants enforced.**

- `revalidate` is mandatory (I-3). The WorkSource fetch in `poll_tick`
  step 3 and the spawn here are racey w.r.t. the WorkSource; the
  revalidate-at-spawn closes that window cheaply. An implementation that
  drops it is **not a port** of this algorithm.
- `claimed.insert` MUST happen *after* the worker handle is obtained. If
  spawn fails, no claim is recorded and the Run is naturally re-attempted
  next tick.
- `retry_attempts.remove(&run.id)` MUST run on dispatch. A successful
  spawn supersedes any pending retry for that Run. Any in-flight retry
  timer for a previous attempt's token will be dropped by `on_retry_timer`'s
  freshness check (I-4).
- The `RunningEntry` shape is the public surface for the snapshot
  channel (spec #4) and the reconciler. Its fields MUST be stable; see §4.

**Failure modes.**

- **Workspace creation fails** (disk full, permission denied,
  symlink-escape rejected). Dispatch returns without inserting into
  `running` or `claimed`. The Run is eligible again next tick.
- **Worker spawn fails** (binary missing, fork failure). Same: no claim,
  retried next tick.
- **`revalidate` reports the Run is no longer dispatchable** (closed,
  reassigned). Dispatch returns; reconcile will observe and clean up
  any stray workspace if it became terminal.
- **Concurrent dispatch attempts for the same `RunId`.** Suppressed by
  the `claimed` check in §3.2 step 4 within a tick, and by `running`
  check across ticks. If both checks somehow fail, I-2 (workspace =
  identity) makes the second worker's `cwd` collide and spec #3's
  workspace lock-file MUST reject the second spawn.

### 3.4 `run_attempt` — defer to spec #2

> **Cite.** SPEC §16.5 lines 1808–1863; `agent_runner.ex:13–145`.

The body of `run_attempt` (the per-attempt session loop, the
turn-by-turn prompt construction, the WorkSource recheck after every
turn, the `before_run`/`after_run` hooks, the three-stage stop) is owned
by `spec-caduceus-agent-runner-contract.md` (spec #2). This spec
constrains it only at the contract surface:

- `run_attempt` MUST be a single async function spawned as a tokio task
  by §3.3. Its return value is an `ExitReason` enum (§4).
- `run_attempt` MUST NOT touch `OrchestratorState` directly. All
  observability flows through `events_tx` (spec #4) and the exit
  channel (`on_worker_exit`, §3.5).
- `run_attempt` MUST re-fetch WorkSource state between turns and exit
  early on `Terminal` or `Neither`. This is the responsiveness mechanism
  for the terminal-state cascade (I-5); the orchestrator MUST NOT push
  cancellation into the session.
- `run_attempt`'s normal exit (turn budget exhausted or WorkSource says
  "stop") returns `ExitReason::Normal`, which triggers the
  *continuation* retry path in §3.5 (1 s default). Its abnormal exit
  (panic, agent crash, unparseable transport, hook failure) returns
  `ExitReason::Abnormal { error }`, which triggers exponential backoff
  in §3.5.
- `run_attempt` MUST emit a `turn_event` per agent message such that
  `RunningEntry.last_activity_at` (updated by the daemon on receipt)
  resets the stall sweep clock.

The full algorithm body, prompt-shape contract, and JSONL schema are
specified in #2.

### 3.5 `on_worker_exit` + `on_retry_timer`

> **Cite.** SPEC §16.6 lines 1868–1913; `orchestrator.ex:119–164` (DOWN),
> `:206–217` (`:retry_issue`), `:829–922` (`handle_retry_issue`),
> `:928–939` (`retry_delay`), `:1456` (RetryToken generation counter).

```rust
async fn on_worker_exit(state: &mut OrchestratorState, run_id: RunId, reason: ExitReason) {
    let entry = match state.running.remove(&run_id) {
        Some(e) => e,
        None => return,                                    // already reconciled away
    };
    // NOTE: claimed is NOT removed here for retry-bound exits. The claim
    // persists until either the retry timer fires (and re-decides) or
    // reconcile observes the Run as Terminal/Neither. This prevents a tick
    // between exit and retry from double-dispatching. For terminal exits
    // (`DaemonTerminated`) the claim IS released here because no retry is
    // scheduled (X-11 cascade-terminal).

    let (delay_ms, retry_reason, attempt, error_message) = match &reason {
        ExitReason::Normal => {
            (CONTINUATION_RETRY_DELAY_MS, "continuation", entry.attempt, None)
        }
        ExitReason::Abnormal { error } => {
            let n = next_retry_attempt(&entry);    // 1, 2, 3, …
            let cap = state.config.agent.max_retry_backoff_ms;
            let base = FAILURE_RETRY_BASE_MS;
            (min(base * (1u64 << (n - 1)), cap),
             "failure",
             n,
             Some(error.to_string()))                      // Y-5: captured at exit, never re-derived
        }
        ExitReason::DaemonTerminated { .. } => {
            // Defensive finalize for an unexpected DaemonTerminated observed
            // in on_worker_exit. Z-3 (`terminate_and_finish`) is the canonical
            // single writer for daemon-termination cascade completion (§3.1,
            // L300). Under the intended contract this arm is unreachable: Z-3
            // removes `state.running[run_id]` BEFORE `stop_cascade`, so the
            // worker's exit notification hits `state.running.remove → None`
            // and short-circuits at the early-return above.
            //
            // BUT: if we DO reach this arm, that proves Z-3 did NOT finalize
            // this run_id (early-return would otherwise have fired). To avoid
            // leaking `claimed` / token state or leaving a stuck RunningEntry,
            // perform the full Z-3-equivalent cleanup/finalize here. The ring
            // entry written here is the FIRST (canonical) entry — not a
            // duplicate — and therefore preserves T-3's "exactly one entry
            // per cascade" rule. The `debug_assert!` traps in test builds so
            // T-3 fails loudly under any Z-3 contract breach; production
            // builds self-heal via the defensive finalize below.
            debug_assert!(
                false,
                "on_worker_exit reached DaemonTerminated arm — Z-3 helper postcondition violated for run_id"
            );
            state.retry_attempts.remove(&run_id);
            state.claimed.remove(&run_id);
            push_finished(state, &entry, &reason, /*final*/ true);
            state.token_totals.remove(&run_id);
            state.last_reported_tokens.remove(&run_id);
            log_diagnostic(
                "on_worker_exit handled unexpected DaemonTerminated via defensive finalize",
                run_id = &run_id,
            );
            return;
        }
    };

    let token = entry.retry_token;                         // reuse — same Run identity
    // NORMATIVE (B22 invariant). `RetryEntry` MUST snapshot enough run
    // metadata to reconstruct a `FinishedRunSummary` in
    // `on_retry_timer`'s `TrackerClass::{Terminal, Neither}` arms and
    // the WorkSource-vanished branch (B22 FIX 1 / FIX 2). The
    // `RunningEntry` is consumed by `state.running.remove` above; if
    // those arms later fire (which they MUST do as the only cascade
    // observer for retry-pending runs — `terminate_and_finish` early-
    // returns on `running.remove → None`, and no §3.4 cascade site
    // iterates `state.retry_attempts`), the ring-write helper will
    // read these snapshot fields. Required fields:
    // {attempt, repo_coordinate, started_at, restart_count, error_message}.
    state.retry_attempts.insert(run_id.clone(), RetryEntry {
        attempt,
        token,
        scheduled_at:    Instant::now() + Duration::from_millis(delay_ms),
        reason:          retry_reason.into(),
        repo_coordinate: entry.run_snapshot.repo_coordinate.clone(),  // spec #4 RetryRow projection
        error_message,                                                // Y-5
        started_at:      entry.started_at,                            // B22 FIX 1 snapshot
        restart_count:   entry.restart_count,                         // B22 FIX 1 snapshot
    });
    schedule_message(
        after_ms = delay_ms,
        msg = Cmd::RetryRun { run_id, token },
    );
}

/// Project a finished `RunningEntry` into a `FinishedRunSummary` and
/// write it to `recent_history_ring` (Y-4). FIFO eviction at insertion
/// when the ring is full. Called on the terminal path of
/// `on_worker_exit` (no retry scheduled) and from
/// `terminate_and_finish` (§3.1). Spec #4 §4.5 reads the ring
/// read-only on the snapshot path.
///
/// Post-B9 lifecycle note: `push_finished` does NOT clean up
/// `state.token_totals` or `state.last_reported_tokens` — that is the
/// caller's responsibility (the post-finalize token-map cleanup
/// postcondition of `terminate_and_finish`; see ring invariant #5
/// in §3.5, Z-3, and the `DaemonTerminated` defensive-finalize arm
/// in `on_worker_exit`). `push_finished` only reads
/// `last_reported_tokens` to project `final_tokens` into the
/// summary; it MUST run before the caller drains those maps.
fn push_finished(
    state: &mut OrchestratorState,
    entry: &RunningEntry,
    reason: &ExitReason,
    _final: bool,
) {
    let summary = FinishedRunSummary {
        run_id:           entry.run_snapshot.id.clone(),
        repo_coordinate:  entry.run_snapshot.repo_coordinate.clone(),
        attempt:          entry.attempt,
        restart_count:    entry.restart_count,             // projected so the ring is the
                                                            // source of truth for
                                                            // `prior_restart_count` (§3.3, §4).
        started_at:       entry.started_at,
        finished_at:      Instant::now(),
        exit_reason:      reason.clone(),
        // Z-8: Surface the engine-attested last-reported tokens, NOT
        // the unattested running totals from `state.token_totals`.
        // `last_reported_tokens` is only ever updated by frames the
        // engine has acknowledged (spec #4 §4.5); reading
        // `token_totals` here would race with in-flight increments
        // from a partially-acked turn. Falls back to default when no
        // turn has yet been acknowledged.
        final_tokens:     state.last_reported_tokens
                              .get(&entry.run_snapshot.id)
                              .cloned()
                              .unwrap_or_default(),
        last_event_text:  None,                            // populated by spec #4 if available
    };
    state.recent_history_ring.push_evicting_oldest(summary);
}

/// B22 FIX 1 / FIX 2 ring-write helper. Symmetric to `push_finished` but
/// projects from a `RetryEntry` rather than a `RunningEntry`. Used by:
///
///   1. WorkSource-vanished branch in `on_retry_timer`.
///   2. `TrackerClass::Terminal` reclassification arm in `on_retry_timer`.
///   3. `TrackerClass::Neither` reclassification arm in `on_retry_timer`.
///   4. `on_shutdown` (§3.6) for retry-pending runs that are not in `state.running`.
///
/// All three are the FIRST AND ONLY observers of the terminal cascade
/// for the affected `run_id`: by the time control reaches them, the
/// `RunningEntry` has already been consumed by `on_worker_exit`'s
/// `state.running.remove` (so `terminate_and_finish` early-returns on
/// `running.remove → None`), and no §3.4 cascade site iterates
/// `state.retry_attempts`. Without this helper, ZERO ring entries are
/// written for these cascades and clients lose the terminal record.
///
/// Single-writer / exactly-once conformance: this helper MUST be invoked at most
/// once per terminalization cause per `run_id`. In `on_retry_timer`, freshness is
/// enforced by I-4 token equality plus draining `state.retry_attempts[run_id]`; in
/// `on_shutdown`, exactly-once is enforced by iterating a snapshot of keys then
/// removing each run's retry entry in the same serial `Cmd` handler (I-13). The caller
/// MUST read `state.last_reported_tokens` (via this helper) BEFORE
/// draining the per-run token maps (ring invariant #5).
fn push_finished_from_retry(
    state: &mut OrchestratorState,
    run_id: &RunId,
    rentry: &RetryEntry,
    cause: TerminationCause,
) {
    let summary = FinishedRunSummary {
        run_id:           run_id.clone(),
        repo_coordinate:  rentry.repo_coordinate.clone(),
        attempt:          rentry.attempt,
        restart_count:    rentry.restart_count,            // B22 FIX 1 snapshot
        started_at:       rentry.started_at,               // B22 FIX 1 snapshot
        finished_at:      Instant::now(),
        exit_reason:      ExitReason::DaemonTerminated { cause },
        // Z-8: engine-attested last-reported tokens, NOT unattested
        // running totals from `state.token_totals`. Symmetric with
        // `push_finished`.
        final_tokens:     state.last_reported_tokens
                              .get(run_id)
                              .cloned()
                              .unwrap_or_default(),
        last_event_text:  None,
    };
    state.recent_history_ring.push_evicting_oldest(summary);
}

async fn on_retry_timer(state: &mut OrchestratorState, run_id: RunId, token: RetryToken) {
    // Freshness check (I-4).
    let rentry = match state.retry_attempts.get(&run_id) {
        Some(r) if r.token == token => r.clone(),
        _ => return,                                       // superseded; drop
    };

    let runs = match state.work_source.fetch_by_ids(&[run_id.clone()]).await {
        Ok(r) => r,
        Err(_) => {
            // Transient — push the timer out by one poll interval.
            schedule_message(
                after_ms = state.config.poll_interval_ms,
                msg = Cmd::RetryRun { run_id, token },
            );
            return;
        }
    };

    if runs.is_empty() {
        // B22 FIX 2: WorkSource-vanished branch. The retry path in
        // `on_worker_exit` does NOT drain `token_totals` /
        // `last_reported_tokens` (those are only drained on terminal
        // finalize via `terminate_and_finish`, §3.1 / ring invariant
        // #5). Because this run vanished from the WorkSource between
        // attempts, no terminal finalize will ever fire for it: no
        // §3.4 cascade site iterates `state.retry_attempts`, and
        // `terminate_and_finish` early-returns on `running.remove →
        // None` (the `RunningEntry` was consumed by `on_worker_exit`).
        // We MUST drain the per-run token maps here AND we MUST write
        // the cascade ring entry here — this is the FIRST AND ONLY
        // observer of this run's terminal cascade. T-3's "exactly one
        // entry per cascade" is preserved precisely because no other
        // site can observe this transition (verified above).
        //
        // Project the ring entry from `rentry` (cloned at L926-L929 from
        // `state.retry_attempts[run_id]`); it carries the snapshot
        // fields populated by `on_worker_exit` (B22 invariant on
        // `RetryEntry`). MUST run BEFORE draining
        // `last_reported_tokens` (ring invariant #5).
        push_finished_from_retry(
            state,
            &run_id,
            &rentry,
            TerminationCause::WorkSourceMissing,
        );
        // No `cleanup_workspace` here: the canonical §3.2 step 5c
        // orphan reaper that drives `WorkSourceMissing` for live runs
        // calls `terminate_and_finish(.., cleanup=false)` (L524–529),
        // so this retry-pending observer of the same cause MUST be
        // symmetric (cleanup=false). The reconciler-driven workspace
        // reclaim in spec #3 §4.5 / §5B owns reclamation for runs
        // that vanish from the WorkSource.
        // `running` was already drained by `on_worker_exit`; the
        // remove below is a defensive no-op that documents the
        // invariant (symmetric with Terminal/Neither arms below).
        drain_per_run_state(state, &run_id);
        log_diagnostic(
            "on_retry_timer finalized cascade ring entry for retry-pending run vanished from WorkSource (B22 FIX 2)",
            run_id = &run_id,
        );
        return;
    }
    let run = runs.into_iter().next().unwrap();

    // Classify the run before doing anything else; daemon-state
    // changes between schedule_message and fire can have moved this
    // run into a terminal or neither class, in which case we MUST
    // drain all maps and not redispatch.
    match state.work_source.classify(&run) {
        TrackerClass::Terminal => {
            // B22 FIX 1. Reclassification race: the run's WorkSource
            // transitioned to `Terminal` between `on_worker_exit`'s
            // retry-pending insertion and this timer's fire.
            // `on_worker_exit` removed the `RunningEntry` from
            // `state.running` at the prior step; this arm is the
            // FIRST AND ONLY observer of the Terminal state for
            // retry-pending runs (no §3.4 cascade site iterates
            // `state.retry_attempts`, and `terminate_and_finish`
            // early-returns on `running.remove → None`). Therefore
            // this arm MUST write the cascade ring entry itself —
            // otherwise ZERO entries are written and clients lose the
            // terminal record.
            //
            // Reconstruct the `FinishedRunSummary` from `rentry`
            // (snapshot fields populated by `on_worker_exit` per the
            // B22 invariant on `RetryEntry`). T-3's "exactly one
            // entry per cascade" is preserved: no other site can
            // observe this transition for this `run_id` (verified
            // above), and any redundant `Cmd::RetryRun` short-circuits
            // at the I-4 freshness check once
            // `state.retry_attempts[run_id]` is drained below. MUST
            // run BEFORE draining `last_reported_tokens` (ring
            // invariant #5).
            // Symmetric to §3.2 step 5b's live `Terminal` arm
            // (L495–502), which drives `terminate_and_finish(..,
            // cleanup=true)` and thus `cleanup_workspace` (§3.1
            // L312–314). Z-3 mandates workspace cleanup on Terminal
            // classification regardless of whether the cascade was
            // observed live or in the retry-pending window; this call
            // restores that symmetry. spec #3 §3.6 owns the cleanup
            // semantics. MUST run BEFORE the ring write so the
            // workspace teardown completes inside this cascade tick
            // (matching `terminate_and_finish`'s ordering at §3.1
            // L311–318: `stop_cascade` → `cleanup_workspace` →
            // `push_finished`).
            cleanup_workspace(&run.id, state).await;        // B23 FIX 1: spec #3 §3.6
            push_finished_from_retry(
                state,
                &run.id,
                &rentry,
                TerminationCause::WorkSourceTerminal,
            );
            // `running` was already drained by `on_worker_exit`; the
            // remove below is a defensive no-op that documents the
            // invariant. Drain the remaining per-run maps:
            drain_per_run_state(state, &run.id);
            log_diagnostic(
                "on_retry_timer finalized cascade ring entry for retry-pending run reclassified Terminal (B22 FIX 1)",
                run_id = &run.id,
            );
            return;
        }
        TrackerClass::Neither => {
            // B22 FIX 1 (symmetric). Reclassification race to
            // `Neither` — same FIRST AND ONLY observer argument as
            // the `Terminal` arm above. The §3.4 cascade for Neither
            // (`WorkSourceLeftQuery`) is normally driven by
            // `terminate_and_finish` from §3.2 step 5b, but for
            // retry-pending runs that helper early-returns on
            // `running.remove → None`. This arm MUST write the
            // cascade ring entry itself.
            //
            // No `cleanup_workspace` here: the canonical §3.2 step 5b
            // live `Neither` arm (L510–517) calls
            // `terminate_and_finish(.., cleanup=false)` because a
            // Run that left the query MAY return (paused /
            // reassigned). This retry-pending observer of the same
            // `WorkSourceLeftQuery` cause MUST be symmetric: do NOT
            // tear down the workspace.
            push_finished_from_retry(
                state,
                &run.id,
                &rentry,
                TerminationCause::WorkSourceLeftQuery,
            );
            // `running` was already drained by `on_worker_exit`;
            // defensive no-op below documents the invariant.
            drain_per_run_state(state, &run.id);
            log_diagnostic(
                "on_retry_timer finalized cascade ring entry for retry-pending run reclassified Neither (B22 FIX 1)",
                run_id = &run.id,
            );
            return;
        }
        TrackerClass::Active => {
            // Fall through to normal retry-timer body.
        }
    }

    if state.running.len() >= state.config.max_concurrency {
        // Slots full — re-queue with explicit reason for the snapshot channel.
        state.retry_attempts.entry(run_id.clone()).and_modify(|r| {
            r.reason = "no available orchestrator slots".into();
        });
        schedule_message(
            after_ms = state.config.poll_interval_ms,
            msg = Cmd::RetryRun { run_id, token },
        );
        return;
    }

    match dispatch_run(state, run).await {
        DispatchResult::Spawned => {
            // dispatch_run cleared `retry_attempts[run_id]` and inserted a
            // fresh RetryToken into the new RunningEntry. It also cleared
            // `dispatch_defer_attempts[run_id]` (Z-9): a successful spawn
            // ends any prior defer streak.
        }
        DispatchResult::Deferred { reason } => {
            // Spawn did not happen (workspace error, spawn error,
            // revalidation race). The run is still `claimed` (per
            // `on_worker_exit`'s contract) but has no live worker. We MUST
            // keep the `RetryEntry` alive and reschedule, otherwise the
            // run is stranded forever (no retry timer, no dispatch).
            //
            // Z-9 livelock guard. If `fetch_by_ids` keeps returning the
            // run but `dispatch_run` keeps deferring (e.g. perpetual
            // `revalidate_raced` because the run is in a stuck
            // closed/reassigned-between-fetch-and-revalidate state),
            // this branch would loop forever: `reconcile_running_runs`
            // (§3.2) only inspects `state.running`, so a claimed-but-
            // never-spawned run is invisible to the reconciler and
            // nothing else releases the claim. Bound the loop with a
            // cumulative defer counter; on the bound abandon the run.
            let attempts = {
                let n = state.dispatch_defer_attempts
                    .entry(run_id.clone()).or_insert(0);
                *n += 1;
                *n
            };
            if attempts >= state.config.max_dispatch_defer_attempts {
                // Abandon. Release every per-run map this code path owns.
                // Drain-only (no ring write) — distinct from the "run
                // vanished from WorkSource" branch above at L943, which
                // ALSO writes a cascade ring entry via
                // `push_finished_from_retry` (B22 FIX 2). Here we abandon
                // without a synthetic terminal cause: the run was either
                // never spawned, or its `RunningEntry` was consumed
                // upstream, and `dispatch_run` has been giving up
                // repeatedly — emitting a `WorkSourceMissing` /
                // `WorkSourceTerminal` ring entry would misrepresent the
                // cause. The diagnostic log is the durable signal.
                // In BOTH this branch and the vanished branch no
                // terminal finalize will fire for `run_id`, so the
                // per-run state that `terminate_and_finish` /
                // `push_finished` would otherwise drain on the terminal
                // arm of `on_worker_exit` MUST be drained here, or it
                // leaks for the lifetime of the daemon process.
                // Specifically:
                //
                //   * `claimed[R1]`, `retry_attempts[R1]`, and
                //     `dispatch_defer_attempts[R1]` are live at entry to
                //     this branch and are dropped here.
                //   * `token_totals[R1]` and `last_reported_tokens[R1]`
                //     MAY be live: a prior worker for R1 may have spawned
                //     and ack'd tokens, then exited Normal/Abnormal.
                //     `on_worker_exit`'s non-terminal arms (Normal /
                //     Abnormal) do NOT drain token maps (see ring
                //     invariant #5 in §4 and the "vanished" branch).
                //     Terminal finalize is the canonical drain site, and
                //     no terminal finalize will fire for R1. We MUST
                //     drain them here. `HashMap::remove` on an absent
                //     key is a no-op, so this is safe in the
                //     never-spawned case (initial dispatch deferred from
                //     the start).
                //
                // We do NOT write to `recent_history_ring` here. A
                // `RetryEntry` IS in scope (cloned as `rentry` at
                // L926-L929) and `push_finished_from_retry` (§3.5) is a
                // canonical ring writer that projects from it — so
                // the writer-availability constraint is no longer
                // the obstacle. The actual obstacle is causal:
                // every `TerminationCause` available
                // (`WorkSourceMissing` / `WorkSourceTerminal` /
                // `WorkSourceLeftQuery`) is a WorkSource transition,
                // and emitting any of them here would invent a
                // terminal cause that did not occur — the run was
                // abandoned by the dispatch-defer livelock guard
                // (Z-9), not by a WorkSource transition. There is
                // no truthful `TerminationCause` for this path, so
                // no ring entry is written. T-3 single-writer is
                // preserved trivially (no writer fires for this
                // `run_id` on this path); the diagnostic log below
                // is the durable signal for this abandonment.
                drain_per_run_state(state, &run_id);
                log_diagnostic(
                    "on_retry_timer dispatch-defer livelock guard fired",
                    run_id   = &run_id,
                    attempts = attempts,
                    reason   = &reason,
                );
                return;
            }
            state.retry_attempts.entry(run_id.clone()).and_modify(|r| {
                r.reason       = "dispatch_deferred".into();
                r.scheduled_at = Instant::now()
                    + Duration::from_millis(state.config.poll_interval_ms);
            });
            schedule_message(
                after_ms = state.config.poll_interval_ms,
                msg = Cmd::RetryRun { run_id, token },
            );
            // The deferral reason (`reason`) is logged for diagnostics;
            // the snapshot channel surfaces "dispatch_deferred" via
            // `RetryEntry.reason`.
        }
    }
}

/// Arms the disconnect lifecycle for a run whose engine RPC has dropped.
/// This is the "already-existing path" referenced from §8.7: the engine
/// RPC handler observes a closed channel, sends `Cmd::EngineDisconnected`
/// on the main mpsc, and this handler runs (I-13). It populates
/// `RunningEntry.{disconnected_since, disconnect_deadline}`, bumps
/// `disconnect_generation` (Z-1) on the None→Some transition, and
/// schedules the expiry `Cmd` carrying both `attempt` and the new
/// `disconnect_gen` snapshot. The freshness key is the **pair**
/// `(attempt, disconnect_gen)`; a fresh dispatch (which bumps `attempt`
/// via `next_attempt_number` in §3.3) drops any prior in-flight
/// `DisconnectTimerExpired`, AND a disconnect→reattach→disconnect cycle
/// on the same `attempt` drops the prior timer via `disconnect_gen`
/// mismatch (the matching `Cmd::Reattach` clears
/// `disconnected_since`/`disconnect_deadline` but does NOT touch
/// `disconnect_generation`, so the next None→Some bump produces a
/// strictly larger gen than any in-flight stale timer carries).
async fn on_engine_disconnected(state: &mut OrchestratorState, run_id: RunId) {
    let entry = match state.running.get_mut(&run_id) {
        Some(e) => e,
        None => return,                                   // already exited / reconciled away
    };
    if entry.disconnected_since.is_some() {
        return;                                           // already armed; idempotent
    }
    let now = Instant::now();
    let timeout = Duration::from_millis(state.config.disconnect_timeout_ms);
    entry.disconnected_since = Some(now);
    entry.disconnect_deadline = Some(now + timeout);
    // Z-1: bump generation on the None→Some transition; capture the new
    // value into the scheduled Cmd. wrapping_add is safe because the
    // freshness check is exact equality, not ordering.
    entry.disconnect_generation = entry.disconnect_generation.wrapping_add(1);
    let attempt = entry.attempt;
    let disconnect_gen = entry.disconnect_generation;
    schedule_message(
        after_ms = state.config.disconnect_timeout_ms,
        msg = Cmd::DisconnectTimerExpired { run_id, attempt, disconnect_gen },
    );
    // The matching `Cmd::Reattach` clears `disconnected_since` /
    // `disconnect_deadline` (spec #2 §4 X-3) but does NOT touch
    // `disconnect_generation`; a stale `DisconnectTimerExpired` that
    // arrives after reattach falls through the freshness check below
    // either by `attempt` mismatch (re-dispatch) or by `disconnect_gen`
    // mismatch (disconnect→reattach→disconnect on same attempt; see T-9).
}

/// Fires `disconnect_timeout_ms` after `Cmd::EngineDisconnected` was
/// received. Routes a still-disconnected run through an Abnormal exit
/// reason with `error = "disconnect_timeout_exceeded"` (Y-5: failure
/// backoff + populated `RetryEntry.error_message`).
///
/// Z-2 ordering: on a fresh fire the handler MUST first call
/// `stop_cascade(reason = "disconnect_timeout_exceeded")` on the live
/// worker (spec #2 §3.3) — this eliminates the overlap between the
/// wedged attempt's worker and the retry's spawn — and THEN routes the
/// synthetic `ExitReason::Abnormal` through `on_worker_exit`. The
/// terminate-then-route ordering is normative; reversing it would let a
/// stuck worker continue producing telemetry against a `RunningEntry`
/// the daemon has already removed.
async fn on_disconnect_timer_expired(
    state: &mut OrchestratorState,
    run_id: RunId,
    attempt: u32,
    disconnect_gen: u64,
) {
    // Z-1 freshness check: ALL THREE of `disconnected_since.is_some()`,
    // `entry.attempt == attempt`, and `entry.disconnect_generation ==
    // disconnect_gen` MUST hold. Mirrors I-4 for retry tokens; cited from
    // §4 `Cmd::DisconnectTimerExpired` docstring and §8.7. Drops cover
    // (a) reattach (disconnected_since cleared), (b) re-dispatch (attempt
    // mismatch), (c) disconnect→reattach→disconnect on the same attempt
    // (disconnect_gen mismatch — see T-9).
    let still_disconnected = match state.running.get(&run_id) {
        Some(e) => e.disconnected_since.is_some()
                   && e.attempt == attempt
                   && e.disconnect_generation == disconnect_gen,
        None => false,
    };
    if !still_disconnected {
        return;                                           // reattached, exited, or re-dispatched — race; drop
    }

    // Z-2 stage 1: stop_cascade on the live worker BEFORE routing through
    // on_worker_exit. spec #2 §3.3 owns the three-stage stop. The
    // synthetic `ExitReason::Abnormal` populates
    // `RetryEntry.error_message = Some("disconnect_timeout_exceeded")`
    // (Y-5) once on_worker_exit runs.
    if let Some(entry) = state.running.get(&run_id) {
        stop_cascade(entry, reason = "disconnect_timeout_exceeded").await;
    }

    // Z-2 stage 2: synthesize the Abnormal exit and route through the
    // standard exit handler. The disconnect-timeout path is
    // Abnormal-with-retry by design (Y-5 backoff applies); no
    // `TerminationCause` variant is emitted because no `DaemonTerminated`
    // cascade fires.
    let reason = ExitReason::Abnormal {
        error: AgentError::from_static("disconnect_timeout_exceeded"),
    };
    on_worker_exit(state, run_id, reason).await;
}
```

**Invariants enforced.**

- The `token` comparison in `on_retry_timer` MUST be exact-equality
  (I-4). Any other check (timestamp, attempt number) is insufficient
  because a Run can have multiple in-flight retry timers across daemon
  restart, hot-reload, or rapid normal/abnormal cycles.
- `claimed` MUST persist across `on_worker_exit`. Removing it there
  would let the next tick double-dispatch in the window between exit
  and retry-timer fire.
- The retry budget is keyed on `RunId`, not on `(RunId, attempt)`
  (Symphony invariant 5). A Run that fails, succeeds-as-continuation,
  fails again uses a single shared backoff schedule. This bounds
  compute on chronically-failing Runs.

**Failure modes.**

- **Daemon crash between worker exit and retry timer fire.** On restart,
  `running` is empty and the WorkSource is the source of truth (I-6).
  Reconcile re-derives the running set; the previously-scheduled retry
  timer is gone (it lived in tokio). The next tick re-dispatches if the
  Run is still active. The lost retry token is fine — no in-flight
  worker is still tagged with it.
- **Stale retry timer fires after a fresh dispatch.** The new dispatch
  inserted a new `RetryToken` into the new `RunningEntry`; the stale
  timer's token does not match `state.retry_attempts.get(&run_id)`'s
  token (or `retry_attempts` no longer contains the Run). Drop. (I-4.)
- **WorkSource unavailable when retry timer fires.** Push the retry
  forward by one `poll_interval_ms`. The retry MUST NOT be cancelled
  on transient WorkSource failure.
- **`max_concurrency` saturated when retry fires.** Re-queue as above;
  set `reason = "no available orchestrator slots"` so the snapshot
  channel can surface this to the runs panel.
- **Stale `Cmd::DisconnectTimerExpired` after reattach or re-dispatch.**
  `on_disconnect_timer_expired` drops unless ALL three checks hold:
  `disconnected_since.is_some() AND e.attempt == attempt AND
  e.disconnect_generation == disconnect_gen`. Any mismatch is a no-op
  (I-13, T-9).

**Disconnect-timer FSM (normative).** The matrix below enumerates every
arrival-time combination of `Cmd::DisconnectTimerExpired { run_id,
attempt: a, disconnect_gen: g }` against the current
`RunningEntry` state. `e.attempt`, `e.disconnect_generation`, and
`e.disconnected_since` denote the live entry's fields at the moment the
`Cmd` is dequeued (I-13 serial processing). Rows are exhaustive over the
truth-table of the three-leg freshness check.

| # | `state.running[run_id]` | `e.attempt == a`? | `e.disconnect_generation == g`? | `e.disconnected_since.is_some()`? | Action (normative) |
|---|---|---|---|---|---|
| D1 | absent (run already exited / reconciled away) | — | — | — | Drop. No mutation. (Pre-condition `still_disconnected = false` via the `None => false` arm.) |
| D2 | present | no (re-dispatch bumped `attempt`) | any | any | Drop. Stale across attempt boundary — mirrors I-4. |
| D3 | present | yes | no (reattach→disconnect cycle bumped `disconnect_generation`) | any | Drop. Stale within the same attempt — Z-1's reason for existing; see T-9. |
| D4 | present | yes | yes | no (reattach cleared `disconnected_since`; no subsequent disconnect) | Drop. Reattach succeeded; the prior arm-cycle's timer is stale. |
| D5 | present | yes | yes | yes (still disconnected at deadline) | **Fire.** Run `stop_cascade(reason = "disconnect_timeout_exceeded")` (Z-2 stage 1) on `e`'s worker, then synthesize `ExitReason::Abnormal { error: "disconnect_timeout_exceeded" }` and route through `on_worker_exit` (Z-2 stage 2). `RetryEntry.error_message = Some("disconnect_timeout_exceeded")` (Y-5). |

Rows D1–D4 are the "drop" partition; only D5 mutates state. The
`Cmd::Reattach` handler (spec #2 §4 X-3) clears
`disconnected_since` and `disconnect_deadline` but MUST NOT touch
`disconnect_generation`; this is what makes D3 distinguishable from D5
when a disconnect→reattach→disconnect cycle occurs on the same
`attempt`.

**Helper definitions (normative).**

```rust
/// `next_attempt_number(state, run_id) -> u32`:
///   Returns the attempt ordinal that the next dispatch of `run_id`
///   will write into the ring. Defined as:
///   - if `state.retry_attempts.contains_key(run_id)`:
///         `state.retry_attempts[run_id].attempt + 1`
///   - else if there is at least one ring entry for `run_id`:
///         max attempt over those entries + 1
///   - else: 1.
///   This helper MUST be called only inside the dispatcher's critical
///   section so that `retry_attempts` cannot be racing with retry-timer
///   consumption.
///
/// `prior_retry_attempt(state, run_id) -> u32`:
///   Returns `state.retry_attempts[run_id].attempt` if a `RetryEntry`
///   exists for `run_id`, else `0`. Used to decide whether the
///   current dispatch is a retry or a fresh attempt.
///
/// `next_retry_attempt(entry: &RunningEntry) -> u32`:
///   Returns the next retry ordinal for the run that just exited,
///   equal to `entry.current_retry_attempt + 1`. The caller MUST pass
///   the `RunningEntry` it just removed from `state.running` (so this
///   helper is pure and observes no shared state). Implementations
///   MUST NOT re-read `state.retry_attempts[run_id]`, since
///   `dispatch_run` clears that entry before the worker spawns.
```

**Numeric defaults caduceus inherits.**

| Constant                          | Default       | Source                                                |
| --------------------------------- | ------------- | ----------------------------------------------------- |
| `CONTINUATION_RETRY_DELAY_MS`     | `1_000`       | `orchestrator.ex:13` `@continuation_retry_delay_ms`   |
| `FAILURE_RETRY_BASE_MS`           | `10_000`      | `orchestrator.ex:14`                                  |
| Failure retry growth              | `* 2^(n-1)`   | `orchestrator.ex:928–939`                             |
| `agent.max_retry_backoff_ms`      | `300_000`     | `orchestrator.ex` config; SPEC §16.6                  |
| `agent.stall_timeout_ms`          | from workflow | SPEC §10.4; defined in spec #6                        |
| Port max line bytes               | `1_048_576`   | `app_server.ex:12`; pinned in spec #2                 |

The Symphony render delay (`@poll_transition_render_delay_ms = 20`) is
**not** ported — see §3 adaptation #2.

### 3.6 `on_shutdown` — drain live runs

Invoked when the main task receives `Cmd::Shutdown`. Force-stops every
live run through the `terminate_and_finish` helper (§3.1) with
`cause = TerminationCause::Shutdown` and `cleanup = false`, so an
operator-restart can resume the workspaces (spec #3 §3.6). This is the
only normative path that emits `TerminationCause::Shutdown`.

```rust
async fn on_shutdown(state: &mut OrchestratorState) {
    // Drain all live runs without retry. Workspace cleanup is deferred
    // (cleanup=false) so an operator-restart can resume.
    let live_run_ids: Vec<RunId> = state.running.keys().cloned().collect();
    for run_id in live_run_ids {
        terminate_and_finish(
            state,
            run_id,
            "daemon_shutdown",
            TerminationCause::Shutdown,
            /*cleanup*/ false,
        ).await;
    }

    // Retry-pending runs are not in `state.running`; they still need a
    // terminal summary before their per-run maps are drained.
    let retry_pending_ids: Vec<RunId> = state.retry_attempts.keys().cloned().collect();
    for run_id in retry_pending_ids {
        let Some(rentry) = state.retry_attempts.get(&run_id).cloned() else { continue };
        push_finished_from_retry(
            state,
            &run_id,
            &rentry,
            TerminationCause::Shutdown,
        );
        drain_per_run_state(state, &run_id);
    }

    // No per-run state may survive shutdown.
    state.retry_attempts.clear();
    state.claimed.clear();
    state.dispatch_defer_attempts.clear();
    state.token_totals.clear();
    state.last_reported_tokens.clear();
    // After `Cmd::Shutdown` is dequeued, the main loop enters shutdown mode.
    // While in shutdown mode it MUST NOT schedule or process new
    // `Cmd::{Tick, RetryRun, DisconnectTimerExpired, WorkflowReloaded,
    // EngineDisconnected, Reattach}` messages; any already-queued instances
    // MUST be dropped unread after `on_shutdown` returns. Mailbox closure is
    // owned by the supervisor layer; this handler MUST be the last command
    // whose effects mutate `OrchestratorState`.
}
```

The function is `async` because `terminate_and_finish` is `async` (it
awaits the spec #2 §3.3 three-stage stop sequence: SIGTERM →
`grace_period_ms` → SIGKILL → reap). Every call site MUST `.await` the
returned future; synchronous shims that block on the runtime are
FORBIDDEN. The composite per-run shutdown budget is `ε₁ +
2·grace_period_ms + ε₂` (spec #2 §3.3, authoritative; see §8.7
*Ownership of timing tunables*).

**Postconditions of `on_shutdown` (normative).** On return:

1. `state.running` is empty.
2. For each run that was in `state.running` **or** `state.retry_attempts` at
   `on_shutdown` entry, exactly one terminal summary with
   `TerminationCause::Shutdown` has been emitted (`push_finished` for live
   runs, `push_finished_from_retry` for retry-pending runs). Because
   `recent_history_ring` is bounded, it contains the newest
   `min(drained_run_count, ring_capacity)` of those summaries; older summaries
   MAY be evicted per §4 ring invariants. Implementations MUST NOT require all
   such summaries to remain present in `recent_history_ring` when
   `N > recent_history_ring_size`.
3. No retry has been scheduled (`retry_attempts` is empty).
4. Workspaces are NOT cleaned up; operator-restart resumes them per spec #3 §3.6.

---

## 4. Data shapes

These are normative shapes (field names, semantics, ordering of mutation).
They are written as Rust type sketches; they do not need to compile as-is.
Field-level types (`u32` vs `u64`, `String` vs `Arc<str>`) are
implementation-defined.

**Token-state discipline (normative).** The orchestrator maintains two
distinct per-run token maps and they MUST NOT be conflated:

- **`token_totals: HashMap<RunId, TokenTotals>`** — the *cumulative
  running totals*. Incremented on **every** `Frame::Tokens` delta
  observed on the inbound event stream (spec #2 §3.2 / spec #4 §4.5),
  including frames that are still in flight w.r.t. engine
  acknowledgement. This map is the orchestrator's working accumulator
  for live "tokens-so-far" diagnostics; it MAY race with partially-
  acked turn boundaries.
- **`last_reported_tokens: HashMap<RunId, TokenTotals>`** — the
  *engine-attested snapshot*. Updated only by frames the engine has
  acknowledged at a turn boundary (spec #4 §4.5). This is the value
  read at terminal-state synthesis (Z-8): `push_finished` (§3.5) MUST
  populate `FinishedRunSummary.final_tokens` from
  `last_reported_tokens`, never from `token_totals`. Reading
  `token_totals` on the terminal path would race with in-flight
  increments from a partially-acked turn and would surface unattested
  values in the recent-history ring.

Both maps are written exclusively by the daemon's main task (I-1);
spec #4 reads them via `Cmd::SnapshotRequest`, which serialises through
the same mailbox (I-13). Implementations MUST NOT add a third token
map; in particular, `RunningEntry` MUST NOT carry its own per-entry
token field that shadows either map.

```rust
struct OrchestratorState {
    config:               Config,
    /// Z-6: Random per-process identity, generated in `start_service`
    /// (§3.1) once at boot. Stable for the lifetime of the daemon
    /// process; regenerated on restart. The orchestrator is the single
    /// owner of `boot_id`. Spec #4 §4.6 fingerprint MUST cite this
    /// field (input #1) and §3.4 SubscribeAck echoes it. NEVER mixed
    /// into a keyed BLAKE3 — see spec #4 §8.5 (Z-18).
    boot_id:              Uuid,
    running:              HashMap<RunId, RunningEntry>,
    claimed:              HashSet<RunId>,
    retry_attempts:       HashMap<RunId, RetryEntry>,
    /// Z-9: cumulative count of consecutive `DispatchResult::Deferred`
    /// outcomes observed by `on_retry_timer` (§3.5) for a given
    /// `run_id`. Bumped once per deferred dispatch attempt
    /// (`revalidate_raced`, `workspace_unavailable`, `spawn_failed`)
    /// and reset to 0 (entry removed) on the first successful
    /// `Spawned` outcome from `dispatch_run` (§3.3). Bounds an
    /// otherwise unbounded retry-loop livelock: when `fetch_by_ids`
    /// keeps returning the run but `dispatch_run` keeps deferring
    /// (e.g. the run is in a perpetual revalidation race because
    /// it became closed/reassigned between fetch and revalidate),
    /// `claimed` and `retry_attempts` would otherwise persist
    /// forever — `reconcile_running_runs` (§3.2) inspects only
    /// `state.running`, so a never-spawned-but-claimed run is
    /// invisible to it. When the counter reaches
    /// `config.max_dispatch_defer_attempts`,
    /// `on_retry_timer` drops `claimed[run_id]`, `retry_attempts[run_id]`,
    /// `dispatch_defer_attempts[run_id]`, `token_totals[run_id]`, and
    /// `last_reported_tokens[run_id]`, emits the livelock-guard diagnostic,
    /// and stops rescheduling. All mutators
    /// (`on_retry_timer`, `dispatch_run`, `on_shutdown`) run on
    /// the orchestrator main task per I-1 single-task ownership.
    dispatch_defer_attempts: HashMap<RunId, u32>,
    last_poll_at:         Option<Instant>,
    next_poll_scheduled:  Option<Instant>,
    token_totals:         HashMap<RunId, TokenTotals>,
    last_reported_tokens: HashMap<RunId, TokenTotals>,
    work_source:          Box<dyn WorkSource>,             // pluggable; spec #6
    events_tx:            mpsc::Sender<Event>,             // snapshot bus; spec #4

    /// Bounded ring buffer of recently-finished Runs, owned exclusively
    /// by the orchestrator main task (Y-4). Inserted into on every
    /// terminalization path that actually observes the final cascade:
    /// `terminate_and_finish` (§3.1, used by the live §3.2 step 5b
    /// cascade sites and the orphan reaper) writes via
    /// `push_finished`; `on_worker_exit`'s `DaemonTerminated`
    /// defensive arm (§3.5) writes via `push_finished` directly; and
    /// the retry-pending cascade-observer arms in `on_retry_timer`
    /// (§3.5 — WorkSource-vanished branch and
    /// `TrackerClass::{Terminal, Neither}` reclassification arms)
    /// write via `push_finished_from_retry`. Capacity is
    /// `config.recent_history_ring_size` (default 32; Z-5). Eviction
    /// is FIFO at insertion time when full. Spec #4 §4.5 reads this
    /// ring (read-only projection) to render the "recent history"
    /// panel and to bound the `subscribe` resync window (spec #4
    /// §3.4 force-resync rule, Y-11).
    ///
    /// **Ring invariants (normative).**
    /// 1. *Single-writer.* Only the daemon's main task writes to the
    ///    ring (I-1). The only writer helpers are `push_finished`
    ///    (§3.5) and `push_finished_from_retry` (§3.5), invoked
    ///    from `terminate_and_finish` (§3.1), `on_worker_exit`'s
    ///    `DaemonTerminated` defensive arm (§3.5), and the three
    ///    (§3.5: WorkSource-vanished branch and
    ///    `TrackerClass::{Terminal, Neither}` arms), plus `on_shutdown` (§3.6)
    ///    for retry-pending runs that never re-enter `state.running`. No worker task
    ///    or RPC handler writes directly. Each of these sites is
    ///    gated such that exactly one fires per `run_id` per
    ///    terminal cascade (T-3).
    /// 2. *Bounded capacity.* The ring's capacity is exactly
    ///    `config.recent_history_ring_size` (default 32; Z-5).
    ///    `BoundedRingBuffer::with_capacity` MUST allocate this once
    ///    at boot and MUST NOT grow at runtime.
    ///    `recent_history_ring_size` MUST be `>= 1`; a value of `0`
    ///    is a `validate_dispatch_config` failure (§3.1).
    /// 3. *Eviction.* When `len() == capacity`, the next
    ///    `push_evicting_oldest` call MUST evict the oldest (FIFO /
    ///    insertion-order-oldest) entry **before** appending the new
    ///    entry. Eviction is observable through subsequent
    ///    `Cmd::SnapshotRequest` reads but emits no event of its own.
    ///    Eviction order is strictly insertion order; reordering by
    ///    `finished_at` is forbidden (the wall vs. monotonic-clock
    ///    distinction in I-7 makes timestamp-based ordering unsafe).
    /// 4. *Read-during-write atomicity.* Reads (spec #4
    ///    `Cmd::SnapshotRequest` handler) are serialised with writes
    ///    via the main mpsc (I-13); a reader therefore observes
    ///    either the pre-push or the post-push-and-eviction state of
    ///    the ring, never an in-progress shift. Implementations
    ///    using a `VecDeque` MUST NOT yield (`.await`) between the
    ///    eviction and the append.
    /// 5. *Post-finalize token-map cleanup.* After `push_finished` (or
    ///    `push_finished_from_retry`) reads
    ///    `state.last_reported_tokens[run_id]` into the emitted
    ///    `FinishedRunSummary.final_tokens`, the writer MUST drain
    ///    `state.token_totals.remove(&run_id)` and
    ///    `state.last_reported_tokens.remove(&run_id)`.
    ///    `terminate_and_finish` (§3.1) performs this drain
    ///    unconditionally; the agent-driven terminal arms in
    ///    `on_worker_exit` (§3.5, `Normal` / `Abnormal` paths that
    ///    do not schedule a retry — currently none under X-11, but
    ///    any future cascade-terminal arm) MUST do the same.
    ///    Additional drain sites in `on_retry_timer` (§3.5) MUST
    ///    also drain both `token_totals` and `last_reported_tokens`
    ///    because no terminal finalize will fire for the affected
    ///    `run_id` from any other site:
    ///    (a) the "run vanished from WorkSource" branch — ALSO writes
    ///        the cascade ring entry via `push_finished_from_retry`
    ///        (B22 FIX 2);
    ///    (b) the Z-9 dispatch-defer abandon branch — drain only, no
    ///        ring write (the run was either never spawned or its
    ///        `RunningEntry` was consumed upstream and the run is
    ///        being abandoned without a synthetic terminal cause);
    ///    (c) the `TrackerClass::{Terminal, Neither}` reclassification
    ///        arms — ALSO write the cascade ring entry via
    ///        `push_finished_from_retry` (B22 FIX 1).
    ///    `on_worker_exit`'s `DaemonTerminated` defensive arm
    ///    (§3.5, L763–794) is also a drain site (and ring writer via
    ///    `push_finished`). The per-run token maps MUST NOT outlive
    ///    the run's `RunningEntry`; the engine-attested tokens are
    ///    preserved by the ring entry, never by the live maps.
    recent_history_ring:  BoundedRingBuffer<FinishedRunSummary>,
}

/// Snapshot of a Run at the moment of terminal exit. Carries enough
/// information for spec #4 §4.5 to render the recent-history panel
/// without joining back to the WorkSource (which may be transiently
/// unreachable).
/// One entry per terminal cascade actually observed by the daemon:
/// live terminal finalize paths write via `push_finished`, while the
/// retry-pending cascade observers in `on_retry_timer` write via
/// `push_finished_from_retry`. Non-terminal exits that merely schedule
/// a retry do NOT push to the ring.
///
/// **Source of truth for `restart_count` (normative).**
/// `recent_history_ring` is the single source of truth for the
/// `restart_count` carried on each new `RunningEntry` at dispatch.
/// `prior_restart_count(state, run_id)` (called from §3.3) scans the
/// ring from newest insertion to oldest insertion and returns the first
/// `FinishedRunSummary` whose `run_id` matches the argument; timestamp
/// ordering MUST NOT be used. It returns:
/// - `summary.restart_count + 1` if
///   `summary.exit_reason == ExitReason::DaemonTerminated { .. }`
///   (the prior cascade-terminal exit is what defines a "restart"),
/// - `summary.restart_count` unchanged if the prior exit was
///   `Normal` or `Abnormal` (those are retry-bound or
///   continuation-bound, not restarts),
/// - `0` if no entry for `run_id` is in the ring (cold-start, or
///   the prior summary was evicted under ring invariant #3).
/// The orchestrator MUST NOT maintain a parallel `restart_counts`
/// map; the ring is authoritative across daemon restarts (the ring
/// itself does not survive a crash, but a fresh-boot dispatch is
/// already a restart by definition and `prior_restart_count` returns
/// `0` correctly).
struct FinishedRunSummary {
    run_id:           RunId,
    repo_coordinate:  RepoCoordinate,
    attempt:          u32,
    restart_count:    u32,                                  // projected from RunningEntry.restart_count
                                                            // at finalize; ring is source of truth for
                                                            // `prior_restart_count` (see docstring above).
    started_at:       Instant,                              // monotonic; project per spec #4 §3.1
    finished_at:      Instant,
    exit_reason:      ExitReason,                           // Normal | Abnormal{error} | DaemonTerminated{cause}
    final_tokens:     TokenTotals,
    last_event_text:  Option<String>,
}

struct RunningEntry {
    handle:                 tokio::task::JoinHandle<ExitReason>,
    exit_rx:                oneshot::Receiver<ExitReason>, // alt path for cooperative exit
    run_snapshot:           Run,                           // last-known WorkSource state
    workspace_path:         PathBuf,
    attempt:                u32,
    retry_token:            RetryToken,
    started_at:             Instant,
    last_activity_at:       Instant,                       // updated on every turn_event

    // ---- Fields required for spec #4 RunRow projection. ----
    // These are populated at dispatch (§3.3) and updated by Cmd handlers.
    // Spec #4 reads them directly to render `RunRow` without a join.

    /// OS pid of the worker, when `spawn_worker` returns a child process
    /// (§8.1). `None` when the worker is an in-process tokio task.
    pid:                    Option<u32>,
    /// Agent-runner session id, set when spec #2 emits `session_started`.
    /// `None` between dispatch and the first session frame.
    session_id:             Option<SessionId>,
    /// Number of times this Run has been re-dispatched within the current
    /// daemon process (across attempts). Carried forward from any prior
    /// `RunningEntry` for the same `RunId` at dispatch time.
    restart_count:          u32,
    /// The `RetryEntry.attempt` that produced this dispatch (0 if the Run
    /// was dispatched directly from the candidate list). Surfaced as
    /// `current_retry_attempt` in spec #4 RunRow.
    current_retry_attempt:  u32,
    /// Highest `runner_seq` observed on any inbound event for this run
    /// (spec #2 §4.4). Used by `Cmd::Reattach` (§4) to validate that the
    /// engine's catch-up cursor has not regressed.
    runner_seq_high_water:  u64,
    /// Set when `Cmd::EngineDisconnected` arms the disconnect lifecycle
    /// (§8.7). `None` while the engine is connected. The transition
    /// `Some -> None` happens on `Cmd::Reattach`.
    disconnected_since:     Option<Instant>,
    /// `disconnected_since + config.disconnect_timeout_ms` when armed
    /// (§8.7). On expiry `Cmd::DisconnectTimerExpired` fires; if the
    /// `attempt` snapshot still matches, `on_disconnect_timer_expired`
    /// (§3.5) routes the run through `on_worker_exit`'s `Abnormal` arm
    /// with `error = "disconnect_timeout_exceeded"` (Y-5).
    disconnect_deadline:    Option<Instant>,
    /// Z-1: Freshness counter paired with `attempt` to fence stale
    /// `Cmd::DisconnectTimerExpired` messages. Bumped (`wrapping_add(1)`)
    /// on every None→Some transition of `disconnected_since` in
    /// `on_engine_disconnected` (§3.5). The matching `Cmd::Reattach`
    /// clears `disconnected_since` and `disconnect_deadline` but does
    /// NOT touch this field, so a disconnect→reattach→disconnect cycle
    /// on the same `attempt` produces a strictly-larger generation than
    /// any in-flight stale timer carries (see T-9). Initialized to 0 at
    /// dispatch (§3.3); preserved across reattach.
    disconnect_generation:  u64,
}

struct RetryEntry {
    attempt:          u32,                                 // 1, 2, 3, …
    token:            RetryToken,                          // freshness check (I-4)
    scheduled_at:     Instant,
    reason:           Cow<'static, str>,                   // "continuation" | "failure" | "dispatch_deferred" | "no available orchestrator slots"
    /// Repo coordinate of the run being retried. Required so spec #4's
    /// `RetryRow` can render `repo_coordinate` without joining back to
    /// the WorkSource (which may be transiently unreachable). Populated
    /// at the moment the RetryEntry is created from the prior
    /// `RunningEntry.run_snapshot.repo_coordinate`.
    repo_coordinate:  RepoCoordinate,
    /// Diagnostic text from the failure that produced this retry.
    /// `Some(error.to_string())` when the prior `ExitReason` was
    /// `Abnormal { error }`; `None` for continuation retries
    /// (`ExitReason::Normal`) and dispatch-deferred / slot-pressure
    /// retries (Y-5). Spec #4 §4.2 `RetryRow.error_message` is a
    /// straight projection of this field; the orchestrator is the
    /// single owner of the string and never re-derives it on the
    /// snapshot path.
    error_message:    Option<String>,
    /// B22 FIX 1 snapshot field. Captured at `RetryEntry` construction
    /// in `on_worker_exit` (§3.5) from the prior `RunningEntry.started_at`.
    /// Required so `on_retry_timer`'s reclassification arms
    /// (`TrackerClass::{Terminal, Neither}`) and the WorkSource-vanished
    /// branch can reconstruct a `FinishedRunSummary` for the cascade
    /// ring write — they are the FIRST AND ONLY observers of the
    /// terminal cascade for retry-pending runs (no §3.4 cascade site
    /// iterates `state.retry_attempts`, and `terminate_and_finish`
    /// early-returns on `state.running.remove → None`). The
    /// `RunningEntry` was consumed by `on_worker_exit`'s
    /// `state.running.remove` at the prior step; `RetryEntry` MUST
    /// carry forward enough metadata to project the ring entry without
    /// it.
    started_at:       Instant,
    /// B22 FIX 1 snapshot field. Carried forward from the prior
    /// `RunningEntry.restart_count` for the same reason as
    /// `started_at` above. Surfaced as `FinishedRunSummary.restart_count`
    /// when the cascade ring write fires from `on_retry_timer`.
    restart_count:    u32,
}

#[derive(Copy, Clone, Eq, PartialEq)]
struct RetryToken(u64);                                    // monotonic; per-process counter

impl RetryToken {
    fn fresh() -> RetryToken { /* atomic fetch_add */ }
}

enum ExitReason {
    Normal,                                                // → continuation retry
    Abnormal { error: AgentError },                        // → backoff retry
    DaemonTerminated { cause: TerminationCause },          // → no retry; cascade-terminal (X-11)
}

/// Why the daemon itself ended a RunAttempt (vs. the agent exiting on its own).
/// Surfaced by the snapshot/exit channel so spec #4 can render a precise reason
/// for `disconnected`/`terminated` rows. No automatic retry is scheduled for
/// `DaemonTerminated`; the next tick re-derives from WorkSource (I-9).
enum TerminationCause {
    /// Z-3: renamed from `StallTimeout` to `Stall` to align with the
    /// terminate-and-finish helper's normative cause list.
    Stall,                    // §3.2 step 5a stall sweep fired
    WorkSourceTerminal,       // §3.2 step 5b classified Terminal
    WorkSourceLeftQuery,      // §3.2 step 5b classified Neither
    WorkSourceMissing,        // §3.2 step 5c orphan reaper
    /// Z-3: emitted when `Cmd::Shutdown` force-stops an active run
    /// without retry. The shutdown handler MUST route each live run
    /// through `terminate_and_finish(state, run_id, "daemon_shutdown",
    /// TerminationCause::Shutdown, /*cleanup*/ false)`; see §3.6.
    Shutdown,
}

`disconnect_timeout_exceeded` remains a v1 `Abnormal` exit (§3.5, §8.7);
no dedicated `TerminationCause` variant is emitted for it in this revision.

enum TrackerClass { Active, Terminal, Neither }

/// Commands processed serially by the daemon's main task (I-1, I-13).
/// The mailbox is `tokio::sync::mpsc::Receiver<Cmd>`; every state-mutating
/// path enumerated in §3 corresponds to one variant. Worker tasks, retry
/// timers, the tick interval, the workflow watcher, and the engine RPC
/// handlers MUST send one of these variants — they MUST NOT mutate
/// `OrchestratorState` directly.
enum Cmd {
    Tick,
    RetryRun        { run_id: RunId, token: RetryToken },
    WorkerExit      { run_id: RunId, reason: ExitReason },
    SnapshotRequest { scope: SnapshotScope, reply: oneshot::Sender<SnapshotResponse> },  // spec #4 §3.2
    WorkflowReloaded,
    EngineDisconnected { run_id: RunId },                  // engine RPC dropped; arms §8.7 timer
    /// Fires when `disconnect_timeout_ms` elapses for a run armed by
    /// `Cmd::EngineDisconnected`. The freshness key is the **pair**
    /// `(attempt, disconnect_gen)` — Z-1. `attempt` carries the
    /// `RunningEntry.attempt` snapshot at the moment the timer was
    /// scheduled; `disconnect_gen` carries the snapshot of
    /// `RunningEntry.disconnect_generation` taken in
    /// `on_engine_disconnected` immediately after the None→Some bump.
    /// `on_disconnect_timer_expired` (§3.5) drops the message if EITHER
    /// `RunningEntry.attempt` no longer matches (race against a fresh
    /// dispatch — I-4) OR `RunningEntry.disconnect_generation` no
    /// longer matches (race against a reattach-then-disconnect cycle
    /// on the same `attempt` — see T-9). On a matched fire, the
    /// handler first calls `stop_cascade(reason =
    /// "disconnect_timeout_exceeded")` (Z-2) on the live worker, then
    /// routes the run through `on_worker_exit`'s `Abnormal` arm with
    /// `error = "disconnect_timeout_exceeded"` (Y-5).
    DisconnectTimerExpired { run_id: RunId, attempt: u32, disconnect_gen: u64 },
    Reattach        { run_id: RunId, runner_seq: u64, session_id: SessionId },  // spec #2 §4 (X-3)
    /// Shutdown command: requests orderly termination of all active runs.
    /// Handler: `on_shutdown` (§3.6). Honors the composite shutdown budget
    /// `ε₁ + 2·grace_period_ms + ε₂` (spec #2 §3.3, authoritative).
    Shutdown,
}

struct Config {
    workflow_path:           PathBuf,
    poll_interval_ms:        u64,
    max_concurrency:         usize,
    disconnect_timeout_ms:   u64,                            // §8.7; default 60_000 ms
    disconnect_retention_ms: u64,                            // §8.7; default 3_600_000 ms (1 h)
    /// Z-9: maximum number of consecutive `DispatchResult::Deferred`
    /// outcomes from `dispatch_run`; on the Nth attempt (where N =
    /// `max_dispatch_defer_attempts`), `on_retry_timer` abandons the
    /// run instead of rescheduling. Guards against perpetual
    /// `revalidate_raced` / `workspace_unavailable` / `spawn_failed`
    /// livelock for runs that are `claimed` but cannot be spawned.
    /// On reaching this bound, `on_retry_timer` drops `claimed[run_id]`,
    /// `retry_attempts[run_id]`, `dispatch_defer_attempts[run_id]`,
    /// `token_totals[run_id]`, and `last_reported_tokens[run_id]`,
    /// emits the livelock-guard diagnostic, and stops rescheduling.
    /// Default `8`. MUST
    /// be `>= 1`. See §3.5 `on_retry_timer` Deferred branch and the
    /// `dispatch_defer_attempts` field on `OrchestratorState`.
    max_dispatch_defer_attempts: u32,                        // default 8
    workspace:               WorkspaceConfig,                // spec #3
    agent:                   AgentConfig,                    // spec #2
    work_source_query:       WorkSourceQuery,                // spec #6
    hooks:                   HookConfig,                     // spec #6
    // …additional top-level groups owned by sibling specs.
}
```

**Disconnect-lifecycle tunables (normative).**

- `disconnect_timeout_ms: u64` — milliseconds after `Cmd::EngineDisconnected`
  before the daemon fires the disconnect cascade
  (`stop_cascade` → `Abnormal` exit with
  `error = "disconnect_timeout_exceeded"`). Default `60_000`. Owned by
  this spec; full algorithm at §8.7 (X-4 first bullet) and §3.5
  (`on_engine_disconnected` / `on_disconnect_timer_expired`).
- `disconnect_retention_ms: u64` — milliseconds after a disconnected run
  reaches a finished state during which its `recent_history_ring` entry
  remains visible to clients so a late-reattaching engine can still
  display the outcome. Default `3_600_000` (1 hour). Owned by this
  spec; full algorithm at §8.7 (X-4 second bullet). This timer governs
  *post-cascade ring visibility only*; it does NOT drive any
  `Disconnected → CleanupPending` state transition (the cascade is
  driven by `disconnect_timeout_ms`, not by retention).

**Mutation discipline.** Only the daemon's main task may mutate
`OrchestratorState` (I-1). All other tasks (workers, retry timers, tick
intervals) communicate by sending `Cmd` enum variants over the main mpsc
channel; the main task receives them via `select!` and applies them
serially. There is no `Mutex<OrchestratorState>`; an implementation that
introduces one violates I-1.

**`RetryToken` semantics.** A `RetryToken` is an opaque, monotonically
increasing per-daemon-process counter. It MUST NOT be derived from a
clock. On daemon restart the counter restarts at 0; this is safe because
no in-flight retry timers survive restart (timers live in tokio).

**`boot_id` lifecycle (normative).** `OrchestratorState.boot_id` is the
daemon's per-process identity nonce. The following lifecycle rules MUST
hold:

1. *Generation point.* `boot_id` is generated **exactly once** in
   `start_service` (§3.1) via `Uuid::new_v4()`, as part of
   `OrchestratorState` allocation **after** `validate_dispatch_config(&config)`
   succeeds and **before** any subscriber (engine, snapshot consumer,
   RPC handler) can bind to the daemon's mailbox. There is no path
   through which `boot_id` is observed in an uninitialised state.
2. *Stability.* `boot_id` is **immutable for the entire lifetime of
   the daemon process**. Workflow hot-reload (§3.2 step 2), WorkSource
   reconfiguration, and reconcile-from-empty MUST NOT regenerate it.
   An implementation that re-derives `boot_id` on hot-reload violates
   the spec #4 §4.6 fingerprint contract (subscribers would observe a
   bogus "daemon restarted" signal).
3. *Restart semantics.* On daemon restart (process exit + respawn), a
   fresh `boot_id` is generated. This is the signal subscribers use to
   detect that in-memory state was lost and that a force-resync is
   required (spec #4 §3.4 / Y-11).
4. *Independence from PID.* `boot_id` is **not** derived from and has
   **no** required relationship to the daemon's OS PID. Two distinct
   daemon processes that happen to receive the same PID (after a
   wraparound on long-lived hosts) MUST surface different `boot_id`s;
   conversely, a single daemon process MUST surface one `boot_id`
   regardless of whether the OS reports its PID consistently. Clients
   MUST key any "is this the same daemon?" check on `boot_id`, not on
   PID.
5. *Single owner.* The orchestrator main task is the sole owner of
   `boot_id`. It is read by spec #4 §4.6 (fingerprint input #1) and
   echoed on §3.4 `SubscribeAck`; it MUST NOT be mixed into a keyed
   BLAKE3 (spec #4 §8.5 / Z-18).

---

## 5. Invariants (normative, MUST)

The daemon MUST maintain the following invariants. Each is followed by
the algorithm steps that enforce it and the test in §6 that exercises it.

### I-1: Single-authority mutation

Only the daemon's main task mutates `OrchestratorState`. Worker tasks,
retry timers, the tick interval, the workflow watcher, and the engine
RPC handlers MUST send `Cmd` variants (§4) on the main mpsc channel and
MUST NOT hold a mutable reference to state across an `await`. The set
of legal `Cmd` variants is closed and enumerated in §4; an
implementation that introduces a side-channel mutation path (e.g. a
shared `Arc<Mutex<OrchestratorState>>`) violates this invariant.

**Symphony parity (cite).** This is a direct port of Symphony's
single-GenServer mailbox model: `orchestrator.ex:52–71` (`init/1`
establishes the GenServer process) and `:119–164` (`handle_info` is the
single dispatch point for DOWN, `:tick`, `:run_poll_cycle`, and retry
messages); SPEC §16.1 ("the orchestrator is a GenServer; all state
transitions happen in `handle_info`") and §16.6 (retry-timer messages
re-enter the same mailbox). On the BEAM, single-mailbox serialisation
is implicit in the GenServer abstraction.

**caduceus-new justification.** Rust/Tokio has no analog of an implicit
process mailbox: spawned tasks share access to anything they capture
unless the data is explicitly sequestered. To preserve Symphony's
property under tokio, caduceus makes the mailbox **explicit**: the
`Cmd` enum (§4 — added in Phase A; see `Cmd::{Tick, RetryRun,
WorkerExit, SnapshotRequest, WorkflowReloaded, EngineDisconnected,
DisconnectTimerExpired, Reattach, Shutdown}`) is the *closed* set of state-mutating messages,
the `tokio::sync::mpsc::Receiver<Cmd>` is the single consumer, and
`OrchestratorState` is owned by the `select!` task that drains it. Any
sender — worker tasks, retry timers, the tick interval, the workflow
watcher, the engine RPC handlers — MUST send a `Cmd`; no other path
mutates state. This is what makes T-3's "no other task wrote to
`OrchestratorState`" assertion mechanically checkable.

*Enforced by:* §3.1 (state owned by main task);
§3.2 / §3.3 / §3.5 (all mutations occur in handlers invoked by the main
loop); §4 (`Cmd` enum is the closed set of state-mutating messages).
*Tests:* T-3.

### I-2: Workspace = identity (registry-owned)

A Run's workspace identity is owned by spec #3's workspace registry,
not by the orchestrator. The orchestrator MUST obtain a workspace by
calling `caduceusd.create_workspace(repo_coordinate, run_id, workflow)`
(spec #3 §3.5) and MUST use the returned `Workspace.path` verbatim as
the worker's `cwd`. The orchestrator MUST NOT compute a workspace path
from `run.identifier` itself. The on-disk layout
(`<workspace_root>/<repo_slug>/<run_id>/`), sanitisation, symlink-escape
rejection, and the per-workspace lock-file are spec #3's responsibility
and MUST be honoured by every dispatch path. At most one agent process
MAY have a given workspace path as its `cwd` at any time.

*Enforced by:* §3.3 (delegates to `caduceusd.create_workspace`); spec
#3 §3.5 + §3.7 lock guard; SPEC §9.1; `app_server.ex:147–173`
`validate_workspace_cwd`.
*Tests:* T-1, T-7 (indirectly, via re-derived workspace claims).

### I-3: Reconcile-then-dispatch ordering within a tick

`reconcile_running_runs` MUST run before the candidate fetch and
dispatch loop in the same tick. If two conflicting truths exist (a
crashed worker not yet observed vs. a `Terminal` WorkSource state), the
WorkSource wins.

*Enforced by:* §3.2 step 1 precedes step 3 in the same async function;
no `await` point between them releases control to other handlers.
*Tests:* T-1, T-4.

### I-4: RetryToken freshness check

A retry timer message carries the `RetryToken` under which it was
scheduled. `on_retry_timer` MUST drop the message if
`state.retry_attempts.get(&run_id).map(|r| r.token) != Some(token)`.
Equality MUST be exact; no timestamp or attempt-number heuristic.

*Enforced by:* §3.5 `on_retry_timer` first check;
`orchestrator.ex:1456`.
*Tests:* T-2.

### I-5: Terminal-state cascade

When the WorkSource classifies a Run as `Terminal`, the daemon MUST:

1. terminate the running attempt (if any) via the agent runner's
   three-stage stop (spec #2),
2. clean up the workspace (spec #3 owns the deletion semantics),
3. remove the Run from `running`, `claimed`, and `retry_attempts`.

There is no "let it finish this turn" path. The cascade also runs at
boot (`startup_terminal_workspace_cleanup`).

*Enforced by:* §3.1 (boot half); §3.2 step 5b (`Terminal` arm).
*Tests:* T-4, T-5.

### I-6 (caduceus-new): Daemon-restart reconciliation

On daemon restart, `OrchestratorState` is empty. The first tick MUST
re-derive the running set from the WorkSource and MUST reap any
in-flight attempts that are not represented in the WorkSource's current
candidate set. The orphan reaper in §3.2 step 5c is the steady-state
mechanism; the boot cleanup in §3.1 is the cold-start mechanism.

In the C-hybrid topology, the engine offers a *reattach* affordance for
runs whose chat scrollback the engine still holds; that affordance is
defined in spec #8 and is independent of this invariant. The daemon
MUST NOT depend on the engine to reconcile state.

*Enforced by:* §3.1 `startup_terminal_workspace_cleanup`; §3.2 step 5c.
*Tests:* T-2, T-7.

### I-7 (caduceus-new): No global clock

All timing in the daemon (poll interval, retry delays, stall sweep)
MUST use the daemon's monotonic clock (`Instant` in Rust). The daemon
MUST NOT depend on wall-clock agreement with the engine, the
WorkSource, or other caduceus instances on the same host.

This is required because (a) the WorkSource may report `updated_at` in
a different timezone or with clock skew, (b) the engine and daemon are
separate processes (C-hybrid) and may see clock adjustments
independently, and (c) cross-agent ordering in the snapshot stream is
explicitly best-effort (spec #4, spec #5).

*Enforced by:* §3 algorithms use `Instant::now()` exclusively;
`scheduled_at` in `RetryEntry` is `Instant`, not `SystemTime`.
*Tests:* T-2 (retry behaviour under wall-clock jump).

### I-8 (caduceus-new): `max_concurrency` is a dispatch ceiling

The daemon MUST NOT dispatch a fresh attempt or fire a retry if doing
so would cause `running.len() > max_concurrency`. Concretely: the
dispatch loop's `state.running.len() >= max_concurrency` break (§3.2
step 4) and the retry-timer's same check (§3.5) are the two
enforcement points.

Hot-reload that lowers `max_concurrency` from `N` to `M < N` while
`running.len() == N` MAY temporarily allow `running.len() >
max_concurrency`; the daemon MUST NOT terminate live attempts to meet
the new ceiling. Instead, no new attempts are admitted until
`running.len() <= M` by natural drain (workers exiting). This is the
"drain interval" referenced from spec #4 (`agents_used` MAY temporarily
exceed `agents_max`).

Transitively-spawned helpers — sub-agents, planner+executor splits,
debate critics — are **not allowed** by this spec. They are a
Pattern-3 concern and are deferred to spec #5 with the recommendation
to defer further to a future spec revision.

*Enforced by:* §3.2 step 4; §3.5.
*Tests:* T-6.

### I-9 (caduceus-new, parity): WorkSource is the queue

The daemon MUST NOT persist its own work queue. Loss of all in-memory
state MUST NOT lose pending work. This is Symphony invariant 1 ("tracker
is the queue", SPEC §7.4) renumbered for caduceus and rephrased in
WorkSource terms; it is the foundation of I-6.

*Enforced by:* `OrchestratorState` has no on-disk shadow; §3.2 step 3
is the sole source of dispatch decisions.
*Tests:* T-7.

### I-13 (caduceus-new): Cmd is the closed mailbox alphabet, processed serially

The set of `Cmd` variants enumerated in §4 is closed — every
state-mutating path in the daemon corresponds to exactly one variant.
The main task processes `Cmd`s strictly in mpsc-receive order: a handler
MUST run to completion (including any internal `await`s) before the
next `Cmd` is dequeued, and a handler MUST NOT `tokio::spawn` a subtask
that mutates `OrchestratorState` after the handler returns — long-running
work MUST send a follow-up `Cmd` instead. This is the structural and
temporal complement to I-1: I-1 constrains *who* mutates state; I-13
constrains *how* state is asked to change and *when* those changes
serialise. Together they make T-3's "no other task wrote to
`OrchestratorState`" assertion mechanically checkable. Disconnect-timer
expiry illustrates the contract: `Cmd::DisconnectTimerExpired` is one
variant in the closed set (§4), and its handler (§3.5) performs the
**three-way** freshness check `(disconnected_since.is_some() && attempt
== current_attempt && disconnect_gen == current_disconnect_generation)`
before any mutation — a stale fire is dropped, mirroring I-4's freshness
discipline for retry tokens. The third leg (`disconnect_gen`) is Z-1:
without it, a disconnect → reattach → disconnect cycle on the *same*
`attempt` would let the original timer's expiry fire spuriously after
the reattach cleared `disconnected_since`/`disconnect_deadline` (see
T-9).

*Enforced by:* §3 algorithms route every cross-task mutation through a
`Cmd`; §4 `enum Cmd` is exhaustive; the main loop is a single `select!`
arm with no state-mutating `tokio::spawn` inside any handler.
*Tests:* T-3.

---

## 6. Test contract

A compliant implementation MUST pass at least the following tests. Each
test cites the invariant it exercises. Test fixtures (mock `WorkSource`,
mock `RunRunner`, virtual time clock) are implementation-defined; the
behavioural contract is normative.

### T-1: Dispatch idempotence under racing WorkSource updates

> Exercises: I-2, I-3.

**Setup.** A mock `WorkSource` returns Run `R1` from `fetch_candidates`.
Between candidate sort and `dispatch_run`'s `revalidate` call, the mock
flips `R1` to non-dispatchable (e.g. status changed to closed).

**Expected.** `dispatch_run` returns without inserting into `running`
or `claimed`. No worker is spawned. No workspace is created. The next
tick observes the Run as `Terminal` (or `Neither`); workspace cleanup
runs only if a workspace exists.

**Variant.** The flip happens between two successive `fetch_candidates`
calls (i.e. across ticks rather than within one). The Run appears in
tick N's candidate list, gets dispatched, and is observed `Terminal` in
tick N+1's reconcile. Cascade fires as I-5.

### T-2: RetryToken freshness across daemon restart

> Exercises: I-4, I-6, I-7.

**Setup.** Daemon dispatches `R1`, worker exits abnormally; daemon
schedules a 10 s backoff retry with `token = T1`. Before the timer
fires, the daemon process is killed (`SIGKILL`) and restarted. The
WorkSource still reports `R1` as active. Restart reconcile re-dispatches
`R1` with a fresh token `T2`.

**Expected.** No double-dispatch. If a stale retry message for `T1`
somehow arrives (e.g. the test injects one via the same `Cmd` channel
shape), `on_retry_timer` drops it because the current
`retry_attempts[R1].token` is `T2` (or `R1` is not in `retry_attempts`
at all because it has been re-dispatched and `retry_attempts.remove`
fired in §3.3).

**Wall-clock variant.** Adjust the system wall-clock backwards by 1 hour
between dispatch and retry-timer fire. The retry MUST still fire on the
correct monotonic-clock schedule (I-7).

**Restart-count variant (Z-3 / ring-as-source-of-truth).** Before the
`SIGKILL`, force the daemon to drive `R1` through
`terminate_and_finish(..., cause = TerminationCause::Shutdown,
...)` so the ring contains a `FinishedRunSummary { run_id = R1,
restart_count = 0, exit_reason = DaemonTerminated{..} }`. Restart the
daemon **without** wiping the ring (in-memory restart, not a process
crash). On reconcile re-dispatch of `R1`, assert
`state.running[R1].restart_count == 1` and that
`prior_restart_count(state, R1)` was sourced from
`state.recent_history_ring` (no parallel `restart_counts` map exists).
A second cascade + re-dispatch MUST yield `restart_count == 2`. Across
a true process crash (ring not preserved), the post-restart dispatch
MUST observe `restart_count == 0` — fresh-boot is itself a restart by
definition.

### T-3: Stall sweep timing

> Exercises: I-1, the stall sweep in §3.2 step 5a.

**Setup.** Dispatch `R1`. Mock agent emits no events. With virtual time
advanced past `agent.stall_timeout_ms`, run one tick.

**Expected.** Reconcile observes `R1` is stalled, calls
`terminate_and_finish(state, R1, "stall_timeout",
TerminationCause::Stall, /*cleanup*/ true)` (Z-3). The helper runs
`stop_cascade(reason = "stall_timeout")`, calls `cleanup_workspace`,
removes from `running` / `claimed` / `retry_attempts`, and pushes a
`FinishedRunSummary` with
`exit_reason = ExitReason::DaemonTerminated { cause:
TerminationCause::Stall }` to `recent_history_ring`. The mutation is
observed on the main task only (I-1): the test asserts no other task
wrote to `OrchestratorState` (e.g. by verifying that the worker
task's exit notification was sent via the mpsc channel, not by
direct mutation), AND that `recent_history_ring` contains **exactly
one** new entry for `R1` across the full cascade — including the
worker's own subsequent exit notification, which MUST hit the
`on_worker_exit` early-return at the `state.running.remove(&run_id) →
None` short-circuit and MUST NOT cause a second `push_finished`
(Z-3 + the §3.5 `DaemonTerminated` defensive-finalize arm; the `DaemonTerminated` arm in
`on_worker_exit` is unreachable under Z-3's single-writer
contract — if ever entered, it `debug_assert!`s in test
builds and self-heals via defensive finalize in production,
in which case the ring entry written there is the FIRST
canonical entry and T-3's "exactly one entry per cascade"
still holds). The test also asserts post-finalize cleanup (ring invariant
#5): `state.token_totals.contains_key(&R1) == false` and
`state.last_reported_tokens.contains_key(&R1) == false` after
`terminate_and_finish` returns.

**Variant.** Mock agent emits a turn_event at `stall_timeout_ms - 1ms`.
`last_activity_at` resets; no termination.

### T-4: Terminal-state cascade with running attempt

> Exercises: I-3, I-5.

**Setup.** Dispatch `R1`. After `R1` is `running`, the mock WorkSource
flips `R1`'s classification to `Terminal`. Run one tick.

**Expected.**

1. Reconcile (step 5b) classifies `R1` as `Terminal`.
2. The worker is terminated via spec #2's three-stage stop.
3. The workspace is deleted (spec #3).
4. `R1` is removed from `running`, `claimed`, and `retry_attempts`.
5. No retry is scheduled.
6. `state.token_totals.contains_key(&R1) == false` and
   `state.last_reported_tokens.contains_key(&R1) == false` once
   `terminate_and_finish` returns (ring invariant #5 — post-finalize
   cleanup; the engine-attested final tokens survive on the new ring
   entry's `final_tokens` field).

The test asserts ordering: the `terminate` call precedes the workspace
deletion, which precedes the state removal. (Out-of-order would leave
a brief window where another tick could observe the workspace without
the `RunningEntry`.)

### T-5: Workspace cleanup-at-boot for terminal-at-boot Runs

> Exercises: I-5 (boot half), I-6.

**Setup.** Pre-populate `workspace.root` with a workspace for `R1`.
Configure the mock WorkSource to classify `R1` as `Terminal` from boot.
Start the daemon.

**Expected.** `startup_terminal_workspace_cleanup` deletes the `R1`
workspace before the first tick. The first tick observes no Runs in
`running` and no candidates in `fetch_candidates`. No worker is
spawned.

**Variant.** Pre-populate workspaces for `R1` (terminal) and `R2`
(active). Boot cleanup deletes `R1` only; first tick dispatches `R2`
and re-uses the existing `R2` workspace.

### T-6: max_concurrency ceiling under burst

> Exercises: I-8.

**Setup.** `config.max_concurrency = 3`. Mock WorkSource returns 10
candidates on the first tick.

**Expected.** Exactly 3 workers spawn. The remaining 7 candidates are
not dispatched; they are not enqueued; they wait for the next tick.

**Variant (retry path).** With 3 running and 7 retry timers pending,
fire 3 retry timers simultaneously. Each `on_retry_timer` observes
`running.len() == 3 >= max_concurrency` and re-queues with
`reason = "no available orchestrator slots"`. No fourth worker spawns.

**Variant (transitive-spawn rejection).** A worker attempts to spawn a
helper agent via the runtime's process API. This is out of scope per
I-8; the test verifies that the agent runner contract (spec #2) does
not expose a primitive for this. (This is a contract test, not a
runtime test.)

### T-7: Reconcile-from-WorkSource on daemon cold-start

> Exercises: I-6, I-9.

**Setup.** Wipe daemon in-memory state. Pre-populate the WorkSource with
3 active Runs (`R1`, `R2`, `R3`) and one terminal Run (`R4`) that has a
leftover workspace on disk. Boot the daemon.

**Expected.**

1. `startup_terminal_workspace_cleanup` deletes `R4`'s workspace.
2. First tick fetches candidates → `[R1, R2, R3]`.
3. Reconcile is a no-op (`running` is empty).
4. Dispatch loop spawns workers for all three (assuming
   `max_concurrency >= 3`).
5. No persistence layer was consulted other than `workspace.root` and
   the WorkSource itself.

The test asserts that no on-disk state file (e.g. a hypothetical
`/var/lib/caduceusd/state.json`) was read. (I-9 forbids such a file.)

### T-8: Orphan reaper across daemon restart

> Exercises: I-6 (steady-state half).

**Setup.** During steady-state operation, `R1` is in `running`. The
WorkSource is reconfigured (test harness) so that `fetch_by_ids([R1])`
returns empty (Run no longer in any query). The daemon is *not*
restarted.

**Expected.** Reconcile step 5c (orphan reaper) terminates the worker
with `reason = "orphan_after_restart"` (or an equivalently-named
reason; the wire-level reason string is defined in spec #4) and
removes from `running` and `claimed`.

### T-9: Disconnect → reattach → disconnect on the same attempt drops the stale timer

> Exercises: I-13 freshness (Z-1), the `disconnect_generation` leg of the
> three-way check.

**Setup.** Dispatch `R1` (attempt = 1). The mock engine RPC drops:
`Cmd::EngineDisconnected { R1 }` is delivered. `on_engine_disconnected`
bumps `RunningEntry.disconnect_generation` from 0 → 1, populates
`disconnected_since`/`disconnect_deadline`, and schedules `Cmd::Dis-
connectTimerExpired { run_id: R1, attempt: 1, disconnect_gen: 1 }` for
`disconnect_timeout_ms` from now (call this Timer1).

Before Timer1 fires, the engine reattaches: `Cmd::Reattach { R1, … }` is
delivered. The Reattach handler clears `disconnected_since` and
`disconnect_deadline` but does NOT touch `disconnect_generation` (per
§3.5 / Z-1).

The engine RPC drops a *second* time: `Cmd::EngineDisconnected { R1 }`
is delivered again. `on_engine_disconnected` observes
`disconnected_since.is_none()`, bumps `disconnect_generation` 1 → 2,
arms a new `disconnected_since`/`disconnect_deadline`, and schedules
Timer2 with `disconnect_gen = 2`.

**Expected.**

- Timer1 (carrying `disconnect_gen = 1`) fires first; the freshness
  check observes `disconnect_generation == 2`, the `disconnect_gen ==
  attempt_disconnect_gen` leg fails, and `on_disconnect_timer_expired`
  drops the message — no `stop_cascade`, no `on_worker_exit`.
- Timer2 (carrying `disconnect_gen = 2`) fires second; the freshness
  check passes. `on_disconnect_timer_expired` calls `stop_cascade(reason
  = "disconnect_timeout_exceeded")` (Z-2) then routes through
  `on_worker_exit`'s `Abnormal` arm with `error =
  "disconnect_timeout_exceeded"` (Y-5).

**Variant (without Z-1).** If Timer1's freshness check were only
`(attempt && disconnected_since.is_some())` — i.e. the pre-Z-1
two-leg form — Timer1 would observe `disconnected_since.is_some() &&
attempt == 1` (both legs satisfied by the second disconnect's state)
and fire spuriously, killing the live worker that the reattach has
just re-anchored. T-9 catches this regression.

---

### T-10: Orchestrator shutdown path conformance

> Exercises: §3.6 `on_shutdown`, §4 `Cmd::Shutdown` docstring, the
> composite shutdown budget owned by spec #2 §3.3, and the §8.7
> *Ownership of timing tunables* cross-reference.

**Setup.** Bring the daemon up with `N ≥ 3` active runs in mixed states:
one streaming tokens (`runner_seq` advancing), one mid-`tool_use_request`
(awaiting daemon response per spec #2 §3.2), and one still in handshake
(spec #2 §3.2, before the `RunnerHello` ack). Dispatch `Cmd::Shutdown`.

**Expected.**

1. `on_shutdown` is `async fn` and is `.await`-ed by the runtime; no
   synchronous shim wraps it (Z6-Q1).
2. Every active run reaches `RunFinished` within `ε₁ +
   2·grace_period_ms + ε₂` wall-clock per run (spec #2 §3.3,
   authoritative; defaults `100 + 2·1000 + 150 = 2250 ms`). No run
   hangs past this composite bound.
3. By I-13 (serial Cmd processing), no `Cmd::Tick` runs between
   `Cmd::Shutdown` dequeue and `on_shutdown` return; therefore §3.2's
   dispatch loop cannot admit new runs during drain. Test asserts
   `state.running` strictly monotonically decreases and `dispatch_run`
   is never called between `Cmd::Shutdown` arrival and final
   `RunFinished`.
4. No workspace is deleted by the shutdown path. Test asserts
   `cleanup_workspace` is NOT invoked for any draining run, per §3.6
   postcondition #4 (`cleanup=false`). Workspaces remain on disk for
   operator-restart resumption per spec #3 §3.6.

**Variant.** If `on_shutdown` were declared as plain `fn` (no `async`,
no `.await` on `terminate_and_finish`), the SIGTERM dispatch would
return before `grace_period_ms` elapses and step (2) would fail for any
run whose worker does not exit on the first SIGTERM. T-10 catches this
regression in tandem with the Z6-Q1 signature lock.

### T-Z9: dispatch-defer livelock guard fires and cleans up

> Exercises: Z-9, the `dispatch_defer_attempts` field and the
> `on_retry_timer` Deferred branch in §3.5, the
> `validate_dispatch_config` bound on `max_dispatch_defer_attempts >= 1`
> from §3.1.

**Setup.** Configure `Config.max_dispatch_defer_attempts = N` (e.g. the
default `8`) and `Config.max_concurrency >= 1`. Seed the orchestrator so
that the retry-path preconditions for `on_retry_timer`'s Deferred branch
hold without depending on the §3 initial dispatch reaching them
organically:

1. **Claim.** `state.claimed.contains(&R1) == true` — the run is on the
   claimed set, mirroring the post-condition `on_worker_exit` would
   leave behind after a non-terminal exit.
2. **Live retry token.** `state.retry_attempts.get(&R1) == Some(entry)`
   with `entry.token == T1` and `entry.attempt >= 1` — i.e. there is a
   `RetryEntry` whose `RetryToken` matches the token carried on the
   in-flight `Cmd::RetryRun { run_id: R1, token: T1 }` message, so the
   freshness check at the top of `on_retry_timer` (§3.5) admits each
   fire instead of dropping it as superseded (I-4). Test harness
   enqueues `Cmd::RetryRun { run_id: R1, token: T1 }` to the
   orchestrator command queue.
3. **Free slot.** `state.running.len() < state.config.max_concurrency`
   — the concurrency-full re-queue branch (§3.5 "Slots full") MUST NOT
   fire; otherwise the handler reschedules without invoking
   `dispatch_run` and the Deferred branch is never evaluated.
4. **Defer counter zeroed.** `state.dispatch_defer_attempts.get(&R1)
   == Some(&0)` at the start of the test (i.e. seeded explicitly to
   `0`, not absent), so the counter transitions `0 → 1 → … → N` are
   observable in order. The harness inserts
   `dispatch_defer_attempts.insert(R1, 0)` before the timer fires.

A mock `WorkSource::fetch_by_ids` repeatedly returns Run `R1` as
dispatchable (i.e. step 2's `runs.is_empty()` branch is NOT taken), and
`dispatch_run` is forced to return `DispatchResult::Deferred` on every
invocation (e.g. by injecting a permanent `revalidate_raced` outcome,
or a `workspace_unavailable` condition that never clears). With the
preconditions above, each retry-timer fire is mechanically guaranteed
to land in the Deferred branch and bump `dispatch_defer_attempts[R1]`
by exactly one. The orchestrator processes `Cmd::RetryRun { run_id: R1, token: T1 }` `N`
times in succession (each fire, on returning `DispatchResult::Deferred`,
re-enqueues the next `Cmd::RetryRun { R1, T1 }` via the
`schedule_message(after_ms = poll_interval_ms, msg = Cmd::RetryRun { … })`
call at the tail of §3.5 `on_retry_timer`'s Deferred branch (~L1212);
the `RetryEntry`'s `token` field is left at `T1` across all N fires —
only `reason` and `scheduled_at` are mutated — so the freshness check
(I-4) admits each fire); each `dispatch_run` invocation deterministically
returns `Deferred` (mocked or stubbed).

**Expected.** On the Nth `Deferred` outcome (i.e. when
`dispatch_defer_attempts[R1]` is incremented to `N`), `on_retry_timer`
takes the abandon branch and:

1. `state.claimed.contains(&R1) == false` — `claimed[R1]` removed.
2. `state.retry_attempts.contains_key(&R1) == false` — retry budget
   removed.
3. `state.dispatch_defer_attempts.contains_key(&R1) == false` — counter
   itself removed (no leak).
4. A diagnostic is emitted with key
   `"on_retry_timer dispatch-defer livelock guard fired"` carrying
   `run_id = R1`, `attempts = N`, and a `reason` field whose value
   is the raw `DispatchResult::Deferred.reason` from the Nth defer
   (e.g., `"revalidate_raced"`, `"workspace_unavailable"`, or
   `"spawn_failed"`). Tests MUST assert the diagnostic key string
   exactly and `attempts == N`; tests MUST NOT assert that `reason`
   contains `"max_dispatch_defer_attempts"` unless the algorithm is
   changed to synthesize that text.
5. `state.token_totals.contains_key(&R1) == false` AND
   `state.last_reported_tokens.contains_key(&R1) == false` — per-run
   token maps drained, mirroring ring invariant #5 (§4) and the
   "run vanished from WorkSource" branch (§3.5). Test variant:
   pre-seed `state.token_totals.insert(R1, TokenTotals { …nonzero… })`
   and `state.last_reported_tokens.insert(R1, …)` before the Nth
   Deferred fire (simulating a prior spawned-and-exited worker that
   ack'd tokens); the abandon branch MUST remove both keys.
6. **No** entry is written to `state.recent_history_ring` for `R1`.
   A `RetryEntry` is available and `push_finished_from_retry` could
   project from it, but this path still MUST NOT write because no
   truthful `TerminationCause` exists: the run was abandoned by the
   dispatch-defer livelock guard, not by a WorkSource transition
   (§3.5 L1176-L1193).
7. No subsequent retry timer is scheduled for `R1`. A subsequent
   `Cmd::Tick` may legitimately re-claim `R1` from the WorkSource as
   a fresh dispatch, which MUST start `dispatch_defer_attempts` from
   zero.

**Config-validation variant.** Construct a `Config` with
`max_dispatch_defer_attempts = 0` and call `start_service`.
`validate_dispatch_config` MUST reject the config at startup; the
`start_service` pseudocode (§3.1) routes the rejection through
`log_diagnostic("config_validation_failed", reason = …)` where `reason`
cites the `>= 1` invariant (e.g.
`"max_dispatch_defer_attempts: must be >= 1"`) and then aborts the
process. Test asserts: (a) the diagnostic is emitted with key
`"config_validation_failed"` and a `reason` string containing both
`max_dispatch_defer_attempts` and `>= 1`; (b) the process exits
non-zero; (c) no `OrchestratorState` is allocated and no other
side-effects (logging configuration, workflow watch, terminal-workspace
cleanup) run.

---

## 7. Out of scope

This section is intentionally non-normative and mirrors §1.2; if any
discrepancy exists, §1.2 is authoritative.

---

## 8. Open questions

These port `symphony-orch-collab.md` Part D items D.2–D.4, D.6, D.7,
D.8 into caduceus terms. D.1 is closed by the C-hybrid topology
decision (locked in §0). D.5 (Pattern-3 shape) is closed by spec #5's
deferral.

### 8.1 (was D.2) Single-process vs multi-process orchestrator

Symphony is a single GenServer (logically a single process). caduceus's
C-hybrid topology fixes the daemon as a single tokio runtime; what
remains open is whether agent runners are *threads of the daemon* or
*child OS processes of the daemon*. Affects: blast radius of crashes,
memory pressure, permission isolation, debuggability.

The agent-runner contract (spec #2) leans toward child OS processes
(JSONL on stdout) but does not strictly require it. The orchestrator
algorithm specified here is indifferent — `spawn_worker` returns a
handle and an exit channel; whether that handle is a `JoinHandle` or
a `Child` is below this spec.

### 8.2 (was D.3) Where does state live across orchestrator restarts?

I-9 says **nowhere persistent**: the WorkSource is the queue and the
daemon cold-starts from it. This works cleanly when the WorkSource is
durable (Linear, GitHub Issues). It works less cleanly when the
WorkSource is a local file in a repo (the local-file adapter): the
file is durable, but *claims* (which Run was being attempted, by which
attempt number, with which retry budget) are not.

Open question: do we need a small on-disk *claim/retry log* — a
write-ahead append-only log of `(RunId, attempt, RetryToken,
scheduled_at, reason)` — that the daemon replays on restart? If yes,
this is an additive extension to §3.1 (`replay_claim_log` between
`startup_terminal_workspace_cleanup` and the first `schedule_tick`)
and a new field in `OrchestratorState`. Keeping it additive means the
core invariants (especially I-9) survive: cold-starting without the
log MUST still work.

### 8.3 (was D.4) Hot-reload semantics for the workflow contract

Symphony watches the workflow file and reloads on change. What does
caduceus do mid-attempt if the workflow changes? Symphony's answer is
"next tick uses new config; in-flight runs use the config they started
with" (the `RunningEntry` snapshots `config` at dispatch time
implicitly). Spec #6 owns the answer; this spec leaves the question
open and only asserts that `validate_dispatch_config` re-runs at the
top of every tick (§3.2 step 2).

A specific sub-question: if the new workflow lowers
`max_concurrency` from `N` to `M < N` and there are currently
`N` running attempts, does the daemon kill `N - M` of them, or does it
let them drain? The proposed answer (to be confirmed by spec #6) is
**drain**: a config change MUST NOT terminate live attempts.

### 8.4 (was D.6) Stall-detection generalisation

Symphony's stall sweep uses one timeout, applied uniformly. caduceus
may want per-tool stall thresholds (a long-running test runner is not
"stalled" the same way a 60 s-silent text generation is). Two shapes
to consider:

- *Single threshold, structured heartbeats.* The agent runner emits
  explicit "I'm working on tool X for N seconds, expect up to M
  seconds total" heartbeats; the orchestrator extends
  `last_activity_at` per-heartbeat-window.
- *Multiple thresholds in the workflow.* `agent.stall_timeout_ms` is
  per-tool, configured in #6; the orchestrator picks the right
  threshold from the active tool name in the most recent
  `turn_event`.

This question is downstream of #2 (which defines the event stream) and
#6 (which owns the workflow schema). The orchestrator algorithm
specified here is parameterised by `stall_timeout_ms` and is forward-
compatible with either shape.

### 8.5 (was D.7) Token-budget enforcement

Symphony tracks tokens but does not enforce a per-Run budget at the
orchestrator layer (it's modelled as agent-side `max_turns`). For
caduceus, should the orchestrator be allowed to terminate a session
that exceeds a per-Run token budget? If yes, this is a new
`ExitReason::TokenBudgetExceeded` and a new reconcile-side check
(`if state.last_reported_tokens[run_id] > budget { terminate }`).

The current spec leaves `last_reported_tokens` in
`OrchestratorState` (§4) for the snapshot channel and for diagnostics,
but does not act on it. A future revision MAY add the check.

### 8.6 (was D.8) Multi-tenancy / multi-user

Symphony assumes one operator. caduceus on a shared dev box / CI
runner might see two users sharing one daemon. Workspace `cwd` safety
(spec #3's symlink-escape check) is the floor; do we need a per-user
identity surface beyond it, or do we say "each user runs their own
daemon instance"?

The C-hybrid topology decision leans toward *per-user daemon*: the
daemon is launched by the user's session (launchd / systemd --user),
binds a Unix socket in the user's runtime dir, and is not shared. This
spec assumes that model; multi-tenant deployment is a future spec.

### 8.7 Reattach contract with the engine

The C-hybrid topology says: on engine crash, the daemon flags affected
runs as `disconnected` and the runs panel offers reattach. The
authoritative wire shape and validation rules for the reattach control
frame (`Reattach { run_id, runner_seq, session_id }`) live in **spec
#2 §4** (X-3); this spec only consumes that contract via
`Cmd::Reattach` (§4) and asserts that *the orchestrator algorithm
itself does not change* on engine disconnect: `running`, `claimed`,
`retry_attempts` all proceed as normal; the daemon does not pause work
because the engine is gone. A worker continues to make progress; its
events stream through `events_tx` to the spec #4 §4.5 snapshot bus and
into the snapshot replay-ring (NOT into the algorithm-side
`recent_history_ring`, whose writers and shape are defined in §3.1
(`terminate_and_finish`) and §3.5 (`push_finished`,
`push_finished_from_retry`) and whose data type lives in §4 — it
carries cascade summaries only, not live event traffic) until the
engine reattaches.

**Two timers govern disconnect lifecycle (X-4); the daemon MUST keep
both distinct.**

- `disconnect_timeout_ms` (default `60_000` ms). Armed by
  `Cmd::EngineDisconnected` (§4) — schedules a
  `Cmd::DisconnectTimerExpired { run_id, attempt, disconnect_gen }`
  on the daemon's timer wheel (Z-1: the freshness key is the **pair**
  `(attempt, disconnect_gen)`; see §3.5, §4 `Cmd` docstring, and T-9).
  On expiry, the daemon FIRST calls `stop_cascade(reason =
  "disconnect_timeout_exceeded")` on the live worker (Z-2; spec #2
  §3.3) and THEN routes the run through `on_worker_exit`'s `Abnormal`
  arm with `error = "disconnect_timeout_exceeded"` (after the
  three-way freshness check; see §3.5
  `on_disconnect_timer_expired`). Failure backoff (Y-5) and the
  `RetryEntry.error_message` producer contract apply. This timer
  governs *when* the cascade fires. The `TerminationCause` enum (§4)
  deliberately omits an `EngineDisconnected` variant: the
  disconnect-timeout exit is `Abnormal-with-retry`, not
  `DaemonTerminated`.
- `disconnect_retention_ms` (default `3_600_000` ms / 1 hour). After
  the disconnect cascade has fired (or the Run otherwise reached a
  finished state with a prior disconnect), the row's
  `recent_history_ring` entry remains visible to clients for this
  window so a late-reattaching engine can still display the run's
  outcome. This timer governs *how long the row remains visible after
  the cascade*.

Spec #4 §I-6 references both timers. Lowering either to zero degrades
UX but is not a correctness violation; raising `disconnect_retention_ms`
is bounded only by the ring's capacity (§4 / §4.5 default 32 finished
Runs).

**Enforcement.** `disconnect_retention_ms` is NOT enforced by this spec's
ring eviction (which is FIFO only). The retention guarantee is owned by
spec #4 §4.5, which MUST implement a time-gated visibility filter on
snapshot reads. If spec #4 does not implement that filter,
`disconnect_retention_ms` has no effect and SHOULD be removed from
`Config`.

**Ownership of timing tunables (cross-spec lock — Z6-A2).** The
shutdown-path tunables `grace_period_ms`, `ε₁`
(`shutdown_enqueue_budget_ms`), and `ε₂` (`sigkill_reap_budget_ms`,
also called `sigkill_reap_budget`) are **defined and owned by spec #2
§3.3 / §5 (I-3, authoritative)**. This spec MUST NOT redefine numeric
defaults for them; if §3.6 / §4 / §6 cite a default, the citation
points back to spec #2 §3.3. The cross-reference table:

| Tunable                             | Default  | Authoritative spec     |
| ----------------------------------- | -------- | ---------------------- |
| `grace_period_ms`                   | 1000 ms  | spec #2 §3.3 / §5 I-3 |
| `shutdown_enqueue_budget_ms` (`ε₁`) | 100 ms   | spec #2 §3.3 / §5 I-3 |
| `sigkill_reap_budget_ms` (`ε₂`)     | 150 ms   | spec #2 §3.3 / §5 I-3 |

To change any of these defaults, amend spec #2 §3.3; this spec's §3.6
and §6 T-10 reference the composite budget `ε₁ + 2·grace_period_ms + ε₂`
symbolically and inherit any spec #2 redefinition automatically. The
disconnect-lifecycle timers above (`disconnect_timeout_ms`,
`disconnect_retention_ms`) are owned by **this** spec — they are not in
the spec #2 §3.3 set.

---

## 9. Cross-references

This spec is referenced by, and references, the following sibling specs.
The C-hybrid topology lock means all of these assume the daemon /
engine split.

| Sibling spec                                        | This spec references it from | It references this spec from           |
| --------------------------------------------------- | ---------------------------- | -------------------------------------- |
| #2 `spec-caduceus-agent-runner-contract.md`         | §3.4, §3.5, §4 (`AgentConfig`, `Cmd::Reattach` shape per #2 §4.5, `runner_seq` per #2 §4.4), §5 I-5 (three-stage stop), §7, §8.7 (reattach contract authority) | §3.4 `run_attempt` contract; I-1 (no direct state mutation); X-3 / X-5 ownership |
| #3 `spec-multi-repo-workspace-model.md`             | §3.1 (boot cleanup), §3.3 (`create_workspace`), §5 I-2, §7 | §5 I-2 (workspace = identity); §3.3 path-construction hook |
| #4 `spec-orchestrator-status-snapshot.md`           | §3 adaptation #2 (no render delay), §4 (`events_tx`), §5 I-1 (event ordering), §7 | §4 (event taxonomy); §8.7 (reattach catch-up snapshot) |
| #5 `spec-caduceus-collab-patterns.md`               | §1.2, §5 I-8, §7, §8 (D.5 closure rationale) | §5 I-8 (hard ceiling); §3.2 (per-Run isolation) |
| #6 `spec-caduceus-workflow-contract.md`             | §2 (WorkSource adapters), §3.1 (`validate_dispatch_config`), §3.2 (`fetch_candidates`, `eligible_for_dispatch`), §4 (`Config`), §5 I-9, §8.3 | §3.1 / §3.2 call surfaces; §8.3 hot-reload semantics |
| #7 `spec-caduceus-runs-panel.md`                    | §1.2, §7 | §3.5 retry reasons; §3.2 reconcile classifications; snapshot stream |
| #8 `spec-caduceus-engine-daemon-protocol.md`        | §1.2, §5 I-6 (reattach independence), §8.7 | §8.7 reattach mechanism; `run_id` join key |

The reference impl (`openai/symphony` @ `58cf97d`) is cited inline at
each algorithm. SPEC § references point into Symphony's `elixir/SPEC.md`
at the same commit.

---

## Appendix Z. Z-namespace registry (normative)

This appendix is the canonical index of all `Z-N` invariants
referenced throughout the four P0 specs (orchestrator algorithm,
agent runner contract, multi-repo workspace model, orchestrator
status snapshot).  Implementations MUST satisfy every Z-N invariant
that appears in their owning spec; this index is a navigation aid,
NOT a substitute for the in-line definitions.

Resolves DAG todo `xs02-z-namespace-registry-appendix`.  Iter-28
backlog item carried forward from spec #4 review B18 fix 8.

| Z-N | Owner | Topic |
|-----|-------|-------|
| Z-1 | spec #1 | `disconnect_generation` freshness key paired with `attempt`; on_reattach MUST NOT mutate (iter-28 #1-6). |
| Z-2 | spec #1, spec #4 | Spec #1 §3.0; cross-cited from spec #4 §3.4 fingerprint stability. |
| Z-3 | spec #1 | Common terminal-path helper (`terminate_and_finish`); §3.5/§3.6/`DaemonTerminated` arm. |
| Z-4 | spec #4 | §4.1 RunRow shape stability between snapshots. |
| Z-5 | spec #1, spec #4 | `RunAttempt` monotonicity caveat (iter-28 #1-2); ring eviction semantics. |
| Z-6 | spec #1, spec #2, spec #4 | `boot_id` scope; per-process random; survives across handlers; replayed in subscribe (iter-28 #4-5). |
| Z-7 | spec #4 | I-7 `SnapshotFingerprint` derivation. |
| Z-8 | spec #1 | Engine-attested last-reported tokens are authoritative; daemon MUST NOT inflate. |
| Z-9 | spec #1, spec #2, spec #4 | Livelock guard counter (`dispatch_defer_attempts`); `max_dispatch_defer_attempts` config field; surfaces in snapshot diagnostics. |
| Z-10 | spec #3 | §3.5 placeholder row insertion ordering (iter-28 #3-2). |
| Z-11 | spec #2 | §4.1 closed v1 event-kind set; cross_run_handoff is reserved out (iter-28 #2-8). |
| Z-12 | spec #3 | §3.6 cleanup short-circuit semantics (iter-28 #3-4): `OrphanedNoSlug` / `OrphanedNoLeaf`. |
| Z-13 | spec #3 | §3.7 caller table — synchronous create vs OrphanReclaim vs cleanup (iter-28 #3-6). |
| Z-14 | spec #3 | I-7 hook-failure rollback contract; `Error::HookFailed` MUST surface unaltered. |
| Z-15 | spec #3 | I-9 hook isolation; default-deny daemon-env inheritance. |
| Z-16 | spec #3 | I-6 derivable `workspace_id` (iter-28 #3-3 + #3-10: BLAKE3-128 keyed, 32-byte key, `safe_run_id` input). |
| Z-17 | spec #4 | §3.4 subscribe outcome algorithm single normative source (iter-28 #4-4). |
| Z-18 | spec #1, spec #4 | Trust boundary capability-scoped Cmd senders (iter-28 #1-1); spec #4 §1.2 local-only transport gate cross-references. |
| Z-19 | spec #3 | §5B.2 OrphanReclaim canonical bypass scope: skip ONLY step 4 (probe) regardless of enqueue source (iter-28 #3-9). |
| Z-20 | spec #4 | §4.5 `RunDetail.exit_reason` invariant: `Some` iff `RunStatus::Finished` (iter-28 #4-2). |
| Z-21 | spec #2 | §3.1 `shell_wrap` fail-closed gate (iter-28 #2-5): runtime input forbidden. |
| Z-22 | spec #2 | §3.3 stop_cascade composite bound `ε₁ + 2·grace_period_ms + ε₂` holds on all platforms (iter-28 #2-4). |
| Z-23 | spec #2 | §4.4 `runner_seq` post-Ok stamp rule (iter-28 #2-7): single canonical statement; stamp ONLY after `forward_to_daemon` returns Ok. |
| Z-24 | spec #2 | §4.1 `seq=0` reserved-value stutter classifier (iter-28 #2-6): fires BEFORE high-water comparison. |
| Z-25 | spec #2 | §4.1 heartbeat-timeout policy (iter-28 #2-3): `protocol_violation` + `stop_cascade(reason="heartbeat_timeout")`. |
| Z-26 | spec #2 | §3.3 stage 3b SIGKILL outcome honesty (iter-28 #2-2): `signal_error` on dispatch fail; `reap_timeout` + `stage="sigkill_timeout"` on reap timeout. |
| Z-27 | spec #2 | §3.2 token reconciliation absolute-mode wins on `turn_end.tokens_at_turn_end` and `exit.final_tokens` (iter-28 #2-1). |
| Z-28 | spec #2 | §3.2 advertised-exit recording on `Exit` frame so the cascade reaper avoids racing the runner's own exit path. |
| Z-29 | spec #4 | §3.4 boot-edge clause (a)/(P) routing when `s_c` absent from replay index (iter-28 #4-3); replay index starts empty on boot. |

**Stability:** Z-numbers MUST NOT be reused after retirement.  When a
Z-N is retired (because the underlying invariant is subsumed or
contradicted by a successor invariant), this table MUST mark it
`RETIRED` with a pointer to the replacement.  No retired Z-numbers
exist as of iter-27.

---

*End of spec.*
