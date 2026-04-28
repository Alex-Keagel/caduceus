# Hardcore Review: spec-caduceus-agent-runner-contract.md

- **Date**: 2026-04-29 20:40
- **Content Type**: Spec
- **Iteration**: 19
- **Reviewer Model(s)**: claude-opus-4.7, gpt-5.4, gpt-5.3-codex + orchestrator synthesis
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|-----------|-------|--------------|-------|
| 1 | Correctness | 6/10 | gpt-5.3-codex | Conflicting ACP/version text and stale seq-seeding wording remain. |
| 2 | Completeness | 7/10 | claude-opus-4.7 / gpt-5.3-codex | A few normative contradictions still leave edge cases underspecified. |
| 3 | Security | 8/10 | claude-opus-4.7 / gpt-5.4 / gpt-5.3-codex | No material new security defect surfaced. |
| 4 | Clarity | 7/10 | claude-opus-4.7 / gpt-5.3-codex | Some stale wording still points implementers the wrong way. |
| 5 | Architecture | 8/10 | claude-opus-4.7 / gpt-5.3-codex | Core structure remains sound. |
| 6 | SRP | 8/10 | claude-opus-4.7 | Scope split is still coherent. |
| 7 | KISS/YAGNI/DRY | 8/10 | claude-opus-4.7 / gpt-5.4 / gpt-5.3-codex | Remaining issues are contradictions, not overdesign. |

**Average**: 7.4/10

## Findings

### 🔴 Critical
1. [docs/specs/spec-caduceus-agent-runner-contract.md:867-874] — §3.4 still says unsupported ACP version is rejected with an explicit `unsupported_acp_version` error frame, which conflicts with §8.2's v1 hard-refusal rule and implies a runner-contract wire response in a path that should terminate upstream instead. **Fix:** Replace lines 867-874 with:

```text
**Version negotiation.** If the agent advertises an ACP version the

daemon does not implement, the daemon MUST hard-refuse the spawn:
return an error to the upstream caller, emit an
`unsupported_acp_version` diagnostic on `daemon_event_channel`, and
MUST NOT silently fall back to JSONL once ACP has been requested. If a
runner process was already started for negotiation, the daemon MUST
reap it before returning. Unsupported ACP version is a refusal-to-run
condition, not an in-session runner-contract error frame.
```

### 🟡 Important
1. [docs/specs/spec-caduceus-agent-runner-contract.md:758-763] — The ε₁ definition is mostly fixed, but the final sentence still assigns SIGTERM dispatch to Stage 2 and to ε₂; that reintroduces the exact timing confusion the prior iteration was supposed to remove. **Fix:** Replace lines 758-763 with:

```text
- `ε₁` — Stage-1 enqueue latency budget. ε₁ is the budget for
  enqueuing-or-skipping the Stage-1 `cancel` control message
  (whichever applies under §3.5 cancellation policy) once the daemon
  has decided to cancel the run; it does NOT measure time to first
  SIGTERM. Stage 3b SIGKILL-reap is bounded by ε₂ per §3.3.
  Default **≥ 100 ms**, configurable via `shutdown_enqueue_budget_ms`.
```

2. [docs/specs/spec-caduceus-agent-runner-contract.md:469-474,1382-1386] — The handshake parser comment still says an unknown capability bit is malformed, but §4.2 says unlisted bits MUST be informational-only. That is a forward-compatibility contradiction in normative pseudocode. **Fix:** Replace lines 469-474 with:

```text
// Malformed handshake payload: line was
// valid JSONL (otherwise try_parse_jsonl
// above would have fired `malformed_jsonl`),
// but the capabilities sub-schema is
// invalid (wrong type for a known bit,
// missing required field, …). Unknown
// capability bits are NOT an error here;
// per §4.2 they MUST be ignored as
// informational-only forward-compat fields.
// Fail-closed:
```

3. [docs/specs/spec-caduceus-agent-runner-contract.md:1566-1569,89-90,1903-1911] — §4.4 says the runner seeds `runner_seq` from spawn-time env/argv, but this spec defines RunnerProcess as daemon-internal and the reserved `CADUCEUS_*` env set is exhaustive with no seed key. That is a cross-section implementability contradiction. **Fix:** Replace lines 1566-1569 with:

```text
3. On a fresh RunAttempt for a Run the daemon already has a
   `runner_seq_high_water` for, the runner MUST seed its counter from
   the daemon's stored high-water at RunnerProcess construction time
   and continue incrementing from there. Because RunnerProcess state is
   daemon-internal (§2), this seed is NOT passed via env or argv.
```

## Recommended Actions
1. Fix the ACP version-negotiation conflict first; it is the only ship-blocker.
2. Land the ε₁, unknown-handshake-bit, and runner_seq-seeding text fixes in the same pass.
3. Re-run iteration 20 after those edits.
