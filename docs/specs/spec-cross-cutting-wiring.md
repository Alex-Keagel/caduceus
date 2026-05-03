# Spec: Cross-Cutting Wiring (caduceus + caduceus-zed)

## §0 Header & Attribution

- **Spec ID:** `spec-cross-cutting-wiring`
- **Tier:** P (cross-cutting platform contract)
- **Status:** Draft, scope-locked
- **Audience:** Implementers of `caduceusd`, host agent runner engines, `caduceus-zed` clients, and anyone wiring an external surface (CLI, IDE, automation) into the system.
- **RFC-2119:** This document uses MUST / MUST NOT / SHOULD / SHOULD NOT / MAY in the senses defined by RFC 2119 / RFC 8174.
- **Z-namespace:** Invariants in this spec are tagged `Z7-W#` ("Wiring"). They are disjoint from `Z6-*` (status snapshot) and the unsubscripted `Z-*` series (orchestrator algorithm) and may be cited from those specs without renumbering.
- **Attribution / source material:**
  - `spec-orchestrator-status-snapshot.md` — clock-capture invariant (Z6-K1), wire-version negotiation, redaction boundary, transport-trust classification.
  - `spec-caduceus-orchestrator-algorithm.md` — single-authority mutation (I-1), no global clock (I-7), boot/shutdown ordering of the orchestrator.
  - `m-e2e-architecture.md` — `~/.copilot/`-style persistence layout, IPC namespace conventions, event taxonomy, audit log shapes.
  - `symphony-orch-collab.md` — agent-expose contract (process invocation, transport, handshake), cross-agent observability.
  - `symphony-fit-analysis.md` — observability model, retry/backoff, token accounting, structured logging notes.
  - `c2-storage-wire-design.md` — storage-vs-wire decoupling pattern (versioning §3.10 inherits this discipline).
- **Non-attribution:** Anything tagged `(caduceus-new)` is original to caduceus and has no Symphony/M counterpart.

This spec is the **wiring layer**. It does not own any subsystem; it owns the contracts, conventions, and invariants that every subsystem MUST honor so that the system as a whole composes without drift. Any rule here is binding on every process: `caduceusd`, every host agent runner / engine, every editor-side client (`caduceus-zed`), every CLI tool, every test harness.

---

## §1 Scope

### 1.1 In scope

This spec normatively covers the following cross-cutting concerns:

1. **ID conventions.** Generation, encoding, equality, lifetime, and cross-process exchange of all primary identifiers: `RunId`, `SessionId`, `RepoId`, `WorkspaceId`, `TaskId`, `RequestId`, `BootId`, `ProcessId` (logical), `AgentId`, `ToolCallId`, `EventSeq`. (§3.1, §4.1, §5 Z7-W1..W7.)
2. **Time and clocks.** Monotonic vs. wall-clock semantics, the projection function from monotonic to wall, NTP failure tolerance, replay/log timestamp ordering, clock skew across processes. Echoes and refines `Z6-K1` from `spec-orchestrator-status-snapshot.md` §3.1. (§3.2, §4.2, §5 Z7-W8..W12.)
3. **Error envelopes.** A single canonical `Error` type, a closed `ErrorCode` enumeration, wrap/unwrap rules across IPC, source-chain preservation, secret redaction at the envelope boundary, and the relationship between transport errors and domain errors. (§3.3, §4.3, §5 Z7-W13..W17.)
4. **Logging and tracing.** Structured-log field taxonomy, mandatory correlation fields, IPC propagation of correlation context, sampling rules, retention policy, and the boundary between developer logs and audit logs. (§3.4, §4.4, §5 Z7-W18..W21.)
5. **Telemetry.** Opt-in policy, the PII boundary, event schema, redaction obligations, and sink contract. Telemetry MUST NOT be a substitute for logs or audit, and vice versa. (§3.5, §4.5, §5 Z7-W22..W24.)
6. **Configuration.** Layer order (defaults → system → user → workspace → session → ephemeral overrides), reload semantics (hot vs. boot-only keys), validation failure handling, and the relationship between `ConfigSnapshot` and live state. (§3.6, §4.6, §5 Z7-W25..W27.)
7. **Feature flags.** Source of truth, evaluation locality (must be deterministic for a given context tuple within a single boot), drift handling between processes, and the relationship between flags and config keys. (§3.7, §4.7, §5 Z7-W28..W30.)
8. **Secrets.** Keychain integration, in-memory handling, redaction in logs/telemetry/errors, rotation hooks, and the NEVER-log invariant. (§3.8, §4.8, §5 Z7-W31..W34.)
9. **Boot and shutdown ordering across processes.** The cross-process startup DAG, daemon supervision of engines, the editor-side connect handshake, graceful shutdown waves, and crash-recovery contracts. Cross-references `spec-system-topology` (forward reference; that spec does not yet exist at the time of writing). (§3.9, §5 Z7-W35..W39.)
10. **Versioning.** Spec versions (this and sibling specs), wire versions (per IPC surface), storage versions (per on-disk file family), capability negotiation, and the canonical mismatch behavior. Cross-references `spec-orchestrator-status-snapshot.md` §3.4.1 (pre-v1 boot_id absence) and `c2-storage-wire-design.md` (storage ≠ wire). (§3.10, §4.9, §5 Z7-W40..W43.)
11. **Acceptance + glossary + out-of-scope + open questions.** Codified test contract (§6), terms (§2), explicit non-goals (§7), and known-unresolved items (§8).

### 1.2 Out of scope

This spec does **not** specify:

- The choice of RPC transport (Unix-domain socket vs. TCP-loopback vs. named pipe vs. stdio framing). Wire contracts here are transport-agnostic; the transport selection is owned by `spec-system-topology`.
- The choice of structured-log format on the wire (JSON Lines vs. CBOR vs. msgpack). The `LogRecord` shape (§4.4) is normative; its serialization is a transport detail.
- The choice of telemetry vendor or sink protocol (OTLP, statsd, Application Insights, etc.). The `TelemetryEvent` shape (§4.5) and the redaction obligations are normative; the sink wire is not.
- The choice of keychain backend (macOS Keychain vs. Secret Service / libsecret vs. Windows DPAPI vs. file-encrypted fallback). The `SecretRef` shape (§4.8) and the NEVER-log invariant are normative; the backend is not.
- Subsystem-specific algorithms (orchestrator scheduling, status snapshot construction, agent runner protocol, conversation history shape, storage engine internals). Those live in their respective specs and only cite this one.
- UI presentation (zed surface layout, CLI argument grammar, error message phrasing).
- Cryptographic primitives beyond redaction (no signing, no encryption-at-rest specification — those go in `spec-secrets-at-rest` if/when written).

---

## §2 Terms

| Term | Definition |
|---|---|
| **AgentId** | Stable identifier of a logical agent role (e.g. `claude-code`, `gpt-5-codex`, `cursor-agent`). String, kebab-case, ASCII. Not a `Uuid`. |
| **AuditLog** | Append-only, retention-bound, redaction-aware record of security-relevant or compliance-relevant events. Disjoint sink from developer logs. |
| **BootId** | A `Uuid v7` minted by a process at startup. Stable for the process lifetime; changes on every restart. Used to disambiguate before-and-after-restart state. See `Z6-K1` and `Z7-W2`. |
| **caduceus-new** | Tag indicating original caduceus content with no Symphony/M parity citation. |
| **ConfigLayer** | One of `defaults`, `system`, `user`, `workspace`, `session`, `ephemeral`, ordered low-to-high precedence. |
| **ConfigSnapshot** | An immutable, fully merged view of all layers at a point in time, with provenance per key. |
| **ErrorCode** | A closed enum of stable string identifiers for error classes. New codes MUST be added by spec amendment. |
| **ErrorEnvelope** | The canonical wire-and-storage representation of an error: code, message, source chain, context fields, redaction-tier. |
| **EventSeq** | A per-process, monotonically increasing `u64` assigned to outgoing events on a given stream. Resets on `BootId` change. |
| **FeatureFlag** | A typed, named knob whose value is determined by a deterministic function of `(name, context)` against a `FlagPolicy` derived from `ConfigSnapshot`. |
| **LocallyTrusted** | Same definition as `spec-orchestrator-status-snapshot.md` §1.2: an IPC peer co-located on the same host and authenticated via OS-level credentials (uid/SID/peer-cred). Loopback-TCP without peer-cred is **not** locally trusted. |
| **LogRecord** | The canonical structured log shape (§4.4). |
| **MonoNow** | A monotonic instant local to one process. Comparable only with other `MonoNow` from the same process and same `BootId`. |
| **PII** | Personally identifiable information. Exact taxonomy in §3.5.2. |
| **ProcessId (logical)** | Caduceus-internal `(role, BootId)` pair, distinct from the OS pid. |
| **RedactionBoundary** | The point in the pipeline at which secrets and PII MUST be replaced with `*REDACTED*` or a structured redaction marker. Always at or before the moment a value crosses out of a process to a non-locally-trusted peer; sometimes earlier. |
| **RepoId** | A stable `Uuid v7` identifying a repository **as registered with caduceus**, not a git remote URL or path. Survives moves and renames. |
| **RequestId** | A `Uuid v7` minted by the originator of an RPC. Echoed in all logs, errors, and responses associated with that request. |
| **RunId** | A `Uuid v7` identifying a single agent run (one orchestrator session of work). Lives strictly inside one `SessionId`. |
| **SecretRef** | An opaque handle to a secret. The plaintext is resolved only when needed and never serialized. |
| **SessionId** | A `Uuid v7` identifying a user-facing session (a conversation / collaboration boundary). Spans many `RunId`s. |
| **SpecVersion** | Semver `MAJOR.MINOR.PATCH` of an owning spec. Distinct from a wire version. |
| **TaskId** | A `Uuid v7` identifying a unit of work scheduled by the orchestrator. Strictly inside one `RunId`. |
| **TelemetryEvent** | The canonical telemetry shape (§4.5). |
| **ToolCallId** | A `Uuid v7` identifying one tool invocation by an agent. Strictly inside one `TaskId`. |
| **WallNow** | A wall-clock instant. Subject to NTP and operator clock changes. Comparable across processes only as an approximation; never authoritative for ordering. |
| **WireVersion** | Semver applied to a single IPC surface (e.g. `status_snapshot/1.3.0`). Negotiated at handshake. Disjoint from `SpecVersion`. |
| **WorkspaceId** | A `Uuid v7` identifying a logical workspace (potentially multi-repo). Spans many `SessionId`s. |

Glossary cross-references: `spec-orchestrator-status-snapshot.md` §2 and `spec-caduceus-orchestrator-algorithm.md` §2 take precedence for terms they define authoritatively. This spec does not redefine them; it only names them.

---

## §3 Normative algorithms

### 3.1 Identifier generation and exchange

#### 3.1.1 `mint_id(kind: IdKind) -> Uuid`

**Purpose.** Produce a fresh identifier of the given kind. All caduceus IDs that are described as `Uuid` in this or any sibling spec MUST be `Uuid v7` (RFC 9562 §5.7) unless the spec defining the field explicitly opts into a different version with rationale.

**Inputs.**

- `kind: IdKind` — one of `Run`, `Session`, `Repo`, `Workspace`, `Task`, `Request`, `Boot`, `ToolCall`. (`AgentId` is **not** in `IdKind`; it is a string, see §2.)

**Output.** A `Uuid` that satisfies all of:

1. Version-7 layout (48-bit unix-millisecond timestamp prefix, 12 bits `rand_a`, 62 bits `rand_b`, version+variant fields per RFC 9562).
2. The 48-bit timestamp prefix MUST be the wall-clock reading at mint time, in unix milliseconds. (See §3.2 for the relationship between this wall-clock read and the monotonic timeline.)
3. The 74 random bits MUST come from a CSPRNG. `rand()`-style PRNGs are forbidden.

**Steps.**

1. Read wall-clock once: `t_ms = wall_now().unix_millis()`.
2. Read 74 random bits from the OS CSPRNG (`getrandom`, `BCryptGenRandom`, `/dev/urandom`).
3. Lay out per RFC 9562 §5.7. If two consecutive calls within the same millisecond from the same process produced equal `(t_ms, rand_b high 12 bits)`, increment `rand_a` to preserve intra-process monotonicity.
4. Return.

**Side effects.** None observable. MUST NOT log the minted ID at debug level (the caller decides log level; this routine is silent).

**Notes.**

- Caduceus does not use Uuid v4 for primary IDs. Existing code that accepts both v4 and v7 inputs (e.g. for backwards compatibility with imported sessions) MUST tag such inputs as "imported" and not mint new v4s itself.
- The cross-process monotonicity property of v7 is **not** load-bearing for ordering. For ordering, see §3.2 (clocks) and §3.4 (`EventSeq`).

