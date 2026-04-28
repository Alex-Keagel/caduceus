# Hardcore Review: spec-orchestrator-status-snapshot.md

- **Date**: 2026-04-29 21:55
- **Content Type**: Spec
- **Iteration**: 21
- **Reviewer Model(s)**: claude-opus-4.6, claude-sonnet-4.6, gpt-5.4, gpt-5.3-codex, orchestrator synthesis
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|-----------|-------|--------------|-------|
| 1 | Correctness | 4/10 | gpt-5.4 | Unreachable T-31a path, impossible `Resync.reason`, clause misreference, boot-edge ambiguity |
| 2 | Completeness | 5/10 | gpt-5.4 | Replay-cancel taxonomy/recovery semantics under-specified vs Z-29; boot-edge retained-range ambiguity |
| 3 | Security | 5/10 | gpt-5.4 | HMAC key requirements insufficient; pre-v1 refusal not representable on wire |
| 4 | Clarity | 5/10 | gpt-5.3-codex | Clause-(a)/(b) mislabel; replay-cancel rationale obscures actual invariant |
| 5 | Architecture | 6/10 | gpt-5.4 | ReplayCancelReason variants do not fit lock-step trim model |
| 6 | SRP | 8/10 | orchestrator | Acceptable |
| 7 | KISS/YAGNI/DRY | 4/10 | gpt-5.3-codex | Dead/over-specified cancellation taxonomy |

**Average**: 5.3/10

## Findings

### 🔴 Critical
1. **[L2304-L2311 vs L949-L951]** T-31a describes a state Z-29 forbids: buffer evicted while index retained. **Fix:** replace T-31a with an invariant-violation-only test or collapse the reason taxonomy.
2. **[L2315-L2320]** T-31b requires waiting for subscriber receipt before trim, which is unimplementable and violates non-blocking publication. **Fix:** require enqueue-before-close, not delivery confirmation.
3. **[L691-L693, L750-L752, L1030-L1033]** `SubscribeAck::Resync { reason: "pre_v1_unsupported" }` is not expressible on the v1 wire. **Fix:** add an explicit reject shape or delete the claim.
4. **[L78-L81]** `daemon_startup_secret` is not required to be high-entropy or distinct from public `boot_id`; `fresh boot_id => fresh key` invites misuse. **Fix:** require a CSPRNG-generated boot-secret and forbid using `boot_id` as the HMAC key.

### 🟡 Important
1. **[L737-L740]** “collision in clause (a)” is wrong; this is clause-(b) fallback failure. **Fix:** rename to clause (b).
2. **[L119-L121, L407-L410, L877-L883, L2304-L2320]** Under atomic co-trim, `RingEviction`/`BufferEviction` are not distinct reachable operational outcomes; `LockStepTrimRace` overlaps the same event. **Fix:** replace with a single `TrimEviction` reason.
3. **[L884-L887]** The mandated clause-(c) retry is correct, but the rationale is wrong: the issue is not “fingerprint context is lost”; it is that the daemon can no longer prove the subscriber’s cursor incrementally once required replay entries are trimmed.
4. **[L363-L364, L919-L944]** `current_stream_seq == 0` / first emitted delta 0 vs 1 is ambiguous and the Z-29 boot edge mixes “span == 1” with “index may be empty.” **Fix:** make `0` a sentinel, first emitted delta `1`, and define boot-retention explicitly.

## Recommended Actions
1. Collapse `ReplayCancelReason` to a single trim-eviction reason and rewrite T-31a/T-31b accordingly.
2. Replace the impossible `Resync.reason` text with either a new reject frame or an out-of-band refusal rule.
3. Tighten the redaction secret requirements for `session_id` pseudonymization.
4. Make the boot-edge `stream_seq` semantics fully explicit.
