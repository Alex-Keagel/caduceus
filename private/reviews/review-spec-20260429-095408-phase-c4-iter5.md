# Hardcore Review: spec-caduceus-agent-runner-contract.md (Phase C4 final verdict)

- **Date**: 2026-04-29
- **Content Type**: Spec
- **Iteration**: 5 (post-B4)
- **Reviewer Model(s)**: claude-opus-4.6, claude-sonnet-4.6, gpt-5.4, gpt-5.3-codex, gpt-5.2
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|-----------|-------|--------------|-------|
| 1 | Correctness | 5/10 | gpt-5.3-codex / gpt-5.2 | T-13 contradicts §3.2 non-fatal seq_regression policy |
| 2 | Completeness | 6/10 | gpt-5.4 / gpt-5.3-codex / gpt-5.2 | I-9.2 handshake-time enforcement not present in §3.2 pseudocode |
| 3 | Security | 7/10 | gpt-5.4 / gpt-5.3-codex / gpt-5.2 | I-9.1 file-mode language overclaims same-UID protection |
| 4 | Clarity | 6/10 | gpt-5.3-codex / gpt-5.2 | Conflicting seq semantics and ambiguous “wire seq” wording |
| 5 | Architecture | 7/10 | gpt-5.4 / gpt-5.3-codex / gpt-5.2 | Priority model improved but cross-section contract still inconsistent |
| 6 | SRP | 7/10 | gpt-5.4 / gpt-5.3-codex | §4.5 engine behavior details leak beyond runner-contract boundary |
| 7 | KISS | 6/10 | gpt-5.3-codex / gpt-5.2 | I-3 timing formulation duplicated/inconsistent across sections |

**Average**: 6.3/10

## Findings

### 🔴 Critical
1. **[§3.2 lines 331–349 vs §6 T-13 lines 1664–1666]** — Spec says agent `seq` stutter/regression is surfaced via `seq_regression` and *non-fatal*, but T-13 expects `protocol_violation` + disconnect.
   **Fix (replace T-13 exactly):**
   ```md
   - **T-13 — Agent seq strictness is diagnostic, not fatal.** Inject two events where
     (a) `seq == seq_high_water` (stutter) and (b) `seq < seq_high_water`
     (regression). Assert the runner emits `seq_regression` with
     `kind_detail = "stutter"` / `"regression"`, continues processing,
     and does **not** trigger StopCascade solely for this condition.
   ```

2. **[§5 I-9.2 lines 1552–1560 vs §3.2 handshake branch lines 281–304]** — I-9.2 requires handshake-time refusal for env `CADUCEUS_DECLARES_TOOL_USE="0"` + handshake `declares_tool_use=true`, but pseudocode does not enforce it.
   **Fix (insert in §3.2 after C2 check, before `handshake_seen = true`):**
   ```text
   if runner.env.CADUCEUS_DECLARES_TOOL_USE == "0"
      and runner.capabilities.declares_tool_use:
       emit_event(daemon_event_channel, {
           kind:   "tool_use_declared_but_not_permitted",
           run_id: runner.run_id,
       })
       stop_cascade(runner,
                    reason = "tool_use_declared_but_not_permitted",
                    grace_period_ms = GraceWindow)
       return
   ```

### 🟡 Important
1. **[§3.3 lines 425, 477, 482–503; §5 I-3 lines 1374–1377; §6 T-1/T-9]** — StopCascade timing math is inconsistent (EPSILON/ε/ε₁/ε₂ decomposition and undefined `sigkill_reap_budget`).
   **Fix (replacement text in §5 I-3):**
   ```md
   - **I-3 — StopCascade is bounded.** `stop_cascade` MUST return within
     `ε₁ + 2 * grace_period_ms + ε₂`, where ε₁ is Stage-1 enqueue-or-skip
     overhead (MUST be ≥ 100 ms per Z-26) and ε₂ is SIGKILL+reap overhead
     (implementation-defined, recommended ≤ 150 ms).
   ```

2. **[§3.5.0 line 612]** — Typo/self-contradiction: says “§3.5.0 and §3.5.2 MUST reference this enum”; should reference §3.5.1/§3.5.2.
   **Fix:**
   ```md
   This enum is the **canonical** priority-class assignment for the runner
   contract. §3.5.1 and §3.5.2 MUST reference this enum without restating
   the membership; any drift between this enum and downstream sections is
   a defect and MUST be reconciled in favour of this enum.
   ```

3. **[§3.5.1 line 639]** — Undefined cross-reference `F1` in `cross_run_handoff` note.
   **Fix:**
   ```md
   - `cross_run_handoff` (reserved in v1; MUST be tolerated and forwarded if observed; runners MUST NOT emit it in v1; treated as Lifecycle priority class — see §3.5.0).
   ```

4. **[§5 I-9.1 lines 1476–1483]** — Security claim overstates file modes (“prevents same-UID process connect”).
   **Fix:**
   ```md
   *File mode (normative).* The UDS path MUST be mode `0600` in a daemon-owned
   `0700` directory. This constrains cross-user/cross-process exposure but does
   not by itself authenticate same-UID peers; the peer-credential + PID-lineage
   checks below are the load-bearing controls.
   ```

## Recommended Actions
1. Apply the two critical fixes first (T-13 + I-9.2 handshake-time enforcement).
2. Normalize I-3 timing notation in one canonical formula and align §3.3/§5/§6 references.
3. Fix the two clarity defects (§3.5.0 section reference and §3.5.1 F1 pointer).
4. Re-run Phase C5; target floor: all dimensions ≥ 8.