#### 3.1.2 ID kinds, lifetimes, and containment

The relationship among IDs is hierarchical and strict. (caduceus-new — codifies what was implicit in `symphony-orch-collab.md` Part A.)

```
WorkspaceId
  └── SessionId
        └── RunId
              └── TaskId
                    └── ToolCallId
```

- A `RunId` MUST be associated with exactly one `SessionId` for its entire lifetime.
- A `TaskId` MUST be associated with exactly one `RunId` for its entire lifetime.
- A `ToolCallId` MUST be associated with exactly one `TaskId` for its entire lifetime.
- A `SessionId` MAY span more than one `WorkspaceId` only via a documented "session move" operation (out of scope here).
- `RepoId` is **orthogonal** to the hierarchy above: a `WorkspaceId` references zero or more `RepoId`s, and a `RepoId` may belong to many workspaces.
- `RequestId` is **orthogonal** to the hierarchy: it correlates one RPC, not one work item.
- `BootId` is per-process and orthogonal to all of the above.

#### 3.1.3 Cross-process equality and stringification

- IDs are compared as 128-bit values, not as strings. Implementations MUST normalize before equality only if they accept multiple input encodings (see Z-9 in `spec-orchestrator-status-snapshot.md`).
- The canonical wire encoding is the lowercase hyphenated form: `01927a3a-7e49-7c30-9b4e-3b27c0aa18cc`. Uppercase forms MUST be accepted on input and MUST be emitted in the canonical lowercase form. Braced or URN forms (`{…}`, `urn:uuid:…`) MUST NOT be emitted; receivers MAY accept them.
- Storage MAY use the 16-byte raw form. The wire MUST use the canonical string form unless the wire spec explicitly opts into raw bytes (none currently does).
- Logs MUST emit IDs as the canonical string form. An ID MUST NOT be truncated in logs at the field level; UI surfaces MAY truncate for display.

#### 3.1.4 `EventSeq`

`EventSeq` is a per-process `u64` counter, one independent counter per outgoing logical stream (each unique `(stream_kind, peer_id)`). It satisfies:

- Starts at 0 on each `BootId`.
- Increments by 1 for each emitted event on the stream.
- Is included in every event so receivers can detect gaps.
- A receiver that observes a regression (`seq_n+1 <= seq_n`) MUST treat the stream as faulted and reconnect (see also `Z6-O1`).

`EventSeq` MUST NOT be used as a wall-clock surrogate or for cross-process ordering.

### 3.2 Time and clocks

#### 3.2.1 The two clocks

Every caduceus process maintains two clocks:

1. **Monotonic** (`mono_now() -> MonoNow`): the platform's `clock_gettime(CLOCK_MONOTONIC)` / `mach_absolute_time` / `QueryPerformanceCounter`. Never decreases. Has no defined relationship to wall time. Resets across reboots (and is implementation-defined across `BootId`).
2. **Wall** (`wall_now() -> WallNow`): the platform's `clock_gettime(CLOCK_REALTIME)` / `GetSystemTimePreciseAsFileTime`. Subject to NTP slew, NTP step, manual operator changes, and (on virt) host suspend/resume jumps. May go backwards.

This spec elevates the convention from `spec-orchestrator-status-snapshot.md` §3.1 (`Z6-K1`) into a system-wide rule:

> **Z6-K1 (echoed and broadened):** All ordering and elapsed-duration arithmetic MUST use `MonoNow`. `WallNow` is for human-readable display, audit logging, telemetry, and cross-process best-effort correlation only. No comparator anywhere in caduceus may resolve correctness on `WallNow` alone.

This spec further requires:

#### 3.2.2 `project_mono_to_wall(t_mono: MonoNow) -> WallNow`

**Purpose.** Convert a monotonic instant captured by **this process** into a wall-clock instant suitable for display or for emission on a wire that consumers expect to be wall-clock.

**Inputs.**

- `t_mono`: a `MonoNow` taken in the current process, after the most recent boot.

**Output.** A `WallNow` value.

**Definition.**

At process boot, each process MUST capture exactly one anchor pair `(M0: MonoNow, W0: WallNow)` immediately after initialization completes (`Z6-K1` step 0). Then:

```
project_mono_to_wall(t_mono) = W0 + (t_mono - M0)
```

If `t_mono < M0`, the projection is undefined and the routine MUST return an error or a sentinel `WallNow::EPOCH`. Callers MUST NOT pass such values; they indicate a programming error.

The anchor pair MUST be re-captured **only** on detection of a wall-clock step exceeding `WALL_STEP_THRESHOLD` (default `±30s`), and the new anchor MUST be emitted on the diagnostic log channel as a `clock_resync` event (§3.4.4).

**Side effects.** None on the steady-state path. On resync, emits one log event.

**Caveats.**

- The projection is **process-local**. A `WallNow` emitted by process P1 and a `WallNow` emitted by P2 for the same external event will differ by up to the maximum of (P1's anchor skew vs. true UTC) + (P2's anchor skew vs. true UTC). Receivers MUST tolerate at least `±2s` of cross-process skew on otherwise-identical events without flagging an inconsistency.
- The projection MUST NOT be used to "correct" historical wall times. Once a `WallNow` has been written to disk or sent on a wire, it is immutable.

#### 3.2.3 NTP failure tolerance

A process whose host has lost NTP synchronization MUST continue to operate. Specifically:

- Monotonic time is unaffected; all internal scheduling, timeouts, and elapsed-duration math MUST keep working.
- Wall-time emissions are produced as best-effort and tagged `wall_quality: degraded` in the `LogRecord` and `TelemetryEvent` (§4.4, §4.5) when the OS reports clock-not-synchronized status (`adjtimex(2)` `STA_UNSYNC` on Linux; equivalent flags on macOS/Windows).
- A daemon MUST NOT refuse to start because its host clock is unsynchronized. (caduceus-new — Symphony was silent here.)

#### 3.2.4 Replay and log timestamps

