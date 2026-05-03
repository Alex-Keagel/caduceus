# Hardcore Review: spec-orchestrator-status-snapshot.md

- **Date**: 2026-04-29
- **Content Type**: Spec
- **Iteration**: 27
- **Reviewer Model(s)**: claude-opus-4.7, claude-sonnet-4.6, gpt-5.4, gpt-5.3-codex
- **Verdict**: NEEDS WORK

## Scores

| # | Dimension | Score | Min-Reviewer | Notes |
|---|-----------|-------|--------------|-------|
| 1 | Correctness | 5/10 | gpt-5.3-codex | Status/error/replay contracts still conflict. |
| 2 | Completeness | 6/10 | gpt-5.4 | Missing terminal-state and subscribe-error wire paths. |
| 3 | Security | 5/10 | gpt-5.3-codex | Redaction model is still schema-unsafe and under-specified. |
| 4 | Clarity | 6/10 | gpt-5.4 | Duplicate normative logic and transport rules still blur intent. |
| 5 | Architecture | 7/10 | claude-opus-4.7 | Replay/history responsibilities remain conflated. |
| 6 | SRP | 7/10 | claude-opus-4.7 | One constant still governs unrelated buffers. |
| 7 | KISS/YAGNI/DRY | 5/10 | gpt-5.3-codex | Redaction and clause logic are still over-complex and duplicated. |

## Findings

### 🔴 Critical
1. [§1.2 L61-96; §4.1 L1363-1382; §4.5 L1575-1583] — The non-locally-trusted redaction model mutates required v1 shapes in place (`workspace_path`, `WorkspaceMeta.slug`, count-only replacements, undefined `error_kind`). That is not wire-stable and leaves delta-path behavior ambiguous. **Fix:** replace the entire non-local redaction block with a v1 locally-trusted-only rule:

   ```text
   **Remote transport rule (normative).** The v1 snapshot / subscribe surface defined by this spec is locally-trusted only. A conforming v1 implementation MUST expose it only over a locally-trusted transport (Unix domain socket with `SO_PEERCRED` UID match, or a platform-equivalent peer-authenticated local transport).

   A transport that is not locally-trusted MUST NOT carry the v1 wire shapes from §4. The daemon MUST reject `dispatch_snapshot_request` and `subscribe` on such a transport with `SnapshotError::Unavailable` rather than mutating field shapes in-place.

   The §4 data shapes are schema-normative. Replacing required fields with `null`, omitting them, or substituting count-only objects requires a future spec bump with distinct redacted wire types.
   ```

2. [§3.3 L401-425; §3.4.1 L1042-1063; §3.5.1 L1234-1238] — The subscribe stream has no wire-level way to carry `SnapshotError::Unavailable` / `PreV1Unsupported`, yet the spec requires those outcomes. **Fix:** replace the stream envelope / protocol text with:

   ```rust
   enum SubscribeFrame {
       Ack(SubscribeAck),      // exactly one, first frame on successful subscribe.
       Error(SnapshotError),   // terminal; MAY be first frame if subscribe fails before Ack,
                               // or final frame after Ack for terminal stream failure.
       PostAck(PostAckFrame),  // zero or more, only after Ack.
   }
   ```

   ```text
   The first frame is exactly one of `SubscribeFrame::Ack(SubscribeAck)` or
   `SubscribeFrame::Error(SnapshotError)`. `Error(...)` is terminal and MUST
   be followed by stream close. After a successful `Ack`, the stream carries
   zero or more `SubscribeFrame::PostAck(PostAckFrame)` frames.
   ```

