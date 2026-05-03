# Hardcore Review: spec-multi-repo-workspace-model.md

- **Date**: 2025-07-14
- **Content Type**: Spec
- **Iteration**: 27
- **Reviewer Models**: claude-opus-4.6, gpt-5.4, claude-sonnet-4.6, gpt-5.3-codex
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Bias | Notes |
|---|-----------|-------|--------------|------|-------|
| 1 | Correctness | 7/10 | GPT-5.4 (6) | Opus/Codex bias → 7 | I-6 raw vs sanitized run_id; §3.3 step 3 "same regex" wrong; §3.5 step 4 dead code; §3.6 step 1 misleading |
| 2 | Completeness | 7/10 | GPT-5.4/Sonnet/Codex (7) | — | runner_uuid undefined; create-time env missing WORKSPACE_ROOT; startup probe fd-prelude unspecified |
| 3 | Security | 8/10 | GPT-5.4 (6) | Opus(9)/Codex(8) bias → 8 | Core model sound; O_CLOEXEC table gaps; CAP_SYS_PTRACE vs CAP_DAC_READ_SEARCH |
| 4 | Clarity | 6/10 | GPT-5.4 (5) | Sonnet(6) anchors | Step numbering unmaintainable; "registry write lock" conflation; runner_uuid phantom |
| 5 | Architecture | 7/10 | GPT-5.4/Sonnet (7) | — | Step 4 zombie; repo_bindings store relationship unclear |
| 6 | SRP | 7/10 | GPT-5.4 (6) | Opus/Codex(8) bias → 7 | §3.5 step 5b mega-step; §3.6 step 5 bundles parent revalidation with hook |
| 7 | KISS/YAGNI/DRY | 7/10 | GPT-5.4 (4) | Opus/Codex(8) bias → 7 | Lock order 4×; bypass scope restated; strategy (b) decorative |

**Average**: 7.0/10

## Findings

### 🔴 Critical

1. **[I-6 / §3.5 step 9, lines 700 & 1553]** — `run_id` (raw) vs `sanitize_run_id(run_id)` in workspace_id hash. I-6 and step 9 hash over raw `run_id`, but step 1.3a (line 390) and §5B.2 step 5 (line 1836) derive workspace_id from the sanitized form (the on-disk leaf). These produce different BLAKE3 outputs.
   **Fix (line 700):** `workspace_id = wsp_<hex(BLAKE3_128_keyed(slug || 0x1F || safe_run_id))>`
   **Fix (line 1553):** `workspace_id = "wsp_" + lower_hex(BLAKE3_128_keyed(slug || 0x1F || sanitize_run_id(run_id)))` where…

2. **[§3.3 step 3, line ~275]** — "passes the same regex" is wrong. The slug regex `^[a-z0-9][a-z0-9_]{0,63}$` rejects uppercase, hyphens, and dots — all valid in sanitize_run_id output (e.g. ULID `01HZX0ABCDEF`).
   **Fix:** Replace step 3 with: `Assert run_id matches the sanitize_run_id output charset ^[A-Za-z0-9._-]+$, does not equal . or .., is not empty, and does not contain .. as a substring. Otherwise MUST return InvalidRunId.`

3. **[§3.5 step 4, line ~432]** — Dead/zombie code. Per-slug lock already acquired at step 1.3. Under strategy (a) step 4 is unreachable; under a non-reentrant mutex, an implementer re-acquiring the same lock will deadlock.
   **Fix:** Replace with: `4. *(Strategies (b)/(c) only; no-op under strategy (a).) Under a non-exclusive lock strategy, perform additional intent-level conflict checking here. Under strategy (a), step 1.3 already provides exclusive serialization and this step is a no-op.*`

4. **[§3.6 step 1, line ~875]** — "Acquire registry write lock for this workspace" conflates the per-workspace lock with the registry-wide mutex. An implementer reading only the lead sentence acquires one lock, violating the three-lock hierarchy.
   **Fix:** Replace with: `1. **Acquire per-workspace lock for this workspace.** A workspace-level lock; concurrent cleanup calls on different workspaces MAY proceed in parallel. Lock-state at entry: the per-slug shared-repo guard is already held from create_workspace (§3.7 Lock-guard contract). The registry-wide mutex is NOT held; it is acquired briefly only for row mutations.`

