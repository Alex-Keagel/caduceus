# Hardcore Review: spec-caduceus-agent-runner-contract.md

- **Date**: 2026-04-29
- **Content Type**: Spec
- **Iteration**: 25
- **Reviewer Model(s)**: claude-opus-4.6, gpt-5.4, gpt-5.3-codex
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|-----------|-------|--------------|-------|
| 1 | Correctness | 6/10 | claude-opus-4.6, gpt-5.3-codex | Probe 6 fails; I-9.1 wording is disputed but ambiguous enough to mis-implement. |
| 2 | Completeness | 6/10 | gpt-5.3-codex | Missing vendor-host boundary decision in §8.3; no explicit T-10 creation_id_unavailable test. |
| 3 | Security | 6/10 | gpt-5.3-codex | I-9.1 mixes “both checks required” with “lineage load-bearing,” risking wrong auth implementation. |
| 4 | Clarity | 6/10 | gpt-5.4, gpt-5.3-codex | §8 still framed as “Open questions”; Z-tags and dense commentary raise onboarding cost. |
| 5 | Architecture | 6/10 | gpt-5.3-codex | Vendor/host boundary unresolved in §8.3; §4.5 reattach prescribes engine behavior that belongs in spec #1. |
| 6 | SRP | 8/10 | claude-opus-4.6, gpt-5.3-codex | Scope is mostly clean, with some spillover into engine-side reaction policy. |
| 7 | KISS / YAGNI / DRY | 7/10 | claude-opus-4.6, gpt-5.3-codex | Open-question framing, reserved future kinds, and repeated timing formulas add avoidable complexity. |

**Average**: 6.4/10

## Probe Verification

1. **PASS** — `grace_period_ms` is declared in `RunnerProcess` at §3.1 line 221.
2. **PARTIAL / AMBIGUOUS** — §I-9.1 at line 2007 says the broker “MUST verify both” UID and lineage, but also says lineage bound to `(pid, process_start_time)` is the load-bearing control and UID only constrains cross-UID access. Fail-closed on `creation_id_unavailable` is present.
3. **PASS** — T-10 at line 2225 has three sub-cases including recycled PID case (c).
4. **PASS** — `CADUCEUS_RUNNER_UUID` appears in the reserved env table at line 1958.
5. **PASS** — T-14 is inserted before T-15 at lines 2311–2313.
6. **FAIL** — §8.3 at lines 2375–2381 is “Streaming vs end-of-turn token reporting,” not a decided vendor-host boundary section, and §8 still uses “Open questions” framing at line 2336.

## Findings

### 🔴 Critical
1. [§8 line 2336; §8.3 lines 2375–2381] — Probe 6 fails. The document still frames §8 as “Open questions,” and §8.3 is about token-reporting cadence rather than the vendor-host boundary.  
   **Fix:** Replace:
   ```md
   ## 8. Open questions
   ```
   with:
   ```md
   ## 8. Versioned design decisions
   ```
   and replace:
   ```md
   ### 8.3 Streaming vs end-of-turn token reporting

   This spec makes no normative requirement on `streams_partials` — both
   end-of-turn-only and streaming modes are contract-valid. Spec #1 MUST
   handle slower liveness signals (e.g. `token_update` only at `turn_end`)
   as a first-class case in its stall-sweep policy. Vendor deployment
   guidance is out of scope here.
   ```
   with:
   ```md
   ### 8.3 Vendor–host boundary — Resolved (v1)

   **Resolution (v1, normative).** This contract standardises only the
   host-facing runner boundary: spawn/reap ownership, reserved env,
   transport, handshake, stop semantics, token accounting, reattach
   surface, and permission-elevation wire shapes. Vendor-specific
   packaging, adapter internals, default command lines, and any catalog of
   bundled or default-shipped vendors are out of scope and MUST live above
   this contract. The daemon MUST NOT special-case vendors in protocol
   validation; conformance is determined solely by this runner contract.
   ```

2. [§I-9.1 line 2007] — Broker authentication wording is ambiguous enough to implement incorrectly: it first requires both UID and lineage, then says lineage is load-bearing and UID only constrains cross-UID access.  
   **Fix:** Replace the authentication paragraph with:
   ```md
   *Authentication (`SO_PEERCRED`, normative).* Every connection accepted on the broker socket MUST be authenticated using `SO_PEERCRED` (Linux) / `LOCAL_PEERCRED` (macOS / `getpeereid`) / `GetNamedPipeClientProcessId` + token query (Windows). The admission rule is single-form: the broker MUST prove that the connecting peer is the AgentProcess PID or a descendant of it, with lineage bound to `(pid, process_start_time)` (or the OS-equivalent creation identifier), not PID alone. That lineage proof is the load-bearing identity check. Peer UID equality is defense-in-depth for cross-UID access only and MUST NOT be treated as sufficient without lineage or as a substitute when lineage cannot be proven. If the creation identifier cannot be obtained or validated, the broker MUST fail closed: reject the connection, emit `broker_peer_rejected` with `reason = "creation_id_unavailable"`, and MUST NOT fall back to PID-only lineage checks. Any connection that fails lineage validation, or presents a different peer UID than the daemon used to spawn the AgentProcess, MUST be closed immediately and reported on `daemon_event_channel`.
   ```