3. [§3.2 L317-319; §4.5 L1560-1615; §4.1.1 L1410-1419] — `RunDetail` is defined as available for finished runs in `recent_history_ring`, but `RunRow.status` cannot represent a finished run. **Fix:** replace §4.1.1 with:

   ```rust
   enum RunStatus {
       Running,                          // live engine connection; agent producing turns.
       Retrying,                         // present in snapshot.retrying; not in snapshot.running.
       Disconnected,                     // caduceus-new: engine RPC dropped, daemon retains row.
       Finished { exit_reason: ExitReason }, // present ONLY on RunDetail.row when sourced
                                             // from recent_history_ring.
   }
   ```

   ```text
   `Retrying` only appears on `RetryRow` in practice; a live `RunRow` carries
   `Running` or `Disconnected`. A `RunDetail` built from `recent_history_ring`
   MUST carry `row.status = Finished { exit_reason }`. The `Finished` variant
   MUST NOT appear in `AggregateSnapshot.running` or `.disconnected`.
   ```

### 🟡 Important
1. [§3.4 L639-643, L806-818, L949-959; §4.5 L1549-1551; §3.5.3 L1275-1277] — `recent_history_ring_size` is overloaded for finished-run retention, replay-index retention, and payload-buffer retention. That makes clause-(d) replay too shallow and couples unrelated budgets. **Fix:** replace the clause-(a) trigger sentence with:

   ```text
   EITHER `current_stream_seq < s_c` OR `current_stream_seq − s_c > delta_replay_window_size`
   (the gap exceeds the bounded delta history the daemon has retained).
   `delta_replay_window_size` is a count of retained deltas, owned by §3.4
   (default `1024`). It is distinct from `recent_history_ring_size` (§4.5,
   finished-run summaries, default `32`) and from `broadcast_channel_capacity`
   (§3.5.3, per-subscriber broadcast slots, default `512`); the three buffers
   have independent budgets and MUST NOT share a tuning constant.
   ```

   Also replace every Z-29 occurrence of `recent_history_ring_size` with `delta_replay_window_size`, and add to §4.5:

   ```text
   `recent_history_ring_size` governs the finished-run summary ring only. The
   replay-index window and delta payload buffer are sized by
   `delta_replay_window_size` (§3.4); changing one MUST NOT implicitly change
   the other.
   ```

2. [§4.3 L1496-1501; §4.6 L1664; §3.3 L386-392] — `AggregateUpdated` omits `agents_max`, so a live subscriber cannot reflect hot-reload concurrency changes even though `agents_max` is fingerprint-significant. **Fix:** replace the `AggregateUpdated` variant with:

   ```rust
   AggregateUpdated { stream_seq: u64,
                      tokens_aggregate: TokenTotals,
                      runtime_total: Duration,
                      rate_limit: RateLimitBlob,
                      agents_used: u32,
                      agents_max: u32,
                      next_poll_at: Option<SystemTime>,
                      snapshot_fingerprint: SnapshotFingerprint },
   ```

   And add:

   ```text
   **`agents_max` emission rule (normative).** The daemon MUST emit an
   `AggregateUpdated` delta whenever `Config.max_concurrency` changes,
   carrying the new `agents_max` value.
   ```

3. [§3.4 L555-631; L1013-1027] — The force-resync / closure logic is still duplicated in two normative-sounding blocks. That is unnecessary drift bait. **Fix:** replace the earlier summary intro with:

   ```text
   **First-delta rules (informative quick-reference).** The clause paragraphs
   below are the sole normative authority. In any conflict between this
   summary and the clause paragraphs, the clause paragraphs prevail.
   ```

   And replace the later intro with:

   ```text
   **Canonical closure rule (single source).** This subsection is non-normative
   shorthand. The only normative closure rule is §3.4 “Force-resync triggers
   (closed set, normative — Z-4 / Z-6 / V-7)”. Implementations MUST follow
   that rule verbatim and MUST NOT maintain a second normative copy.
   ```

## Recommended Actions
1. Collapse v1 remote redaction to a transport gate or define separate redacted wire types in a future spec bump.
2. Add explicit subscribe error frames and a finished-run `RunStatus` variant.
3. Split replay retention from finished-run retention and carry `agents_max` in `AggregateUpdated`.
