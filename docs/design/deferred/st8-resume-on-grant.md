# ST8 — Resume-on-grant: deferred

**Status**: Deferred (April 2026). Not actionable in current architecture without UX/policy decisions.

## What ST8 asks for

> When `Decision::Deny` fires on a tool call and the orchestrator later emits
> `ScopeExpansionGranted` with an updated `PermissionEnvelope`, re-issue the
> original tool call.

Source: `crates/caduceus-orchestrator/src/agent_harness.rs:2595-2627` (deny
path), `crates/caduceus-permissions/src/envelope.rs:537`
(`PermissionEvent::ScopeExpansionGranted`).

## Why it's not a single-PR change

The mechanical part (storing pending denied calls, swapping the envelope) is
~50 LOC. The **semantic** part is the L-effort blocker:

1. **The deny tool_result is already in the transcript.**
   When preflight denies, the harness appends a synthetic
   `ToolResult::error(...)` to the LLM message history before the grant could
   possibly arrive. The model has typically already produced a follow-up
   message branching away from the denied call. "Re-issuing" can't just
   replay the call — it has to reconcile with what the model already saw.

2. **No grant consumer exists.**
   `PermissionEvent::ScopeExpansionGranted` is defined in the permissions
   crate but has zero producers and zero consumers in the codebase. There's
   no orchestrator path that converts a user "approve expansion" UI gesture
   into a harness mutation.

3. **History-replay policy is undefined.**
   At least three plausible policies, each with different UX implications:
   - **Splice**: rewrite the deny `tool_result` in-place to the success
     result. Mutates conversation history; risks confusing the model on
     subsequent turns and breaks transcript reproducibility.
   - **Synthetic next-turn**: emit a new `assistant` tool_use + `tool` result
     pair with the original input. Doesn't mutate history but produces an
     out-of-band sequence the model didn't request — may be ignored or
     duplicated.
   - **Pause-before-deny-commit**: hold the deny `tool_result` in a pending
     buffer until either the grant arrives (then dispatch + commit success)
     or a timeout fires (then commit deny). Cleanest semantics, but requires
     a synchronous channel from the orchestrator UI back into mid-flight
     fan-out — which the current harness join_set composition doesn't
     support without restructuring.

4. **No UI surface for "approve expansion" lives today.**
   Even if the harness supported resumption, no Zed UI flow surfaces the
   `ScopeExpansionRequested` event as an actionable prompt. The user has
   no way to grant.

## What to do when picking this up

A real ST8 needs three coordinated PRs:

1. **Permissions crate**: add `EnvelopeMutator` interface so callers can
   compose grants safely; document the contract that grants are append-only
   (envelope can only widen) and idempotent.

2. **Orchestrator harness**: pick a history-replay policy (recommend
   "pause-before-deny-commit" with a 5s default timeout — simplest to reason
   about). Add `pending_grants: tokio::sync::Mutex<HashMap<ToolUseId,
   oneshot::Sender<GrantOutcome>>>` and gate the deny-commit on this
   channel. Make the join_set composition aware of grant-arriving
   pre-emption. Surface an `AgentEvent::GrantPending { tool_use_id, deadline
   }` so consumers can show a countdown.

3. **Zed agent UI**: add a "Approve scope expansion" inline picker triggered
   by `ScopeExpansionRequested`, sending the grant over a typed channel that
   produces `ScopeExpansionGranted` on the orchestrator side.

## Related signals

- `crates/caduceus-orchestrator/src/agent_harness.rs:751` — current deny
  doc-comment hints at the future ("the user may grant scope expansion. Do
  not retry; await scope expansion or pick a different action") but no path
  exists.
- `crates/caduceus-permissions/src/envelope.rs:24-29` — envelope module
  docstring mentions the request-grant flow as expected behaviour.

## Tracking

This document supersedes the ST8 entry in the wave-plan decomposition.
Re-open as a fresh strategy-C decomposition when picking up; do not attempt
as a direct (strategy-A) change.
