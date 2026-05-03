# spec-caduceus-collab-patterns

> **Attribution.** © 2025 OpenAI, derivative work under Apache-2.0;
> `openai/symphony` @ `58cf97d`. The collaboration stance, the three-pattern
> taxonomy, and the cross-agent observability rules are ported from
> Symphony's `SPEC.md` §11.5 and the orchestrator deep-dive notes
> (`symphony-orch-collab.md`, Parts B.2/B.3/B.5). Verbatim citations are
> retained so the derivation is auditable. A copy of the Apache-2.0 license
> is at <http://www.apache.org/licenses/LICENSE-2.0>.

- **Status:** Draft
- **Author:** caduceus core
- **Last-updated:** 2026-04-28
- **Priority:** P1 (sibling of specs #1/#2/#3/#4/#6).
- **Scope-locked:** This spec assumes the **C-hybrid topology** decision
  recorded by spec #1: a separate `caduceusd` daemon owns Run dispatch and
  retry state; engine processes own per-thread chat state; they join on
  `run_id`. Within that topology, this spec further locks the v1
  collaboration surface to **Pattern 1 (sequential handoff)** and **Pattern
  2 (concurrent isolated)**. **Pattern 3 (concurrent shared-context
  multi-agent)** is explicitly **NON-SUPPORTED** in v1; §3.3 records the
  forward-compatibility hooks that v1 implementations MUST preserve so the
  pattern can be added later without a breaking change.

---

## 0. Header

This document is the normative specification for how multiple Runs (and the
agents that execute them) are permitted to interact in caduceus v1. Its
purpose is to fix:

1. Which collaboration patterns are supported, and by exactly which
   primitive (so two implementations agree).
2. The cross-agent observability event taxonomy, the per-Run sequencing
   rule, and the no-global-clock invariant — i.e., what an external tool
   tailing the event log can rely on.
3. The forward-compatibility reservations that v1 MUST preserve so a future
   spec can introduce shared-context collaboration without breaking the
   wire shape of v1 Runs, events, or the WorkSource adapter contract.

RFC-2119 keywords (MUST, MUST NOT, SHOULD, SHOULD NOT, MAY) are used in
their RFC-2119 sense.

This spec is a *projection* of three sibling specs and adds no new
algorithms of its own beyond the forward-compat reservations:

- The dispatch loop and its invariants live in spec #1
  (`spec-caduceus-orchestrator-algorithm.md`). Pattern 1 falls out of #1
  §3.2 (`poll_tick`) for free; Pattern 2 is enforced by #1 I-2
  (workspace = identity) and #1 I-8 (`max_concurrency` hard ceiling).
- The JSONL event schema lives in spec #2
  (`spec-caduceus-agent-runner-contract.md`). The event taxonomy in §4.1
  below is a strict re-grouping of #2 §4.1 by *source* (orchestrator vs
  reconciler vs runner vs agent), not a new wire format.
- Workspace identity and isolation primitives live in spec #3
  (`spec-multi-repo-workspace-model.md`). #1 I-2 cites that contract; this
  spec does not redefine it.

If this spec and any of #1/#2/#3/#4/#6 disagree, the sibling spec wins and
this spec is buggy.

---

## 1. Scope

### 1.1 In scope

- **Pattern 1 — sequential handoff.** Normative description; data flow;
  the invariants its supporting primitive (the WorkSource poll loop) must
  satisfy.
- **Pattern 2 — concurrent isolated.** Normative description; the
  isolation invariants it depends on; aggregate accounting rules.
- **Cross-agent observability.** Event taxonomy by source, per-Run
  sequencing, no-global-clock invariant, and the reconstruction property
  that lets external tools tail the event log without daemon RPC.
- **Pattern 3 — explicit non-support.** Definition, rationale for
  deferral, and the forward-compatibility hooks that v1 implementations
  MUST preserve.
- **Cross-Run idempotence.** The minimum guarantees the dispatch loop must
  honour when a single tick observes both Agent A's artifact and a
  candidate dispatch for Agent B.

### 1.2 Out of scope

- Pattern 3 *implementation*. When chosen, it will land as
  `spec-caduceus-shared-context-multi.md`. The choice between the two
  viable shapes (same-process multi-session vs multi-process
  shared-workspace-with-locks; cite `symphony-orch-collab.md:664-673`,
  Part B.3) is deferred to that spec.
