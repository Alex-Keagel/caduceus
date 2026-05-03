# Hardcore Review: spec-orchestrator-status-snapshot.md

- **Date**: 2026-04-29
- **Content Type**: Spec
- **Iteration**: 23
- **Reviewer Model(s)**: claude-opus-4.6, gpt-5.3-codex, gpt-5.4, orchestrator synthesis
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|-----------|-------|--------------|-------|
| 1 | Correctness | 5/10 | gpt-5.4 | Boot-edge routing and replay-cancel rules still contradict themselves. |
| 2 | Completeness | 6/10 | gpt-5.4 | Terms/test coverage lag the actual protocol shape. |
| 3 | Security | 7/10 | claude-opus-4.6 | Untrusted-transport redaction is fail-open and `pid` rules conflict. |
| 4 | Clarity | 6/10 | gpt-5.4 | Core subscribe semantics are restated in multiple drift-prone summaries. |
| 5 | Architecture | 7/10 | claude-opus-4.6 / gpt-5.4 | Canonical invariants are split across terms, algorithm, notes, and tests. |
| 6 | SRP | 7/10 | claude-opus-4.6 / gpt-5.4 | Sections keep re-specifying the same state machine instead of pointing to one canonical rule. |
| 7 | KISS / YAGNI / DRY | 5/10 | claude-opus-4.6 / gpt-5.4 | Repetition and near-duplicate summaries are causing correctness drift. |

**Average**: 6.1/10

## Findings

### 🔴 Critical
1. [docs/specs/spec-orchestrator-status-snapshot.md:91-94] — Untrusted-transport redaction is fail-open for future fields. **Fix:**

```md
Redaction is applied at the serialization boundary (post-`build_snapshot`,
pre-wire). Locally-trusted callers receive the unredacted form. On
non-locally-trusted transports, serialization is default-deny: only
fields explicitly classified by this spec as safe for untrusted
transport MAY be emitted unredacted; every other field MUST be redacted
or omitted. Adding a new field without an explicit redaction
classification is a spec violation and MUST fail closed.
```

2. [docs/specs/spec-orchestrator-status-snapshot.md:118-121] — Terms table is stale and misdefines `BufferEviction`. **Fix:**

```md
| **SnapshotDelta** | One of `RunStarted`, `RunUpdated`, `RunFinished`, `RetryScheduled`, `RetryUpdated`, `RetryCleared`, `AggregateUpdated` (§3.3). Carries a small payload describing what changed; subscribers apply locally. |
| **PostAckFrame** | Post-ack subscription wire frame (§3.4): `Delta(SnapshotDelta)` for live deltas or `ReplayCancelled { stream_seq, reason }` as a terminal cancellation under clause-(d) replay. Carries everything emitted on a subscribe stream *after* the initial `SubscribeAck`. |
| **SubscribeFrame** | Outer subscribe-stream envelope (§3.4): `Ack(SubscribeAck)` (exactly one, first frame) followed by zero or more `PostAck(PostAckFrame)` frames. The full subscriber-bound stream type is `Stream<SubscribeFrame>`. |
| **ReplayCancelReason** | Discriminator on `PostAckFrame::ReplayCancelled` indicating why a clause-(d) replay was aborted: `RingEviction` (required replay entry was trimmed from the replay index and payload buffer in lock-step), `BufferEviction` (required replay payload could not be materialized even though the replay index still covered the requested window), `LockStepTrimRace` (a lock-step trim raced replay-mode buffering). All three terminate the stream and require subscriber re-subscribe via clause (c). |
```

3. [docs/specs/spec-orchestrator-status-snapshot.md:941-973] — The claimed boot-edge fix is still internally inconsistent: `ring_oldest_seq` is treated as both defined and undefined at boot, clause labels are swapped, and `(d′)` is described as unreachable for the wrong reason. **Fix:**

