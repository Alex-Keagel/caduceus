# ST9 — Untestable scenarios (deferred until upstream fixes ship)

**Status**: Deferred (April 2026). Documents two ST9 scenarios that have no
shippable code to assert against today.

## Scope of ST9

ST9 enumerated four integration-tier scenarios:

| # | Scenario | Status |
|---|---|---|
| 1 | 30-turn thread with first-user pin survival | ✅ shipped (caduceus `tests/st9_pin_compaction_integration.rs`) |
| 2 | ContextCompacted event fired and consumed | ✅ shipped (covered by scenario 1's compaction-summary contract test + zed-side `context_compacted_is_notice` unit test) |
| 3 | Sub-agent timeout routes via different vendor | ❌ **deferred — feature does not exist** |
| 4 | Copilot Chat visible from cold-start unauth | ❌ **deferred — fix not yet shipped** |

## Scenario 3 — vendor fallback on sub-agent timeout

### Why deferred

`crates/agent/src/tools/spawn_agent_tool.rs` classifies timeouts as
`SubAgentFailure::Timeout(TimeoutFailure)` and returns it to the caller. There
is **no retry mechanism** that re-dispatches the failed call with a different
vendor's model. A grep across `crates/agent/src/` and
`crates/caduceus_bridge/src/` for `retry`, `fallback`, `alternate_model`, and
`backup_model` returns zero hits.

The ST9 description ("routes via different vendor") describes desired future
behaviour, not current behaviour. Writing a regression test today would
either:

* Assert the current behaviour (timeout produces no fallback) — which is the
  **opposite** of the desired behaviour and would block any future
  implementation.
* Assert the desired behaviour against absent code — guaranteed-failing test,
  not a regression test.

Neither serves the purpose of the integration tier.

### What's needed to pick this up

1. Design decision: which axis is "different vendor" along — model id, vendor
   tag, or arbitrary user-configured fallback list?
2. Failure-class predicate: timeout always falls back, but does
   `ModelRefusal` also fall back? `ProviderError` rate-limits? The
   classification at `spawn_agent_tool.rs:62-110` is the right place to start.
3. Loop-prevention budget: a fallback chain must terminate; cap at N attempts
   with explicit `SubAgentFailure::FallbackExhausted` to surface the giving-up
   moment to the user.
4. PB6 behaviour-rule update: orchestrator must know when fallback ran so it
   can mention vendor diversity in its synthesis (and not double-count
   "agent A succeeded" when A was the fallback).

When that lands, the integration test trivially fits: spawn an agent against
a model that times out fast, assert the result includes `fallback_used: true`
with a different vendor in the trace.

## Scenario 4 — Copilot Chat visible from cold-start unauth

### Why deferred

The fix this scenario regression-tests is **ST1b**, currently W4-pending. From
the wave-plan:

> **ST1b scope**: at `crates/agent_ui/src/language_model_selector.rs:489`,
> remove `.filter(|provider| provider.auth_state(cx).can_provide_models())`.
> Add `RowDescriptor::Provider { auth_state, action }` vs `RowDescriptor::Model
> { ... }`. Render unauth provider rows with `[Sign In]` badge button.

`language_model_selector.rs` line 489 today still applies the filter, so
unauthenticated providers are invisible in the picker. A test asserting "after
cold-start unauth, Copilot Chat row is rendered" would fail on main, then
pass once ST1b lands. That's a per-feature acceptance test, not a regression
guard — it belongs in the ST1b PR.

ST1b also needs `ST1b-prereq` (a GPUI render-snapshot harness) to land first.
Until both ship:

* No surface to assert against.
* No harness to assert through.

### What's needed to pick this up

This scenario should ship **inside the ST1b PR** itself, not as a separate
integration tier addition. The W4 entry checklist already calls for
`iterate-hardcore` review on ST1b — a render-snapshot test demonstrating the
unauth provider visible with the `[Sign In]` badge is part of the acceptance
criteria for that PR, not a follow-up.

Once ST1b ships, the integration tier can additionally add a longer-running
test that drives:

1. Cold-start, no provider authenticated.
2. Open the picker.
3. Assert the snapshot lists Copilot Chat with `[Sign In]`.
4. Click the badge → `AuthAction` dispatched → re-snapshot shows the model
   rows now appear.

This is more of an E2E flow than an integration seam, so it may belong in a
zed-side `crates/agent_ui/tests/` tier rather than the caduceus_bridge tier.

## Tracking

This file documents the deferred half of ST9. The shipped half lives at
`crates/caduceus-orchestrator/tests/st9_pin_compaction_integration.rs`
(caduceus side) and continues to be exercised by existing zed-side unit tests
on the notice channel. Re-open this doc when scenario 3 or 4's prerequisite
work lands.
