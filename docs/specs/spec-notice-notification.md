# Notice & Notification System — Specification

> Status: P-tier (Proposed). Normative. Cross-runtime (daemon + UI + CLI/TTY).
> Owner: caduceus / caduceus-zed.
> Layer: surfaces over the orchestrator status snapshot subscription channel
> (see `spec-orchestrator-status-snapshot.md §3.4`) and the permission engine
> (see `spec-m-permissions.md`).

---

## 0. Header & Attribution

### 0.1 Provenance

This specification is derived from internal analysis of two upstream codebases
plus original synthesis:

- **M (internal Microsoft EMU codename "Clawpilot")** — closed-source reference
  studied at commit `ffd8b054c8ee6c562a690d70f3e97ba287e8ad8c`. M is the source
  of the two-surface contract (in-thread `NoticeBanner` vs ambient
  `NotificationToast`), the click-action taxonomy, the OS-fallback rule when
  the host window is unfocused, the burst/coalesce rules, and the
  accessibility role mapping.
- **Symphony (Apache-2.0)** — open-source reference. Source of the
  notification event taxonomy that feeds the daemon-side `Notice` enum and
  the heartbeat tick that drives stale-banner expiry.
- **Original synthesis** — this document. The wire envelope, the cross-runtime
  CLI/TTY rendering, the persistence/history shape, and the exact coupling to
  `spec-orchestrator-status-snapshot.md §3.4` (subscribe / since_fingerprint /
  PostAckFrame) are original to caduceus.

### 0.2 Cleanroom Statement

This specification is a **cleanroom re-expression** of behaviour observed in
the upstream references. Implementers MUST NOT copy code or strings verbatim
from M (which is not licensed for redistribution). Symphony-derived behaviour
MUST be implemented by reading this spec, not by copy-pasting Symphony source;
where Symphony code is referenced for fidelity, the file MUST carry an
Apache-2.0 attribution comment matching the template in `symphony-fit-analysis.md §A`.

The behavioural surface described here is the **specified surface**. Where the
upstream behaviour is ambiguous, this document MAKES A CHOICE and labels it
normatively; the choice is the contract regardless of whether the upstream
made the same choice.

### 0.3 RFC-2119 keywords

The keywords MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT,
RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as
described in RFC 2119, and only when in ALL CAPS.

### 0.4 Document conventions

- Inline invariants are tagged `Z-N` (e.g., `Z-7`, `Z-12`); they appear inside
  normative paragraphs and constrain a single behaviour.
- The MUST list at §5 is tagged `I-N` (e.g., `I-1`, `I-12`); each `I-N` is
  an externally testable promise.
- The test contract at §6 is tagged `T-N`; each `T-N` MUST be exercised by at
  least one automated test before this spec is treated as implemented.
- Cross-spec references use the form `spec-XYZ §N.M`.
- Code identifiers are rendered `like_this`. Wire identifiers are rendered
  `"like_this"` and are normative byte strings.

---

## 1. Scope

### 1.1 In scope

This specification defines:

1. The taxonomy distinguishing a **Notice** (blocking, in-thread, no
   auto-expire, explicit-dismiss-only) from a **Notification** (non-blocking,
   ambient, auto-expiring, queueable, OS-fallbackable).
2. The severity (`Info` / `Warning` / `Error`) and urgency
   (`Background` / `Routine` / `Attention` / `Immediate`) tiers, and the
   in-app → OS-notification → sound escalation policy.
3. The lifecycle of every notice and notification from emission inside the
   daemon through queue, display, acknowledgement / dismissal, and
   persistence.
4. The wire format that carries notices and notifications from `caduceusd`
   to a connected UI (zed pane, CLI, future hosts) over the existing
   orchestrator-status-snapshot subscribe channel
   (`spec-orchestrator-status-snapshot.md §3.4`).
5. Cross-runtime rendering — how a Notice/Notification appears in a TTY/CLI
   client when no GUI is attached.
6. Coalescing, rate-limiting, and suppression rules. A retry-exhausted storm
   MUST NOT produce 50 toasts; the user sees at most one coalesced
   surface.
7. Persistence and the **Notification Center** history view, including
   retention bounds and replay-on-reconnect semantics.
8. The interplay with the permission engine (`spec-m-permissions.md`):
   permission-denied with elevation potential becomes a Notice; permission
   prompt is a special Notice with reply RPCs.
9. The acceptance criteria that gate "this is implemented": the `I-N`
   invariant list, the `T-N` test scenarios, the glossary, and the
   out-of-scope list.

### 1.2 Out of scope

The following are explicitly NOT specified here:

- The visual treatment of notices and toasts (icons, motion curves, exact
  pixels). Visual design is defined in design-system documents that are not
  part of this contract; this spec only constrains the **structural** and
  **timing** behaviour.
- The localisation pipeline. Notices carry an `i18n_key` and an
  `i18n_args` map (§4.1); the resolution of `i18n_key` to a string is
  delegated to the host's localisation layer.
- The crash-reporter / telemetry pipeline. Notices that report errors MAY be
  observed by telemetry, but the telemetry contract is out of scope.
- Any push-notification delivery to a remote device (mobile, email, Teams).
  Out-of-host notification is a future extension; see §8.
- The orchestrator status snapshot itself. This spec consumes that channel as
  a transport; see `spec-orchestrator-status-snapshot.md` for its semantics.
- Permission engine internals. This spec couples to permissions only at the
  boundary; see `spec-m-permissions.md` for the evaluate pipeline.

### 1.3 Transport-trust redaction

When a Notice or Notification crosses an inter-process boundary
(`caduceusd` → host UI, host UI → CLI bridge, persisted history file), the
transport-trust posture for each field MUST be one of:

| Field                   | Trust         | Redaction rule                                  |
| ----------------------- | ------------- | ----------------------------------------------- |
| `id`                    | Trusted       | Verbatim. Stable across reconnect.              |
| `kind`                  | Trusted       | Verbatim. Closed enum.                          |
| `severity`, `urgency`   | Trusted       | Verbatim. Closed enum.                          |
| `i18n_key`              | Trusted       | Verbatim. Closed registry, no PII.              |
| `i18n_args`             | Untrusted     | Each value is opaque; UI MUST treat as text only and MUST NOT interpret as markup. |
| `body_plain`            | Untrusted     | Plain-text fallback. UI MUST escape before rendering. |
| `body_markdown`         | Untrusted     | Markdown fallback. UI MUST render with a sanitiser; raw HTML MUST be rejected.    |
| `action_target` (URL)   | Untrusted     | UI MUST validate scheme against allowlist (§3.5.5) before dispatch.               |
| `coalesce_key`          | Trusted       | Verbatim. Opaque to UI; daemon-defined.         |
| `created_at`            | Trusted       | Daemon clock; see `Z-3`.                        |
| `expires_at` (toast)    | Trusted       | Daemon clock; UI MUST NOT trust local clock.    |
| `permission_request_id` | Trusted       | Opaque; round-tripped on ack.                   |

The list above is the closed redaction table for the wire envelope. Adding a
new field requires updating this table and bumping the wire schema version
(see §3.4.7).

---

## 2. Terms

| Term                  | Meaning                                                                                                              |
| --------------------- | -------------------------------------------------------------------------------------------------------------------- |
| **Notice**            | A blocking, in-thread surface that requires explicit user dismissal. Rendered as `NoticeBanner` in a GUI host. Severity is immutable from emit. Has no auto-expire timer. |
| **Notification**      | A non-blocking, ambient surface that auto-expires. Rendered as `NotificationToast` in a GUI host. May fall back to an OS notification when the host window is unfocused. |
| **Surface**           | The rendered representation of a Notice or Notification on a host. A single `NoticeId` MAY have at most one active surface per host at a time (see `Z-9`).               |
| **NoticeId**          | A daemon-issued opaque identifier. Stable across reconnect. Uniquely identifies one logical Notice or Notification across its full lifecycle. |
| **Severity**          | One of `Info`, `Warning`, `Error`. Drives default rendering (icon, role, default urgency).                           |
| **Urgency**           | One of `Background`, `Routine`, `Attention`, `Immediate`. Drives escalation policy (in-app → OS → sound).             |
| **Channel**           | The orchestrator-status-snapshot subscribe channel (`spec-orchestrator-status-snapshot.md §3.4`). Notices ride a dedicated `notice` namespace on this channel. |
| **Delta**             | A wire envelope carrying one of `notice_added`, `notice_updated`, `notice_removed`, `notice_replay_complete`. See §3.4. |
| **AckOutcome**        | The terminal outcome recorded for a notice: `dismissed`, `clicked`, `expired`, `superseded`, `revoked`, `host_lost`. See §3.3.5. |
| **CoalesceKey**       | A daemon-computed string that groups equivalent notices. When a new notice arrives whose `coalesce_key` matches a live notice's, the live notice MAY be replaced or its `coalesce_count` incremented. See §3.6.3. |
| **SuppressionWindow** | A per-`(kind, severity)` token bucket (§3.6.2) that bounds emit rate. While the bucket is empty, additional emits are dropped or coalesced. |
| **OS-Escalation**     | The act of forwarding a notification to the operating system's notification surface (Notification Center on macOS, Action Center on Windows, freedesktop.org notification daemon on Linux) when the host window is unfocused or when urgency ≥ `Attention`. |
| **NotificationCenter**| The host-side history view that lists all notices and notifications from the current daemon's session, regardless of whether they were dismissed. See §3.7.    |
| **Ack RPC**           | A request from a host to the daemon recording the user's response to a notice. See §3.4.4.                            |
| **Replay**            | The act of re-sending live notices to a freshly-(re)connected host. See §3.4.6.                                       |
| **Click-action**      | A typed instruction attached to a notice that the host MUST execute on user click. One of `open_external_url`, `show_thread_id`, `show_main`, `dismiss_only`, `permission_reply`. See §3.5.5. |
| **Burst**             | A sequence of ≥ 4 emissions sharing the same `(kind, severity)` within a 2-second window. Triggers the burst rule (§3.6.4). |
| **Permission Notice** | A notice subkind whose `kind` is `permission_prompt`. Carries a `permission_request_id` and is closed by an Ack RPC carrying a `permission_reply`. See §3.8.   |
| **Host-lost**         | The state in which a notice was live, but no host was connected long enough that the daemon abandoned the surface. The notice is persisted with `AckOutcome = host_lost`. |
| **Boot ID**           | A daemon-generated identifier rotated on every daemon process start. Used by hosts to detect that the daemon restarted and a full replay is required. Defined in `spec-orchestrator-status-snapshot.md §3.4.2`; reused verbatim. |

