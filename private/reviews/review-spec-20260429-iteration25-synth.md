# Hardcore Review: spec-orchestrator-status-snapshot.md

- **Date**: 2026-04-29 23:59
- **Content Type**: Spec
- **Iteration**: 25
- **Reviewer Model(s)**: claude-opus-4.6, gpt-5.4, gpt-5.3-codex
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|-----------|-------|--------------|-------|
| 1 | Correctness | 7/10 | claude-opus-4.6, gpt-5.3-codex | Replay-cancel partition is mostly right, but the top-level cancellation paragraph and summary text still leave causal-path ambiguity. |
| 2 | Completeness | 6/10 | gpt-5.3-codex | T-31/T-31a/T-31b partition is not fully reflected in the summary block, and the cancellation paragraph does not fully encode the 3-way split. |
| 3 | Security | 8/10 | gpt-5.4, gpt-5.3-codex | No blocking security issues in the reviewed sections. |
| 4 | Clarity | 6/10 | gpt-5.3-codex | T-31 remains a dense wall of prose; one sentence still blurs replay-mode activation vs trim-guard registration. |
| 5 | Architecture | 8/10 | claude-opus-4.6, gpt-5.3-codex | Clause system remains coherent. |
| 6 | SRP | 8/10 | gpt-5.4, gpt-5.3-codex | Responsibilities are separated acceptably for a spec. |
| 7 | KISS/YAGNI/DRY | 6/10 | gpt-5.3-codex | The same distinctions are repeated inconsistently across the cancellation paragraph, tests, and summary block. |

**Average**: 7.0/10

## Findings

### 🟡 Important
1. [`docs/specs/spec-orchestrator-status-snapshot.md:698-704`] Independence note overstates runtime behavior with “Implementations MUST evaluate both,” even though `(d′)` is same-boot-only and may be short-circuited once `(b)` is already true on explicit boot mismatch.

   **Fix:**
   ```md
   - *Independence from clause (d′).* Clauses (b) and (d′) are logically independent: clause (b) is the **cross-incarnation** trigger (subscriber state predates the current daemon `boot_id`); clause (d′) is the **same-boot in-window fingerprint-divergence** trigger (cursor recoverable but fingerprint at that cursor disagrees). Implementations MUST preserve both predicates as distinct checks. Ack selection still follows the §3.4 closure rule; when explicit `since_boot_id != current_boot_id`, an implementation MAY short-circuit before evaluating clause (d′)`s same-boot-only predicate.
   ```

2. [`docs/specs/spec-orchestrator-status-snapshot.md:884-895`] The normative replay-cancellation paragraph still reads as eviction-centric and does not state the 3-way `ReplayCancelReason` partition in one place with mutually exclusive causal criteria.

   **Fix:**
   ```md
   **Replay cancellation frame (normative).** When replay-mode buffering must cancel a replay, the daemon MUST emit exactly one `PostAckFrame::ReplayCancelled { stream_seq: current_stream_seq_at_cancel, reason }` frame to the affected subscriber, then close the per-subscriber send queue. `reason` MUST be selected by causal path and MUST be exactly one of: `ReplayCancelReason::LockStepTrimRace` (the owning task observed an in-progress clause-(d) replay on the pre-commit trim path and enqueued terminal cancellation before trim commit; see T-31b), `ReplayCancelReason::RingEviction` (replay processing later discovered that the required window had already been evicted from both the replay index and the delta payload buffer outside the T-31b pre-commit path; see T-31), or `ReplayCancelReason::BufferEviction` (the replay index still covered the requested window but the payload could not be materialized for a non-trim reason; see T-31a). The subscriber MUST treat receipt of `ReplayCancelled` as a signal to re-subscribe; the new subscribe takes clause (c) with `since_fingerprint = None` and `since_stream_seq = None`, while `since_boot_id` MAY carry the last-known boot_id when available.
   ```

3. [`docs/specs/spec-orchestrator-status-snapshot.md:1296-1303`] The summary block still compresses all replay cancellation into one T-31 sentence and no longer matches the 3-way test split.

   **Fix:**
   ```md
   T-10 covers clause (d) (UpToDate + `(s_c, current]` replay), and T-10-pre-v1 covers the same path under `since_boot_id = None`. T-26..T-29 cover clause (b) (cross-incarnation / out-of-window Resync). T-30 and T-30b cover clause (d′) (same-boot in-window fingerprint divergence), with T-30b-pre-v1 covering (d′) under `since_boot_id = None`. T-31, T-31a, and T-31b cover replay cancellation under clause-(d) replay via the `ReplayCancelled` frame: `RingEviction` (post-commit discovery), `BufferEviction` (payload not materializable while replay-index coverage remains intact), and `LockStepTrimRace` (pre-commit trim-side enqueue) respectively.
   ```

4. [`docs/specs/spec-orchestrator-status-snapshot.md:2295-2320`] T-31 is directionally correct, but one sentence still implies replay mode is not yet established even though replay mode starts at ack emission. The real race is trim-guard visibility / affected-subscriber detection.

   **Fix:**
   ```md
   ### T-31: Replay discovers a previously-evicted window (post-commit RingEviction)

   Setup: subscriber attaches in clause (d) replay mode at `s_c =
   current_stream_seq − 1`.

   Race condition: after `SubscribeAck::UpToDate` has been emitted (replay
   mode is active per §3.4 buffering rule step 1), but before the
   subscriber's replay window has been registered with the trim guard, the
   owning task processes a backlog burst that lock-step-trim-commits
   (Z-29 sub-invariant 3). Because the pre-commit T-31b enqueue path did
   NOT observe an in-progress clause-(d) replay for this subscriber at the
   time of the trim (replay setup raced the trim, or the subscriber's
   replay window had not yet been registered with the trim guard), the trim
   commits without enqueuing a `LockStepTrimRace` cancel for it.

   Discovery: replay then finds, on its next replay-buffer read, that the
   required window is gone from both the replay index and the delta payload
   buffer.

   **Expected:** daemon emits exactly one `PostAckFrame::ReplayCancelled
   { stream_seq, reason: ReplayCancelReason::RingEviction, .. }` frame
   to the affected subscriber, then closes its send queue; no further
   frames (including live deltas) are written to that stream. Other
   subscribers in non-replay mode receive the new deltas normally. The
   cancelled subscriber's re-subscribe carries `since_fingerprint =
   None` and takes clause (c) (snapshot RPC re-anchor). Verifies the
   post-eviction discovery path for `RingEviction` (distinct from
   T-31b's pre-commit `LockStepTrimRace` path) and the `ReplayCancelled`
   frame contract (§3.4 clause (d), replay-mode buffering rule, replay
   cancellation frame paragraph) — and explicitly that the daemon does
   NOT emit `SubscribeAck::Resync` mid-stream.
   ```

### 🟢 Suggestions
1. [`docs/specs/spec-orchestrator-status-snapshot.md:978-982`] Clause (P) is correct, but `replay_index_len > 0` would be slightly crisper than tying emptiness to `current_stream_seq == 0`.

   **Optional fix:**
   ```md
   - *Trigger.* `since_stream_seq = Some(s_c)` and `replay_index_len > 0` and `s_c < ring_oldest_seq`, regardless of whether `last_known_fingerprint` was supplied (`(Some(stale_seq), None)` is the canonical case this clause closes). If `replay_index_len == 0`, clause (P) is inapplicable.
   ```