5. **[§3.1 collision policy step 4, lines 159–162]** — Extension to 32 hex chars can force `keep=0`, producing slugs like `_<hash>` that violate `^[a-z0-9]…`.
   **Fix:** Add `…trimming keep as needed **but never below 1** so the slug still matches ^[a-z0-9][a-z0-9_]{0,63}$. If uniqueness cannot be achieved within the 64-byte cap while preserving keep ≥ 1, MUST return Error::RepoSlugCollisionExhausted.`

6. **[§3.2 worked example, line 266]** — Cites wrong rejection rule. Input `"   "` passes rule 1 (3 bytes), then rules 2–3 produce empty, then rule 4 rejects.
   **Fix:** `| \`   \` | rejected (rule 4 after rules 2–3) | sanitizes to empty |`

### 🟡 Important

7. **[§5A.5 / step 8.5, lines ~1696–1725]** — `fchownat` only chowns the leaf directory entry, not contents. Files created by daemon-uid `after_create` (git clone) remain daemon-owned; a runner at a different uid cannot edit them.
   **Fix:** Add normative note: `Step 8.5 chowns only the leaf directory entry, NOT contents recursively. Deployments where daemon_uid ≠ runner_uid SHOULD use hooks.run_as = runner_uid for after_create, or the hook MUST include a recursive chown.`

8. **[§5A.3 table, lines 1618–1620]** — `workspace_root_fd` and `slug_fd (create)` missing `O_CLOEXEC` in the normative table. Hook subprocesses would inherit privileged fds.
   **Fix:** Add `O_CLOEXEC` to both rows. Add table note: `All fds opened under <workspace_root> MUST include O_CLOEXEC.`

9. **[§3.5 step 6, lines 658–680]** — Create-time env missing `CADUCEUS_WORKSPACE_ROOT` (present in cleanup env table line 1126). Asymmetry is undocumented.
   **Fix:** Add `CADUCEUS_WORKSPACE_ROOT = workspace_root` and `CADUCEUS_PARENT_PATH = <workspace_root>/<repo_slug>` to the create-time env list.

10. **[§2 Terms]** — `runner_uuid` used in 4 places (step 5b table, §3.6 step 4, §4.2, §3.8), defined nowhere.
    **Fix:** Add to §2: `RunnerUuid: A per-spawn opaque identifier assigned by the daemon at spawn_worker time (spec #2). Written into the heartbeat file; used to correlate heartbeat files with specific runner instances.`

11. **[§4.2 / §3.6 step 4]** — Heartbeat file JSON `{pid, runner_uuid, mtime}` is write-only from daemon's perspective. Daemon reads ONLY `mtime` via `fstatat`. Pid/runner_uuid from file are never consumed.
    **Fix:** Add to §3.8: `The daemon reads ONLY the heartbeat file's mtime via fstatat; the JSON body is for operational visibility only and MUST NOT be treated as authoritative by the liveness probe.`

12. **[§5B.2 step 4, line ~1781]** — Startup liveness probe references §3.6 step 4 which requires `leaf_fd` from step 3, but no fd-prelude is specified for the startup path.
    **Fix:** Prepend to step 4: `Fd-prelude requirement: Before running the §3.6 step 4 probe for any row, the daemon MUST first execute §3.6 step 3's fd-acquisition prelude. OrphanedNoSlug/OrphanedNoLeaf short-circuits apply identically.`

13. **[I-6, line ~1553]** — BLAKE3 key derivation is "e.g." not normative. Two compliant implementations can produce different workspace_ids.
    **Fix:** Replace with normative derivation: `BLAKE3 key MUST be BLAKE3-128(b"caduceus.workspace_id.v1" || 0x00 || workspace_root_canonical_utf8).`

14. **[§5A.4, lines ~1645–1648]** — `CAP_DAC_READ_SEARCH` is insufficient for cross-uid `/proc/<pid>/cwd` on Yama ptrace_scope ≥ 1. Need `CAP_SYS_PTRACE`.
    **Fix:** Replace all three occurrences with: `CAP_SYS_PTRACE (Linux) for cross-uid /proc/<pid>/cwd probing. On kernels with Yama ptrace_scope ≥ 1, CAP_DAC_READ_SEARCH alone is insufficient.`