The terms above form the closed vocabulary of this spec. Implementers MUST NOT
introduce a new term in the user-visible surface (CLI text, log lines, error
codes) that overlaps with these without updating §2 first.

---

## 3. Normative algorithms

### 3.1 Emission API: `emit_notice` and `emit_notification`

The daemon exposes two emission entry points. Producers (orchestrator,
permission engine, multi-repo lock manager, agent runner, build coordinator,
heartbeat tick) MUST use exactly one of them per logical event. Choosing
the wrong entry point is a `MUST NOT`: a Notification is not a Notice with a
short timer, and a Notice is not a Notification with auto-expire turned off.

#### 3.1.1 Decision rule

A producer MUST call `emit_notice` if **any** of the following hold:

- The user MUST take an action before the producer can proceed
  (e.g. permission prompt, dangerous-tool confirmation, profile-switch
  acknowledgement). `Z-1`
- The state surfaced is **persistent and ongoing**: it remains true until the
  producer revokes it (e.g. `auth_expired`, `connection_lost`,
  `rate_limited`). `Z-2`
- The producer requires structured ack data on dismissal
  (e.g. `permission_reply { allow | deny | once | always }`). `Z-3a`

A producer MUST call `emit_notification` if **all** of the following hold:

- The user does not need to take action; the surface is informational.
- The state surfaced is **point-in-time** (e.g. "build done",
  "agent finished", "retry exhausted on attempt 5/5").
- The dismissal carries no structured payload beyond `dismissed | clicked`.

If a single producer event satisfies neither rule cleanly (e.g. a build
failure that is both informational and re-runnable), the producer MUST emit a
Notification first; the Notification MAY carry a click-action that opens a
thread containing a Notice for the persistent failure state. The two surfaces
are separate `NoticeId`s and have independent lifecycles.

#### 3.1.2 `emit_notice(notice)` algorithm

```
fn emit_notice(notice: Notice) -> NoticeId:
    1. Validate fields (§3.1.4). Reject if invalid.
    2. id := allocate_notice_id()                                # Z-4
    3. notice.id := id
    4. notice.created_at := monotonic_wall_clock_now()           # Z-5
    5. apply_redaction(notice) per §1.3
    6. coalesce_key := compute_coalesce_key(notice)              # §3.6.3
    7. if exists live notice L with same coalesce_key:
           if notice.severity >= L.severity:
               L := supersede(L, notice)                          # §3.6.5
               broadcast_delta(notice_updated(L))
               return L.id
           else:
               L.coalesce_count += 1
               broadcast_delta(notice_updated(L))
               return L.id
    8. live_table.insert(id, notice)
    9. broadcast_delta(notice_added(notice))                      # §3.4.3
   10. if persistence_enabled: history.append(snapshot(notice))   # §3.7
   11. return id
```

`Z-4` mandates that `NoticeId` is monotonically increasing within a single
boot, and unique within all of recorded history of that daemon's data
directory; see §4.5 for the format.

`Z-5` mandates that `created_at` is sampled from a single monotonic clock at
emit; the wall-clock time MAY be additionally recorded for display but the
ordering between notices is determined by `created_at` only.

#### 3.1.3 `emit_notification(notification)` algorithm

```
fn emit_notification(notification: Notification) -> NoticeId:
    1. Validate fields (§3.1.4). Reject if invalid.
    2. id := allocate_notice_id()
    3. notification.id := id
    4. notification.created_at := monotonic_wall_clock_now()
    5. notification.expires_at := created_at + duration(notification)  # §3.2.4
    6. apply_redaction(notification) per §1.3
    7. coalesce_key := compute_coalesce_key(notification)
    8. if suppression_bucket(notification.kind, notification.severity).empty():
           if exists live notification L with same coalesce_key:
               L.coalesce_count += 1
               L.expires_at := max(L.expires_at, notification.expires_at)
               broadcast_delta(notice_updated(L))
               return L.id
           else:
               # Bucket empty AND no coalesce target: drop. Record stat.
               metric("notice.suppressed", labels={kind, severity}) += 1
               return id_drop_sentinel
       else:
           suppression_bucket.consume(1)
    9. if exists live notification L with same coalesce_key:
           L := supersede(L, notification)
           broadcast_delta(notice_updated(L))
           return L.id
   10. live_table.insert(id, notification)
   11. broadcast_delta(notice_added(notification))
   12. schedule_expiry(id, notification.expires_at)               # §3.3.4
   13. if persistence_enabled: history.append(snapshot(notification))
   14. return id
```

Steps 8–9 implement the **bucket-empty fallback**: if the rate budget is
exhausted, a new notification with no coalesce target is silently dropped (a
metric is incremented, not a notice — recursion is forbidden, see `Z-7`),
while a new notification that matches an existing coalesce key is allowed to
update the live surface even past the budget. This preserves the user-visible
"build failed (×17)" badge during a storm.

`Z-7` (no recursion): the notice/notification pipeline MUST NOT itself emit a
notice or notification on internal errors. Errors raise metrics and structured
log lines; they do not surface to the user.

#### 3.1.4 Field validation

On entry to either emit function, the daemon MUST reject the call if any of
the following hold:

- `kind` is not a member of the closed enum `NoticeKind` (§4.2).
- `severity` is not a member of `{Info, Warning, Error}`.
- `urgency` is not a member of `{Background, Routine, Attention, Immediate}`.
- `i18n_key` is empty or not present in the registered i18n keyspace.
- `body_markdown` contains a raw HTML tag (matched by the sanitiser's
  reject-list).
- A click-action of `open_external_url` carries a URL whose scheme is not in
  the allowlist `{https, mailto, vscode, vscode-insiders, x-caduceus}`.
- For `emit_notice`: an auto-expire field is set (an emit-time bug — notices
  do not auto-expire). `Z-8`
- For `emit_notification`: a `permission_request_id` is set
  (notifications cannot be permission prompts).

A rejected emit MUST log the rejection at `error` level with the field that
failed and MUST NOT raise to the caller in a way that crashes the producer; a
sentinel `NoticeId::REJECTED` is returned.

#### 3.1.5 Producers

The closed list of producers entitled to call `emit_notice` /
`emit_notification` is:

| Producer                     | Surfaces                                                                       |
| ---------------------------- | ------------------------------------------------------------------------------ |
| Permission engine            | Notice: `permission_prompt`. Notice: `permission_denied_blocking`.             |
| Orchestrator                 | Notice: `profile_switch`. Notification: `agent_finished`, `retry_exhausted`.   |
| Multi-repo lock manager      | Notice: `lock_contention_blocking`. Notification: `lock_acquired_after_wait`.  |
| Agent runner                 | Notification: `agent_started`, `agent_progress_milestone`, `agent_finished`.   |
| Build coordinator            | Notification: `build_started`, `build_done`, `build_failed`.                   |
| Heartbeat tick               | Notice: `auth_expired`, `connection_lost`, `rate_limited`.                     |
| Update manager               | Notice: `update_required`. Notification: `update_available`.                   |
| Dangerous-tool gate          | Notice: `dangerous_tool_confirm`.                                              |

Adding a producer requires this table to grow and a corresponding `NoticeKind`
addition (§4.2). A producer outside this list calling `emit_notice` /
`emit_notification` is a `MUST NOT`.

---

### 3.2 Severity, urgency, and OS-escalation policy

#### 3.2.1 Severity tier

Severity is an immutable property of a notice, set at emit time. It maps to:

| Severity  | ARIA role for `NoticeBanner` | ARIA role for `NotificationToast` | Default urgency |
| --------- | ---------------------------- | --------------------------------- | --------------- |
| `Info`    | `status` (polite)            | `status` (polite)                 | `Background`    |
| `Warning` | `alert` (assertive)          | `status` (polite)                 | `Routine`       |
| `Error`   | `alert` (assertive)          | `alert` (assertive)               | `Attention`     |

A `permission_prompt` notice has severity `Warning` and the surface MUST be
rendered with role `alertdialog` (modal-like, focus trap optional but
RECOMMENDED for `Immediate` urgency); see §3.8. `Z-10`

Severity MUST NOT be mutated after emit. If state changes
(e.g. `Warning → Error`), the producer MUST emit a new notice with a fresh
`NoticeId` and explicitly revoke the old (§3.3.6). `Z-11`

#### 3.2.2 Urgency tier

Urgency is a mutable property — a notification MAY be re-emitted (via
coalesce-update) with a higher urgency. It drives escalation:

| Urgency      | In-app behaviour                                       | OS notification?                       | Sound? |
| ------------ | ------------------------------------------------------ | -------------------------------------- | ------ |
| `Background` | Toast (notification) or banner (notice). No focus.    | No.                                    | No.    |
| `Routine`    | Toast or banner. No focus.                             | Only if window unfocused for ≥ 5 s.   | No.    |
| `Attention`  | Toast or banner. UI MAY pulse the dock/taskbar icon.  | Yes, if window unfocused.              | No.    |
| `Immediate`  | Banner (notice) MUST be shown above all other UI in the active thread. Toast (notification) MUST appear regardless of focus. | Yes, always. | OS default sound. |

Urgency `Immediate` is reserved for notices that block forward progress
(e.g. `permission_prompt` while a tool call is suspended,
`dangerous_tool_confirm`). It MUST NOT be used for ambient notifications.
`Z-12`

#### 3.2.3 OS-escalation rule

When the host window for the relevant thread is unfocused at emit time AND
the urgency is ≥ `Routine` per the table above, the host MUST:

1. Render the in-app toast as normal (do not skip the toast on the assumption
   that the OS notification covers it).
2. Additionally call the host-capability `showNotification` surface
   (`m-e2e-architecture` "DesktopCapabilities") with a de-duplication key
   equal to `NoticeId`.
