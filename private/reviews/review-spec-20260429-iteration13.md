# Hardcore Review: spec-multi-repo-workspace-model.md

- **Date**: 2026-04-29
- **Content Type**: Spec
- **Iteration**: 13
- **Reviewer Model(s)**: claude-opus-4.7, gpt-5.4, claude-sonnet-4.6, synthesis
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|-----------|-------|--------------|-------|
| 1 | Correctness | 6/10 | synthesis | Step 1b/step 10 still disagree on 10e vs 10c behavior; step 8.5 mislabeled. |
| 2 | Completeness | 6/10 | synthesis | Error-table parity still missing rollback-side ParentRevalidationFailed; diagram still omits one branch. |
| 3 | Security | 9/10 | all | No new security regression in the reviewed fixes. |
| 4 | Clarity | 6/10 | synthesis | One mislabeled step and one ambiguous diagram edge remain. |
| 5 | Architecture | 8/10 | synthesis | State model remains coherent, but one diagram path is still not faithfully rendered. |
| 6 | SRP | 9/10 | all | Responsibilities remain well-separated. |
| 7 | KISS/YAGNI/DRY | 8/10 | synthesis | Mostly disciplined, with some duplicated/lagging status terminology. |

## Findings

### 🟡 Important
1. [§3.5 step 1b / step 10] — Step 1b says rollback-side `CleanupIncomplete` retains the row and skips step 10c, but step 10 still sequences `10c` before `10e` and never normatively says mid-walk failure bypasses placeholder removal. **Fix:** replace `"(and step 8.5, the placeholder-row insert)"` with `"(and step 8.5, the leaf-ownership handoff)"`, and add: `On mid-walk unlinkat failure in sub-step (a), transition the placeholder row to Status::CleanupFailed, skip sub-steps (b) and (c), proceed directly to sub-step (d), and after lock release return the original triggering error.`
2. [§3.5 step 5b, lines 457-460] — Stale status name remains: ``Cleaned` / `Failed``. **Fix:** replace with ``Cleaned` / `CleanupFailed``.
3. [§4.4 Error taxonomy] — Missing rollback-side row for `ParentRevalidationFailed`, even though §3.5 step 10a sends that case to `Status::CleanupFailed`. **Fix:** add `| ParentRevalidationFailed (rollback-side) | §3.5 step 10a (rollback-side parent-fd revalidation failure) | Yes — row transitions to Status::CleanupFailed; reconcile retries identically; persistent failure ⇒ operator. |`
4. [§5B.3 diagram] — `OrphanPending` is still not explicitly drawn as a `CleaningUp`/§3.6 step 4 branch; the current arrow reads like an `Active`-origin path. **Fix:** add an explicit `CleaningUp -> OrphanPending` edge labeled `§3.6 step 4 / liveness inconclusive` and keep `CleanupFailed -> CleaningUp` as the separate retry path.

## Recommended Actions
1. Fix the step 1b/step 10 mismatch first.
2. Clean up the remaining stale `Failed` token.
3. Add the rollback-side `ParentRevalidationFailed` error row.
4. Correct the diagram branch so it matches §3.6 step 4.