- Inter-daemon federation (one `caduceusd` calling another).
  `spec-multi-repo-workspace-model.md` Q8 owns the multi-daemon question.
- Ad-hoc agent-to-agent tool calls (Agent A invoking Agent B as a
  sub-call). This is one of the four shapes Symphony's strict-isolation
  stance rules out by construction
  (cite `symphony-orch-collab.md:608-616`, Part B.2); §3.3 below restates
  that for caduceus.
- ACP-extension vs separate-CLI dispatch (Symphony Part D.1). Closed by
  the C-hybrid topology decision; this spec assumes the daemon.
- Agent runtime sandboxing primitives (different problem space; see
  `spec-m-permissions.md`).

---

## 2. Terms

- **CollabPattern.** A discrete, named shape for how two or more Runs
  (and the agents executing them) are permitted to interact. caduceus v1
  defines exactly two: `SequentialHandoff` and `ConcurrentIsolated`.
- **SequentialHandoff** (Pattern 1). Run A finishes, an artifact lands in
  the WorkSource, and the next `poll_tick` of `caduceusd` dispatches Run B
  against the changed WorkSource state. Communication is durable and
  asynchronous; there is no direct A→B IPC.
- **ConcurrentIsolated** (Pattern 2). N Runs proceed concurrently against
  distinct `(run_id, workspace_path)` pairs. No inter-Run messaging
  primitive exists.
- **SharedContextMulti** (Pattern 3, *deferred*). Two or more agents
  collaborate within a single logical task — pair programming,
  planner+executor, debate critique — sharing scratchpad / blackboard /
  bidirectional event bus. Non-supported in v1; §3.3 enumerates the
  forward-compat reservations.
- **AgentEventSeq.** The monotonic, per-Run sequence number an agent
  attaches to each event it emits. Defined by spec #2 §4.1 (the `seq`
  field of a JSONL line) and §4.2 (the capability handshake).
- **OrchestratorEventSeq.** The monotonic counter the orchestrator
  attaches to every event as it is *received*, regardless of origin. Per
  spec #1 I-7 (No global clock), this counter MUST NOT be derived from a
  clock.
- **ObservabilityChannel.** The append-only event log emitted by
  `caduceusd`. Consumers tail it by `received_at`; the channel does not
  expose orchestrator-internal in-memory state.
- **EventTaxonomy.** The classification of events by *source*
  (Orchestrator / Reconciler / Runner / Agent-passthrough) defined in
  §4.1 below. The wire shape of any individual event is owned by spec #2
  §4.1 — this spec only groups by source.

---

## 3. Normative algorithms

### 3.1 Pattern 1 — sequential handoff (`SequentialHandoff`)