3. When the host re-gains focus, the OS notification MUST be dismissed
   programmatically if and only if the in-app surface is still visible and
   has been seen (see §3.3.3 "seen" definition).
4. When the user clicks the OS notification, the host MUST raise the host
   window AND dispatch the click-action of the notice (§3.5.5) AND emit a
   single click ack (§3.4.4).

`Z-13` mandates that an OS-fallback and an in-app surface for the same
`NoticeId` produce **at most one** click ack. Whichever path the user takes,
the other MUST be dismissed silently.

#### 3.2.4 Default toast durations

Per severity, the toast auto-expire timer (used by `emit_notification`'s
step 5) is:

| Severity  | Default duration | Min | Max  |
| --------- | ---------------: | --: | ---: |
| `Info`    | 6 s              | 3 s | 10 s |
| `Warning` | 8 s              | 5 s | 15 s |
| `Error`   | 10 s             | 6 s | 20 s |

A producer MAY override the duration within `[Min, Max]` by setting
`expires_in_ms` at emit; values outside the range MUST be clamped without
error.

While the user's pointer hovers over the toast, the host MUST suspend the
expiry timer; on pointer leave, the timer resumes from where it was suspended
(not restarted from full). `Z-14`

A toast whose timer has expired but whose surface has not yet animated out
(animation-in-progress window) MUST treat any incoming user click as a
**dismissal**, not a click-action. `Z-15`

#### 3.2.5 Sound policy

When the urgency tier mandates a sound, the host MUST use the OS default
notification sound. The host MUST NOT play a custom sound. The host MUST
respect the OS-level "do not disturb" / "focus assist" setting and MUST
suppress sound (but not the visual surface) while DND is on. `Z-16`

---

### 3.3 Lifecycle state machine

A notice or notification progresses through a finite state machine. The
states are:

```
                +---------+   emit (rejected: I-2)
                |  EMIT   |---------------------------> [drop, no surface]
                +---------+
                     |
                     | accepted
                     v
                +---------+   no host connected
                | QUEUED  |--------+
                +---------+        |
                     |             v
                     | host        +-----------+
                     | connects    | DEFERRED  |
                     v             +-----------+
                +---------+              |
                | LIVE    |<-------------+
                +---------+   replay (§3.4.6)
                  |  |  |
                  |  |  +-- timer fires (notification only) --> EXPIRED
                  |  +----- user clicks/dismisses ------------> ACKED
                  +-------- producer revokes -----------------> REVOKED
                                  |
                                  v
                           [persisted to history (§3.7)]
                                  |
                                  v
                              +--------+
                              | CLOSED |
                              +--------+
```

The legal transitions are:

| From       | To         | Trigger                                      |
| ---------- | ---------- | -------------------------------------------- |
| `EMIT`     | `QUEUED`   | Validation passed, no live host yet.         |
| `EMIT`     | `LIVE`     | Validation passed, ≥ 1 host connected.       |
| `QUEUED`   | `LIVE`     | First host connects.                         |
| `QUEUED`   | `DEFERRED` | Notice age exceeds `queue_max_age` (§3.3.2). |
| `DEFERRED` | `LIVE`     | A host connects after deferral.              |
| `DEFERRED` | `CLOSED`   | Notice age exceeds `host_lost_max_age` (`AckOutcome = host_lost`). |
| `LIVE`     | `EXPIRED`  | Auto-expire (notification only).             |
| `LIVE`     | `ACKED`    | User dismissed or clicked.                   |
| `LIVE`     | `REVOKED`  | Producer called `revoke_notice(id)`.         |
| `LIVE`     | `LIVE`     | Coalesce-update increments `coalesce_count`. |
| `EXPIRED`  | `CLOSED`   | Persistence write completes.                 |
| `ACKED`    | `CLOSED`   | Persistence write completes.                 |
| `REVOKED`  | `CLOSED`   | Persistence write completes.                 |

Any transition not listed above is a `MUST NOT`. The daemon MUST log a state
machine violation at `error` level and treat the notice as `CLOSED` with
`AckOutcome = revoked` and a reason field of `state_machine_violation`.

#### 3.3.2 `QUEUED` and `DEFERRED`

When `emit_notice` is called and no host is connected, the notice enters the
`QUEUED` state. It remains there until a host connects (transitioning to
`LIVE`) or until its age (`now() - created_at`) exceeds `queue_max_age`.
`queue_max_age` defaults to:

- For `emit_notice` of severity `Info` or `Warning`: 5 minutes.
- For `emit_notice` of severity `Error` and `kind` in `{auth_expired, connection_lost, rate_limited}`: 30 minutes.
- For `emit_notice` of `kind = permission_prompt`: equal to the permission
  prompt's wall-clock deadline as set by the permission engine
  (`spec-m-permissions.md §6`). When the deadline elapses with no host, the
  permission prompt is `REVOKED` with reason `prompt_timeout`.
- For `emit_notification`: equal to `expires_in_ms` plus a 30-second grace.
  After that, the notification is `EXPIRED` without ever being shown.

After `queue_max_age`, the notice transitions to `DEFERRED`. A `DEFERRED`
notice is still in `live_table` and is replayed when a host connects, but if
no host connects within `host_lost_max_age` (default 24 hours), the notice
transitions directly to `CLOSED` with `AckOutcome = host_lost`. `Z-17`

#### 3.3.3 "Seen"

A `LIVE` notice is "seen" once any host has rendered its surface for at
least one frame. The host MUST send a `notice_seen(id)` ack within 1 second
of first rendering (§3.4.4.4). Until "seen", a live notice is **not** a
candidate for OS-fallback dismissal (§3.2.3 step 3). `Z-18`

The "seen" event is purely informational and does not transition the
state machine. It is recorded for telemetry and for the
`first_seen_at` field in history (§3.7).

#### 3.3.4 Auto-expiry scheduler

The daemon maintains a single per-notification expiry scheduler. When a
notification enters `LIVE`, an entry is added to a min-heap keyed on
`expires_at`. A single timer wakes when the heap's root expires and:

1. Pops all entries whose `expires_at <= now()`.
2. For each, transitions the notification to `EXPIRED`, broadcasts
   `notice_removed(id, AckOutcome = expired)`, and triggers persistence.
3. Reschedules itself for the new heap root (if any).

If a notification's `expires_at` is updated by a coalesce-update, the heap
MUST be re-keyed (`O(log n)` decrease- or increase-key). `Z-19`

Hover-suspend (§3.2.4) is **host-side**: the host pauses its local timer but
the daemon does NOT learn of the pause. To prevent the daemon from expiring a
hovered toast prematurely, the host MUST send `notice_extend(id, by_ms)`
when hover begins, with `by_ms` equal to the current daemon-side time
remaining; the daemon MUST grant up to `expires_in_ms_max` (the upper bound
in §3.2.4) total lifetime. On hover end, the host MAY send
`notice_extend(id, 0)` to allow the timer to resume, or simply allow the
remaining lifetime to elapse. `Z-20`

#### 3.3.5 `AckOutcome` enum

The terminal `AckOutcome` recorded for every closed notice is one of:

| Value         | When                                                                      |
| ------------- | ------------------------------------------------------------------------- |
| `dismissed`   | User pressed the dismiss control (X) without invoking a click-action.     |
| `clicked`     | User invoked the click-action (in-app or OS-fallback path).               |
| `expired`     | Auto-expire timer fired (notification only).                              |
| `superseded`  | A coalesce-supersede replaced the live surface (§3.6.5).                  |
| `revoked`     | Producer called `revoke_notice(id)`.                                      |
| `host_lost`   | No host connected within `host_lost_max_age` (§3.3.2).                    |

The `AckOutcome` is immutable once set. `Z-21`

#### 3.3.6 Producer revocation

A producer MAY revoke a still-`LIVE` notice by calling `revoke_notice(id, reason)`.
The daemon MUST:

1. Mark the notice `REVOKED` with the supplied reason.
2. Broadcast `notice_removed(id, AckOutcome = revoked, reason)`.
3. Persist with the supplied reason in `History.reason`.
4. NOT emit a new notice on the user's behalf (no "this was withdrawn"
   notification — `Z-7` / no-recursion).

Revocation of an already-`CLOSED` notice is a no-op and MUST NOT raise. `Z-22`

---

### 3.4 Wire over the snapshot subscribe channel

#### 3.4.1 Channel reuse

Notices and notifications travel over the **same** transport as the
orchestrator status snapshot, defined in
`spec-orchestrator-status-snapshot.md §3.4`. A host opens a single subscription
to the daemon and receives a single ordered byte stream containing snapshot
deltas and notice deltas, multiplexed by an envelope tag.

The host MUST subscribe with the parameters defined in
`spec-orchestrator-status-snapshot.md §3.4.1`:

```
subscribe(
    channels:        ["snapshot", "notice"],
    since_fingerprint: Option<Fingerprint>,
    since_stream_seq:  Option<u64>,
    since_boot_id:     Option<BootId>,
)
```

The `notice` channel MUST be requested explicitly; clients that do not request
it MUST NOT receive notice deltas. The daemon MUST validate that the host has
the entitlement to receive notices for the requested workspace (§3.4.5).

#### 3.4.2 Envelope

Every frame on the multiplexed channel is wrapped as:

```
Frame {
    stream_seq:  u64,            // monotonically increasing per (boot_id, channel-set)
    boot_id:     BootId,
    channel:     "snapshot" | "notice",
    payload:     bytes (CBOR-encoded; schema below)
}
```

`stream_seq` is shared across both channels in a single subscription;
this is the same `stream_seq` that the snapshot spec mandates, and the
notice channel MUST piggy-back on it so that a single
`SubscribeAck { since_stream_seq: N }` resumes both streams atomically.
`Z-23`

#### 3.4.3 Notice channel payloads

A `notice`-channel frame carries one of the following CBOR payloads:

