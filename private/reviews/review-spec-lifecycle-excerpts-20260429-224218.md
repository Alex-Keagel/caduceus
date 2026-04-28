# Hardcore Review: spec-multi-repo-workspace-model.md lifecycle excerpts

- **Date**: 2026-04-29
- **Content Type**: Spec
- **Iteration**: 1
- **Reviewer Model(s)**: orchestrator synthesis
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|-----------|-------|--------------|-------|
| 1 | Lifecycle invariant fidelity | 6/10 | orchestrator | B22 overstates the invariant as gating all cleanup rather than only destructive cleanup. |
| 2 | Destructive-action gating precision | 6/10 | orchestrator | `cleanup begins only after...` is too broad; only destructive steps are CleaningUp-gated. |
| 3 | Startup recovery correctness | 7/10 | orchestrator | Startup must not assume the prior runner has already exited when a row is persisted as `CleaningUp`. |
| 4 | Queue-drain transition correctness | 9/10 | orchestrator | B23 correctly transitions to `CleaningUp` before destructive cleanup proceeds. |
| 5 | Cross-section consistency | 7/10 | orchestrator | Same overclaim is duplicated in the enqueue-source rationale. |
| 6 | Wording / ambiguity | 6/10 | orchestrator | The causal sentence is stronger than the actual invariant. |
| 7 | Completeness / enforceability | 8/10 | orchestrator | No material finding. |

## Findings

### 🟡 Important
1. [docs/specs/spec-multi-repo-workspace-model.md:1805-1814] — B22 says "cleanup begins only after...", but §4.3/B23 only guarantee that **destructive cleanup** runs from `CleaningUp`; §3.6 step 4 may still observe a live runner.  
   **Fix:**
   ```md
      daemon. Entering `Status::CleaningUp` does **not** prove the prior
      runner has already exited: §4.3 gates only the destructive portion
      of §3.6 on `CleaningUp`, and §3.6 step 4 may still observe a live
      runner for `RunCancelled` / `OperatorRequested` cleanup. Therefore
      the startup layered probe MUST treat the persisted runner metadata
      as still authoritative and MUST route the row exactly as the bullets
      above specify: positively disproved ⇒ enqueue `OrphanReclaim` (then
      step 7's drain re-runs §3.6 from step 3, skipping step 4);
      Inconclusive ⇒ transition to `OrphanPending`; Alive ⇒ leave for
      operator review.
   ```

2. [docs/specs/spec-multi-repo-workspace-model.md:1999-2004] — The same overbroad rationale reappears in the canonical bypass-source list, creating cross-section drift.  
   **Fix:**
   ```md
         Entering `Status::CleaningUp` does **not** prove the runner has
         already exited; it proves only that the row is in the state from
         which destructive cleanup may execute. Accordingly, startup
         re-runs the layered probe against the persisted runner metadata,
         and only a positively disproved verdict enqueues
         `OrphanReclaim`. Re-running step 4 at drain would then be
         redundant.
   ```

## Recommended Actions

1. Replace the B22 causal sentence with invariant-accurate language tied to destructive cleanup only.
2. Mirror the same correction in the enqueue-source rationale so §5B.2 stays internally consistent.