- Every persisted record (log, audit, replay-index, telemetry) MUST carry **both** `t_mono_unix_micros` (the projection of the originating `MonoNow` at emit time, in microseconds since the unix epoch as projected through the process's anchor) **and** `t_wall_unix_micros` (a fresh `WallNow.unix_micros()` read at emit time). Consumers ordering records from a single process MUST sort by `(BootId, EventSeq)` first, by `t_mono_unix_micros` second, and only fall back to `t_wall_unix_micros` when both prior keys are absent.
- A consumer merging records from multiple processes for human display MAY sort by `t_wall_unix_micros`. Such a display is best-effort and MUST NOT be the basis of any control-flow decision.

### 3.3 Error envelopes

Caduceus uses a single canonical error type across every IPC surface, every persisted error, and every log error field. Subsystems MUST NOT define their own wire-visible error shapes.

#### 3.3.1 Shape

```rust
pub struct Error {
    pub code: ErrorCode,           // closed enum, see §3.3.2
    pub message: String,           // human-readable, may be redacted
    pub context: BTreeMap<String, JsonValue>, // structured, redaction-aware
    pub source: Option<Box<Error>>,// chain; bounded depth (Z7-W14)
    pub redaction_tier: RedactionTier, // see §3.8.4
    pub request_id: Option<Uuid>,  // correlation
    pub at_mono_unix_micros: i64,  // when the error was MINTED
    pub at_wall_unix_micros: i64,  // see §3.2.4
}
```

Field rules:

- `code`: MUST be one of the values listed in §3.3.2. Receivers encountering an unknown code MUST treat it as `internal` and log a `unknown_error_code` warning with the unknown string preserved in `context`.
- `message`: MUST be a stable, mostly-static string. Per-instance variability (paths, IDs) goes in `context`, not interpolated into `message`. This makes deduplication and translation tractable.
- `context`: MUST contain only redaction-aware values; see §3.8.4.
- `source`: MUST NOT exceed `MAX_SOURCE_DEPTH = 8` (Z7-W14). When wrapping would exceed it, the wrapper MUST collapse the deepest two entries by concatenating their messages and codes into a single composite `source` entry.
- `redaction_tier`: MUST be the maximum (most-restrictive) tier of any field that contributed to this error.

#### 3.3.2 `ErrorCode` (closed enum)

The complete set of codes at this spec version. New codes MUST be added by amendment.

| Code | Meaning | Retry hint |
|---|---|---|
| `ok` | reserved sentinel; never carried by a real error | n/a |
| `internal` | unhandled internal fault | no |
| `invariant_violation` | a `Z*` invariant tripped | no |
| `not_found` | named entity does not exist | no |
| `already_exists` | named entity exists when uniqueness was required | no |
| `permission_denied` | OS permission, keychain ACL, etc. | no |
| `unauthenticated` | peer did not present credentials | no |
| `precondition_failed` | request preconditions not met (e.g. wrong session state) | no |
| `failed_precondition` | alias retained for proto compatibility; SHOULD NOT be used in new code | no |
| `aborted` | operation aborted by orchestrator/user | no |
| `cancelled` | operation cancelled by caller | no |
| `deadline_exceeded` | timeout | yes (with backoff) |
| `unavailable` | transient — service not currently available | yes (with backoff) |
| `resource_exhausted` | quota / DoS clamp hit | sometimes |
| `out_of_range` | value outside valid range | no |
| `invalid_argument` | malformed input | no |
| `unimplemented` | route exists, behavior is not implemented | no |
| `data_loss` | unrecoverable data corruption detected | no |
| `version_mismatch` | wire- or storage-version negotiation failure | no |
| `transport_closed` | the transport closed before the operation completed | yes (with backoff) |
| `secret_unavailable` | a referenced secret could not be resolved | no |
| `flag_evaluation_failed` | feature flag evaluator failed on a non-graceful flag | no |

Retry semantics: callers MUST honor the retry hint. The orchestrator's retry/backoff (`symphony-fit-analysis.md` §2.6.1) is parameterized by this column.

#### 3.3.3 `wrap_error(inner: Error, context: WrapContext) -> Error`

**Purpose.** Add a layer of context to an existing error while preserving the chain.

**Steps.**

1. Compute the new `code`: if `context.override_code` is `Some(c)` and `c != inner.code`, use `c`; else propagate `inner.code`.
2. Compute the new `message`: from `context.message`, which MUST be a static string.
3. Merge `context.fields` into the new top-level `context`. **Do not** copy `inner.context` upward; consumers walking the chain see it via `source`.
4. Set `redaction_tier = max(inner.redaction_tier, context.redaction_tier)`.
5. Set `source = Some(Box::new(inner))`. If `MAX_SOURCE_DEPTH` would be exceeded, collapse per §3.3.1.
6. Set `at_mono_unix_micros` and `at_wall_unix_micros` to the wrap time, not the inner time.
7. Carry `request_id` from `inner` unless `context.request_id` is `Some`, in which case the latter wins (this allows an RPC boundary to attach its own correlation).

#### 3.3.4 Crossing the IPC boundary

When an `Error` crosses from process P1 to P2 over a wire:

- P1 MUST apply redaction at-or-before serialization (Z7-W31). Specifically, P1 MUST walk the chain and replace every field whose containing record has `redaction_tier >= peer_max_visible_tier(P2)` with the redaction marker. The mapping `peer -> max_visible_tier` is fixed by the trust classification (§3.8.4).
- P2 MUST treat the received `Error` as authoritative metadata; P2 MUST NOT attempt to reconstruct the un-redacted fields.
- `request_id` MUST survive the boundary unchanged.
- `at_*_unix_micros` MUST survive unchanged. P2 MUST NOT replace them with its own clock readings; it MAY annotate the error in its **own** log with its receive time as a separate field.

#### 3.3.5 Transport errors vs. domain errors

A transport error (TCP RST, broken pipe, framing violation) MUST be surfaced to the application as `code = transport_closed` (or `unavailable` for transient-but-recoverable conditions). The application layer MUST NOT receive a synthetic domain-shaped error invented by the transport.

A domain error returned by an RPC MUST always be wrapped in the canonical `Error`, even when the underlying language has its own exception type. "Bare" exceptions / panics MUST be caught at the IPC boundary and converted to `code = internal` with the original type/message preserved in `context.cause_kind` / `context.cause_message`.

### 3.4 Logging and tracing

#### 3.4.1 Two sinks, three streams

Every caduceus process emits to **two** sinks and produces **three** logical streams.

**Sinks:**

1. **Developer log sink** (`devlog`): structured `LogRecord`s for engineers debugging the system. Sampled. Bounded retention. May contain redacted PII (with redaction markers).
2. **Audit log sink** (`audit`): structured `AuditRecord`s for security and compliance. Never sampled. Long retention. MUST NOT contain any PII or secret material at all (not even redacted markers — the boundary excludes those records by construction).

**Streams** (orthogonal to sinks):

1. **Foreground** — emitted as part of normal operation.
2. **Diagnostics** — emitted only when `diagnostics_enabled = true` (a config key, §3.6).
3. **Trace** — emitted only when `trace_enabled = true`, may include very high-volume per-event records.

The sink-stream matrix:

| | Foreground | Diagnostics | Trace |
|---|---|---|---|
| Devlog | always on | gated | gated |
| Audit | always on | n/a | n/a |

Audit MUST NOT be turned off by configuration. Audit MAY be redirected (sink path), MUST NOT be silenced.

#### 3.4.2 `LogRecord` field taxonomy

See §4.4. The mandatory fields on every record are:

- `t_mono_unix_micros`, `t_wall_unix_micros` (§3.2.4)
- `wall_quality` ∈ `{ ok, degraded }`
- `level` ∈ `{ trace, debug, info, warn, error, fatal }`
- `process` — `{ role, boot_id, pid }`
- `module` — Rust-style path of the emitter
- `message` — static; variability goes in `fields`
- `fields` — structured, redaction-aware
- `correlation` — `{ request_id?, run_id?, task_id?, tool_call_id?, session_id? }`
- `redaction_tier`
- `event_seq` — per-process per-stream, see §3.1.4

#### 3.4.3 Correlation propagation

When process P1 calls process P2 over IPC, P1 MUST send the correlation tuple `(request_id, run_id?, task_id?, tool_call_id?, session_id?)` in the request envelope. P2 MUST attach all five (when present) to every `LogRecord` it emits while processing that request. P2 MUST NOT invent correlation IDs that did not arrive on the wire.

When P2 itself initiates an outgoing call (e.g. an engine calling an external LLM), it MAY mint a new `RequestId` for that outgoing call, but MUST log both the inbound `request_id` and the outbound one in the same record. (caduceus-new — Symphony's notion was looser.)

#### 3.4.4 Reserved diagnostic events

The following event names are reserved on the `devlog` sink. Implementations MUST emit them at the prescribed times and MUST NOT use these names for any other purpose:

| Event | When | Stream | Required fields |
|---|---|---|---|
| `boot` | once, after init | foreground | `boot_id`, `role`, `version`, `git_sha`, `start_args_hash` |
| `clock_resync` | each anchor re-capture | diagnostics | `old_anchor`, `new_anchor`, `delta_micros` |
| `version_negotiated` | each successful handshake | foreground | `surface`, `local_version`, `remote_version`, `agreed_version` |
| `version_mismatch` | each failed handshake | foreground | `surface`, `local_version`, `remote_version`, `mismatch_reason` |
| `subscribe_storm_clamped` | when Z6-O1 fires | diagnostics | `peer`, `topic`, `dropped_count` |
| `redaction_applied` | when a non-trivial redaction happens at an IPC boundary | diagnostics | `peer`, `tier`, `field_path` |
| `secret_resolve_failed` | each failed `resolve_secret` | foreground | `secret_ref_id`, `error_code` |
| `flag_evaluator_failed` | each failed `eval_flag` on a non-graceful flag | foreground | `flag`, `error_code` |
| `shutdown_wave_complete` | per wave during shutdown | foreground | `wave`, `duration_micros` |

#### 3.4.5 Sampling and retention

- Devlog `info`/`warn`/`error`/`fatal` MUST NOT be sampled.
- Devlog `debug` MAY be sampled at a configured rate (default 1.0).
- Devlog `trace` MAY be sampled aggressively (default 0.01) and MAY be ring-buffered in memory and only flushed on a triggering event (e.g. `error`+).
- Audit MUST NOT be sampled.
- Default retention: devlog 14 days, audit 365 days. Both bounded by total bytes (`devlog_max_bytes`, `audit_max_bytes`) with oldest-first eviction.

### 3.5 Telemetry

#### 3.5.1 Opt-in policy

Telemetry is **off by default**. A user MUST take an explicit, per-install affirmative action to enable telemetry. Once enabled, the consent is per-`UserId` (a stable, salted hash of the OS account; see §4.5) and is honored by every caduceus process.

A daemon MUST NOT emit telemetry until it has read the consent state and confirmed `opt_in = true`. The consent is read at boot and watched for changes; revocation MUST cause emission to stop within `TELEMETRY_REVOKE_DEADLINE = 5s`.

#### 3.5.2 PII boundary

PII includes (non-exhaustive): file paths, source code, file contents, prompts, completions, repository names other than the salted `RepoId`, branch names, commit messages, hostnames, IP addresses, email addresses, OS user names, screen content. Telemetry MUST NOT contain PII.

The redaction and PII boundaries are not the same:

- A `LogRecord` in the devlog **may** carry redacted PII, marked with `*REDACTED*` and a tier.
- A `TelemetryEvent` MUST NOT carry PII at all, redacted or otherwise — the field is absent.

#### 3.5.3 `emit_telemetry(event: TelemetryEvent) -> ()`

**Steps.**

1. If `consent.opt_in == false`: return without side effect.
2. Validate `event` against the `TelemetryEvent` schema (§4.5). On failure, emit a devlog `telemetry_validation_failed` and drop.
3. Fill in `t_wall_unix_micros`, `wall_quality`, `app_version`, `boot_id`.
4. Apply the closed-allowlist redaction filter: any field whose key is not in the per-event-type allowlist MUST be dropped; any value type-mismatching its declared schema MUST be dropped.
5. Hand to the sink. The sink contract is buffered, lossy on backpressure, and MUST NOT block the caller for more than `TELEMETRY_EMIT_BUDGET = 1ms`.

### 3.6 Configuration

#### 3.6.1 Layer order

From lowest to highest precedence:

1. `defaults` — compiled-in.
2. `system` — `/etc/caduceus/config.toml` (or platform equivalent).
3. `user` — `~/.config/caduceus/config.toml` (or `~/.caduceus/config.toml` on platforms using XDG-fallback).
4. `workspace` — `<workspace_root>/.caduceus/config.toml`.
5. `session` — set by an editor or CLI for a particular session, persisted under `~/.caduceus/sessions/<session_id>/config.toml`.
6. `ephemeral` — set by environment variable (`CADUCEUS_*`) or by `--set key=value` on the command line. Not persisted.

A higher layer's value for a key wins. **Lists do not merge.** A higher layer that defines `tools.allowed = ["a"]` replaces a lower layer's `tools.allowed = ["a", "b", "c"]` entirely. (caduceus-new — explicit non-merge rule.)

A higher layer **omitting** a key does not unset it; the lower layer's value remains. To explicitly unset, a layer MUST contain `key = { _unset = true }`.

#### 3.6.2 `load_config(layers: &[ConfigLayer]) -> Result<ConfigSnapshot, Error>`

**Steps.**

1. For each layer in order, parse and validate against the schema. A parse error in any layer fails the whole load with `code = invalid_argument` and `context.layer = <layer>`.
2. Compute the merged snapshot per §3.6.1.
3. For each key, record provenance: the layer that won. Provenance is exposed in `ConfigSnapshot.provenance[key]`.
4. Run cross-key validators (e.g. "if `mode = managed` then `daemon.socket_path` MUST be set"). Failure → `code = precondition_failed`.
5. Stamp `snapshot_id = mint_id(Boot)` (re-use the IdKind; this is a snapshot identity, not a process identity). Update on each successful reload.

#### 3.6.3 Reload semantics

Each key is annotated in the schema with one of:

- `reload = hot` — change applies on next read after reload.
- `reload = on_session_boundary` — change applies on the next new `SessionId`.
- `reload = on_run_boundary` — change applies on the next new `RunId`.
- `reload = boot_only` — change requires a process restart.

A `caduceusd` reload that touches a `boot_only` key MUST log a `boot_only_change_pending` warning and MUST NOT apply that key. An editor-side client that sees a `boot_only` change MUST surface a "restart required" indication to the user.

#### 3.6.4 Validation failures

If a reload's new candidate snapshot fails validation, the existing snapshot remains active, and the reload is reported as failed with the canonical `Error`. Partial application is forbidden (Z7-W26).

### 3.7 Feature flags

#### 3.7.1 Source of truth

Feature flags are derived from the merged `ConfigSnapshot`. There is no separate "feature flag service" in caduceus; the source of truth is `<config>.flags.<name>`. (caduceus-new — explicit choice. Symphony's design left this open.)

This means flag drift between processes is bounded by config-snapshot drift, which in turn is bounded by reload latency.

#### 3.7.2 `eval_flag(name: &str, ctx: &FlagContext) -> FlagValue`

**Inputs.**

- `name`: e.g. `experiments.parallel_tool_calls`.
- `ctx`: a `FlagContext` carrying available correlation IDs (`session_id?`, `run_id?`, `user_id?`, `workspace_id?`).

**Output.** A typed `FlagValue` (`Bool(bool)` | `Int(i64)` | `Str(String)` | `Json(JsonValue)`).

**Steps.**

1. Look up `name` in the schema. If unknown:
   - If the config schema marked the flag as `unknown_policy = graceful`, return the schema default and emit `flag_evaluator_failed` warning at most once per `(boot_id, name)`.
   - Else return an `Error` with `code = flag_evaluation_failed`.
2. Resolve the value from the snapshot. If a per-context override is configured (`flags.<name>.overrides[predicate] = value`), evaluate predicates in declaration order. Predicates are pure functions of `ctx` (e.g. `ctx.session_id == "..."`). The first match wins.
3. Validate the value's type matches the schema. Type mismatch → `code = invalid_argument`.
4. Return.

**Determinism.** Within a single `BootId`, two calls to `eval_flag(name, ctx)` with byte-equal `ctx` and the same `ConfigSnapshot.snapshot_id` MUST return the same value. (Z7-W29.)

#### 3.7.3 Cross-process drift

Two processes evaluating the same `(name, ctx)` MAY return different values if their `ConfigSnapshot.snapshot_id` differs. Code paths that require cross-process flag agreement (e.g. an orchestrator and an engine deciding together whether a feature is on) MUST exchange `snapshot_id` and refuse to proceed when they differ for a `cross_process_consistent = required` flag.

The set of `cross_process_consistent = required` flags is small and explicitly enumerated in the config schema. Default is `false`.

### 3.8 Secrets

#### 3.8.1 Storage

Secrets are stored in the OS keychain (or platform equivalent) under a fixed namespace `caduceus`. They are never written in plaintext to any caduceus-managed file.

A secret is referenced by a `SecretRef`:

```rust
pub struct SecretRef {
    pub id: SecretId,             // stable Uuid v7
    pub label: String,            // for UI; not the secret
    pub kind: SecretKind,         // ApiKey, OAuthRefresh, Password, etc.
    pub created_at_wall: i64,     // wall micros
    pub rotation_due_at_wall: Option<i64>,
}
```

#### 3.8.2 `resolve_secret(secret_ref: &SecretRef) -> Result<SecretValue, Error>`

**Steps.**

1. Open the OS keychain in the `caduceus` namespace.
2. Look up by `secret_ref.id`. Not found → `code = secret_unavailable`, with `context.secret_ref_id` set (the **id**, not the value).
3. Read into a `SecretValue` newtype that:
   - Implements `Drop` to zero the buffer.
   - Has no `Display` / `Debug` impl that exposes the value; default `Debug` is `"<redacted>"`.
   - Has no `Serialize` impl. Attempting to serialize it MUST be a compile-time error or a runtime panic, never a silent emission.
4. Return.

#### 3.8.3 NEVER-log invariant

A `SecretValue` MUST NEVER appear in plaintext in:

- any `LogRecord.fields`,
- any `TelemetryEvent`,
- any `AuditRecord`,
- any `Error.message` or `Error.context`,
- any IPC payload,
- any persisted state file other than the keychain.

If a code path is forced to compose a string that would contain a secret (e.g. an outbound HTTP `Authorization` header), the composition MUST happen in the smallest possible scope and the result MUST NOT be logged. A redaction marker (see §3.8.4) is logged in its place.

#### 3.8.4 Redaction tiers

| Tier | Visible to | Examples |
|---|---|---|
| `T0_public` | anyone | static strings, code identifiers |
| `T1_local` | locally-trusted peers | file paths, repo names, log lines without PII |
| `T2_user_pii` | the originating user only | prompts, completions, file contents, branch names |
| `T3_secret` | nobody (keychain only) | API keys, passwords, OAuth refresh tokens |

A peer's `peer_max_visible_tier` is determined by trust classification:

- A `LocallyTrusted` peer with the same OS user: `T2`.
- A `LocallyTrusted` peer with a different OS user (rare; typically a system service): `T1`.
- A non-locally-trusted peer: `T1`.
- Telemetry sink: `T0`.

Crossing a boundary, all fields with `tier > peer_max_visible_tier` MUST be replaced with the redaction marker:

```json
{ "_redacted": true, "tier": "T2_user_pii", "kind": "string" }
```

#### 3.8.5 Rotation

A secret with `rotation_due_at_wall.is_some() && now > rotation_due_at_wall` is **rotation-due**. The keychain layer SHOULD emit a `secret_rotation_due` audit event once per `BootId` per ref. Caduceus does not auto-rotate; rotation is performed by a human or external system. (caduceus-new.)

### 3.9 Boot and shutdown ordering

#### 3.9.1 Process roles

The runtime topology comprises:

- `caduceusd` — the long-lived daemon.
- `engine[i]` — short-lived host agent runner processes spawned by the daemon.
- `client[j]` — `caduceus-zed` (or another editor / CLI) processes, lifetime independent of `caduceusd`.
- `tooling` — ad-hoc CLI invocations (admin, migration, replay).

Cross-references: `spec-caduceus-orchestrator-algorithm.md` §3.1 (`start_service`) and §3.6 (`on_shutdown`) own the orchestrator's internal ordering. This spec owns the cross-process ordering.

#### 3.9.2 Boot sequence (cold)

```
caduceusd:
  W0  read config
  W1  capture clock anchor (Z6-K1 step 0)
  W2  open keychain handle
  W3  open audit sink (Z7-W37: audit MUST be open before any other emission)
  W4  open devlog sink
  W5  emit `boot` event
  W6  open IPC listener (no accept yet)
  W7  bring orchestrator subsystems online (per spec-caduceus-orchestrator-algorithm.md §3.1)
  W8  begin accepting client connections
  W9  begin spawning engines on demand
```

Until `W3` completes, the daemon MUST NOT emit any structured log or telemetry. Pre-`W3` errors MUST be written to stderr only.

`engine` (spawned by daemon):

```
  E0  inherit from daemon: agreed wire version (handshake parameters, env)
  E1  capture own clock anchor
  E2  open audit-relay (forwards to daemon's audit sink) and devlog-relay
  E3  emit `boot`
  E4  signal `ready` to daemon
```

`client[j]`:

```
  C0  read user config
  C1  capture own clock anchor
  C2  open devlog sink (client-local file)
  C3  emit `boot`
  C4  attempt to connect to `caduceusd`; on success run `negotiate_version` (§3.10)
  C5  on negotiation success, subscribe to status snapshot stream (per spec-orchestrator-status-snapshot.md)
```

#### 3.9.3 Shutdown sequence (graceful)

Triggered by SIGINT/SIGTERM/admin RPC. The daemon MUST execute the following waves in order. Each wave MUST complete (or hit its `wave_deadline`) before the next begins.

```
Wave 1 (≤ 250ms):  stop accepting new client connections; stop accepting new run starts
Wave 2 (≤ 5s):     ask orchestrator to drain in-flight runs (cancellable points only)
Wave 3 (≤ 10s):    SIGTERM engines; await their `shutdown_complete` events
Wave 4 (≤ 1s):     flush devlog buffers; flush telemetry buffer
Wave 5 (≤ 250ms):  flush audit sink; emit `shutdown_complete`; close audit sink last
```

Engine shutdown wave (within Wave 3):

```
  ES1  receive SIGTERM
  ES2  cancel current tool call (if any) at next cancellable point
  ES3  flush devlog-relay
  ES4  emit `shutdown_complete`
  ES5  exit 0
```

If a wave's deadline is hit, the daemon escalates to the next wave anyway; processes that did not complete are killed with SIGKILL at the end of Wave 3 (engines) or are abandoned (clients are not killed by the daemon).

#### 3.9.4 Crash recovery

A daemon coming up after a crash (i.e. a previous instance did not reach `shutdown_complete`) MUST:

1. Read the previous instance's last persisted state.
2. Mark every then-running `RunId` as `aborted_by_crash` in the run journal, with a synthesized `Error { code = aborted, message = "daemon restart" }`.
3. Increment the per-run `crash_count` counter for telemetry/audit.
4. Continue with the normal cold boot.

Engines orphaned by a daemon crash MUST detect the daemon's disappearance (the IPC peer-cred socket closes) and exit cleanly within `ENGINE_ORPHAN_DEADLINE = 5s` (Z7-W38).

### 3.10 Versioning and wire negotiation

#### 3.10.1 Version dimensions

Caduceus tracks **three** orthogonal version axes:

1. **SpecVersion**: the version of an owning spec document. Affects the universe of legal behaviors.
2. **WireVersion**: the version of one IPC surface (e.g. `status_snapshot/1.3.0`, `tool_call/0.4.1`). Negotiated per-surface at handshake.
3. **StorageVersion**: the version of one on-disk file family (e.g. `run_journal/1.0`, `replay_index/2.1`). Determined by the file's own header; not negotiated.

These axes MUST NOT be conflated. In particular: the storage type MAY differ in shape from the wire type for the same logical entity (per `c2-storage-wire-design.md`). A spec change that only affects storage MUST NOT bump any wire version.

#### 3.10.2 `negotiate_version(local: &VersionTuple, remote: &VersionTuple) -> Result<VersionTuple, Error>`

For each negotiable surface, both peers send their supported version range `[min, max]`. The agreed version is `max(local.min, remote.min) ≤ v ≤ min(local.max, remote.max)`, choosing the maximum `v` if any exists.

Failure modes:

- Disjoint ranges → `code = version_mismatch`, with both ranges in `context`.
- A surface marked `optional`: a peer that doesn't speak it at all MUST receive a `version_mismatch` only if the peer needs that surface. Otherwise the surface is silently disabled for that peer.

#### 3.10.3 Pre-v1 (`0.x`) policy

For surfaces with `agreed_version.major == 0`:

- Forward compatibility is **not** guaranteed. A change in `0.MINOR` MAY break clients that reported support for the older `0.MINOR-1`.
- Receivers MUST tolerate unknown fields **only** if the sender's wire version is `>= 1.0.0`. Unknown fields under `0.x` SHOULD trigger a hard validation failure to surface drift early.

This is the same policy referenced by `spec-orchestrator-status-snapshot.md` §3.4.1 (the pre-v1 boot_id absence rule); this spec hereby owns it for all surfaces.

#### 3.10.4 Post-v1 (`>= 1.0.0`) policy

- Minor and patch bumps MUST be backwards-compatible: receivers ignore unknown fields, senders never remove fields, types of existing fields never change. (The classic protobuf rules.)
- Major bumps are intentionally breaking and require both peers to support the new range.
- Storage versions follow the same rules independently per file family.

#### 3.10.5 Mismatch behavior

When `version_mismatch` is unavoidable:

- Daemon ↔ engine: the daemon refuses to spawn the engine and surfaces the error to the run that requested it. The engine MUST exit with non-zero.
- Daemon ↔ client: the daemon closes the connection after sending the error. The client surfaces a non-blocking notification ("update caduceus-zed") and degrades to read-only mode (it MAY still display cached state but MUST NOT mutate).
- Storage version too new for current binary: the binary MUST refuse to read and exit with `data_loss` only after escalating. The default is `version_mismatch`, telling the operator to upgrade.


---

## §4 Data shapes

This section defines the canonical types. Field-level invariants are in §5. Serialization (JSON / msgpack / etc.) is per-surface, but the JSON form below is the default unless a sibling spec specifies otherwise.

### 4.1 Identifier types

```rust
pub struct Uuid(pub [u8; 16]);            // canonical 128-bit
pub struct RunId(pub Uuid);
pub struct SessionId(pub Uuid);
pub struct WorkspaceId(pub Uuid);
pub struct RepoId(pub Uuid);
pub struct TaskId(pub Uuid);
pub struct ToolCallId(pub Uuid);
pub struct RequestId(pub Uuid);
pub struct BootId(pub Uuid);

pub struct AgentId(pub String);            // kebab-case ASCII, 1..64 chars

pub enum IdKind {
    Run, Session, Workspace, Repo, Task, ToolCall, Request, Boot,
}
```

JSON form:

```json
{ "run_id": "01927a3a-7e49-7c30-9b4e-3b27c0aa18cc" }
```

`AgentId` JSON form: `"agent_id": "claude-code"`.

### 4.2 Time types

```rust
pub struct MonoNow(pub i128);              // nanoseconds since some unspecified epoch
pub struct WallNow(pub i64);               // microseconds since unix epoch (UTC)

pub struct ClockAnchor {
    pub mono: MonoNow,
    pub wall: WallNow,
    pub captured_at_boot_id: BootId,
}

pub enum WallQuality { Ok, Degraded }
```

JSON form for time fields in records:

```json
{ "t_mono_unix_micros": 1731350123456789, "t_wall_unix_micros": 1731350123456789, "wall_quality": "ok" }
```

`t_mono_unix_micros` is **the projection** of the originating `MonoNow` to wall via the process's anchor; it is in the same unit as `t_wall_unix_micros` so consumers can compare them, but it is **not** a wall-clock value (it does not move when the OS clock moves).

### 4.3 Error envelope

```rust
pub enum ErrorCode {
    Internal, InvariantViolation, NotFound, AlreadyExists,
    PermissionDenied, Unauthenticated, PreconditionFailed,
    Aborted, Cancelled, DeadlineExceeded, Unavailable,
    ResourceExhausted, OutOfRange, InvalidArgument, Unimplemented,
    DataLoss, VersionMismatch, TransportClosed,
    SecretUnavailable, FlagEvaluationFailed,
}

pub enum RedactionTier { T0Public, T1Local, T2UserPii, T3Secret }

pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub context: BTreeMap<String, JsonValue>,
    pub source: Option<Box<Error>>,
    pub redaction_tier: RedactionTier,
    pub request_id: Option<RequestId>,
    pub at_mono_unix_micros: i64,
    pub at_wall_unix_micros: i64,
}
```

JSON form (un-redacted, locally-trusted):

```json
{
  "code": "deadline_exceeded",
  "message": "tool call timed out",
  "context": {
    "tool_call_id": "0192...",
    "deadline_micros": 30000000
  },
  "source": null,
  "redaction_tier": "T1_local",
  "request_id": "0192...",
  "at_mono_unix_micros": 1731350123456789,
  "at_wall_unix_micros": 1731350123456800
}
```

JSON form (redacted at boundary):

```json
{
  "code": "invalid_argument",
  "message": "completion contained banned token",
  "context": {
    "completion_excerpt": { "_redacted": true, "tier": "T2_user_pii", "kind": "string" }
  },
  ...
}
```

### 4.4 LogRecord

```rust
pub enum LogLevel { Trace, Debug, Info, Warn, Error, Fatal }
pub enum LogStream { Foreground, Diagnostics, Trace }

pub struct ProcessIdent {
    pub role: String,        // "caduceusd" | "engine" | "client" | "tooling"
    pub boot_id: BootId,
    pub os_pid: u32,
    pub agent_id: Option<AgentId>,  // only set on engine processes
}

pub struct CorrelationCtx {
    pub request_id: Option<RequestId>,
    pub run_id: Option<RunId>,
    pub task_id: Option<TaskId>,
    pub tool_call_id: Option<ToolCallId>,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
    pub upstream_request_id: Option<RequestId>, // §3.4.3 outgoing-call case
}

pub struct LogRecord {
    pub t_mono_unix_micros: i64,
    pub t_wall_unix_micros: i64,
    pub wall_quality: WallQuality,
    pub level: LogLevel,
    pub stream: LogStream,
    pub event: String,           // event-name; static; reserved set in §3.4.4
    pub message: String,         // static phrase
    pub fields: BTreeMap<String, JsonValue>,
    pub process: ProcessIdent,
    pub module: String,          // e.g. "caduceus_orchestrator::scheduler"
    pub correlation: CorrelationCtx,
    pub redaction_tier: RedactionTier,
    pub event_seq: u64,
}
```

Constraints:

- `event` MUST match `^[a-z][a-z0-9_]{0,63}$`.
- `message` SHOULD be `<= 200` chars.
- `fields` size SHOULD be `<= 16 KiB` after JSON serialization. Larger values are dropped per `Z7-W19`.

### 4.5 AuditRecord

```rust
pub struct AuditRecord {
    pub t_wall_unix_micros: i64,        // wall is authoritative for audit
    pub t_mono_unix_micros: i64,        // for relative ordering inside a process
    pub wall_quality: WallQuality,
    pub event: String,                  // closed enum, see below
    pub actor: AuditActor,
    pub subject: AuditSubject,
    pub outcome: AuditOutcome,          // Allowed | Denied | Errored
    pub fields: BTreeMap<String, JsonValue>, // redaction-aware, T1_local max
    pub process: ProcessIdent,
    pub correlation: CorrelationCtx,
    pub event_seq: u64,
}

pub enum AuditActor {
    User { user_id_salted: String },
    Agent { agent_id: AgentId, run_id: RunId },
    System,
}

pub enum AuditSubject {
    Secret { secret_ref_id: SecretId },
    Config { key: String },
    Run { run_id: RunId },
    Tool { tool_call_id: ToolCallId, tool_name: String },
    Connection { peer: String },
}
```

The `event` enum is closed and MUST be one of:

`secret_resolved`, `secret_resolve_denied`, `secret_rotation_due`,
`config_loaded`, `config_reload_failed`, `config_unset_attempted`,
`run_started`, `run_completed`, `run_aborted_by_crash`,
`tool_call_started`, `tool_call_completed`, `tool_call_denied`,
`connection_accepted`, `connection_rejected`, `version_negotiated`, `version_rejected`.

### 4.6 TelemetryEvent

```rust
pub struct TelemetryEvent {
    pub kind: TelemetryKind,            // closed enum
    pub t_wall_unix_micros: i64,
    pub wall_quality: WallQuality,
    pub app_version: String,            // build version
    pub boot_id: BootId,
    pub user_id_salted: String,         // BLAKE3(user_secret_salt || os_user)[..16]
    pub fields: BTreeMap<String, JsonValue>, // strict allowlist per kind
}

pub enum TelemetryKind {
    DaemonBoot, DaemonShutdown,
    ClientConnect, ClientDisconnect,
    RunStart, RunComplete,
    ToolCallStart, ToolCallComplete,
    FlagEvaluated,
    ErrorReport,                      // redacted, code-only
}
```

Per-kind allowlists (excerpt):

- `RunComplete`: `{ duration_micros: i64, outcome: "ok"|"aborted"|"errored", num_tool_calls: u32, agent_id: AgentId }`
- `ToolCallComplete`: `{ duration_micros: i64, outcome: "ok"|"denied"|"errored", tool_name: String, agent_id: AgentId }`
- `ErrorReport`: `{ code: ErrorCode, location_module: String }` — **no message, no context**.

Any field outside the allowlist for its `kind` MUST be dropped at emit (§3.5.3 step 4).

### 4.7 ConfigSnapshot

```rust
pub struct ConfigSnapshot {
    pub snapshot_id: Uuid,                       // bumped on each successful reload
    pub created_at_wall_unix_micros: i64,
    pub values: BTreeMap<String, JsonValue>,     // dotted keys
    pub provenance: BTreeMap<String, ConfigLayerName>,
    pub schema_version: SemVer,
}

pub enum ConfigLayerName {
    Defaults, System, User, Workspace, Session, Ephemeral,
}
```

Schema annotations (per-key, in the schema, not in the snapshot):

- `type`: one of `bool`, `int`, `string`, `enum<...>`, `list<T>`, `object<...>`, `secret_ref`.
- `reload`: `hot` | `on_session_boundary` | `on_run_boundary` | `boot_only`.
- `default`: any value of `type`.
- `redaction_tier`: default `T1_local`. Keys named for secrets MUST be `T3_secret` (and they store a `SecretRef`, not a value).

### 4.8 FeatureFlag types

```rust
pub struct FlagContext {
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub user_id_salted: Option<String>,
    pub workspace_id: Option<WorkspaceId>,
    pub repo_id: Option<RepoId>,
    pub agent_id: Option<AgentId>,
}

pub enum FlagValue {
    Bool(bool), Int(i64), Str(String), Json(JsonValue),
}

pub struct FlagSchemaEntry {
    pub name: String,
    pub kind: FlagKind,                  // Bool, Int, Str, Json
    pub default: FlagValue,
    pub unknown_policy: UnknownPolicy,   // Graceful | Strict
    pub cross_process_consistent: CrossProcessPolicy, // Optional | Required
}
```

### 4.9 SecretRef

(See §3.8.1 for the definition.)

```rust
pub struct SecretId(pub Uuid);

pub enum SecretKind {
    ApiKey, OAuthRefresh, OAuthAccess, Password, ClientCertPrivKey, Other(String),
}
```

`SecretRef` is `Serialize`/`Deserialize`. `SecretValue` is **not** `Serialize`. (See §3.8.2.)

### 4.10 Version types

```rust
pub struct SemVer { pub major: u32, pub minor: u32, pub patch: u32 }

pub struct VersionRange { pub min: SemVer, pub max: SemVer }

pub struct VersionTuple {
    pub surface: String,                 // e.g. "status_snapshot"
    pub range: VersionRange,
    pub capabilities: BTreeSet<String>,  // optional capability strings
}
```

### 4.11 BootManifest

```rust
pub struct BootManifest {
    pub role: String,
    pub boot_id: BootId,
    pub app_version: SemVer,
    pub git_sha: String,
    pub start_at_wall_unix_micros: i64,
    pub config_snapshot_id: Uuid,
    pub anchor: ClockAnchor,
    pub spec_versions: BTreeMap<String, SemVer>,   // owned specs only
    pub wire_versions_supported: Vec<VersionTuple>,
}
```

The `BootManifest` is emitted as the payload of the `boot` event (§3.4.4) and is also persisted to `~/.caduceus/run/<role>/boot/<boot_id>.json` for crash diagnostics.

---

## §5 Invariants

The `Z7-W*` namespace is owned by this spec. Every invariant is a MUST. Numbering is stable: do not renumber on amendment; deprecate in place.

### 5.1 Identifiers (Z7-W1..W7)

- **Z7-W1.** Every primary ID (`RunId`, `SessionId`, `WorkspaceId`, `RepoId`, `TaskId`, `ToolCallId`, `RequestId`, `BootId`) MUST be a `Uuid v7` minted by `mint_id` (§3.1.1) at the originating process.
- **Z7-W2.** Every process MUST mint exactly one `BootId` at startup, after the clock anchor is captured and before the `boot` event is emitted. The `BootId` MUST appear in every `LogRecord`, `AuditRecord`, and `TelemetryEvent` emitted by that process.
- **Z7-W3.** ID containment MUST hold: a `RunId` belongs to exactly one `SessionId`; a `TaskId` belongs to exactly one `RunId`; a `ToolCallId` belongs to exactly one `TaskId`. A receiver detecting a containment violation MUST treat the offending record as malformed and discard it (logging an `invariant_violation` warning).
- **Z7-W4.** Wire stringification of `Uuid` MUST be canonical lowercase hyphenated (`"01927a3a-7e49-7c30-9b4e-3b27c0aa18cc"`). Senders MUST emit this form; receivers MUST accept canonical, uppercase, braced, and URN forms but MUST normalize to canonical before equality comparison.
- **Z7-W5.** IDs MUST NOT be truncated in any structured field. UI display MAY truncate; the underlying record MUST carry the full value.
- **Z7-W6.** `EventSeq` per `(stream_kind, peer_id)` MUST start at 0 on each `BootId`, increment by 1 per emitted event, and never regress. A receiver detecting `seq_n+1 <= seq_n` on a stream MUST treat the stream as faulted (cross-ref `Z6-O1`).
- **Z7-W7.** `AgentId` MUST match `^[a-z][a-z0-9-]{0,63}$`. New agents MUST NOT collide with the reserved ASCII prefix `caduceus-` except for officially-vended agents.

### 5.2 Time (Z7-W8..W12)

- **Z7-W8.** All ordering and elapsed-duration arithmetic MUST use `MonoNow`. No comparator MAY resolve correctness on `WallNow` alone. (Echo of `Z6-K1` extended to all subsystems.)
- **Z7-W9.** Each process MUST capture exactly one `ClockAnchor` immediately after init (Z6-K1 step 0) and MUST re-capture only on detection of a wall-clock step exceeding `WALL_STEP_THRESHOLD = 30s`, emitting a `clock_resync` diagnostic event for each re-capture.
- **Z7-W10.** `project_mono_to_wall(t_mono)` for `t_mono < anchor.mono` is undefined; callers MUST NOT pass such values, and the routine MUST return a sentinel rather than guess.
- **Z7-W11.** A daemon MUST NOT refuse to start because its host clock is unsynchronized. Wall-time emissions during `STA_UNSYNC` MUST be tagged `wall_quality = degraded`.
- **Z7-W12.** Persisted records MUST carry **both** `t_mono_unix_micros` and `t_wall_unix_micros`. Once written, neither field MAY be rewritten or "corrected" by any later process.

### 5.3 Errors (Z7-W13..W17)

- **Z7-W13.** Every IPC-visible error MUST be the canonical `Error` (§4.3). Subsystems MUST NOT define alternative error wire types. Bare exceptions/panics MUST be caught at the IPC boundary and converted to `code = internal` with the original kind/message preserved in `context.cause_kind` / `context.cause_message`.
- **Z7-W14.** `Error.source` chain depth MUST NOT exceed `MAX_SOURCE_DEPTH = 8`. When wrapping would exceed it, the wrapper MUST collapse the deepest two entries into one composite entry; the chain length MUST NOT grow without bound.
- **Z7-W15.** `Error.message` MUST be a static string (no per-instance interpolation). Per-instance variability MUST live in `Error.context`.
- **Z7-W16.** `Error.code` MUST be a value of the closed enum in §3.3.2. New codes require a spec amendment. Receivers encountering unknown codes MUST treat them as `internal` and log `unknown_error_code`.
- **Z7-W17.** When an `Error` crosses an IPC boundary, the sender MUST apply redaction per §3.8.4 against the peer's `peer_max_visible_tier`. The receiver MUST NOT attempt to reconstruct redacted fields.

### 5.4 Logging and tracing (Z7-W18..W21)

- **Z7-W18.** Audit MUST NOT be sampled, MUST NOT be silenced by configuration (sink path MAY be redirected), and MUST NOT contain any value at tier `T2_user_pii` or `T3_secret`, redacted or not.
- **Z7-W19.** A `LogRecord.fields` map exceeding `LOG_FIELDS_MAX_BYTES = 16 KiB` after JSON serialization MUST be replaced (whole map) with `{ "_oversized": true, "original_bytes": <n> }`. Truncation of individual values is forbidden because it can leak partial secrets.
- **Z7-W20.** Correlation propagation: a process MUST attach the inbound `(request_id, run_id?, task_id?, tool_call_id?, session_id?)` tuple to every `LogRecord` emitted while processing the corresponding request. A process MUST NOT invent a `RequestId` it did not receive on the wire and present it as inbound; outgoing-call `RequestId`s MUST be tagged `correlation.upstream_request_id` distinct from `correlation.request_id`.
- **Z7-W21.** Reserved event names (§3.4.4) MUST NOT be reused for unrelated events. A `LogRecord` with one of these `event` names MUST carry the prescribed fields.

### 5.5 Telemetry (Z7-W22..W24)

- **Z7-W22.** Telemetry MUST be off by default. A process MUST NOT emit any `TelemetryEvent` until consent is read and confirmed `opt_in = true`. Revocation MUST stop emission within `TELEMETRY_REVOKE_DEADLINE = 5s`.
- **Z7-W23.** `TelemetryEvent` MUST NOT carry any value at tier `T2_user_pii` or higher, redacted or not. Fields outside the per-`kind` allowlist MUST be dropped at emit.
- **Z7-W24.** A telemetry sink MUST NOT block its caller longer than `TELEMETRY_EMIT_BUDGET = 1ms`. On backpressure the sink is lossy: events are dropped and a single `telemetry_drop` diagnostic event is emitted per drop window of 60s.

### 5.6 Configuration (Z7-W25..W27)

- **Z7-W25.** Layer precedence MUST be `defaults < system < user < workspace < session < ephemeral`. Lists MUST NOT merge across layers; a higher layer's list value REPLACES the lower layer's.
- **Z7-W26.** A failed reload MUST leave the existing `ConfigSnapshot` active. Partial application is forbidden. A reload that touches a `boot_only` key MUST log `boot_only_change_pending` and MUST NOT apply that key.
- **Z7-W27.** `ConfigSnapshot.snapshot_id` MUST be re-minted on every successful reload. Code paths that branch on flag values MAY cache the value keyed by `(snapshot_id, name, ctx_hash)` and MUST invalidate on `snapshot_id` change.

### 5.7 Feature flags (Z7-W28..W30)

- **Z7-W28.** Feature flags MUST be derived from `ConfigSnapshot` only. There MUST NOT be a separate flag service or out-of-band flag source.
- **Z7-W29.** Within a single `BootId` and `ConfigSnapshot.snapshot_id`, two calls to `eval_flag(name, ctx)` with byte-equal `ctx` MUST return the same value.
- **Z7-W30.** For flags marked `cross_process_consistent = required`, two cooperating processes MUST exchange `ConfigSnapshot.snapshot_id` and refuse to proceed when they differ. The default is `cross_process_consistent = optional`.

### 5.8 Secrets (Z7-W31..W34)

- **Z7-W31.** A `SecretValue` MUST NEVER appear in plaintext in any log, audit record, telemetry event, error envelope, IPC payload, or persisted file other than the OS keychain. Any such appearance is a `Z7-W31` violation regardless of intent.
- **Z7-W32.** `SecretValue` MUST NOT be `Serialize`. Attempting to serialize MUST be a compile-time error or a runtime panic; silent emission is forbidden.
- **Z7-W33.** A process resolving a secret MUST emit a `secret_resolved` audit event (success) or `secret_resolve_denied` (failure with `code = permission_denied`) carrying `secret_ref_id` (the **id**, not the value), once per resolution.
- **Z7-W34.** A secret with `now > rotation_due_at_wall` MUST cause a `secret_rotation_due` audit event once per `BootId` per ref. Caduceus MUST NOT auto-rotate.

### 5.9 Boot/shutdown ordering (Z7-W35..W39)

- **Z7-W35.** `caduceusd` boot order MUST be W0 → W9 (§3.9.2). The audit sink MUST be open by W3, **before** any other structured emission. Pre-W3 errors MUST go to stderr only.
- **Z7-W36.** An engine MUST NOT emit on the foreground devlog stream before its `boot` event (E3). Pre-`boot` errors MUST go to a daemon-supervised stderr pipe only.
- **Z7-W37.** Audit sink MUST be the **last** sink closed during shutdown (Wave 5). All earlier waves MAY emit audit; flushing audit happens at the very end.
- **Z7-W38.** An engine whose IPC peer-cred socket to the daemon is observed closed MUST exit cleanly within `ENGINE_ORPHAN_DEADLINE = 5s`. After the deadline, the daemon supervisor (if any) is entitled to SIGKILL the orphan.
- **Z7-W39.** Crash recovery MUST mark every then-running `RunId` from the previous instance as `aborted_by_crash` with a synthesized `Error { code = aborted, message = "daemon restart" }`, increment `crash_count`, and only then begin normal cold boot. A daemon MUST NOT silently resume a run that did not reach a terminal state pre-crash.

### 5.10 Versioning (Z7-W40..W43)

- **Z7-W40.** Wire versions MUST be negotiated per surface. A surface with no agreed version MAY only be used as `optional`; a peer that requires the surface MUST receive `version_mismatch` on absence.
- **Z7-W41.** For `agreed_version.major == 0`, receivers MUST reject unknown fields. For `agreed_version.major >= 1`, receivers MUST tolerate unknown fields and senders MUST NOT remove or retype existing fields.
- **Z7-W42.** Storage versions MUST be carried in the file header (or filename) of each on-disk file family. A binary that cannot read a given storage version MUST surface `version_mismatch` (or `data_loss` only after escalation) and MUST NOT silently corrupt the file.
- **Z7-W43.** A `SpecVersion` change that affects only on-disk shape MUST NOT bump any `WireVersion`; conversely a wire-only change MUST NOT bump any `StorageVersion`. (Storage-vs-wire decoupling per `c2-storage-wire-design.md`.)

### 5.11 Catch-all (Z7-W44..W46)

- **Z7-W44.** No subsystem may introduce a primary ID type, error type, log format, telemetry sink, config layer, flag source, secret store, or version dimension outside the ones enumerated in this spec without a spec amendment.
- **Z7-W45.** Cross-process clock skew tolerance: receivers merging records from multiple processes MUST tolerate at least `±2s` of `t_wall_unix_micros` skew between otherwise-identical events without flagging an inconsistency.
- **Z7-W46.** "Locally trusted" MUST be determined by OS-level peer credentials (uid/SID/peer-cred), never by transport choice alone. A loopback-TCP connection without peer credentials is **not** locally trusted (echo of `spec-orchestrator-status-snapshot.md` §1.2).

---

## §6 Test contract

Tests MUST cover every invariant in §5. Numbering: `T-N` corresponds 1:1 to `Z7-WN`. Tests are written from the perspective of a black-box conformance suite that can run a `caduceusd` plus engines plus a fake-client harness; many tests can also be written as in-process unit tests against the relevant module. Whichever level: each invariant MUST have at least one test that fails when the invariant is violated.

### 6.1 Identifier tests (T-1..T-7)

- **T-1.** Mint 10,000 IDs of each kind from a fresh process; assert all are `Uuid v7` (version nibble `0x7`, RFC 9562 variant), all unique, and the 48-bit timestamp prefix is monotonic non-decreasing in mint order.
- **T-2.** Boot two processes back-to-back; assert each emits exactly one `boot` event with a distinct `BootId`; assert every later `LogRecord` from each process carries its own `BootId`. Restart one process; assert it mints a new `BootId`.
- **T-3.** Drive the orchestrator to create a `SessionId` that owns two `RunId`s, each with two `TaskId`s, each with two `ToolCallId`s. Inject a malformed event claiming a `TaskId` belongs to a different `RunId`; assert the receiver discards it and emits `invariant_violation`.
- **T-4.** Send IDs in canonical, uppercase, braced, and URN forms; assert receiver normalizes and matches; assert outgoing emissions are always canonical lowercase hyphenated.
- **T-5.** Inspect every `LogRecord` emitted in a 10-minute soak run; assert no ID field is truncated. UI snapshot test asserts UI may display a 7-char prefix while the structured field carries the full 36-char form.
- **T-6.** Subscribe to a stream; record `EventSeq` for 1,000 events; assert strictly increasing by 1 starting at 0. Restart the emitter; assert `EventSeq` resets to 0 with the new `BootId`. Inject a regression in a fake server; assert the client treats the stream as faulted and reconnects.
- **T-7.** Property test: `AgentId` validator accepts strings matching `^[a-z][a-z0-9-]{0,63}$` and rejects all others. Reserved-prefix test: registering a non-vended `caduceus-foo` agent fails.

### 6.2 Time tests (T-8..T-12)

- **T-8.** With a fake monotonic clock that advances and a wall clock the test moves backwards, assert that all orchestrator scheduling decisions, timeouts, and run-elapsed-duration values are unaffected by the wall-clock manipulation.
- **T-9.** Boot a process; capture the anchor; step the wall clock by `+45s`; assert the process re-captures the anchor and emits one `clock_resync` diagnostic event with the correct `delta_micros`. Step the wall clock by `+5s` (below threshold); assert no resync.
- **T-10.** Call `project_mono_to_wall(t_mono)` with `t_mono < anchor.mono`; assert the call returns the documented sentinel (`WallNow::EPOCH`) or an error, and never a "guessed" value.
- **T-11.** Run a daemon on a host where `adjtimex` reports `STA_UNSYNC`; assert the daemon starts successfully; assert all `LogRecord`s emitted carry `wall_quality = degraded`; assert recovery to `wall_quality = ok` after sync is restored.
- **T-12.** Append a `LogRecord` to disk; restart the process (new `BootId`, new anchor); assert the persisted record's `t_mono_unix_micros` and `t_wall_unix_micros` are unchanged. Mutation by any later code path is detected by a digest of the file's first MB.

### 6.3 Error tests (T-13..T-17)

- **T-13.** Wrap an `IO_ERROR` from `std::io::Error` at the IPC boundary; assert the wire form is canonical `Error { code: internal, context: { cause_kind: "io::ErrorKind::PermissionDenied", cause_message: ... } }`; assert no bare `IOError` shape leaks to the wire.
- **T-14.** Construct a chain of 12 wraps; assert the persisted/wire form has `source` depth ≤ 8 and the deepest entry is the documented composite collapse with both messages and codes preserved in `context.composite_of`.
- **T-15.** Static analysis / lint test: scan the codebase for `format!` patterns inside `Error::new(... message: format!(...))`; assert zero matches in non-test code. Runtime test: dedup logs by `(code, message)` over a 1-hour run and assert dedup works (≤ 100 distinct keys).
- **T-16.** Drive a fake peer that returns an unknown `code` string; assert the receiver maps to `internal` and emits `unknown_error_code` warning carrying the unknown string in `context`.
- **T-17.** From an internal error containing `T2_user_pii` context (e.g. a file-content excerpt), serialize across an IPC to a non-locally-trusted peer; assert all `T2` fields are replaced by the structured redaction marker, `redaction_tier = T2_user_pii`, and `request_id` survives.

### 6.4 Logging/tracing tests (T-18..T-21)

- **T-18.** Set devlog level to `error` and `audit_max_bytes` to 1 MiB; drive 10,000 audit-relevant operations; assert audit count == 10,000 (not sampled), assert audit retention enforces oldest-first eviction at the 1 MiB cap. Attempt to set `audit_enabled = false` via config; assert the schema rejects the key.
- **T-19.** Emit a `LogRecord` with a 32 KiB `fields` map; assert the persisted record's `fields = { "_oversized": true, "original_bytes": <n> }` (whole-map replacement, no per-field truncation).
- **T-20.** Issue an RPC `R1` carrying `request_id = X` to the daemon; the daemon issues an outbound RPC `R2` to an engine with `request_id = Y`; assert every `LogRecord` emitted by the daemon while processing `R1` has `correlation.request_id = X`; assert `LogRecord`s describing `R2` additionally have `correlation.upstream_request_id = X` and `correlation.request_id = Y`. Assert no `LogRecord` invents an inbound `request_id`.
- **T-21.** Test that emitting a record with `event = "boot"` but missing `boot_id` fails validation; same for each reserved event in §3.4.4.

### 6.5 Telemetry tests (T-22..T-24)

- **T-22.** Boot daemon with `consent.opt_in = false`; drive 1,000 candidate telemetry emissions; assert the sink received 0 events. Flip consent to `true`; assert subsequent emissions land. Flip back to `false`; assert emission stops within 5s and no in-flight buffered events leak.
- **T-23.** Construct a `RunComplete` telemetry event with a `prompt_excerpt` field (not on the allowlist); assert the field is dropped at emit. Construct one with a properly-typed `duration_micros`; assert it lands.
- **T-24.** Block the telemetry sink for 5s; emit 100 events; assert each emit returns within 1ms and that the eventual delivery count is ≤ 100 with one `telemetry_drop` diagnostic per minute of backpressure.

### 6.6 Configuration tests (T-25..T-27)

- **T-25.** Set `tools.allowed = ["a", "b"]` in `system` and `tools.allowed = ["c"]` in `user`; assert the snapshot shows `["c"]` (replacement, not merge), with `provenance["tools.allowed"] = User`. Set `key = { _unset = true }` in `user` over a `system` value; assert the merged snapshot has the key absent.
- **T-26.** Submit a reload that violates a cross-key validator; assert the existing snapshot remains active; assert the call returns an `Error` with `code = precondition_failed`. Submit a reload that changes a `boot_only` key; assert that key is not applied and `boot_only_change_pending` is logged.
- **T-27.** Successful reload bumps `snapshot_id`; cached `eval_flag` results keyed by old `snapshot_id` are invalidated. Verify by changing a flag default and observing the next eval picks up the new value within one snapshot bump.

### 6.7 Feature flag tests (T-28..T-30)

- **T-28.** Try to inject a flag value via an environment variable not also defined as a config key; assert the value is ignored. Confirm `eval_flag("experiments.parallel_tool_calls", ctx)` reads from `<config>.flags.experiments.parallel_tool_calls`.
- **T-29.** Property test: for any `(name, ctx)` and a fixed `snapshot_id`, 100 evaluations return identical `FlagValue`. Counter-test: changing `snapshot_id` MAY change the value.
- **T-30.** Run daemon at `snapshot_id_a` and engine at `snapshot_id_b`; both evaluate `flags.experiments.distributed_planner` (declared `cross_process_consistent = required`); assert one of the peers refuses to proceed with `code = precondition_failed`. Same flag declared `optional`: both proceed and may diverge silently.

### 6.8 Secret tests (T-31..T-34)

- **T-31.** End-to-end test: configure a fake LLM client to read `secret_ref = R`; drive a tool call; capture all stdout, stderr, devlog, audit, telemetry, IPC traffic, and on-disk state files; grep for the secret plaintext; assert zero matches. Repeat with a deliberately leaky test agent and assert the grep DOES match (test-of-the-test).
- **T-32.** Compile-time: a test source file that does `serde_json::to_string(&secret_value)` MUST fail to compile (or panic at runtime if the language doesn't support compile-time enforcement). Verified by `cargo test --no-run` on a fixture crate.
- **T-33.** Resolve a secret successfully; assert one `secret_resolved` audit record carrying `secret_ref_id` (not the value). Resolve a missing secret; assert one `secret_resolve_denied` audit record.
- **T-34.** Set `rotation_due_at_wall` to 1 hour ago; restart daemon; assert one `secret_rotation_due` audit event in this `BootId`; restart daemon again; assert exactly one new event (not zero, not two).

### 6.9 Boot/shutdown tests (T-35..T-39)

- **T-35.** Inject a sink-open failure between W2 and W3; assert no devlog/telemetry is emitted before W3 (only stderr) and that the daemon exits non-zero with the error printed to stderr.
- **T-36.** Spawn an engine with a deliberate pre-`boot`-event panic; assert the panic message is captured by the daemon-supervised stderr pipe and surfaced as the run's `Error`; assert no orphan devlog file is written by the engine.
- **T-37.** Drive a graceful shutdown; capture the order of sink closes; assert audit is the last sink to close.
- **T-38.** Terminate `caduceusd` with SIGTERM-9 (i.e. signal 9); observe an engine; assert the engine exits cleanly within 5s.
- **T-39.** Force-terminate `caduceusd` (signal 9) while a run is in flight; restart; assert the run journal contains an `aborted_by_crash` record for the run with synthesized `Error { code: aborted, message: "daemon restart" }`, that `crash_count` for the run is incremented, and that no run silently resumes.

### 6.10 Versioning tests (T-40..T-43)

- **T-40.** Server supports `status_snapshot/1.0.0..1.3.0`, client supports `1.5.0..2.0.0`; assert `negotiate_version` returns `version_mismatch` with both ranges in `context`. Optional surface: a peer that doesn't speak `replay/0.1` connects without error; required surface: same scenario yields `version_mismatch`.
- **T-41.** Send a payload with an unknown field over `agreed_version = 0.4.0`; assert receiver rejects with `code = invalid_argument`. Send the same over `1.2.0`; assert receiver tolerates the unknown field.
- **T-42.** Read a `run_journal` file with `storage_version = 99`; assert binary surfaces `version_mismatch` and exits non-zero without truncating or rewriting the file.
- **T-43.** Bump the storage version of `replay_index/2.1` → `2.2` (e.g. add a column); confirm `WireVersion` of `replay/0.1` is unchanged. Reverse case: bump `replay/0.1` → `replay/0.2`; confirm `replay_index` storage version is unchanged.

### 6.11 Catch-all tests (T-44..T-46)

- **T-44.** Lint test: scan the codebase for `enum Error|struct Error|enum LogLevel|...` definitions outside the wiring crate; assert zero matches. Same for hand-rolled UUID generators.
- **T-45.** Generate two `LogRecord`s for the same logical event from two processes whose anchors differ by 1.8s; assert a merging consumer emits no inconsistency warning. Repeat at 2.5s; assert a warning IS emitted.
- **T-46.** Connect from loopback TCP without peer-cred; attempt to read a `T2_user_pii` field; assert redaction; attempt over Unix-domain socket with peer-cred matching the OS user; assert no redaction.

### 6.12 Test infrastructure requirements

- The conformance suite SHOULD provide a fake-clock driver (`MonoNow`+`WallNow` controlled by tests).
- The suite SHOULD provide a fake-keychain backend (in-memory, ephemeral) and verify that production builds reject it (`cfg(test)` or build-flag gate).
- The suite SHOULD provide a fault-injection layer for IPC (drop, dup, reorder, regress `EventSeq`, corrupt fields) so that Z7-W6 / Z7-W17 / Z7-W41 negative paths are exercisable.

---

## §7 Out of scope

This spec is deliberately silent on:

1. **Transport selection.** Whether `caduceusd` ↔ engine uses a Unix-domain socket, a named pipe, stdio framing, or loopback TCP is owned by `spec-system-topology` (TBD). The wiring contracts here apply regardless of transport.
2. **Wire serialization format.** JSON Lines vs. CBOR vs. msgpack vs. protobuf. The data shapes in §4 are abstract; per-surface specs choose the encoding.
3. **Telemetry vendor / sink protocol.** OTLP, statsd, Application Insights, and friends. Caduceus has a `TelemetryEvent` abstract type and a redaction obligation; the sink is a plug-in.
4. **Keychain backend implementation.** macOS Keychain vs. Secret Service / libsecret vs. Windows DPAPI vs. encrypted-file fallback. The `SecretRef` shape and the NEVER-log invariant are normative; the backend is not.
5. **Cryptography beyond redaction.** No signing, no encryption-at-rest, no TLS profile is specified here. If a sibling spec needs them, it owns them.
6. **Subsystem-specific algorithms.** The orchestrator scheduler, the status snapshot construction, the agent runner protocol, the conversation history shape, the replay-index storage engine — all live in their own specs and only cite this one.
7. **UI presentation.** Editor surface layout, CLI argument grammar, error message phrasing. Strings exposed to humans pass through localization and presentation layers that this spec does not constrain.
8. **Migration tooling.** Scripts and procedures for upgrading config files, run journals, or replay indices across `StorageVersion` bumps. A migration may produce records that look like this spec's shapes but the migration path itself is outside scope.
9. **Self-update / installer.** How `caduceus-zed` discovers a new `caduceusd` and triggers an update is policy that lives in `spec-distribution` (TBD).
10. **Identity / SSO.** Beyond the `user_id_salted` derivation rule for telemetry/audit, no notion of identity, login, or session-cookie management is owned here.
11. **Multi-tenant deployments.** Caduceus today assumes one `caduceusd` per OS user. A future `spec-multi-tenant` may extend or override the trust classification (§3.8.4); until then the single-user model is normative.
12. **Plugin / extension authoring.** Whether tools, agents, and toolkits are statically linked, dynamically loaded, or sandboxed in subprocesses is owned by their respective specs. Plugins MUST honor every invariant in this spec; the mechanism is theirs to choose.

---

## §8 Open questions

These are tracked items that this spec deliberately leaves unresolved. Each MUST be closed (resolved or explicitly out-of-scope) before this spec leaves Draft status.

1. **Q-OQ-01: Should `MAX_SOURCE_DEPTH` be 8 or 16?** Symphony observed real chains of 6–7 in practice; 8 leaves headroom for two synthetic wraps (transport + RPC dispatch). 16 is safer but doubles the wire size of pathological errors. Provisional answer: 8. Revisit after first soak.
2. **Q-OQ-02: `WALL_STEP_THRESHOLD = 30s`** is a heuristic. NTP-disciplined hosts step in increments of `< 1s` once steady. Suspended laptops can step by hours. Should the threshold be asymmetric (e.g. `+5s / -1s`)? Provisional answer: symmetric `±30s`; revisit if false-positive resyncs are observed in the field.
3. **Q-OQ-03: `LOG_FIELDS_MAX_BYTES = 16 KiB`** is conservative. Some structured contexts (e.g. parsed AST snippets) routinely exceed this. Should we provide a "spillable" mechanism that writes the oversized payload to a side-file referenced by id? Risk: side-files are easier to leak. Provisional answer: no spill in v1; teams that need it must shrink the field. Revisit in v2.
4. **Q-OQ-04: Cross-process flag consistency** (Z7-W30) currently fails closed (peers refuse to proceed on mismatch). Should there be a third mode `cross_process_consistent = warn` that logs but does not refuse? Provisional answer: no; warn-mode invites silent inconsistency.
5. **Q-OQ-05: Audit retention default** (365 days) is chosen for "feels-right" reasons, not a regulation. Some deployments will need 7-year retention for compliance. Should this be a template, not a default? Provisional answer: keep the default at 365; document override mechanism.
6. **Q-OQ-06: Shutdown wave deadlines** (250 ms / 5 s / 10 s / 1 s / 250 ms) were lifted from operator experience with M-style daemons. They have not been validated against caduceus-zed's actual workloads. Revisit after the first beta release.
7. **Q-OQ-07: `BootId` semantics on hot-reload of dynamic libraries.** A daemon that hot-reloads a plugin DOES NOT mint a new `BootId`, because the OS process is unchanged. Should it? Provisional answer: no; hot reloads emit a `plugin_reloaded` diagnostic event with the old and new plugin versions, but the `BootId` does not change.
8. **Q-OQ-08: User-identity salt.** `user_id_salted = BLAKE3(user_secret_salt || os_user)[..16]`. Where does `user_secret_salt` live and rotate? Currently: a per-install secret in the keychain, rotated by uninstall+reinstall. Should rotation be supported in-place? Provisional answer: defer to `spec-secrets-at-rest`.
9. **Q-OQ-09: Optional surfaces vs. capability bits.** Some surfaces are optional in the spec but their behavior is mandatory for any peer that speaks them at all. Capability bits offer a finer-grained alternative. Should we promote `capabilities: BTreeSet<String>` (already on `VersionTuple`, §4.10) to first-class, with a registry? Provisional answer: yes, but registry maintenance is owned by `spec-system-topology`.
10. **Q-OQ-10: Redaction tiers and structured logging.** The current scheme tags records, not fields. Field-level tagging would let a single record mix `T1` and `T2` fields and have the boundary redact only `T2`. This is more correct but more complex. Provisional answer: stay record-level in v1; revisit if real workloads suffer over-redaction.
11. **Q-OQ-11: `EventSeq` and reconnection.** When a client reconnects to a daemon and resubscribes to a stream, does the stream's `EventSeq` continue or restart? This spec says it restarts (per `BootId`), which means a reconnecting client must reconcile by some other key (timestamp, content hash). Should reconnections preserve `EventSeq` to ease consumers? Provisional answer: no; reconnection is orthogonal to `BootId`, and tying them invites confusion when the daemon restarted in between.
12. **Q-OQ-12: Telemetry on first run.** A user who has never set consent runs caduceus for the first time. The default is `opt_in = false`. Do we surface a one-time prompt? When? Where? Provisional answer: `caduceus-zed` shows a non-blocking prompt in the status bar on first connect; the CLI does not prompt. The mechanism is owned by `spec-zed-crdt` (UI integration) — this spec only states that telemetry MUST start `false`.
13. **Q-OQ-13: The `system` config layer on Windows.** Linux/macOS use `/etc/caduceus/`. Windows has no such convention; candidate paths include `%PROGRAMDATA%\caduceus\`, `%ALLUSERSPROFILE%\caduceus\`, or HKLM registry. Provisional answer: `%PROGRAMDATA%\caduceus\config.toml`. Revisit if a registry-based override is needed for managed deployments.
14. **Q-OQ-14: Engine cgroup / job-object policy.** Should the daemon place each engine in a per-engine cgroup (Linux) / job object (Windows) so that resource limits and orphan cleanup are enforced by the OS? This spec is silent; it is plausibly owned by `spec-system-topology`. Provisional answer: defer.
15. **Q-OQ-15: Replay-index `StorageVersion` interaction with `EventSeq`.** A replay index that bumps `StorageVersion` MUST decide whether existing `EventSeq` values are preserved or regenerated. Provisional answer: preserved. Cite this answer from the (forthcoming) `spec-replay-index`.

---

## §9 Cross-references

The following sibling specs intersect this one. When a conflict appears, follow the priority rules in `spec-orchestrator-status-snapshot.md` §0 (which itself defers to this spec for cross-cutting concerns).

- **`spec-orchestrator-status-snapshot.md`** — owner of `Z6-K1` (clock anchor capture), the canonical wire-version negotiation example (§3.4.1), and the `subscribe-storm coalescing` invariant `Z6-O1`. This spec extends `Z6-K1` to all subsystems (`Z7-W8..W12`).
- **`spec-caduceus-orchestrator-algorithm.md`** — owner of orchestrator-internal invariants `I-1` (single-authority mutation) and `I-7` (no global clock). This spec's `Z7-W8` is the same rule lifted to the system level. The orchestrator's `start_service` (§3.1) and `on_shutdown` (§3.6) are sub-procedures of this spec's boot/shutdown sequences (§3.9).
- **`spec-caduceus-agent-runner-contract.md`** — owner of the engine ↔ daemon protocol in detail. This spec governs what the engine MUST do at boot, on shutdown, and across IPC; the agent-runner spec governs the per-message protocol on top of those guarantees.
- **`spec-caduceus-collab-patterns.md`** — owner of the multi-agent collaboration patterns. Every pattern's IDs, errors, logs, telemetry, configs, flags, and secrets MUST conform to this spec.
- **`spec-multi-repo-workspace-model.md`** — defines `RepoId` and `WorkspaceId` semantics in detail. Caduceus's wiring MUST treat both as opaque IDs at this layer.
- **`spec-repo-owned-workflow-contract.md`** — workflows owned by a repo carry `RepoId` correlation; this spec demands the correlation field exists (`Z7-W20`).
- **`spec-zed-crdt.md`** — `caduceus-zed` IDE integration. Inherits all wiring contracts; surface-specific UI is out of scope here.
- **`spec-hermes-ide.md`** / **`spec-hermes-ide-supplement.md`** — alternate IDE surface. Same inheritance.
- **`spec-m-permissions.md`** — defines per-tool permission policy. This spec demands that permission decisions are recorded as `tool_call_denied` audit events (§4.5).
- **`spec-m-session-lifecycle.md`** — session boundaries in detail. This spec demands `SessionId` is `Uuid v7` and used in correlation everywhere.
- **`spec-open-multi-agent.md`** — open-protocol multi-agent endpoint. Inherits all wiring contracts; cross-org peers are by definition not locally-trusted (§3.8.4) and therefore see `T0/T1` only.
- **`spec-tree-sitter.md`** / **`spec-qdrant.md`** — internal subsystems for parsing and embedding storage. Do not cross IPC by default; their internal logging is bound by this spec only when records are forwarded to a sink.
- **`spec-claurst-full.md`** / **`spec-claw-code.md`** / **`spec-e2b.md`** — agent and sandbox specs. Each agent is an `AgentId` here (§4.1); each spec MUST honor §3.8 (secrets) and §3.4 (logs).
- **`spec-system-topology.md`** (forward reference; not yet authored) — will own transport selection, port/socket conventions, supervision model, and the registry of optional capabilities (§Q-OQ-09).
- **`spec-secrets-at-rest.md`** (forward reference) — will own keychain rotation, cryptographic at-rest policy.
- **`spec-distribution.md`** (forward reference) — will own self-update and installer behavior.
- **`spec-multi-tenant.md`** (forward reference; if needed) — will own multi-user-on-one-host trust model overrides.
- **`m-e2e-architecture.md`** (input doc, M project) — informed §3.4 (sink/stream taxonomy), §3.6 (config layout), §3.9 (boot/shutdown waves). Caduceus may diverge from M in detail; divergences are tagged `(caduceus-new)`.
- **`symphony-orch-collab.md`** (input doc) — informed §3.1 (ID containment) and §3.9 (cross-process startup). Symphony's looser correlation rules are tightened in `Z7-W20`.
- **`symphony-fit-analysis.md`** (input doc) — informed §3.4.5 (sampling), §3.5 (telemetry shape).
- **`c2-storage-wire-design.md`** (input doc) — informed §3.10.1 (three version axes); `Z7-W43` codifies its core rule.

---

## Appendix A. Canonical examples

### A.1 A successful tool call, end-to-end

The example shows the wiring from `caduceus-zed` issuing a tool call request to the daemon, which dispatches to an engine, which returns. Only fields relevant to wiring are shown.

**Step 1 — client → daemon: request**

```json
{
  "wire_version": "tool_call/1.2.0",
  "request_id": "01927a4e-8000-7a01-9000-c1c1c1c10001",
  "session_id": "01927a3a-7e49-7c30-9b4e-3b27c0aa18cc",
  "run_id": "01927a4e-7800-7b00-9100-c1c1c1c10100",
  "task_id": "01927a4e-7c00-7b00-9100-c1c1c1c10101",
  "tool_call_id": "01927a4e-7e00-7b00-9100-c1c1c1c10102",
  "tool_name": "fs.read_file",
  "args": { "path": "src/main.rs" }
}
```

**Step 2 — daemon devlog**

```json
{
  "t_mono_unix_micros": 1731350123456789,
  "t_wall_unix_micros": 1731350123456790,
  "wall_quality": "ok",
  "level": "info",
  "stream": "foreground",
  "event": "tool_call_dispatched",
  "message": "dispatching tool call",
  "fields": { "tool_name": "fs.read_file" },
  "process": { "role": "caduceusd", "boot_id": "01927a3a-0000-7000-8000-000000000001", "os_pid": 4711 },
  "module": "caduceus_orchestrator::dispatch",
  "correlation": {
    "request_id": "01927a4e-8000-7a01-9000-c1c1c1c10001",
    "session_id": "01927a3a-7e49-7c30-9b4e-3b27c0aa18cc",
    "run_id": "01927a4e-7800-7b00-9100-c1c1c1c10100",
    "task_id": "01927a4e-7c00-7b00-9100-c1c1c1c10101",
    "tool_call_id": "01927a4e-7e00-7b00-9100-c1c1c1c10102"
  },
  "redaction_tier": "T1_local",
  "event_seq": 4711042
}
```

**Step 3 — daemon → engine: request (note `upstream_request_id`)**

```json
{
  "wire_version": "engine_dispatch/1.0.0",
  "request_id": "01927a4e-9000-7a02-9000-c1c1c1c10002",
  "upstream_request_id": "01927a4e-8000-7a01-9000-c1c1c1c10001",
  ...
}
```

**Step 4 — engine devlog (correlation includes both request_ids)**

```json
{
  ...
  "correlation": {
    "request_id": "01927a4e-9000-7a02-9000-c1c1c1c10002",
    "upstream_request_id": "01927a4e-8000-7a01-9000-c1c1c1c10001",
    "session_id": "01927a3a-7e49-7c30-9b4e-3b27c0aa18cc",
    "run_id": "01927a4e-7800-7b00-9100-c1c1c1c10100",
    "task_id": "01927a4e-7c00-7b00-9100-c1c1c1c10101",
    "tool_call_id": "01927a4e-7e00-7b00-9100-c1c1c1c10102"
  },
  ...
}
```

**Step 5 — engine → daemon: response**

```json
{
  "request_id": "01927a4e-9000-7a02-9000-c1c1c1c10002",
  "result": { "content": "...file content..." }
}
```

**Step 6 — daemon → client: response**

```json
{
  "request_id": "01927a4e-8000-7a01-9000-c1c1c1c10001",
  "result": { "content": "...file content..." }
}
```

**Step 7 — daemon audit (note `tool_call_completed`)**

```json
{
  "t_wall_unix_micros": 1731350123459000,
  "t_mono_unix_micros": 1731350123459000,
  "wall_quality": "ok",
  "event": "tool_call_completed",
  "actor": { "agent": { "agent_id": "claude-code", "run_id": "01927a4e-7800-7b00-9100-c1c1c1c10100" } },
  "subject": { "tool": { "tool_call_id": "01927a4e-7e00-7b00-9100-c1c1c1c10102", "tool_name": "fs.read_file" } },
  "outcome": "allowed",
  "fields": {},
  "process": { "role": "caduceusd", "boot_id": "01927a3a-0000-7000-8000-000000000001", "os_pid": 4711 },
  "correlation": {
    "request_id": "01927a4e-8000-7a01-9000-c1c1c1c10001",
    "session_id": "01927a3a-7e49-7c30-9b4e-3b27c0aa18cc",
    "run_id": "01927a4e-7800-7b00-9100-c1c1c1c10100",
    "task_id": "01927a4e-7c00-7b00-9100-c1c1c1c10101",
    "tool_call_id": "01927a4e-7e00-7b00-9100-c1c1c1c10102"
  },
  "event_seq": 113
}
```

**Step 8 — daemon telemetry (consent on)**

```json
{
  "kind": "ToolCallComplete",
  "t_wall_unix_micros": 1731350123459000,
  "wall_quality": "ok",
  "app_version": "0.4.2",
  "boot_id": "01927a3a-0000-7000-8000-000000000001",
  "user_id_salted": "9c5d8e4a1b2c3d4e",
  "fields": {
    "duration_micros": 2200,
    "outcome": "ok",
    "tool_name": "fs.read_file",
    "agent_id": "claude-code"
  }
}
```

Note: `args.path` and `result.content` are PII (`T2`) and **never** appear in audit or telemetry. They appear in the daemon devlog only when `diagnostics_enabled = true` and even then under `T2` redaction when crossing to a non-locally-trusted peer.

### A.2 An error end-to-end

Engine encounters a permission denied reading the requested file:

```json
{
  "code": "permission_denied",
  "message": "denied by tool policy",
  "context": {
    "tool_name": "fs.read_file",
    "policy_rule_id": "no-read-outside-workspace",
    "attempted_path": { "_redacted": true, "tier": "T2_user_pii", "kind": "string" }
  },
  "source": null,
  "redaction_tier": "T2_user_pii",
  "request_id": "01927a4e-9000-7a02-9000-c1c1c1c10002",
  "at_mono_unix_micros": 1731350123458500,
  "at_wall_unix_micros": 1731350123458501
}
```

Daemon wraps for the client:

```json
{
  "code": "permission_denied",
  "message": "tool call denied",
  "context": {
    "tool_call_id": "01927a4e-7e00-7b00-9100-c1c1c1c10102",
    "policy_rule_id": "no-read-outside-workspace"
  },
  "source": { ...the engine error above... },
  "redaction_tier": "T2_user_pii",
  "request_id": "01927a4e-8000-7a01-9000-c1c1c1c10001",
  ...
}
```

When the response crosses to `caduceus-zed` (locally-trusted, same OS user, max tier `T2`), the `attempted_path` field MAY be un-redacted. When the same response crosses to a non-locally-trusted peer (say a remote multi-agent endpoint per `spec-open-multi-agent.md`), it MUST remain redacted.

### A.3 Boot and shutdown observed externally

A test that snapshots the daemon's `LogRecord` stream from W0 to `shutdown_complete` MUST observe (in order, modulo concurrent unrelated events):

```
boot                       (foreground, fields include boot_id, app_version, git_sha)
version_negotiated         (one per surface, foreground)
... steady-state ...
shutdown_wave_complete     (wave=1)
shutdown_wave_complete     (wave=2)
shutdown_wave_complete     (wave=3)
shutdown_wave_complete     (wave=4)
shutdown_wave_complete     (wave=5)
shutdown_complete          (the very last record)
```

A daemon emitting `shutdown_complete` after the audit sink has been closed is a `Z7-W37` violation.

---

## Appendix B. Reserved name registry

The following names are reserved by this spec. Implementations MUST NOT use them for any other purpose.

### B.1 Reserved log event names

`boot`, `clock_resync`, `version_negotiated`, `version_mismatch`,
`subscribe_storm_clamped`, `redaction_applied`, `secret_resolve_failed`,
`flag_evaluator_failed`, `shutdown_wave_complete`, `shutdown_complete`,
`telemetry_drop`, `telemetry_validation_failed`, `unknown_error_code`,
`invariant_violation`, `boot_only_change_pending`, `plugin_reloaded`,
`tool_call_dispatched`.

### B.2 Reserved audit event names

`secret_resolved`, `secret_resolve_denied`, `secret_rotation_due`,
`config_loaded`, `config_reload_failed`, `config_unset_attempted`,
`run_started`, `run_completed`, `run_aborted_by_crash`,
`tool_call_started`, `tool_call_completed`, `tool_call_denied`,
`connection_accepted`, `connection_rejected`, `version_negotiated`,
`version_rejected`.

### B.3 Reserved error codes

(See §3.3.2 for the complete list.) New codes require a spec amendment.

### B.4 Reserved config-key prefixes

`caduceus.*` and `flags.*` are reserved for caduceus-vended config. Plugins MUST namespace their keys under `plugins.<plugin_id>.*` to avoid collision.

### B.5 Reserved `AgentId` prefixes

`caduceus-` is reserved for caduceus-vended agents. Third-party agents MUST NOT begin with this prefix.

---

## Appendix C. Constants

Single source of truth for tunables referenced in the body of this spec.

| Constant | Default | Where |
|---|---|---|
| `MAX_SOURCE_DEPTH` | 8 | §3.3.1, Z7-W14 |
| `WALL_STEP_THRESHOLD` | 30 s | §3.2.2, Z7-W9 |
| `LOG_FIELDS_MAX_BYTES` | 16 KiB | §3.4.2, Z7-W19 |
| `TELEMETRY_REVOKE_DEADLINE` | 5 s | §3.5.1, Z7-W22 |
| `TELEMETRY_EMIT_BUDGET` | 1 ms | §3.5.3, Z7-W24 |
| `ENGINE_ORPHAN_DEADLINE` | 5 s | §3.9.4, Z7-W38 |
| Wave 1 deadline | 250 ms | §3.9.3 |
| Wave 2 deadline | 5 s | §3.9.3 |
| Wave 3 deadline | 10 s | §3.9.3 |
| Wave 4 deadline | 1 s | §3.9.3 |
| Wave 5 deadline | 250 ms | §3.9.3 |
| Devlog default retention | 14 days | §3.4.5 |
| Audit default retention | 365 days | §3.4.5 |
| Cross-process wall skew tolerance | ±2 s | Z7-W45 |

Implementations MAY override these via config (under `caduceus.tunables.*`); deviations from defaults SHOULD be documented in operational runbooks.

---

## Appendix D. Document conventions

- **Diff-friendly numbering.** `Z7-W*` and `T-*` are stable identifiers; do not renumber on amendment. Deprecate in place by editing the entry to begin with `**(deprecated as of vN.M)**` and leaving the number.
- **Code blocks.** Rust-ish syntax is illustrative, not a binding implementation contract. The binding contract is the prose plus the JSON examples.
- **JSON examples.** Every JSON example MUST be parseable. Reviewers SHOULD run `jq` over the spec's example blocks as part of CI.
- **`(caduceus-new)` tags.** Mark divergences from Symphony / M source material so reviewers can audit them quickly.
- **Forward references.** When this spec cites `spec-X.md` that does not yet exist, the citation is a placeholder and the dependency MUST be resolved before this spec leaves Draft.

---

*End of `spec-cross-cutting-wiring.md`.*