```
NoticeAdded {
    notice: Notice,                   // §4.1
}

NoticeUpdated {
    id:               NoticeId,
    coalesce_count:   Option<u32>,    // present iff updated by coalesce
    expires_at:       Option<Timestamp>,  // present iff toast timer extended
    body_plain:       Option<String>,
    body_markdown:    Option<String>,
    i18n_args:        Option<Map<String, String>>,
    urgency:          Option<Urgency>,
    // Severity, kind, and id are immutable. Fields not listed are immutable.
}

NoticeRemoved {
    id:               NoticeId,
    outcome:          AckOutcome,
    reason:           Option<String>,    // free-form; producer-supplied for revoke
}

NoticeReplayBegin {
    fingerprint:      Fingerprint,    // matches snapshot's notion at the same stream_seq
}

NoticeReplayComplete {
    fingerprint:      Fingerprint,
}
```

The daemon MUST NOT emit `NoticeUpdated` mutating any field that is not in
the explicit list above. A field is either declared mutable here (and MAY
appear in `NoticeUpdated`) or immutable (and MUST NOT). `Z-24`

A `NoticeReplayBegin` frame opens a replay window (§3.4.6); a
`NoticeReplayComplete` frame closes it. Between the two, the host MUST
treat any received `NoticeAdded` as a replay frame (semantically equivalent
to a fresh add for state-machine purposes; the host already has a logical
"replay buffer", but it has no obligation to suppress UI). `Z-25`

#### 3.4.4 Ack RPCs

A host informs the daemon of user actions by issuing one of the following
ack RPCs over the same transport. Each RPC carries the `NoticeId` and is
idempotent: a second ack with the same outcome on the same id MUST be a
no-op (the daemon returns `AckResult::AlreadyAcked`).

##### 3.4.4.1 `notice_dismiss(id)`

User pressed the dismiss control. Daemon transitions `LIVE → ACKED`,
records `outcome = dismissed`, broadcasts `NoticeRemoved`, persists.

##### 3.4.4.2 `notice_click(id, action)`

User invoked a click-action. `action` is one of `{open_external_url, show_thread_id, show_main, dismiss_only, permission_reply}` and MUST match
the click-action declared on the notice at emit (§3.5.5). Daemon transitions
`LIVE → ACKED`, records `outcome = clicked`, attaches the click-action to
the persisted record, broadcasts `NoticeRemoved`, persists.

If `action == permission_reply`, the daemon MUST additionally invoke the
permission engine with the reply payload (§3.8); the click ack and the
permission engine call MUST be atomic with respect to the
`live_table` — no other emit, supersede, or ack may interleave. `Z-26`

##### 3.4.4.3 `notice_extend(id, by_ms)`

Host-driven hover suspend (§3.3.4). Daemon recomputes `expires_at` and
emits `NoticeUpdated { expires_at }`. If the requested extension would push
`expires_at` beyond `created_at + max_duration` (§3.2.4 Max), the daemon
MUST clamp without error.

##### 3.4.4.4 `notice_seen(id)`

Host first rendered the surface. Daemon records `first_seen_at` in the
in-memory state and (eventually) in history (§3.7). Does not transition the
state machine. `Z-18`

##### 3.4.4.5 `notice_query_history(filter, cursor, limit)`

Host requests the notification-center history view. See §3.7.

#### 3.4.5 Entitlement

A host's right to receive notices on a given workspace is governed by the
existing daemon entitlement model. The daemon MUST:

- For an unauthenticated subscriber: deny the `notice` channel.
- For a subscriber with workspace entitlement W: deliver only notices whose
  `scope = workspace W` or `scope = global`.
- For a subscriber with no workspace entitlement but with daemon-admin: deliver
  all notices. Reserved for diagnostic tooling.

A subscriber's revocation of entitlement (e.g. profile switch) MUST NOT race
the notice channel: the daemon MUST drop the subscription with a
`SubscribeError::EntitlementRevoked` and the host MUST close the channel
without further deltas. `Z-27`

#### 3.4.6 Replay on (re)connect

When a host (re)connects with a `since_*` triple, the daemon MUST:

1. Compare `since_boot_id` against current `boot_id`. If different, treat
   the subscription as fresh: `since_stream_seq` and `since_fingerprint` are
   ignored.
2. For every notice currently in `live_table` whose `created_at` is after
   the boundary computed in step 1, send `NoticeAdded(notice)` in
   `created_at` order, framed by a `NoticeReplayBegin` /
   `NoticeReplayComplete` pair carrying the fingerprint.
3. After replay completes, resume normal delta delivery from the next
   `stream_seq` after the replay's last frame. `Z-28`

A `LIVE` notice that was already acked while the host was disconnected
MUST NOT be replayed; instead, the host MUST infer its existence from
history (§3.7) if it cares.

A coalesce-update that occurred during the disconnect window collapses to a
single `NoticeAdded` carrying the post-update fields; the host does NOT see
the intermediate `NoticeUpdated` frames. `Z-29`

#### 3.4.7 Wire schema versioning

The CBOR payload for `Notice` and `Notification` carries an explicit
`schema_version: u16` field. The host MUST reject a frame whose
`schema_version` is greater than its supported maximum, surfacing a
diagnostic Notice (`kind = host_schema_outdated`) emitted by the host's
local diagnostics layer (NOT by the daemon — the daemon cannot meaningfully
emit a notice the host cannot parse).

The current schema version is `1`. Adding a field is a `MUST` schema bump;
the new field MUST be optional. Removing a field is a `MUST` schema bump and
requires a migration plan; see §8.

---

### 3.5 Cross-runtime CLI / TTY rendering

#### 3.5.1 Renderer selection

The daemon does not know what host is connected. A renderer is determined by
the host itself:

- **GUI host (zed pane)**: renders `NoticeBanner` for notices and
  `NotificationToast` for notifications, per §3.2.1.
- **CLI host (`caduceus tail`, `caduceus notices`)**: renders ANSI-colored
  one-liners to the connected TTY.
- **Headless host (CI, scripted)**: emits structured JSON lines to stdout.

The host announces its renderer in the subscribe call:

```
subscribe(..., renderer: "gui" | "cli" | "headless")
```

The renderer affects ONLY the local rendering and MUST NOT change which
notices the daemon delivers. `Z-30`

#### 3.5.2 CLI rendering

A CLI host MUST render each `NoticeAdded` as:

```
[<severity-glyph>] <kind> :: <localised-body>          [id=<NoticeId>]
```

where `<severity-glyph>` is `i` / `!` / `x` for `Info` / `Warning` /
`Error`, colored cyan / yellow / red respectively if the TTY supports color
(detected via `isatty(stdout) && $TERM != "dumb"`).

A `NoticeUpdated` carrying a coalesce-count update MUST render as:

```
[<glyph>] <kind> :: <localised-body>  (×<coalesce_count>)  [id=<NoticeId>]
```

A `NoticeRemoved` MUST render as:

```
[<glyph>] <kind> :: closed (<outcome>)                [id=<NoticeId>]
```

The CLI host MUST flush stdout after each rendered frame (no buffering). It
MUST handle SIGWINCH (terminal resize) by re-rendering the live set on the
next frame.

#### 3.5.3 CLI input

A CLI host with an interactive TTY MAY accept ack input. The keystroke
mapping is:

| Keystroke   | Effect                                                             |
| ----------- | ------------------------------------------------------------------ |
| `d <id>`    | `notice_dismiss(id)`                                               |
| `c <id>`    | `notice_click(id, dismiss_only)`                                   |
| `c <id> y`  | For `permission_prompt`: `notice_click(id, permission_reply{allow:once})`. |
| `c <id> Y`  | For `permission_prompt`: `notice_click(id, permission_reply{allow:always})`. |
| `c <id> n`  | For `permission_prompt`: `notice_click(id, permission_reply{deny:once})`. |
| `c <id> N`  | For `permission_prompt`: `notice_click(id, permission_reply{deny:always})`. |

A CLI host MUST NOT auto-dismiss notices on a timer; auto-expire is a
notification-only behaviour and is daemon-driven. The CLI renders the
expiry visually (e.g. dimmed line) but never closes the surface itself.
`Z-31`

#### 3.5.4 Headless rendering

A headless host MUST emit one JSON line per delta to stdout, of the form:

```
{"t":"add","frame":{...Notice...}}
{"t":"upd","frame":{"id":"...","coalesce_count":3}}
{"t":"rem","frame":{"id":"...","outcome":"dismissed"}}
{"t":"replay_begin","fingerprint":"..."}
{"t":"replay_complete","fingerprint":"..."}
```

Headless mode is intended for CI / scripted use and MUST NOT prompt for
input. A `permission_prompt` notice received in headless mode MUST be
auto-replied with the policy `deny:once` after a configurable grace
(default 0 ms). The daemon MUST log this auto-deny clearly. `Z-32`

#### 3.5.5 Click-action allowlist

The closed click-action enum is:

| Action                     | Payload                                  | Renderer behaviour                                           |
| -------------------------- | ---------------------------------------- | ------------------------------------------------------------ |
| `open_external_url`        | `{ url: String }`                        | GUI: open in OS browser. CLI: print URL. Headless: log URL.  |
| `show_thread_id`           | `{ thread_id: ThreadId }`                | GUI: focus host window, navigate to thread. CLI: print thread id. |
| `show_main`                | `{}`                                     | GUI: focus host window, default pane. CLI: no-op.            |
| `dismiss_only`             | `{}`                                     | GUI/CLI: dismiss surface, no other action.                   |
| `permission_reply`         | `{ allow_or_deny, scope (once|always) }` | GUI/CLI: dismiss surface, daemon invokes permission engine.  |

The URL allowlist (§3.1.4) gates only the daemon → host validation. The host
MUST additionally prompt the user before opening any `open_external_url`
target whose host part is not in a host-side allowlist (§3.5.6). `Z-33`

#### 3.5.6 Host-side URL allowlist