### 🟡 Important
1. [§6 line 2225] — T-10 does not explicitly exercise the normative `creation_id_unavailable` fail-closed path from §I-9.1.  
   **Fix:** Replace T-10 with:
   ```md
   - **T-10 — Broker peer authentication.** Four sub-cases: (a) wrong-UID peer — assert `broker_peer_rejected`; (b) same-UID non-descendant PID — assert `broker_peer_rejected`; (c) same-UID recycled PID — after the original AgentProcess exits, connect from a new process that reuses the same PID but has a different `process_start_time` / creation identifier; assert `broker_peer_rejected`; (d) creation identifier unavailable — simulate a platform path where peer credentials are available but the creation identifier cannot be obtained or validated; assert `broker_peer_rejected` with `reason = "creation_id_unavailable"` and assert the broker does NOT fall back to PID-only checks. This test normatively validates the I-9.1 `(pid, process_start_time)` rule and fail-closed behaviour.
   ```

2. [§4.3 lines 1469–1476] — Token-accounting ownership prose says the runner forwards payload to the daemon, but the pseudocode immediately reads and mutates `daemon.last_reported_tokens`, creating a two-owner narrative.  
   **Fix:** Replace the ownership comment with:
   ```md
       // The reconcile_tokens algorithm below runs in the runner using a
       // reference to the daemon-owned
       // OrchestratorState.last_reported_tokens (spec #1 §4) passed in as
       // `daemon.last_reported_tokens`. The runner is the single writer of
       // this map for its own `run_id`; after updating it, the runner calls
       // `publish_run_token_update` to republish the resulting TokenTotals
       // on the snapshot bus. There is no separate "forwarding" hop here:
       // `daemon.last_reported_tokens` is authoritative across RunAttempts;
       // `RunnerProcess.last_reported_tokens` is informational only.
   ```

3. [§4.5 lines 1747–1795] — Reattach section prescribes engine next-actions in detail, which risks overlapping spec #1 ownership and weakens SRP.  
   **Fix:** Replace:
   ```md
   **Per-variant orchestrator next-action (Z-21 normative).** The engine
   (spec #1's caller of `Cmd::Reattach`) MUST react to each variant
   exactly as follows; implementations MAY add diagnostics but MUST NOT
   deviate from the action:
   ```
   with:
   ```md
   **Per-variant engine obligations (cross-spec hook).** The daemon-side
   meanings of these variants are normative here; the engine-side retry,
   resubscribe, and caller-surfacing policy is owned by spec #1 and MUST
   mirror the semantics below without redefining the wire variants.
   ```

### 🟢 Suggestions
1. [§2 line 97] — `GraceWindow` is too dense in the terms table.  
   **Fix:** Replace the table row body with:
   ```md
   Composite shutdown timing envelope. Expands to `ε₁ + 2·grace_period_ms + ε₂`; see §3.3 for component definitions and authoritative bounds. Not a separate configurable parameter.
   ```

2. [Across Z-11/Z-22/Z-23/Z-24/Z-25/Z-26/Z-27/Z-28] — Add a Z-tag index to improve scanability.  
   **Fix:** Add:
   ```md
   ### Z-tag index
   | Tag | Defined in | One-line summary |
   |-----|-----------|-----------------|
   | Z-11 | §4.1 | Closed-set enforcement of message kinds |
   | Z-21 | §4.5 | Reattach response contract and idempotency |
   | Z-22 | §4.4 | `runner_seq` stamped post-Ok only |
   | Z-23 | §4.4 | Stamp point is after `forward_to_daemon -> Ok` |
   | Z-24 | §3.2 / §4.1 | Inbound `seq` strictly increasing; first frame is 1 |
   | Z-25 | §3.4 | ACP adapter uses the same validation+dispatch path |
   | Z-26 | §3.3 | Stage-1 cancel must flow through outbound queue |
   | Z-27 | §5 I-9.1 | `CADUCEUS_DAEMON_SOCKET` broker contract |
   | Z-28 | §5 I-9.2 | `CADUCEUS_DECLARES_TOOL_USE` fail-closed rule |
   ```

## Recommended Actions
1. Fix §8 framing and replace §8.3 with the resolved vendor-host boundary text.
2. Rewrite I-9.1 auth as a single-form lineage-first rule.
3. Add the missing T-10 `creation_id_unavailable` sub-case.
4. Clarify token-accounting ownership and optionally trim engine-policy spillover in §4.5.
