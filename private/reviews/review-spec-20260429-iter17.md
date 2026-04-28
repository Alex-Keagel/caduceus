# Hardcore Review: spec-orchestrator-status-snapshot.md (iteration 17)

- Date: 2026-04-29
- Content Type: Spec
- Iteration: 17
- Reviewer Models: claude-opus-4.6, gpt-5.4, gpt-5.3-codex
- Verdict: NEEDS WORK

## Scores (min across reviewers)
- Correctness: 6/10
- Completeness: 6/10
- Security: N/A
- Clarity: 5/10
- Architecture: 8/10
- SRP: 7/10
- KISS/DRY: 5/10

## Real issues
1) Pre-v1 omitted `since_boot_id` path conflicts with (d)/(d′) narrative (L620-636).
2) Clause-(d) replay can interleave with live deltas (L698-705).
3) Clause-set text claims "independent" despite admitted overlaps (L763-769).
4) T-26..T-30b coverage statement overclaims clause-(d) coverage and T-27/T-28 are under-specified (L1027, L1937-1955).
5) `WorkspaceStatus` alias points to non-existent spec #3 type (L1277-1282).
6) Replay payload structure not normatively defined (L685-691, L701-702).

## Notes
- Z-7 vs Z-8 collision: no actual collision conflict found; they are orthogonal (encoding rule vs replay-index retention invariant).