A GUI host MUST consult its OS keychain or per-workspace config for the
list of "trusted" hosts (e.g. `github.com`, the user's GHE host). For URLs
matching a trusted host, the click-action proceeds without prompt. For
all others, the host MUST surface a confirmation modal (which is **not**
itself a notice — modals do not multiplex over the notice channel). On
confirm, the host opens the URL; on cancel, the click-ack is still sent to
the daemon (the user did invoke the click), but the URL is not opened.
`Z-34`

---

### 3.6 Coalescing, rate-limiting, and suppression

#### 3.6.1 Goals

The user MUST NOT see 50 toasts when the daemon sees 50 retry-exhausted
events. The mechanisms are, in priority order:

1. **Coalescing (§3.6.3)** — a single live surface with a count badge.
2. **Token-bucket rate limit (§3.6.2)** — a bucket per `(kind, severity)`.
3. **Burst suppression (§3.6.4)** — a host-side rule on top of the
   transport rate.
4. **Producer-side dedup (§3.6.6)** — producers SHOULD short-circuit obvious
   duplicates before calling emit.

#### 3.6.2 Token bucket

The daemon maintains one token bucket per `(kind, severity)` pair, with
defaults:

| Tier                | Capacity | Refill rate    |
| ------------------- | -------: | -------------- |
| `Notification`-`Info`    | 10  | 1 / 5 s          |
| `Notification`-`Warning` | 5   | 1 / 10 s         |
| `Notification`-`Error`   | 5   | 1 / 10 s         |
| `Notice`-`*`             | 50  | 5 / s            |

`Notice` bucket is intentionally generous: notices are user-blocking and
their rate is determined by the user, not the daemon. The bucket exists only
as a backstop against runaway producers. A daemon-admin client MAY query the
current bucket levels for diagnostics.

When `emit_notification` runs and its bucket is empty AND no coalesce target
exists, the emit is dropped (§3.1.3 step 8). Drops MUST be counted in the
metric `notice.dropped` labelled by `(kind, severity)`.

#### 3.6.3 Coalesce key

The `coalesce_key` is computed from a producer-supplied function. The default
is:

```
coalesce_key(notice) = sha256(kind || ":" || scope_id || ":" || canonical_subject(notice))
```

where `canonical_subject` is the producer-defined identity of the
underlying entity (e.g. for `build_failed`: the package name and target;
for `retry_exhausted`: the tool-call id; for `permission_prompt`: the
permission-request-id). Two notices with equal `coalesce_key` are
"equivalent" for coalesce purposes.

A producer MAY override `coalesce_key` by setting it explicitly at emit;
overriding to a non-default value MUST be documented in the producer's spec.

#### 3.6.4 Burst rule

When ≥ 4 emits of the same `(kind, severity)` arrive within a 2-second
window, the daemon MUST collapse them as follows:

- The first emit produces a `NoticeAdded`.
- Subsequent emits in the burst window produce `NoticeUpdated` with
  `coalesce_count` incremented, even if their `coalesce_key`s differ.
- The notice's `body_plain` and `body_markdown` MUST be updated to a
  burst-mode template `<kind>-burst` (e.g. "5 builds failed"), and
  `i18n_args` MUST be set to `{ count: <n>, kind: <kind> }`.

Once 2 seconds elapse with no new emits in the same `(kind, severity)`, the
burst window closes. A new emit thereafter starts a fresh notice (new
`NoticeId`, fresh `NoticeAdded`). `Z-35`

The burst rule operates **on top of** the bucket: a burst that exhausts the
bucket produces only the first add and a single `NoticeUpdated` per
admitted post-bucket emit (§3.6.2 fallback).

#### 3.6.5 Supersede

When a coalesce-target exists and the new emit has strictly higher severity,
the daemon performs a **supersede**:

1. The old live notice is closed with `outcome = superseded` (a
   `NoticeRemoved` is broadcast).
2. The new notice is emitted as fresh (a `NoticeAdded` is broadcast).
3. Persistence records both, linked by `superseded_by` and `supersedes`
   pointers (§3.7.2).

Supersede MUST NOT skip the `NoticeRemoved`: clients that have local UI
state for the old `NoticeId` need to know the id is closed. `Z-36`

A supersede where the new severity equals the old severity is **not** a
supersede; it is a coalesce-update (§3.1.2 step 7 else-branch).

#### 3.6.6 Producer-side dedup

Producers SHOULD short-circuit obvious duplicates before calling emit. For
example, the build coordinator SHOULD compare a build's failure hash against
its last-emitted notice for the same target and call `revoke_notice` +
`emit_notification` only if the failure hash changed. This is a `SHOULD`,
not a `MUST`: the daemon's coalesce machinery is the safety net, and a
correct producer that hits the safety net is not a bug.

---

### 3.7 Persistence and the Notification Center

#### 3.7.1 Storage

The daemon MUST persist every closed notice to a per-workspace history file
located at `<data_dir>/workspaces/<workspace_id>/notices.log`. The file
format is one CBOR-encoded `HistoryRecord` per line, append-only.

Writes MUST be atomic at the record level: either the whole record is
present or none of it is. The daemon MAY use a write-then-fsync pattern,
or a per-record CRC followed by a startup-time scan that truncates partial
tails. `Z-37`

Retention defaults:

- `Info` notifications: 7 days.
- `Warning` notifications and notices: 30 days.
- `Error` notifications and notices: 90 days.
- `permission_prompt` notices: indefinite (never auto-pruned). Pruning is the
  responsibility of the audit pipeline (out of scope).

A daily compaction task MUST run that:

1. Reads `notices.log` end-to-end.
2. Drops records past the retention bound.
3. Writes a new file `notices.log.new` and atomically renames over
   `notices.log`.

If compaction fails, the daemon MUST NOT delete the old file and MUST emit a
diagnostic log line; user-visible behaviour is unchanged. `Z-38`

#### 3.7.2 `HistoryRecord` shape

```
HistoryRecord {
    id:             NoticeId,
    kind:           NoticeKind,
    severity:       Severity,
    urgency:        Urgency,
    scope:          Scope,
    created_at:     Timestamp,
    first_seen_at:  Option<Timestamp>,
    closed_at:      Timestamp,
    outcome:        AckOutcome,
    reason:         Option<String>,
    coalesce_count: u32,
    supersedes:     Option<NoticeId>,
    superseded_by:  Option<NoticeId>,
    body_plain:     String,
    i18n_key:       String,
    i18n_args:      Map<String, String>,
    click_action:   Option<ClickAction>,
    click_payload:  Option<Bytes>,        // present iff outcome = clicked
}
```

`HistoryRecord` MUST NOT carry `body_markdown` (the plain-text fallback is
the canonical persisted form; markdown is a presentation detail). `Z-39`

#### 3.7.3 Query API

A host MAY request history via `notice_query_history(filter, cursor, limit)`:

```
filter: {
    severity_min: Option<Severity>,
    kinds:        Option<Set<NoticeKind>>,
    since:        Option<Timestamp>,
    until:        Option<Timestamp>,
    outcome_in:   Option<Set<AckOutcome>>,
}
cursor: Option<HistoryCursor>      // opaque, daemon-issued
limit:  u32                         // 1..=500
```

The daemon MUST return records in `closed_at` descending order by default
(most recent first), paginated. The cursor is opaque and stable across
daemon restarts only if the underlying file has not been compacted; clients
MUST handle `CursorInvalidated` by restarting the query without a cursor.
`Z-40`

#### 3.7.4 Notification Center UI behaviour

A GUI host MAY render a notification-center UI that calls
`notice_query_history` on demand. The UI MUST:

1. Distinguish closed notices from live ones visually (live notices appear at
   the top with "live" tag).
2. Re-issue the query when the user re-opens the center (cached state across
   tabs is a `MAY`).
3. Allow the user to invoke a `clicked`-style action on a closed notice
   only if the click-action is still meaningful (e.g. `open_external_url`
   remains valid; `show_thread_id` only valid if the thread still exists).
   The host MUST validate before invoking. `Z-41`

#### 3.7.5 Cross-host history visibility

A history record is per-workspace and MUST be delivered only to hosts with
the workspace entitlement (§3.4.5). A daemon-admin host MAY read across
workspaces. `Z-42`

---

### 3.8 Permission interplay

The permission engine (`spec-m-permissions.md`) ends its 9-step evaluate
pipeline at "Step 9 — Default prompt", which surfaces a permission prompt to
the user. That prompt is exclusively delivered as a `permission_prompt`
notice. This section specifies the boundary contract.

#### 3.8.1 Emission

When the permission engine reaches Step 9, it MUST call:

```
emit_notice(Notice {
    kind:                  permission_prompt,
    severity:              Warning,
    urgency:               Immediate,
    scope:                 workspace(<wid>),
    permission_request_id: <prid>,        // opaque, set by permission engine
    deadline:              <wallclock>,    // see §3.8.4
    click_action:          permission_reply,
    i18n_key:              "permission.prompt.<resource>.<verb>",
    i18n_args:             { resource: ..., verb: ..., risk_tier: ... },
    body_plain:            <localised plain text>,
    coalesce_key:          format!("permission_prompt:{}", prid),
    ...
})
```

`coalesce_key` is uniquely derived from `permission_request_id`, so two
permission prompts for the same request collapse to one surface
(see §3.8.5). `Z-43`

#### 3.8.2 Reply

A user reply is delivered as a `notice_click(id, permission_reply)` ack.
The `permission_reply` payload is:

```
permission_reply {
    decision: allow | deny,
    scope:    once | always,
}
```

The daemon MUST:

1. Validate that the notice is still `LIVE` and the deadline has not passed.
2. Atomically (`Z-26`) close the notice with `outcome = clicked` and call
   the permission engine with the reply payload.
3. On `scope = always`, the permission engine persists the reply to the
   policy store (per `spec-m-permissions.md §7`); the notice spec is not
   responsible for that persistence.

A `notice_dismiss` on a `permission_prompt` notice MUST be treated as
`permission_reply { decision: deny, scope: once }`. The host MUST NOT
allow a "raw dismiss" of a permission prompt. `Z-44`

#### 3.8.3 Cancellation

If the operation that requested the permission is cancelled (e.g. session
destroy per `spec-m-session-lifecycle.md §11.2`), the permission engine
MUST call `revoke_notice(id, reason = "operation_cancelled")` to close the
prompt before the underlying operation tears down. The notice MUST NOT
linger past its requesting operation. `Z-45`

In-flight permission/ask-user prompts on cancel are explicitly addressed in
`spec-m-session-lifecycle.md §11.2`; this spec defers to that contract for
the ordering of revoke vs. cancel.

#### 3.8.4 Deadline

A permission prompt carries a wall-clock `deadline` set by the permission
engine (typically session-tied or fixed at 5 minutes). When the deadline
elapses with no host or no reply:

- If `LIVE` with a host: the daemon MUST `revoke_notice(id, reason =
  "prompt_timeout")` and the permission engine MUST treat the timeout as
  `deny:once`.
- If `QUEUED` or `DEFERRED`: same behaviour; the deadline applies regardless
  of whether the user ever saw the prompt.

The deadline MUST be the same value passed to the permission engine; the
notice spec MUST NOT use a different deadline. `Z-46`

#### 3.8.5 Coalesce of duplicate prompts

When two operations independently request the same permission within an
overlapping window, the permission engine MAY issue a single
`permission_request_id` (its prerogative). If it issues two distinct
ids, two notices appear; the daemon MUST NOT coalesce them across
distinct ids, since the user's reply is bound to the id. `Z-47`

#### 3.8.6 `permission_denied_blocking`

Some permission denials are not prompts but *outcomes*: a tool call denied
by policy with no possibility of the user overriding (e.g. policy:
"this workspace forbids `read_file` on `~/.ssh/`"). The permission engine
MUST emit a `permission_denied_blocking` notice (severity `Error`,
urgency `Routine`, no click-action other than `dismiss_only`) so the user
sees that the tool's failure was permission-driven, not a bug.

`permission_denied_blocking` notices coalesce per
`(resource, verb)`; a stream of denials on the same resource produces one
notice with a count badge. `Z-48`

---

### 3.9 Failure modes

The closed list of daemon-side failure modes that produce a notice is:

| Failure mode             | Kind                       | Severity | Urgency     | Coalesce key                         |
| ------------------------ | -------------------------- | -------- | ----------- | ------------------------------------ |
| Auth token expired       | `auth_expired`             | `Error`  | `Attention` | `auth_expired:<account_id>`          |
| Connection lost          | `connection_lost`          | `Warning`| `Routine`   | `connection_lost:<endpoint>`         |
| Rate-limited             | `rate_limited`             | `Warning`| `Routine`   | `rate_limited:<endpoint>:<scope>`    |
| Update required          | `update_required`          | `Error`  | `Attention` | `update_required:<channel>`          |
| Update available         | `update_available`         | `Info`   | `Background`| `update_available:<channel>`         |

When the underlying condition resolves (auth refreshed, connection restored,
rate window passed, update applied), the producer MUST `revoke_notice`.
The user SHOULD see the surface disappear; the producer MUST NOT emit a
"recovered" notification (no recursion / `Z-7`). `Z-49`

---

## 4. Data shapes

### 4.1 `Notice`

```rust
struct Notice {
    schema_version:        u16,         // currently 1
    id:                    NoticeId,
    kind:                  NoticeKind,
    severity:              Severity,
    urgency:               Urgency,
    scope:                 Scope,
    created_at:            Timestamp,
    expires_at:            Option<Timestamp>,    // None for notices; Some for notifications
    deadline:              Option<Timestamp>,    // permission_prompt only
    permission_request_id: Option<PermissionRequestId>,  // permission_prompt only
    click_action:          Option<ClickAction>,
    i18n_key:              String,
    i18n_args:             Map<String, String>,
    body_plain:            String,
    body_markdown:         Option<String>,
    coalesce_key:          String,
    coalesce_count:        u32,        // initially 1
    supersedes:            Option<NoticeId>,
    sender:                ProducerTag,
}
```

- A `Notification` is a `Notice` with `expires_at = Some(_)` and
  `deadline = None`. There is no separate type at the wire level;
  the discrimination is by the optional fields.
- `Scope` is `enum { Global, Workspace(WorkspaceId), Thread(ThreadId) }`.
  `Thread`-scoped notices appear in-thread only; `Workspace`-scoped notices
  appear in any thread within the workspace; `Global` appears everywhere.

### 4.2 `NoticeKind`

The closed enum of all valid `NoticeKind` values:

```
NoticeKind ::=
    // Notices (blocking)
    permission_prompt
  | permission_denied_blocking
  | profile_switch
  | dangerous_tool_confirm
  | auth_expired
  | connection_lost
  | rate_limited
  | update_required
  | lock_contention_blocking

    // Notifications (non-blocking)
  | agent_started
  | agent_progress_milestone
  | agent_finished
  | retry_exhausted
  | build_started
  | build_done
  | build_failed
  | lock_acquired_after_wait
  | update_available
  | host_schema_outdated      // host-emitted only
  | diagnostic_burst           // burst-rule synthetic
```

Adding a `NoticeKind` requires:

1. Updating this enum.
2. Updating §3.1.5 producers table.
3. Adding the kind's coalesce-key formula (or relying on default).
4. Adding the kind's bucket-tier mapping (defaults exist; a kind MAY pin a
   custom bucket).
