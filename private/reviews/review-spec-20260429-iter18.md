# Hardcore Review: spec-multi-repo-workspace-model.md

- Date: 2026-04-29
- Content Type: Spec
- Iteration: 18
- Reviewer Models: claude-opus-4.7, gpt-5.4, gpt-5.3-codex
- Verdict: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|---|---|---|---|
| 1 | Correctness | 6/10 | claude-opus-4.7 | Queue-path contradictions remain. |
| 2 | Completeness | 6/10 | claude-opus-4.7 | EEXIST and no-row reclaim semantics are not fully table-closed. |
| 3 | Security | 8/10 | claude-opus-4.7 | Core safety posture is intact; remaining issues are semantic/cross-ref defects. |
| 4 | Clarity | 5/10 | gpt-5.3-codex | Multiple cross-sections still say different things. |
| 5 | Architecture | 7/10 | claude-opus-4.7 | Queue remains canonical, but some tables still imply direct transitions. |
| 6 | SRP | 8/10 | gpt-5.4 | Scope separation is mostly clean. |
| 7 | KISS/YAGNI/DRY | 6/10 | claude-opus-4.7 | Duplicate lifecycle statements diverge. |

**Average**: 6.6/10

## Findings

### 🔴 Critical
1. [docs/specs/spec-multi-repo-workspace-model.md:788-802] — `OrphanReclaim` bypass is scoped only to §5B.2 step 4, but §5B.2 step 5 also enqueues no-row orphan leaves. A synthetic no-row cleanup that does not bypass step 4 has no row-backed liveness inputs and can fail-closed forever.
   **Fix:** Replace the paragraph starting at line 788 with:
   
   > **OrphanReclaim-queue bypass (normative).** Entries on the `OrphanReclaim` queue MUST skip ONLY the layered liveness probe of this step 4; steps 5, 5a, 6, 7, 8, and 9 MUST run unchanged. This applies to both queue sources: (a) §5B.2 step 4 rows whose liveness was positively disproved, and (b) §5B.2 step 5 no-row orphan leaves dispatched via the synthetic `Workspace` constructor. The bypass MUST NOT re-transition an already-queued row back to `Status::OrphanPending`.

2. [docs/specs/spec-multi-repo-workspace-model.md:1618] — `Active` crash-recovery enqueues `OrphanReclaim { workspace_id }`, but everywhere else the queue payload is `{ slug, run_id }`. That is a normative payload-shape mismatch.
   **Fix:** Replace line 1618 with:
   
   > | `Active` | leaf exists; runner may be alive | LIVE-OR-ORPHAN | §3.6 step 4 layered liveness probe (heartbeat file + pid+pgrp). Alive ⇒ leave `Active`. Disproved ⇒ enqueue `OrphanReclaim { slug, run_id }`; row stays in `Active` until reconcile drains the queue (consistent with §5B.2 step 4). Inconclusive ⇒ `OrphanPending`. |

3. [docs/specs/spec-multi-repo-workspace-model.md:1619] — `CleaningUp` says `cleanup_workspace(_, OrphanReclaim)` resumes from §3.6 step 6, which contradicts the claimed fix that queue bypass skips only step 4.
   **Fix:** Replace line 1619 with:
   
   > | `CleaningUp` | leaf may be partially removed | PARTIAL-CLEANUP | `cleanup_workspace(_, OrphanReclaim)` re-enters §3.6 from step 3 and, per §3.6 step 4 OrphanReclaim-queue bypass, skips ONLY the layered liveness probe; steps 5, 5a, 6, 7, 8, and 9 remain mandatory. |

