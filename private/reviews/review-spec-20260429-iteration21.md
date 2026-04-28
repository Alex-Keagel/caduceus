# Hardcore Review: docs/specs/spec-orchestrator-status-snapshot.md

- **Date**: 2026-04-29 21:55
- **Content Type**: Spec
- **Iteration**: 21
- **Reviewer Model(s)**: claude-opus-4.7, gpt-5.4, gpt-5.3-codex, orchestrator synthesis
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|-----------|-------|--------------|-------|
| 1 | Correctness | 5/10 | claude-opus-4.7, gpt-5.4, gpt-5.3-codex | T-31a/T-31b and pre_v1 signaling contain normative contradictions; boot-edge stream_seq remains ambiguous. |
| 2 | Completeness | 6/10 | claude-opus-4.7, gpt-5.4 | Replay-cancel reason reachability and pre-v1 refusal path are not fully closed. |
| 3 | Security | 6/10 | claude-opus-4.7, gpt-5.4, gpt-5.3-codex | HMAC redaction key/algorithm/output handling are underspecified. |
| 4 | Clarity | 5/10 | claude-opus-4.7, gpt-5.4 | Clause label is wrong; boot-edge wording and replay-cancel reason semantics are ambiguous. |
| 5 | Architecture | 6/10 | claude-opus-4.7, gpt-5.4 | ReplayCancelReason/Test matrix does not align with Z-29 atomic co-trim. |
| 6 | SRP | 8/10 | all | Responsibilities are mostly separated correctly. |
| 7 | KISS / YAGNI / DRY | 6/10 | gpt-5.4 | Eviction taxonomy is over-split relative to what the invariants actually permit. |

**Average**: 6.0/10

## Findings

### 🔴 Critical
1. **[L2304-L2311]** — **T-31a contradicts Z-29 lock-step trim.** The test requires a state where the payload buffer evicted an entry while the replay index still retained it, but Z-29.3 forbids any state where one side retains a `stream_seq` the other does not.
   **Fix:** Replace lines 2304-2311 with:
   ```markdown
   ### T-31a: ReplayCancelled after atomic co-trim overtakes replay

   A subscriber requests clause-(d) replay of `[s_c+1 .. current]`.
   While replay is draining, an atomic trim of the replay index and
   delta payload buffer (per Z-29.3) advances `ring_oldest_seq`
   beyond a `stream_seq` still required by that subscriber's replay.
   Daemon MUST emit exactly one
   `PostAckFrame::ReplayCancelled { stream_seq, reason: ReplayCancelReason::RingEviction }`
   frame and terminate the stream. Conformant daemons MUST NOT treat
   "buffer evicted but index retained" as a reachable production
   state under Z-29.3.
   ```

2. **[L2313-L2320]** — **T-31b requires impossible delivery confirmation / blocking.** “The trim itself MUST NOT proceed until the subscriber receives the cancel frame” is unobservable and conflicts with the non-blocking publication contract.
   **Fix:** Replace lines 2313-2320 with:
   ```markdown
   ### T-31b: ReplayCancelled when trim overtakes replay drain

   A subscriber's replay is in progress when a later atomic trim
   advances `ring_oldest_seq` past a frame that the subscriber has not
   yet been sent. Daemon MUST detect the race, enqueue exactly one
   `PostAckFrame::ReplayCancelled { stream_seq, reason: ReplayCancelReason::LockStepTrimRace }`,
   and close that subscriber's send queue. The daemon MUST NOT wait
   for client-side delivery acknowledgement before completing the trim.
   ```

3. **[L693, L752; impacts L1019-L1033]** — **`pre_v1_unsupported` is specified in a field the wire format does not have.** `SubscribeAckBody::Resync` only carries `{ snapshot }`.
   **Fix:** Replace the sentence at lines 691-693 and 750-752 with:
   ```markdown
   Daemons MAY refuse pre-v1 subscriptions where this residual is
   unacceptable by terminating the subscribe RPC with
   `SnapshotError::Unavailable("pre_v1_unsupported")` before any
   `SubscribeAck` is emitted.
   ```