5. Adding the kind's i18n key prefix.
6. Bumping `schema_version` per §3.4.7.

A kind not in this list MUST be rejected at emit (§3.1.4). `Z-50`

### 4.3 `Severity`, `Urgency`

```
Severity ::= Info | Warning | Error
Urgency  ::= Background | Routine | Attention | Immediate
```

Both enums are closed and frozen at schema_version 1; adding a value
requires a schema bump.

### 4.4 `AckOutcome`

```
AckOutcome ::= dismissed | clicked | expired | superseded | revoked | host_lost
```

See §3.3.5.

### 4.5 `NoticeId`

`NoticeId` is a 128-bit identifier composed as:

```
NoticeId = boot_id (64 bits) || monotonic_counter (64 bits)
```

The leading 64 bits are the daemon's `BootId` (truncated to 64 bits if the
boot id is larger; see `spec-orchestrator-status-snapshot.md §3.4.2`), and
the trailing 64 bits are a monotonic counter incremented on every `allocate_notice_id`. The counter MUST NOT wrap within a single boot;
if it ever does, the daemon MUST refuse further emits (a daemon-internal
fatal). `Z-51`

The wire encoding is a 32-character hex string with a `n_` prefix
(`n_<32-hex>`).

### 4.6 `ClickAction`

```rust
enum ClickAction {
    OpenExternalUrl { url: String },
    ShowThreadId    { thread_id: ThreadId },
    ShowMain,
    DismissOnly,
    PermissionReply,                        // payload supplied at click time
}
```

The `PermissionReply` action is special: its payload is supplied by the host
at click time, not at emit. All other actions carry their payload at emit.

### 4.7 `PermissionReply`

```rust
struct PermissionReply {
    decision: PermissionDecision,           // Allow | Deny
    scope:    PermissionScope,              // Once | Always
}
```

See `spec-m-permissions.md §7` for the policy-store coupling on
`scope = Always`.

### 4.8 `CoalesceKey`

A `String`. Opaque to the host. The daemon-defined defaults are §3.6.3.

### 4.9 `Fingerprint`

Re-used from `spec-orchestrator-status-snapshot.md §3.4.4`. A
`Fingerprint` is a hash of the live state at a stream_seq boundary; the
notice channel's fingerprint covers the live notice set at that boundary.

### 4.10 `ProducerTag`

A short string identifying the producer ('orchestrator', 'permission',
'multi_repo_lock', 'agent_runner', 'build_coord', 'heartbeat',
'update', 'dangerous_tool'). Reserved for diagnostics; the host SHOULD NOT
display it but MAY surface it in a debug overlay.

---

## 5. Invariants (MUST)

This section enumerates the externally testable promises of this spec.
Each `I-N` is exercised by at least one test in §6.

- **I-1** (Validation): An emit call with any field violating §3.1.4 MUST
  return `NoticeId::REJECTED` and MUST NOT mutate `live_table`,
  `history`, or any subscriber's stream.

- **I-2** (Severity-immutable): Once a notice is in `live_table`, its
  `severity` MUST NOT change. A producer wishing to "raise severity" MUST
  emit a fresh notice and revoke the old.

- **I-3** (Coalesce-or-bucket): For `emit_notification`, if the
  `(kind, severity)` bucket is empty AND no live notice with the same
  `coalesce_key` exists, the emit MUST be silently dropped (with metric);
  it MUST NOT appear on any subscriber's stream and MUST NOT enter
  `history`.

- **I-4** (No recursion): The notice pipeline MUST NOT emit a notice or
  notification on its own internal errors. Internal errors raise log lines
  and metrics only.

- **I-5** (Replay completeness): On (re)connect with `since_*`, the daemon
  MUST replay every `LIVE` notice the host is entitled to, in
  `created_at` order, framed by `NoticeReplayBegin` /
  `NoticeReplayComplete`.

- **I-6** (At-most-one ack): For any `NoticeId`, at most one of
  `dismissed`, `clicked`, `expired`, `superseded`, `revoked`, `host_lost`
  MUST be recorded as the terminal outcome. A second ack MUST be a no-op.

- **I-7** (OS-fallback dedup): When a notice has both an in-app surface and
  an OS notification, exactly one click ack MUST be delivered to the
  daemon regardless of which surface the user clicked.

- **I-8** (Hover suspends timer): While the host reports a hover (via
  `notice_extend(id, by_ms)`), the daemon MUST NOT expire the toast.

- **I-9** (Permission-prompt deadline): A `permission_prompt` notice not
  acked by `deadline` MUST close with `outcome = revoked, reason = "prompt_timeout"` and the permission engine MUST be informed equivalent
  to `deny:once`.

- **I-10** (Permission-prompt dismiss = deny): A `notice_dismiss` on a
  `permission_prompt` MUST be treated as `permission_reply { deny:once }`.

- **I-11** (CLI never auto-dismiss): A CLI host MUST NOT close any notice
  on a local timer; auto-expire is daemon-driven only.

- **I-12** (URL allowlist): An `open_external_url` click-action whose URL
  scheme is not in `{https, mailto, vscode, vscode-insiders, x-caduceus}`
  MUST be rejected at emit.

- **I-13** (Schema enforce): A frame whose `schema_version` exceeds the
  host's max MUST be dropped at the host with a host-emitted local
  `host_schema_outdated` notice.

- **I-14** (Persistence atomicity): A `HistoryRecord` write MUST be all-
  or-nothing: a startup scan MUST NOT yield half-records.

- **I-15** (Entitlement scope): A subscriber MUST NOT receive a notice
  whose `scope` is outside its entitlement.