4. [docs/specs/spec-multi-repo-workspace-model.md:1620, 1730-1736] — `OrphanPending` still has contradictory exits: §5B.1 says direct `→ CleaningUp`; §5B.2 says enqueue first and let queue drain be the only reclaim path.
   **Fix:** Replace line 1620 with:
   
   > | `OrphanPending` | leaf exists, runner dead-or-uncertain | ORPHAN-PENDING | Re-run §3.6 step 4 on each reconcile pass. Once liveness is positively disproved, MUST enqueue `OrphanReclaim { slug, run_id }`; the row remains `OrphanPending` until queue drain dispatches `cleanup_workspace(_, OrphanReclaim)`, whose §3.6 step 1 transitions it to `CleaningUp`. Direct `OrphanPending → CleaningUp` outside the queue is FORBIDDEN. |
   
   And replace lines 1730-1736 with:
   
   > Independently, the same reconcile pass re-evaluates rows in `OrphanPending` and `CleanupFailed` per §5B.1. For `OrphanPending`, re-run §3.6 step 4 layered liveness and, if liveness becomes positively disproved, enqueue the row onto `OrphanReclaim` rather than directly calling `cleanup_workspace`; the queue remains the single canonical reclaim path. `CleanupFailed` continues to use bounded retry.

### 🟡 Important
5. [docs/specs/spec-multi-repo-workspace-model.md:451-476] — The EEXIST decision table still does not close `Status ∈ {CleanupFailed, OrphanPending}` in-table; the override lives only in prose below the table, so two readers can classify the same row differently.
   **Fix:** Replace lines 451-456 with:
   
   > | Dir at `<slug>/<run_id>/` exists | Registry row state | `alive()` | Classification | Action |
   > |---|---|---|---|---|
   > | Yes | No row | n/a | ORPHAN (crash mid-create / no-row leaf) | Enqueue `(slug, run_id)` for asynchronous `OrphanReclaim` on the reconcile queue (§4.5); return `Error::WorkspaceBusyOrReclaiming`. |
   > | Yes | `CleanupFailed` or `OrphanPending` | n/a | RECONCILE-OWNED | Enqueue `(slug, run_id)` for asynchronous `OrphanReclaim` on the reconcile queue (§4.5); return `Error::WorkspaceBusyOrReclaiming`. |
   > | Yes | `Creating`, `Active`, or `CleaningUp` | Yes (`heartbeat_fresh` **OR** `pid_live_same_pgrp`) | DUPLICATE | Return `Error::WorkspaceAlreadyExists`. |
   > | Yes | `Creating`, `Active`, or `CleaningUp` | No (`!heartbeat_fresh` **AND** `!pid_live_same_pgrp`) | ORPHAN (stale row) | Enqueue `(slug, run_id)` for asynchronous `OrphanReclaim` on the reconcile queue (§4.5); return `Error::WorkspaceBusyOrReclaiming`. |

6. [docs/specs/spec-multi-repo-workspace-model.md:603-605] — Success-path text says the per-slug guard is released at cleanup §3.6 step 8, but release actually occurs at step 9.
   **Fix:** Replace lines 603-605 with:
   
   > Release the per-workspace lock. Return the `Workspace` (the per-slug shared-repo guard remains held — it is released only at cleanup, §3.6 step 9).

7. [docs/specs/spec-multi-repo-workspace-model.md:1282, 1606, 1616, 1894, 1897, 1951, 1953] — Several cross-references are stale or incomplete: `§7 item 4` points at this spec's out-of-scope section, and the spec #4 touchpoint omits `workspace_id` / `WorkspaceStatus` even though the snapshot spec consumes them.
   **Fix:** Apply these exact replacements:
   - line 1282: replace `§7 item 4` with `§5B.2 step 7`
   - line 1606: replace `§7 item 4` with `spec #1’s registry-persistence contract and §5B.2`
   - line 1616: replace `§7 item 4` with `§5B.2 steps 5-7`
   - line 1894: replace `§7 item 4` with `§5B.2 step 7`
   - line 1897: replace `§7 item 4 contract` with `§5B.2 steps 5-7 contract`
   - line 1951: replace `§7 item 4` with `spec #1’s reconcile loop`
   - line 1953: replace the whole row with:
     > | **#4 Snapshot** | reads | `workspace_id`, `Workspace.path`, `Workspace.repo_coordinate`, `Workspace.created_at`, and registry `status` (exported for cross-spec use as `WorkspaceStatus`, §4.2) appear in the per-row snapshot. The runs panel (spec #8) renders these. |

## Recommended Actions
1. Fix the queue-path contradictions first (bypass scope, payload shape, `CleaningUp`/`OrphanPending` exits).
2. Collapse the EEXIST classification into a single authoritative table.
3. Repair stale internal/spec-#4 cross-references.