15. **[§5B.2 step 7, lines 1899–1902 vs 1966–1971]** — "does NOT scan the registry by Status column" contradicts the same step's "re-evaluates rows in OrphanPending and CleanupFailed per §5B.1".
    **Fix:** Rewrite lines 1899–1902: `Reconcile runs in two phases each tick: (a) drain the OrphanReclaim in-memory queue end-to-end; (b) scan registry rows in OrphanPending and CleanupFailed per §5B.1. Deletion work is queue-driven only.`

16. **[§3.6 step 8, lines 1112–1116 & 1134]** — `CADUCEUS_WORKSPACE_REMOVED` can be `"0"` per table, but step 8 only runs after successful step 7 leaf removal.
    **Fix:** `CADUCEUS_WORKSPACE_REMOVED` is ALWAYS `"1"` on `after_cleanup` because step 8 is entered only after successful step 7. Replace line 1134: `| CADUCEUS_WORKSPACE_REMOVED | "1" (always; after_cleanup follows successful removal) | yes |`

17. **[§3.1 steps 5–6, lines 132–134]** — Operand ambiguous. Steps 5–6 don't specify whether they apply to host and path independently or combined.
    **Fix:** `5. In **both** host and path independently: replace each maximal run of characters not in [a-z0-9] with a single _. 6. Trim leading and trailing _ from **both** host and path independently.`

18. **[§5B.3 diagram, lines 2029–2041]** — Missing `Creating → CleanupFailed` edge (normatively defined in §4.3 line 1451 for step 10a-pre/10a/10e rollback aborts).
    **Fix:** Add edge: `Creating ──§3.5 step 10a-pre/10a/10e rollback abort──► CleanupFailed.`

19. **[§3.5 step 9, lines ~722–724]** — Registry-wide mutex re-acquired while per-workspace lock held. Deadlock-safety depends on step 1.3a being try-lock, but this is never stated at step 9.
    **Fix:** Add after "Briefly re-acquire the registry-wide mutex": `Deadlock-safety: This re-acquisition is safe because step 1.3a uses try-lock (not blocking-wait). A concurrent caller holding registry-wide at step 1.1 will try-lock per-workspace, fail, and return WorkspaceBusy — it never blocks.`

20. **[§5B.4 / §5B.2 step 7, lines ~2067–2068]** — `orphan_pending_max_age` (24h) has no reconcile-loop action. Permanently-Inconclusive rows have no timeout path.
    **Fix:** Add to §5B.2 step 7: `If an OrphanPending row exceeds orphan_pending_max_age (default 24h) with every re-probe returning Inconclusive, the daemon MUST transition it to CleanupFailed with Error::OrphanPendingTimeout.`

### 🟢 Suggestions

21. Lock order restated 4× (§3.5 step 1.3, §3.6 step 1, §3.5 step 10d, §4.5). Replace non-canonical sites with pointer: `(lock order: §4.5 normative)`.
22. §3.7 strategy (b) RwLock is decorative ("enforcement is best-effort via hook conventions"). Either remove from normative spec or define concrete enforcement.
23. `CADUCEUS_REPO_REMOTE_URL_SAFE_B64` is YAGNI — shell quoting already solves the same problem.
24. §5A.3 table should include leaf-scan traversal entries for §5B.2 step 5.
25. §4.4 `HookEnvConflict` source scope should cite all four hook sites, not just §3.6 step 8.
26. §4.2 line 1323 "recently-cleaned-up" contradicts terminal=row removal. Fix: "active and recoverable (non-terminal) Workspace rows".
27. Step numbering (1, 1b, 1.3a, 8.5, 10a-pre, 10e) is unmaintainable. Consider flat renumbering.

## Recommended Actions

1. Fix the 6 critical items (I-6 hash input, §3.3 regex, step 4 dead code, §3.6 step 1 wording, collision exhaustion, §3.2 example).
2. Address the 14 important items in priority order (chown recursion, O_CLOEXEC, env asymmetry, runner_uuid definition, heartbeat semantics, fd-prelude, BLAKE3 key, CAP_SYS_PTRACE, reconcile model, WORKSPACE_REMOVED, §3.1 operand, diagram edge, deadlock rationale, orphan timeout).
3. Re-review after fixes.