- **I-16** (Burst rule): ≥ 4 emits of the same `(kind, severity)` within 2
  seconds MUST collapse to one `NoticeAdded` plus `NoticeUpdated`s; the
  user MUST NOT see ≥ 4 distinct surfaces.

- **I-17** (Supersede ordering): A coalesce-supersede MUST broadcast
  `NoticeRemoved(old, superseded)` before broadcasting `NoticeAdded(new)`.

- **I-18** (No focus theft): A notification toast MUST NOT steal keyboard
  focus from the active input.

- **I-19** (DND honored): When OS-level "do not disturb" is on, the host
  MUST suppress sound and MAY suppress OS-fallback, but MUST still render
  the in-app surface.

- **I-20** (Boot-id tag): Every `NoticeId` MUST embed the daemon's
  `BootId`; a host comparing two `NoticeId`s with different boot ids MUST
  treat them as referring to disjoint logical notices.

- **I-21** (Click-action validation pre-dispatch): A host MUST validate the
  click-action's payload (URL scheme, thread existence) before dispatching
  the side effect; an invalid payload becomes `dismiss_only` and the click
  ack is still sent.

- **I-22** (Permission revoke on cancel): When the operation requesting a
  permission is cancelled, the producer MUST call `revoke_notice` on the
  prompt before the operation tears down; the host MUST NOT see a
  permission prompt outliving its requesting operation.

- **I-23** (Default toast duration clamp): A producer-supplied
  `expires_in_ms` outside `[Min, Max]` per §3.2.4 MUST be clamped, not
  rejected.

- **I-24** (One surface per id per host): At most one in-app surface per
  `NoticeId` per host MUST be visible at any time. Replay during a
  reconnect MUST NOT produce a duplicate.

- **I-25** (Stream-seq monotonic): On a stable subscription with no
  reconnect, `stream_seq` on the multiplexed channel MUST monotonically
  increase by exactly 1 per frame.

---

## 6. Test contract

The following `T-N` scenarios are normative test obligations. Each MUST be
exercised by at least one automated test before this spec is treated as
implemented. Tests SHOULD live under
`crates/caduceus-notice/tests/` and `crates/caduceus-zed/tests/`.

### 6.1 Emit and validation

- **T-1** (`emit_notice` happy path): emit a `Notice {kind: profile_switch,
  severity: Info}`; assert `id` is allocated, `live_table` contains it,
  one `NoticeAdded` is broadcast, `history.append` is queued.
- **T-2** (`emit_notification` happy path): emit a `Notification {kind:
  build_done, severity: Info, expires_in_ms: 6000}`; assert
  `expires_at == created_at + 6000ms`, expiry scheduler enqueues it.
- **T-3** (Reject invalid kind): emit with a kind not in §4.2; assert
  `NoticeId::REJECTED` returned, no broadcast, no history write,
  `error`-level log emitted.
- **T-4** (Reject auto-expire on notice): call `emit_notice` with
  `expires_in_ms = Some(_)`; assert REJECTED. Covers `Z-8`.
- **T-5** (Reject unsafe URL): emit with click-action `open_external_url
  { url: "javascript:alert(1)" }`; assert REJECTED. Covers `I-12`.

### 6.2 Coalesce, bucket, burst, supersede

- **T-6** (Coalesce same-severity): emit two notifications with equal
  `coalesce_key` and equal severity; assert one `NoticeAdded` plus one
  `NoticeUpdated{coalesce_count: 2}`, only one entry in `live_table`.
- **T-7** (Bucket fallback drops): empty the bucket for `(retry_exhausted, Warning)`;
  emit a fresh notification (no coalesce target); assert dropped (no
  broadcast), metric `notice.dropped` incremented. Covers `I-3`.
- **T-8** (Coalesce permitted past empty bucket): empty the bucket; emit a
  notification whose `coalesce_key` matches a live one; assert
  `coalesce_count` incremented, `NoticeUpdated` broadcast, no drop.
- **T-9** (Burst rule): emit 5 `(build_failed, Error)` notifications within
  1 second; assert one `NoticeAdded` and ≤ 4 `NoticeUpdated`s with
  `body_plain` updated to burst template, no separate `NoticeAdded`s.
  Covers `I-16`.
- **T-10** (Supersede on severity bump): live `Warning` notice exists with
  key K; emit `Error` notice with same K; assert
  `NoticeRemoved(old, superseded)` precedes `NoticeAdded(new)`. Covers
  `I-17`.

### 6.3 Lifecycle

- **T-11** (`QUEUED → LIVE` on connect): emit with no host; assert
  notice in `live_table` with `state = QUEUED`. Connect host;
  assert `NoticeReplayBegin`, `NoticeAdded`, `NoticeReplayComplete`
  delivered in order. Covers `I-5`.
- **T-12** (`QUEUED → DEFERRED → CLOSED:host_lost`): emit; advance time
  past `host_lost_max_age` with no host; assert
  `outcome = host_lost` in `history`, no broadcast (no host).
- **T-13** (Auto-expire fires): emit notification with 6 s expiry;
  advance time 7 s; assert `NoticeRemoved(_, expired)` broadcast,
  removed from `live_table`.
- **T-14** (Hover suspends): emit notification, 6 s expiry; at t=2 s
  send `notice_extend(id, 4000)`; advance time 5 s; assert notice
  still `LIVE`. Covers `I-8`.
- **T-15** (Producer revoke): emit; producer calls `revoke_notice(id, "test")`;
  assert `NoticeRemoved(_, revoked, "test")` broadcast, history records
  reason.
- **T-16** (Idempotent ack): send `notice_dismiss(id)` twice; assert second
  call returns `AckResult::AlreadyAcked`, no second broadcast. Covers
  `I-6`.

### 6.4 Wire and replay

- **T-17** (Subscribe with `since_*` after reconnect): two LIVE notices
  emitted at stream_seq 7 and 11; host reconnects with `since_stream_seq:
  10`; assert only the second is replayed.
- **T-18** (Boot-id rotation forces full replay): host reconnects with a
  stale `since_boot_id`; assert both notices replayed regardless of
  `since_stream_seq`. Covers `I-20`.
- **T-19** (Coalesce collapses across disconnect): host disconnects; daemon
  emits 3 coalesce-equivalent notifications; host reconnects; assert
  exactly one `NoticeAdded` is replayed with `coalesce_count: 3`. Covers
  `Z-29`.
- **T-20** (Schema-version reject at host): host receives a frame with
  `schema_version: 99`; assert host drops the frame and emits a local
  `host_schema_outdated` notice. Covers `I-13`.
- **T-21** (Multiplex stream_seq monotonic): subscribe with both channels;
  emit a snapshot delta and a notice delta interleaved; assert
  `stream_seq` increases by exactly 1 per frame across both channels.
  Covers `I-25`.

### 6.5 Permissions

- **T-22** (Permission prompt → reply): permission engine calls
  `emit_notice {permission_prompt}` with `prid: P`; host sends
  `notice_click(id, permission_reply{allow, once})`; assert engine
  receives reply with prid `P`, notice closes `clicked`. Covers atomicity
  per `Z-26`.
- **T-23** (Permission dismiss = deny): host sends `notice_dismiss(id)` on
  a `permission_prompt`; assert engine receives `deny:once`, notice
  closes `clicked` (with payload `deny:once`). Covers `I-10`.
- **T-24** (Deadline timeout): emit `permission_prompt` with deadline 5 s;
  no host; advance 6 s; assert `revoke_notice(reason = "prompt_timeout")`,
  engine treated as `deny:once`. Covers `I-9`.
- **T-25** (Cancel revokes prompt): emit `permission_prompt` for an
  in-flight tool call; cancel the session per
  `spec-m-session-lifecycle.md §11.2`; assert `revoke_notice` is called
  before the session destroy completes. Covers `I-22`.

### 6.6 Cross-runtime

- **T-26** (CLI render add/upd/rem): with a CLI host attached, emit a
  notification, increment its coalesce_count, dismiss it; assert three
  ANSI-colored lines on stdout matching the §3.5.2 template, in order.
- **T-27** (CLI no-auto-dismiss): with CLI host attached, emit a 6 s
  notification; advance host clock 7 s without daemon expiry; assert
  the CLI surface is still rendered (the daemon, not the CLI, owns
  expiry). Covers `I-11`.
- **T-28** (Headless permission auto-deny): with headless host attached,
  emit `permission_prompt`; assert the host issues a
  `permission_reply{deny, once}` ack within the configured grace, daemon
  closes notice `clicked`. Covers `Z-32`.
- **T-29** (CLI permission keystrokes): with CLI host attached and TTY,
  emit `permission_prompt`; user types `c <id> Y\n`; assert
  `permission_reply{allow, always}` ack delivered.

### 6.7 Persistence and history

- **T-30** (History append): emit, dismiss; assert `notices.log` contains
  one record with `outcome = dismissed`.
- **T-31** (History query by severity): emit 3 notices (Info/Warning/Error),
  close all; query `severity_min: Warning`; assert 2 records returned.
- **T-32** (Compaction drops old): write 100 `Info` records dated 8 days
  ago; run compaction; assert all dropped, file rewritten atomically.
  Covers `Z-38`.
- **T-33** (History cursor invalidation): begin paginated query; trigger
  compaction; resume with cursor; assert `CursorInvalidated` returned.
  Covers `Z-40`.
- **T-34** (Atomic record write): inject a crash mid-write; restart;
  assert startup scan truncates the partial tail and the prior records
  are intact. Covers `I-14`.

### 6.8 OS escalation and accessibility

- **T-35** (OS fallback when unfocused): GUI host with window unfocused
  for ≥ 5 s; emit `Routine` notification; assert `showNotification` host
  capability called once with key = NoticeId.
- **T-36** (OS fallback dedup on focus): user focuses window with both
  in-app and OS surfaces present and unseen; assert OS surface is
  programmatically dismissed only after `notice_seen` is sent. Covers
  `Z-18`.
- **T-37** (Single click ack): user clicks OS notification; assert
  exactly one `notice_click` ack delivered, in-app surface dismissed
  silently. Covers `I-7`.