**Definition.** Agent A finishes its Run; on exit it has written some
artifact (PR, tracker comment, file under the WorkSource's view) that
changes the WorkSource state. On the next `poll_tick` of `caduceusd`, the
reconciler observes the new state, classifies it, and (subject to
`max_concurrency`) dispatches Agent B against that changed state.

**Data flow.**

```
                          (durable, asynchronous)
   Agent A             WorkSource (tracker / PR /         Agent B
   ┌──────┐            file under repo-owned-flow)        ┌──────┐
   │ Run  │──── write ─►  ┌────────────────────┐ ── poll ─│ Run  │
   │ a_id │  artifact     │ state_n → state_n+1│  reconc. │ b_id │
   └──┬───┘               └─────────┬──────────┘          └───▲──┘
      │                             │                         │
      │       caduceusd                                       │
      │       ┌────────────────────────────────────────┐      │
      └─exit─►│ poll_tick (#1 §3.2):                    │──── dispatch_run (#1 §3.3)
              │   reconcile_all() ──► classify ──►      │
              │   choose ready set (cap by I-8) ──►     │
              │   dispatch_run(b_id, workspace_b, …)    │
              └────────────────────────────────────────┘
```

**Pseudocode.**

```text
# Tick N: Agent A is running, no artifact yet
poll_tick(N):
    refresh = work_source.fetch()                          # cite #1 §3.2
    classify(refresh)                                      # state_a == "active for A"
    no new dispatch                                        # B not yet eligible

# Agent A exits; on_worker_exit fires (#1 §3.5).
# Between tick N and tick N+1, A's artifact lands in the WorkSource.

# Tick N+1
poll_tick(N+1):
    refresh = work_source.fetch()                          # cite #1 I-9
    classify(refresh)                                      # state_b == "ready for B"
    dispatch_run(run_id=b_id, workspace=workspace_b, …)    # cite #1 §3.3
```

**Cited primitives.**

- The dispatch loop is #1 §3.2 `poll_tick` (`reconcile-then-dispatch`,
  enforced by #1 I-3).
- The WorkSource is the queue (#1 I-9). caduceus v1 has no other queue
  primitive; this is what makes Pattern 1 the only durable, async cross-Run
  channel.
- The exit path that delivers Agent A's terminal state into the next
  tick's reconcile is #1 §3.5 `on_worker_exit` plus the terminal-state
  cascade #1 I-5.

**Invariants implied (cross-references; not redefined here).**

- No direct A→B IPC. Communication is durable (lives in the WorkSource)
  and async (driven by the next `poll_tick`).
- Agent B's first dispatch tick MUST observe Agent A's artifact in the
  WorkSource. This follows from #1 I-3 (reconcile-then-dispatch within a
  tick): the reconcile half of tick N+1 fetches A's now-durable state
  before the dispatch half decides B's eligibility.
- **Race within a tick.** If A's artifact lands during reconcile of the
  same tick that dispatches B, idempotence is guaranteed by #1 I-3 plus
  #1 I-2: B's `(run_id, workspace)` claim is atomic at dispatch, and a
  second observation in the next tick is a no-op revalidation (#1 §3.3
  step `revalidate`).

### 3.2 Pattern 2 — concurrent isolated (`ConcurrentIsolated`)

**Definition.** N Runs proceed concurrently. Each Run has a distinct
`(run_id, workspace_path)` pair. No inter-Run messaging primitive is
exposed by caduceus v1.

**Cited primitives.**

- #1 I-2 (workspace = identity): the `workspace_path` is the unique
  handle for a Run; no two live Runs share a workspace.
- #1 I-8 (`max_concurrency` is a hard ceiling): the dispatch loop never
  spawns more than `config.max_concurrency` concurrent Runs.
- #2 I-1 (`cwd` is the workspace, always): the agent runner refuses to
  start with `cwd` outside the configured workspace, and the symlink-escape
  backstop in #2 T-7 catches escape attempts.

**Invariants implied (cross-references).**

- Each Run sees only its own workspace. Symlink-escape is checked at
  runner start (cite #2 I-1); a Run cannot widen its filesystem view by
  pointing a symlink out of the workspace.
- **No runtime shared-state primitive exists.** The only durable
  cross-Run channel in caduceus v1 is the WorkSource — and the WorkSource
  loop *is* Pattern 1, not Pattern 2. Pattern 2 explicitly forbids using
  the WorkSource as an inter-Run message bus during the lifetime of either
  Run; if two Runs need to coordinate, the design intent is to model that
  as Pattern 1 (Run A finishes, then Run B starts), not as two concurrent
  Runs reading each other's WorkSource state.
- **Per-Run token accounting.** The aggregate token cost of N concurrent
  Runs is the sum of the per-Run costs. caduceus v1 MUST NOT introduce a
  cross-Run token-pool primitive; spec #2 §4.3 owns per-Run token
  reconciliation, and #1 carries `token_totals: HashMap<RunId, …>` (cite
  #1 §4 `OrchestratorState`) keyed strictly by `run_id`.

### 3.3 Pattern 3 — shared-context multi-agent (NON-SUPPORTED in v1)

**Definition.** Two or more agents collaborate within one logical task,
sharing context at runtime — pair programming on the same workspace,
planner+executor staying conversationally coupled, debate-style critique,
shared scratchpad/blackboard architectures, or Agent A directly invoking
Agent B as a sub-call.

This corresponds 1:1 to the four shapes Symphony's strict-isolation stance
rules out by construction (cite `symphony-orch-collab.md:608-616`,
Part B.2: real-time pair-programming, conversationally-coupled
plan-and-execute, shared scratchpad/blackboard, agent-to-agent tool calls).

**Why deferred from v1.**

1. Pattern 3 requires new abstractions caduceus does not have and Symphony
   does not have either — at minimum, a runtime event bus the
   orchestrator owns, plus a serialised-write primitive over a single
   workspace. Adding either before the topology question (D.1, now closed
   by C-hybrid) had stabilised would have been premature.
2. Two viable shapes survive
   (cite `symphony-orch-collab.md:664-673`, Part B.3):
   - **Same process, multiple sessions.** `caduceusd` runs both agents as
     child sessions of one orchestrator instance, mediates I/O through a
     shared event bus, and serialises filesystem writes through a single
     queue. Most expressive; biggest correctness surface.
   - **Multiple processes, shared workspace, exclusive locks.** Two agent
     processes share `cwd`; `caduceusd` owns a per-file or per-subtree
     write lock that they must take. Closer to v1's process model; needs
     an IPC primitive caduceus does not currently expose.

   Both shapes are now feasible under C-hybrid (the daemon is a real
   process and can host either an in-process bus or a lock service).
   Choosing between them is non-trivial and depends on telemetry from
   real Pattern 1 + Pattern 2 deployments. The choice is deferred to a
   future spec.
3. ACP-style cross-agent protocols are still consolidating. Designing a
   bespoke caduceus shared-context primitive now risks being obsoleted by
   a stable cross-agent protocol later.

**Forward-compatibility hooks (MUST be preserved by v1 implementations).**

The intent is that v1 ships a wire shape, an event log shape, and a
WorkSource adapter shape that are all *forward-compatible* with Pattern 3,
so the future spec is additive rather than breaking.

- **`Run.parent_run_id` field.** The `Run` shape (cite spec #4 §4.1)
  reserves a `parent_run_id: Option<RunId>` field. In v1 it MUST be
  emitted as `None` and v1 implementations MUST NOT branch on its value.
  Snapshot consumers MUST round-trip the field unchanged.
- **`cross_run_handoff` event kind.** The event taxonomy in §4.1 below
  reserves the `cross_run_handoff` kind. v1 orchestrators MUST NOT emit
  it; v1 consumers MUST tolerate (skip without erroring) the kind if a
  forward-deployed component emits it. Reservation makes the kind
  unavailable for ad-hoc reuse so a future spec can give it stable
  semantics.
- **WorkSource virtual issues.** The WorkSource adapter contract (spec
  #6) MUST allow the orchestrator to materialise a *virtual* issue that
  does not exist in the upstream tracker — needed for the future
  semantics "the orchestrator created a child task on behalf of a parent
  Run". v1 MUST NOT actually create virtual issues; it MUST preserve the
  shape of the adapter so v2 can. This is a property of the *contract*,
  not the *current adapter implementations*.

These three reservations are the minimum set; §8 records the open
question of whether `child_run_ids` is also needed.

---

## 4. Cross-agent observability

### 4.1 Event taxonomy by source

The wire shape of every event listed below is owned by spec #2 §4.1 (the
JSONL line schema). This section *re-groups* those events by emission
source so external tools tailing the event log know who to trust for
which payload.

| Source                              | Kind               | Payload fields                                          |
|-------------------------------------|--------------------|---------------------------------------------------------|
| Orchestrator                        | `dispatch`         | `run_id`, `workspace_id`, `attempt`, `started_at`       |
| Orchestrator                        | `exit`             | `run_id`, `exit_reason`, `attempt`                      |
| Orchestrator                        | `retry_scheduled`  | `run_id`, `in_ms`, `error`                              |
| Reconciler                          | `tracker_refresh`  | `run_id`, `classify`, `snapshot_diff`                   |
| Reconciler                          | `worker_stalled`   | `run_id`, `last_activity_at`, `threshold`               |
| Runner                              | `turn_start`       | `run_id`, `turn`, `prompt_hash`                         |
| Runner                              | `turn_end`         | `run_id`, `turn`, `summary`, `tokens`                   |
| Agent (passthrough via runner)      | `turn_event`       | `run_id`, `turn`, `payload`                             |
| Agent (handshake)                   | `capabilities`     | `run_id`, `capability_set`                              |
| *(reserved for v2; MUST NOT emit)*  | `cross_run_handoff`| reserved; see §3.3                                      |

Cite Symphony deep-dive `symphony-orch-collab.md:728-740` (Part B.5) for
the original tabulation; the caduceus version splits Symphony's
"Orchestrator" column into Orchestrator vs Reconciler so the dispatch
authority (the main task; #1 I-1) and the polling authority (the
reconcile half of `poll_tick`; #1 §3.2) are distinguishable in the log.

### 4.2 Per-Run sequencing

Each agent assigns a monotonic `seq` to events within its session
(per-Run). This is the `seq` field of the JSONL line; the capability
handshake (cite #2 §4.2) MAY advertise the agent's commitment to the
contract.

The orchestrator wraps every event with `(received_at, orchestrator_seq)`
on ingest. `orchestrator_seq` is the `OrchestratorEventSeq` defined in
§2: a monotonic per-daemon-process counter, NOT clock-derived (see #1
I-7).

**Gap detection.** The runner MUST detect non-monotonic agent `seq` (a
gap, a regression, or a duplicate) and surface the violation as
`error{kind:"seq_gap"}`. The runner MUST NOT silently renumber. This is
T-4 in §6.

### 4.3 No global clock invariant

There is **no global counter that orders events across Runs** (cite
`symphony-orch-collab.md:741-745`, Part B.5). Cross-Run ordering is
reconstructed by consumers from `received_at` only, on a best-effort
basis.

This is normative for two reasons:

1. It is consistent with #1 I-7 (no global clock for retry scheduling).
   Introducing a synthetic cross-Run counter inside `caduceusd` would
   create a second authority over time and a second source of skew under
   restart.
2. The snapshot fingerprint specified by spec #4 (cite spec #4 I-7)
   hashes per-Run state. Per-Run hashing is only sound if cross-Run
   ordering is *not* part of the snapshot identity — i.e., if two
   reorderings of the same set of cross-Run events produce the same
   fingerprint. A global counter would silently break this property.

The orchestrator MUST NOT introduce a synthetic global counter that orders
events across Runs (other than `received_at`, which is wall-clock and
explicitly best-effort).

### 4.4 Reconstruction property

The dashboard (and any external tool) MUST be able to reconstruct, purely
from the event log + WorkSource state:

- the running set,
- the claimed set, and
- the retry schedule.

The orchestrator's in-memory state MUST NOT be a required reconstruction
source. This is the property that lets engines, dashboards, and external
tools tail the log without RPC into `caduceusd`. It is the cross-agent
projection of #1 I-6 (daemon-restart reconciliation): if `caduceusd` can
cold-start from event log + WorkSource state, then by construction so can
any read-only consumer.

Implementations MUST emit *enough* events that no observable orchestrator
decision is invisible in the log. In particular: every dispatch, every
exit, every `retry_scheduled`, and every terminal-state cascade step MUST
appear.

---

## 5. Invariants (MUST)

The daemon MUST maintain the following invariants. Each is followed by the
test in §6 that exercises it.

### I-1 — Pattern 1 dispatch ordering

For any Pattern 1 handoff (Agent A → Agent B), Agent B's `dispatch_run`
call MUST observe a `reconcile_result` that includes Agent A's
WorkSource state change. Equivalently: Agent B is never dispatched
against a WorkSource snapshot that pre-dates Agent A's artifact.

This is the cross-Run face of #1 I-3 (reconcile-then-dispatch within a
tick).

*Tests:* T-1.

### I-2 — Pattern 2 isolation

A Run MAY NOT read or write outside its own workspace. The runner
enforces this via `cwd` + the symlink-escape backstop (cite #2 I-1).
caduceus v1 MUST NOT expose any other primitive that would let two live
Runs share filesystem state.

*Tests:* T-2.

### I-3 — No agent-to-agent direct IPC

caduceus v1 exposes no primitive for direct agent-to-agent communication
(no shared bus, no shared scratchpad, no agent-as-tool invocation). The
forward-compat reservations in §3.3 (`parent_run_id`,
`cross_run_handoff`, virtual WorkSource issues) are *opt-in* for future
specs and MUST be inert in v1.

*Tests:* T-3.

### I-4 — Per-Run sequencing is monotonic

Within one Run's session, the agent's `seq` field MUST be strictly
monotonic non-decreasing. The runner MUST detect any gap, regression, or
duplicate and emit `error{kind:"seq_gap"}`. The runner MUST NOT
renumber.

*Tests:* T-4.

### I-5 — No global clock

The orchestrator MUST NOT introduce a synthetic global counter that
orders events across Runs. The only cross-Run timestamp on the
ObservabilityChannel is `received_at`, which is wall-clock and
explicitly best-effort. (Restates the cross-Run face of #1 I-7.)

*Tests:* T-5.

### I-6 — Reconstruction from event log + WorkSource

The event log + WorkSource state SHOULD be sufficient to reconstruct
orchestrator state on cold start. This is the basis for #1 §3.2's
reconcile-on-cold-start path and the data contract that lets read-only
consumers reconstruct the running set, claimed set, and retry schedule
without RPC into `caduceusd`.

`SHOULD` rather than `MUST` because cold-start reattachment to *running*
agent processes is a separate problem owned by spec #1 I-6 and is
exercised by #1 T-7; this spec only requires that the *log shape* admit
the reconstruction.

*Tests:* T-6, with overlap into #1 T-7.

### I-7 — Token isolation

Run A's token totals MUST never be charged against Run B's accounting,
and vice versa. `OrchestratorState.token_totals` (cite #1 §4) is keyed
strictly by `run_id`. The aggregate of N concurrent Runs is the sum of
the per-Run totals.

*Tests:* implicit; surfaces as a contract on #1 §4 and #2 §4.3.

### I-8 — Forward-compat reservation: `Run.parent_run_id`

The `Run` shape carries a `parent_run_id: Option<RunId>` field (cite
spec #4 §4.1). v1 implementations MUST accept its presence (always
`None`) and MUST round-trip it through snapshots and the event log
unchanged. v1 implementations MUST NOT depend on its value, branch on
it, or reject Runs that carry a non-`None` value if a forward-deployed
component emits one.

*Tests:* covered indirectly by spec #4's snapshot round-trip tests.

---

## 6. Test contract

### T-1 — Pattern 1 end-to-end

> Exercises: I-1; touches #1 I-3, #1 I-9.

Stand up `caduceusd` with a WorkSource adapter whose state can be
controlled by the test harness. Dispatch Agent A; on Agent A's exit,
have the harness mutate the WorkSource into the "ready for B" state.
Assert that the next `poll_tick` calls `dispatch_run` for `b_id` and
that the `reconcile_result` consumed by that dispatch reflects A's
state change.

### T-2 — Pattern 2 isolation

> Exercises: I-2; touches #2 I-1.

Spawn two Runs concurrently against adjacent workspaces
(`/tmp/ws-a`, `/tmp/ws-b` — paths are illustrative; use the test
harness's workspace fixture). From Run A, attempt to read or write a
file under Run B's workspace. The runner MUST refuse the operation
(symlink resolution out of `cwd` is rejected by #2 I-1's backstop).
Cross-read attempts MUST surface as agent diagnostic events, not as
silent successes.

### T-3 — No agent-to-agent IPC

> Exercises: I-3.

Inject (via a synthetic agent harness) an event that purports to "send
to another run" — e.g., a JSONL line carrying a `target_run_id`
distinct from the runner's own Run, or a `cross_run_handoff` event in
v1. The runner MUST reject the event as a protocol error and MUST NOT
deliver it to any other Run. The orchestrator MUST NOT have a delivery
path for such an event.

### T-4 — Per-agent `seq` monotonicity

> Exercises: I-4.

Spawn an agent harness that emits events with a deliberate gap (1, 2,
4) or regression (1, 2, 1). The runner MUST emit an
`error{kind:"seq_gap"}` event for the offending boundary, and MUST NOT
renumber the agent's events. Downstream consumers MUST observe the
agent's original `seq` values, not a runner-rewritten sequence.

### T-5 — No global clock under reordered cross-Run arrival

> Exercises: I-5; touches spec #4 I-7.

Drive two concurrent Runs A and B emitting interleaved events. Compute
the snapshot fingerprint (cite spec #4 I-7) under (a) the original
arrival order and (b) a permutation that reorders cross-Run event
arrivals while preserving each Run's intra-Run order. The fingerprints
MUST be equal.

### T-6 — Reconstruction across orchestrator restart

> Exercises: I-6; overlaps with #1 T-7.

Mid-Run, kill `caduceusd`. Restart. Assert that:

1. The running set, claimed set, and retry schedule observed by an
   external consumer (tailing the event log + reading WorkSource state)
   match the post-restart orchestrator's view.
2. No state was reconstructed from `caduceusd`'s in-memory state, and
   no on-disk daemon scheduler database was read (forbidden by #1 I-9).

This test is structurally a superset of #1 T-7 with the additional
assertion that an external consumer's view is consistent.

---

## 7. Out of scope

- Pattern 3 implementation. Will land as
  `spec-caduceus-shared-context-multi.md` when chosen.
- Inter-daemon federation (Symphony Part D analogue;
  `spec-multi-repo-workspace-model.md` Q8).
- ACP-extension vs separate-CLI dispatch (Symphony Part D.1) — already
  settled by C-hybrid; this spec assumes the daemon.
- Agent runtime sandboxing primitives (different problem space; see
  `spec-m-permissions.md`).
- Hot-reload of the workflow contract during a Pattern 1 chain
  (Symphony Part D.4).
- Cross-Run "lessons learned" / a daemon-owned summary store readable by
  future Runs. Flagged in §8.

---

## 8. Open questions

### 8.1 Pattern 3 shape — same-process vs multi-process

When v2 supports Pattern 3, which shape (same-process multi-session vs
multi-process shared-workspace-with-locks; cite
`symphony-orch-collab.md:664-673`, Part B.3 / Part D.5)? Both are
feasible under C-hybrid. The decision likely depends on telemetry from
real Pattern 1 + Pattern 2 deployments; defer until that telemetry
exists.

### 8.2 Sufficiency of `parent_run_id` as the only forward-compat hook

Is `parent_run_id` enough, or do we also need a reserved
`child_run_ids: Vec<RunId>` field on `Run`? The argument for adding it:
some Pattern 3 shapes (planner+executor) want the parent to enumerate
its children directly. The argument against: snapshot round-trip cost
plus the risk of v1 implementations populating it accidentally. Defer;
revisit when the v2 spec is drafted.

### 8.3 Cross-Run "lessons learned" store

Should there be a daemon-owned summary store that future Runs can read
on dispatch (a kind of long-term memory shared across Runs)? Out of
scope here, but flagging: this is a third durable channel beyond
WorkSource and the event log, and would interact non-trivially with
I-3 (no agent-to-agent IPC) — a "lessons learned" store *is*
indirectly an inter-Run channel, just one with a slow, batched, opt-in
shape.

### 8.4 Pattern-1 cycles

Pattern 1 silently allows a cycle: Agent A finishes → triggers Agent B
→ Agent B finishes → triggers Agent A again on a different state.
Should v1 cap this at the WorkSource level (e.g., a per-`run_id`
attempt cap that survives WorkSource state changes), or trust the
WorkSource adapter (spec #6) to enforce it? v1 currently does not cap;
revisit when we have evidence from real workloads.

---

## 9. Cross-references

- **spec #1 (`spec-caduceus-orchestrator-algorithm.md`).** I-2
  (workspace = identity) and I-8 (`max_concurrency` hard ceiling) are
  the enforcement primitives for Pattern 2. I-9 (WorkSource is the
  queue) is the foundation of Pattern 1. I-3 (reconcile-then-dispatch)
  underwrites I-1 of this spec. I-7 (no global clock for retries)
  underwrites I-5 of this spec.
- **spec #2 (`spec-caduceus-agent-runner-contract.md`).** §4.1 owns
  the JSONL line schema; §4 in this spec is a re-grouping of that
  schema by source. §4.2 (capability handshake) is the
  agent-side contract for `seq` monotonicity (I-4 here). I-1
  (`cwd` = workspace) is the runner backstop for I-2 here.
- **spec #3 (`spec-multi-repo-workspace-model.md`).** Owns
  `workspace_path` semantics, including symlink-escape. Pattern 2's
  isolation invariant (I-2 here) is its read/write loop.
- **spec #4 (snapshot spec).** §4.1 owns the `Run` shape, including
  `parent_run_id` (I-8 here). I-7 (snapshot fingerprint) depends on
  the no-global-clock invariant (I-5 here); `LastEventSummary` is a
  projection of the event log defined in §4.1 here.
- **spec #6 (`spec-caduceus-worksource-adapter`).** WorkSource adapter
  contract; Pattern 1 is its read/write loop. The forward-compat hook
  for virtual WorkSource issues (§3.3 here) is a property of that
  contract.
- **Symphony deep-dive (`symphony-orch-collab.md`).** Part B.2
  (lines 591-638) — strict-isolation rationale and what it rules out.
  Part B.3 (lines 640-679) — the three-pattern taxonomy and the two
  Pattern-3 shapes. Part B.5 (lines 721-752) — original event taxonomy
  and the no-global-clock stance.