4. **[L121, L407-L410, L877-L882]** — **Replay-cancel reason taxonomy does not match the atomic co-trim invariant.** As written, `RingEviction` and `BufferEviction` read like mutually exclusive conformant states, but Z-29 says index and payload buffer trim together.
   **Fix:** Replace line 121 with:
   ```markdown
   | **ReplayCancelReason** | Discriminator on `PostAckFrame::ReplayCancelled` indicating why a clause-(d) replay was aborted: `RingEviction` (the required replay entry fell below `ring_oldest_seq` after the daemon atomically co-trimmed the replay index and delta payload buffer), `BufferEviction` (NON-CONFORMANT diagnostic only: payload buffer missing while the replay index still retained the entry, i.e. a Z-29.3 violation), `LockStepTrimRace` (a post-ack atomic trim overtook per-subscriber replay drain before the next required frame was materialized). All three terminate the stream and require subscriber re-subscribe. |
   ```
   Replace lines 407-410 with:
   ```rust
   enum ReplayCancelReason {
       RingEviction,     // Conformant path: atomic co-trim advanced ring_oldest_seq past a required replay frame.
       BufferEviction,   // NON-CONFORMANT diagnostic: payload buffer missing while replay index still retained the frame (Z-29.3 violation).
       LockStepTrimRace, // Replay was admitted, but a later atomic co-trim overtook per-subscriber replay drain.
   }
   ```
   Replace lines 877-882 with:
   ```markdown
   **Replay cancellation frame (normative).** When replay-mode buffering
   must cancel a replay because a required `(s_c, current]` frame can no
   longer be materialized, it MUST emit a single
   `PostAckFrame::ReplayCancelled { stream_seq: current_stream_seq_at_cancel,
   reason: ReplayCancelReason::RingEviction | ReplayCancelReason::LockStepTrimRace }`
   frame to the affected subscriber, then close the per-subscriber send
   queue. `ReplayCancelReason::BufferEviction` is reserved for
   non-conformant / injected-invariant-violation diagnostics only.
   ```

### 🟡 Important
1. **[L78-L81]** — **Redaction HMAC key is underspecified.** No algorithm, key length, encoding, or key-handling rules are given.
   **Fix:** Replace lines 78-81 with:
   ```markdown
   - `session_id` — replace with
     `hex(HMAC-SHA-256(daemon_startup_secret,
     "caduceus.snapshot.session_id.v1" || 0x00 || session_id_bytes)[0..16])`
     so subscribers can correlate without recovering the raw id. The
     HMAC key MUST be a 32-byte CSPRNG-generated secret created once at
     daemon startup, kept in process memory only, never logged or
     serialized, discarded on shutdown, and regenerated on every daemon
     boot (fresh `boot_id` ⇒ fresh key).
   ```

2. **[L737-L745]** — **“collision in clause (a)” mislabels the clause.** The collision risk is in clause (b)'s fingerprint-fallback detection, not clause (a)'s arithmetic gap test.
   **Fix:** Replace lines 737-745 with:
   ```markdown
   **Pre-v1 cross-incarnation collision risk (documented).** Pre-v1
   clients (those subscribing with `since_boot_id = None`) MAY hit a
   low-probability collision in clause (b)'s fingerprint-fallback
   detection when an fp-hash collision occurs across daemon
   incarnations with identical replay-relevant state. The risk is
   bounded by:

   1. `boot_id` is the first hash input to fp (per §4.6.1), so a true
      cross-incarnation collision requires hashing two distinct
      `boot_id`s to identical fp output — at the fp-hash digest size
      (≥128 bits), this is cryptographically negligible.
   ```

3. **[L924-L944]** — **`stream_seq` boot edge is ambiguous between 0 and 1.** The spec currently allows `ring_oldest_seq == 0` “or” the first published `stream_seq`, which changes replay/vacuous-UpToDate reasoning.
   **Fix:** Replace lines 923-944 with:
   ```markdown
      - During warm-up after the first delta (`1 ≤ current_stream_seq <
        recent_history_ring_size`): `ring_oldest_seq == 1`; the
        index/buffer retains every emitted `stream_seq` from `1`
        through `current_stream_seq`.
      - **Z-29 boot edge case.** When `current_stream_seq == 0`
        (daemon just booted, no deltas observed yet), no delta has been
        emitted and the replay index / delta payload buffer are empty.
        `stream_seq = 0` is reserved for the ack-side sentinel “no
        deltas yet”; it MUST NOT appear on any emitted `SnapshotDelta`.
        In this regime, neither clause (d) nor (d′) can fire. A
        subscriber with `since_stream_seq == 0` and matching fp/boot may
        take vacuous `UpToDate`; other tuples route through the normal
        force-resync clauses.
   ```

## Suspected Issues Checked

- **Confirmed:** T-31a contradicts Z-29 lock-step.
- **Confirmed:** T-31b requires impossible delivery confirmation/blocking.
- **Confirmed:** `Resync { reason: ... }` has no wire field.
- **Confirmed:** “collision in clause (a)” mislabels the clause.
- **Confirmed:** not all `ReplayCancelReason` variants are cleanly reachable under atomic co-trim as currently worded.
- **Confirmed:** redaction HMAC key/material handling is underspecified.
- **Rejected as issue:** who closes the old stream / whether subscriber opens a new one — already adequately specified by “emit ReplayCancelled, then close the per-subscriber send queue” plus “subscriber MUST re-subscribe”.
- **Rejected as issue:** `since_fingerprint = None` after `ReplayCancelled` is conservative but not incorrect; the current text is stricter than necessary, not broken.

## Recommended Actions
1. Fix T-31a/T-31b and align `ReplayCancelReason` semantics with Z-29.
2. Fix pre-v1 refusal signaling so it uses an actually representable wire/transport shape.
3. Specify the redaction HMAC algorithm, key length, derivation, and lifecycle.
4. Remove the 0-vs-1 `stream_seq` ambiguity at boot.