- **T-38** (DND suppresses sound): with OS DND on, emit `Immediate`
  notice; assert in-app surface rendered, no sound played. Covers
  `I-19`.
- **T-39** (ARIA roles): assert `NoticeBanner` for `Info` carries
  role="status", for `Warning|Error` role="alert"; `NotificationToast`
  for `Info|Warning` role="status", for `Error` role="alert";
  `permission_prompt` carries role="alertdialog". Covers `Z-10`.

### 6.9 Coverage of every Z-invariant

The mapping of `Z-N` invariants to tests is:

| Z-N    | Covered by                          |
| ------ | ----------------------------------- |
| Z-1    | T-22, T-23                          |
| Z-2    | (Producer behaviour; covered by failure-mode integration tests of `auth_expired`/`connection_lost`/`rate_limited`) |
| Z-3a   | T-22                                |
| Z-4    | T-1, T-20 (boot-id format)          |
| Z-5    | T-1                                 |
| Z-7    | T-3 (no recursion: rejected emit produces no further notice) |
| Z-8    | T-4                                 |
| Z-9    | T-24 (one surface per id)           |
| Z-10   | T-39                                |
| Z-11   | T-10 (severity-bump → fresh id)     |
| Z-12   | (Static check on enum + linter on producers) |
| Z-13   | T-37                                |
| Z-14   | T-14                                |
| Z-15   | (UI-side test in caduceus-zed)      |
| Z-16   | T-38                                |
| Z-17   | T-12                                |
| Z-18   | T-36                                |
| Z-19   | (Internal scheduler unit test)      |
| Z-20   | T-14                                |
| Z-21   | T-16 (no second outcome)            |
| Z-22   | T-15 (revoke after close: no-op)    |
| Z-23   | T-21                                |
| Z-24   | (Static check on `NoticeUpdated` field set) |
| Z-25   | T-19                                |
| Z-26   | T-22                                |
| Z-27   | (Entitlement integration test)      |
| Z-28   | T-17                                |
| Z-29   | T-19                                |
| Z-30   | T-26 (CLI renderer per host choice) |
| Z-31   | T-27                                |
| Z-32   | T-28                                |
| Z-33   | (Host-side URL allowlist UI test)   |
| Z-34   | (Host-side URL allowlist UI test)   |
| Z-35   | T-9                                 |
| Z-36   | T-10                                |
| Z-37   | T-34                                |
| Z-38   | T-32                                |
| Z-39   | (HistoryRecord field shape unit)    |
| Z-40   | T-33                                |
| Z-41   | (Notification center UI test)       |
| Z-42   | (Cross-host history visibility unit)|
| Z-43   | T-22                                |
| Z-44   | T-23                                |
| Z-45   | T-25                                |
| Z-46   | T-24                                |
| Z-47   | (Permission engine integration test)|
| Z-48   | (`permission_denied_blocking` coalesce unit) |
| Z-49   | (Failure-mode revoke integration test) |
| Z-50   | T-3                                 |
| Z-51   | (NoticeId allocator unit test)      |

Tests not labelled T-N exist as unit tests in the relevant crate. The point
of the table is that every `Z-N` has at least one named owner.

---

## 7. Out of scope

The following are NOT part of this contract and MUST NOT be relied on by
implementers of this spec:

1. **Visual design / motion**: exact pixel layout, animation easing curves,
   color tokens. The spec constrains role, severity-glyph identity, and
   timing (auto-expire) only. Visual polish is a separate document.
2. **Localisation pipeline**: how `i18n_key` resolves to a string, how
   `i18n_args` are interpolated, plural forms, fallback chain. Hosts MUST
   ship with a bundled English resolver as a baseline; everything else is
   out of scope.
3. **Push to remote devices**: mobile, email, Teams, Slack, webhook. The
   notice channel is host-local. A future spec MAY define a webhook
   adapter; this spec does not.
4. **Telemetry payloads**: counters and metrics referenced (e.g.
   `notice.dropped`, `notice.suppressed`) are NOT specified in shape here.
   The metrics layer owns that contract.
5. **Per-user policy editor for which kinds escalate to OS**: out of
   scope. The default escalation policy in §3.2.3 is the only contract.
6. **Cross-daemon notice sharing**: notices are scoped to one daemon. A
   user with two daemons running sees two separate notice streams.
7. **End-to-end encryption of notice payload**: the daemon → host channel
   uses the orchestrator-snapshot transport's existing security posture.
   Notice content is not specially encrypted on top.
8. **Notice editor / authoring UI for end users**: users do not author
   notices. Producers are a closed set (§3.1.5).
9. **Notice templates / themes**: a producer cannot ship a custom toast
   shape. The host owns rendering.
10. **A "snooze" feature**: dismissing a notice is permanent for that id.
    Snooze (re-fire later) is not in this spec.
11. **Reordering of live notices by user**: the daemon orders by
    `created_at`; the host MUST NOT permute. UI MAY group by kind for
    display, but the underlying order is fixed.
12. **"Mark all read"**: there is no batch-ack. Each notice is its own
    surface.

---

## 8. Open questions

These questions are NOT answered by this spec. They MUST be resolved before
schema_version 1 is frozen for external consumers; until then they are
documented to track outstanding ambiguity.

### 8.1 Is `lock_contention_blocking` a Notice or a Notification?

The multi-repo lock manager can either *block* an operation (the user must
release a contended lock before progress is made) or *report* a contended
lock (informational). The current §3.1.5 table assigns the `Notice` form to
the blocking case and the `Notification` to the post-acquire case. Open
question: are there intermediate cases (e.g. "lock held for >30 s, would you
like to break it?") that need a third kind? Tentative answer: surface as a
`permission_prompt`-shaped Notice with a custom `i18n_key`, no new kind.

### 8.2 Should `coalesce_count` be wire-truncated?

A pathological producer could in theory drive `coalesce_count` to UINT32_MAX.
The spec currently uses `u32`. Open question: do we cap at, say, 9999 and
display "9999+"? If yes, where — daemon (clamps the field) or host (clamps
the display)? Tentative answer: daemon clamps to 9999 to keep wire traffic
predictable; host displays "9999+" as appropriate.

### 8.3 Re-emit a permission prompt after host disconnect?

If the host disconnects mid-prompt and reconnects within deadline, replay
covers it (§3.4.6). But what if the disconnect spans the deadline? The spec
currently revokes the prompt with `prompt_timeout` regardless of host
presence. Open question: should the host presence delay the deadline?
Tentative answer: no — the deadline is the producer's contract with the
permission engine, which MUST be honoured even with a happy host. A user
who can't see the prompt is treated as no user; deny is correct.

### 8.4 Should `notice_extend` be coalescable with itself?

Hover events fire many times. The current §3.4.4.3 says each `notice_extend`
is its own RPC. Open question: should the daemon coalesce repeated extends
within a 100 ms window? Tentative answer: yes, but as a transport
optimization, not a behavioural change; the daemon's effective state is the
last extend received.

### 8.5 Multi-host interaction

Two hosts subscribed (e.g. zed pane + CLI tail). User dismisses on host A.
The current spec says host B sees `NoticeRemoved` and dismisses its surface.
Open question: should the *act* of host B re-rendering (post-replay) of an
already-dismissed-on-A notice ever happen? The replay is filtered by
`live_table`, so dismissed-on-A is no longer LIVE and won't replay. So the
edge case is "host B was already showing the surface when A dismissed" —
covered by `NoticeRemoved`. Tentative: this is fine; document explicitly in
§3.4.6.

### 8.6 Ordering of `permission_reply` ack and `revoke_notice` on cancel

§3.8.3 requires `revoke_notice` before cancel completes. But what if the
user clicks `allow` at the exact moment the cancel arrives? Either:

- The click ack wins: permission engine receives `allow`, but the
  underlying operation is already cancelled.
- The revoke wins: permission engine receives nothing or
  `prompt_revoked_due_to_cancel`.

Tentative answer: the daemon resolves the race via the atomicity of `Z-26`
on the live_table — whichever holds the lock first wins, and the loser
sees `AlreadyAcked`. The permission engine MUST be tolerant of receiving an
allow for a now-cancelled operation; it is the operation's responsibility
to re-check its own liveness before acting on the permission grant.

### 8.7 Schema migration

§3.4.7 mandates `schema_version` bumps. We do not yet have a migration plan
for cross-daemon-version subscriptions (newer host talking to older daemon
or vice versa). Tentative answer: hosts always tolerate older daemons by
falling back; daemons reject hosts whose declared `max_supported_schema` is
lower than the daemon's minimum. The migration plan is deferred.

---

## 9. Cross-references

- **Wire transport**: `spec-orchestrator-status-snapshot.md §3.4`
  (subscribe / fingerprint / stream_seq / boot_id / replay).
- **Permissions**: `spec-m-permissions.md §6` (deadline source) and
  `§7` (always-scope policy persistence) and `§9` (default prompt
  pipeline endpoint).
- **Session cancel**: `spec-m-session-lifecycle.md §11.2` (in-flight
  permission/ask-user prompts on cancel — §3.8.3 of this spec).
- **Apache-2.0 attribution**: `symphony-fit-analysis.md §A` (template for
  Symphony-derived files).

---

## 10. Implementation notes (informative)

This section is informative; nothing here is normative.

- The daemon-side notice service should be a single actor owning
  `live_table`, the suppression buckets, and the expiry heap. All emit /
  ack / extend / revoke operations should serialize through it. This
  satisfies `Z-26` atomicity naturally.
- The wire codec should share the orchestrator-snapshot's existing CBOR
  encoder; doing so lets the multiplex `stream_seq` invariant `I-25` be
  enforced by the existing snapshot transport without a second sequencer.
- The CLI renderer should use the existing daemon-state subscriber that
  the `caduceus tail` command already uses; adding a `--notices` flag to
  that command is a cheap path to T-26..T-29.
- Persistence should reuse the existing per-workspace data directory layout;
  `notices.log` sits alongside `snapshot.log` and shares the same
  fsync-on-close discipline.
- The notification-center UI in caduceus-zed should be a single pane that
  calls `notice_query_history` lazily; it is NOT a permanent subscriber to
  the `notice` channel.

---

*End of spec-notice-notification.md.*