```md
      - **Z-29 boot edge case.** When `current_stream_seq == 0`
        (daemon just booted, no deltas observed yet), the replay index
        and delta payload buffer are both empty and `ring_oldest_seq`
        is undefined. In this regime, the reachable routing clauses are:
        - (a) — future-cursor request (`since_stream_seq = Some(s_c)`
          where `s_c > current_stream_seq`; at boot this means any
          `s_c > 0`).
        - (b) — explicit boot mismatch (`since_boot_id = Some(x)` and
          `x != daemon.boot_id`).
        - (c) — `since_fingerprint == None` clients.
        - vacuous-`UpToDate` — `s_c == 0 && fp matches && boot matches`
          (subscriber has nothing to replay; `SubscribeAck` carries
          `current_stream_seq == 0`, no post-ack frames).

        Clauses (P), (d), and (d′) cannot fire at boot:
        - (P) requires retained replay history; the replay index is empty.
        - (d) requires `s_c < current_stream_seq`, which reduces to
          `s_c < 0` and is unsatisfiable for `u64`.
        - (d′) requires replay-index lookup at `s_c` to yield a
          fingerprint; at boot every lookup is absent.

        Replay-index emptiness is therefore not an error; it is the
        natural state of the warm-up regime.
```

4. [docs/specs/spec-orchestrator-status-snapshot.md:1398-1404] — `pid` is a MUST-redact field in §1.2, but this note downgrades that to transport policy / future ADR. **Fix:**

```md
**Note on `pid`.** On locally-trusted transports (per §1.2), this
field exposes the daemon-host process identifier. On
non-locally-trusted transports, `pid` MUST be replaced with `null`
or omitted per §1.2's redaction rule. Implementations MUST treat
absence of `pid` as semantically equivalent to "not disclosed", not
"no process".
```

5. [docs/specs/spec-orchestrator-status-snapshot.md:2363-2374] — T-31b still permits a silent close when the queue is full, which directly contradicts the normative rule that `ReplayCancelled` is the only legal mid-stream cancellation frame and must be final. **Fix:**

```md
*Atomicity contract (write-before-trim, owning-task-local).* On the
orchestrator's owning task, the trim operation MUST, for each affected
replay subscriber whose stream is still open, enqueue exactly one
`PostAckFrame::ReplayCancelled { stream_seq: current_stream_seq_at_cancel,
reason: ReplayCancelReason::LockStepTrimRace }` on a terminal-control
path before the trim commits. Ordinary data-buffer backpressure MUST
NOT suppress this control frame; implementations MUST reserve capacity
(or equivalent) for one terminal control frame per replay subscriber.
Streams already closed before trim begins are excluded from the affected
set. Only after every affected open subscriber has the cancel frame
enqueued MAY the trim commit. The daemon MUST NOT wait for
subscriber-side receipt or ACKs (per §3.3 delivery semantics).
```

### 🟡 Important
1. [docs/specs/spec-orchestrator-status-snapshot.md:553-603] — This “normative summary” re-specifies behavior already defined elsewhere and is causing drift. **Fix:**

```md
**First-delta rules (summary only).** The normative behavior is
defined by the immediately preceding first-delta-ordering rule, the
closure rule below, and §3.4.1's definition of
`SubscribeAck.stream_seq`. In summary: (1) zero-gap, same-boot,
fingerprint-matching subscribers receive vacuous `UpToDate`; (2)
same-boot, in-window, fingerprint-at-cursor-matching gaps receive
clause-(d) `UpToDate { stream_seq: s_c }` plus replay
`(s_c, current]`; (3) all other valid inputs receive `Resync`; and
(4) invalid `(since_fingerprint = Some(_), since_stream_seq = None)`
routes through clause (c). If this summary and the normative sections
diverge, the normative sections control.
```

2. [docs/specs/spec-orchestrator-status-snapshot.md:1113-1115] — “guaranteed” fingerprint inequality overstates a hash property. **Fix:**

```md
`since_fingerprint = 0xCAFE…` was computed under
`boot_id_old = 0xA0…`. Because `boot_id` is fingerprint hash input
#1 (§4.6.1) and `boot_id_new ≠ boot_id_old`,
`fingerprint_new ≠ 0xCAFE…` except with cryptographically negligible
collision probability — clause (b) fires via the
fingerprint-inequality fallback (since `since_boot_id` was not
sent in this example, exercising the pre-v1 / T-27 path).
```

## Recommended Actions
1. Fix the boot-edge block and T-31b first; both are protocol-behavior blockers.
2. Collapse duplicated subscribe summaries to a single canonical source of truth.
3. Make untrusted-transport redaction fail closed and align the `pid` note with it.
4. Refresh the terms table so the protocol vocabulary matches §3.3/§3.4.
