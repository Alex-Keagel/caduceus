# Hardcore Review: spec-caduceus-orchestrator-algorithm.md

- Date: 2026-04-29
- Content Type: Spec
- Iteration: 1
- Reviewer Models: claude-opus-4.7, gpt-5.4, gpt-5.3-codex
- Verdict: NEEDS WORK

## Scores (min across reviewers)

| Dimension | Score |
|---|---|
| Correctness | 4/10 |
| Completeness | 5/10 |
| Security | 8/10 |
| Clarity | 5/10 |
| Architecture | 5/10 |
| SRP | 6/10 |
| KISS/DRY | 5/10 |

## Core verdict

B21 does **not** fully resolve C20. In the retry-pending path (`on_worker_exit` removed `running`; only `RetryEntry` remains), `on_retry_timer` Terminal/Neither performs drain-only and no ring write, and no earlier normative path guarantees a prior `recent_history_ring` write.

## Critical findings

1. False guarantee in `on_retry_timer` Terminal arm that prior cascade already wrote ring entry.
2. Retry-pending Terminal/Neither path can end with zero `FinishedRunSummary`.
3. Internal stale/wrong references (`§3.4` vs `§3.2`, brittle `Lxxx`, wrong ring-invariant section citations).

## Replacement text (for all sub-8 dimensions)

### Correctness (replace Terminal/Neither comment block in §3.5)
"Because `on_worker_exit` Normal/Abnormal removes `state.running[run_id]`, `reconcile_running_runs` cannot guarantee a prior ring write for this run (it inspects only `state.running`). Therefore, when `on_retry_timer` classifies retry-pending work as `Terminal` or `Neither`, this handler MUST perform terminal finalize for that run id (emit exactly one `FinishedRunSummary` with the mapped termination cause) before draining retry/claim/token maps."

### Completeness (replace postcondition note after `terminate_and_finish`)
"If `terminate_and_finish` is invoked when `state.running` lacks `run_id`, postcondition 4’s no-write arm applies only to this helper invocation; it is not a global guarantee that another path already wrote a summary. Retry-pending terminalization (`on_worker_exit` Normal/Abnormal -> `on_retry_timer` Terminal/Neither) MUST still produce exactly one terminal summary before state drain."

### Clarity (replace stale line-number refs)
"This spec MUST NOT use numeric in-document line anchors (`Lxxx`) in normative references. Use stable anchors by function/branch name (for example: `on_retry_timer` WorkSource-vanished branch, `on_worker_exit` DaemonTerminated defensive arm, Deferred-branch `schedule_message` tail)."

### Architecture (replace wrong section reference)
"Replace `§3.4 cascade call site` with `§3.2 cascade call site` wherever referring to stall sweep / WorkSourceTerminal / WorkSourceLeftQuery / orphan reaper. `§3.4` is `run_attempt` and is not the cascade section."

### SRP (replace duplicated drains with helper contract text)
"`on_retry_timer` MUST route all map-reclaim paths through a single helper `drain_run_state(run_id, why)` that removes `running`, `claimed`, `retry_attempts`, `dispatch_defer_attempts`, `token_totals`, and `last_reported_tokens` atomically, then logs one diagnostic."

### KISS/DRY (replace ring-invariant cross-ref text)
"Replace `Ring Invariant #5 (§6)` with `Ring Invariant #5 (§4)` everywhere. Keep one canonical drain-site list in §4 and reference it from §3.5 instead of duplicating partial lists."
