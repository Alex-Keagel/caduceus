# Caduceus Behavioural Specification — Agent Runner Contract

> **Attribution.** Portions of this specification are derived from
> OpenAI Symphony, `openai/symphony` commit `58cf97d` © OpenAI 2025,
> licensed under Apache-2.0. Specific derivations are cited inline as
> `(file:line)` against the Symphony tree, or as `SPEC §N` against the
> Symphony reference SPEC. Caduceus carries the Apache-2.0 grant
> forward; this document and the implementation it specifies are
> Apache-2.0 licensed.

Status: **P0 normative** — sibling to `spec-caduceus-orchestrator-algorithm.md`
(forthcoming, hereafter "spec #1") and `spec-multi-repo-workspace-model.md`
(forthcoming, hereafter "spec #3"). The orchestrator is meaningless
without a runner contract; this document fixes the boundary between the
`caduceusd` daemon and any agent subprocess it owns.

The keywords **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, **MAY**,
and **REQUIRED** are to be interpreted as in RFC 2119.

> **⚠️ Known residual issues — iter-28 backlog (2026-04-29).**
> The following items were surfaced by `gpt-5.4` standalone review at iter-27
> with verbatim replacement text saved in
> `private/reviews/iter27-spec2-gpt.md`. They were not blocking for the
> iter-27 ship — the spec converged on `claude-opus-4.6` + `gpt-5.3-codex`
> at min 9 / 9 respectively. Resolve in iter-28+.
>
> 1. **§3.2 token reconciliation** — `turn_end.tokens_at_turn_end` and
>    `exit.final_tokens` are required wire fields but only `token_update`
>    reaches `reconcile_tokens`. Agents that report only at turn end or
>    exit will undercount. Add absolute-mode `reconcile_tokens` calls in
>    both `turn_end` and `exit` branches.
> 2. **§3.3 Stage 3b SIGKILL** — "Always succeeds" overclaims;
>    `posix_kill(-pid, SIGKILL)` can fail and `wait_for_exit(ε₂)` can time
>    out. Emit `signal_error` on `Err`, emit `reap_timeout` on timeout,
>    return `stage = "sigkill_timeout"` with `exit_code: None`.
> 3. **§4.1 heartbeat timeout** — heartbeat emission cadence is normative
>    but no rule covers heartbeat *receipt* timeout. Add: if no heartbeat
>    is accepted for `heartbeat_timeout_ms` (default `3 × heartbeat_interval`,
>    min `1000ms`), emit `protocol_violation { reason: "heartbeat_timeout" }`
>    and invoke `stop_cascade`.
> 4. **§3.3 platform mapping** — pseudocode is POSIX-only despite the spec
>    being cross-platform. Add Windows mapping: Stage 2 `CloseHandle(hStdinWrite)`
>    + `WaitForSingleObject`, Stage 3a `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)`
>    when child has console else no-op, Stage 3b `TerminateProcess(hProcess, 1)`.
>    Composite bound `ε₁ + 2·grace_period_ms + ε₂` MUST hold on all platforms.
> 5. **§3.1 `shell_wrap` injection** — opt-in `bash -lc` is a shell-injection
>    footgun unless `workflow.command_string` is constrained. Add fail-closed
>    rule: command string MUST be a static workflow-authored literal, MUST
>    NOT be assembled from prompt/agent/tool/env input; refuse spawn with
>    `SpawnRefused::ShellWrapUntrustedInput` if non-static.
> 6. **§4.1 `seq=0` rationale** — explanation cites the wrong branch
>    (high-water comparison); the explicit `event.seq == 0` reserved-value
>    guard fires first. Reword to cite the explicit guard, classify via
>    `seq_regression { kind_detail: "stutter" }`, drop without forwarding.
> 7. **§4.4 `runner_seq` stamp rule (Z-23) duplication** — restated across
>    §4.1, §3.2, §4.4 with slightly varying wording. Collapse to a single
>    canonical Z-23 statement in §4.4 ("stamp only after `forward_to_daemon`
>    returns `Ok`"); other sections cite as `"§4.4 Z-23 stamp rule"` and
>    MUST NOT restate.
> 8. **§4.1 `cross_run_handoff`** — listed in v1 closed set but payload is
>    owned by future spec #5. Reserve the name only; in v1 any received
>    `cross_run_handoff` frame MUST trigger
>    `stop_cascade(reason = "unknown_message_kind")`. Spec #5 promotes
>    it into the closed set when payload lands.

---

## 0. Architectural premise

Caduceus has chosen the **C-hybrid** topology: a long-lived `caduceusd`
daemon spawns and owns one OS process per agent. The **runner contract**
specified here is the boundary between `caduceusd` (parent) and the agent
process (child). Everything inside the agent process is opaque to the
daemon except through this contract.

This spec is normative for:

- the daemon-side **RunnerProcess** that spawns, supervises, and reaps
  one **AgentProcess**;
- the wire shape on the agent's stdio;
- the **lifecycle Session** (the runner-managed agent lifetime),
  distinct from Engine B's user-facing chat session;
- the failure surface and exit-code semantics;
- token accounting reconciliation between agent reports and daemon
  state;
- the ACP (Agent Coding Protocol) adapter shim.

This spec is **not** normative for:

- the orchestrator's decision logic — what to dispatch, when to retry,
  how to schedule (→ spec #1);
- the workspace filesystem layout, snapshotting, or branch isolation
  (→ spec #3);
- specific MCP tool server protocols (already covered by
  `spec-m-permissions.md`).

---

## 1. Scope

**In scope.**

- argv construction for the agent subprocess (no implicit shell wrapper
  unless the workflow explicitly opts in);
- environment variable merge order (daemon defaults < workflow declared
  < hook exported);
- the JSONL line-oriented event schema and its required `kind` values;
- the three-stage **stop cascade** (cooperative cancel → stdin close →
  signal escalation);
- the **permission elevation** request/response surface that bridges
  the runner contract into the existing `caduceus-permissions` envelope;
- exit-code semantics (`0 == normal, anything else == abnormal`; nothing
  else);
- token accounting reconciliation rules (delta vs absolute; monotonic
  non-decreasing per Run);
- the **ACP adapter** rules: when to use ACP, how it maps onto the same
  internal `Session` trait;
- the agent-side **WorkSource** read surface: agents **MAY** read
  WorkSource state through the daemon, but **MUST NOT** write to it
  directly. WorkSource writes are an orchestrator responsibility per
  Symphony SPEC §11.5.

**Out of scope.** Specific MCP server protocols; specific agent vendor
implementations (OpenAI Codex, Claude Code, Gemini, etc.); the
orchestrator's dispatch decision (→ spec #1); workspace fs layout (→
spec #3); approval-card UI (→ `spec-m-ui-approval-card.md`).

---

## 2. Terms

| Term | Definition |
| --- | --- |
| **`caduceusd`** | The long-lived caduceus daemon. Owns the WorkSource, orchestrator, runners, and approval broker. |
| **RunnerProcess** | The daemon-internal supervisor for exactly one AgentProcess. State lives in the daemon's address space. There is one RunnerProcess per running attempt. |
| **AgentProcess** | The OS subprocess spawned by a RunnerProcess. Speaks JSONL or ACP on its stdio. Opaque to the daemon except through this contract. |
| **RunAttempt** | A single end-to-end execution of an agent for a Run, identified by `run_id` plus an attempt number assigned by the orchestrator (spec #1 §2). A Run may have many RunAttempts over its life; a RunAttempt does not survive process death. The runner contract is scoped to one RunAttempt at a time. **Run** (logical-across-attempts identity) is owned by spec #1; this contract operates on RunAttempts. |
| **Session** *(runner-scoped)* | The runner-managed lifetime of one AgentProcess. Bounded by spawn at one end and process reap at the other. **Distinct** from Engine B's chat-level Session, which is user-facing and may outlive multiple runner Sessions. Where ambiguous, this document writes **runner Session** vs **engine Session**. |
| **TurnEvent** | A single JSONL line with `kind == "turn_event"`. Streamed by the AgentProcess between `turn_start` and `turn_end`. |
| **JSONL line** | A single line of UTF-8 terminated by `\n`. Each line is one self-contained JSON object. No multi-line records. |
| **ACP adapter** | The thin shim that translates the Agent Coding Protocol into the same internal `Session` trait the JSONL transport implements. Same events surface to the daemon either way. |
| **StopCascade** | The three-stage shutdown sequence defined in §3.3. |
| **`GraceWindow`** | Composite timing-budget shorthand used wherever this spec needs the full runner shutdown / wedged-queue wall-clock envelope. It expands to the three independently tuned bounds defined in §3.3: `ε₁` (Stage-1 control-frame enqueue deadline; default **100 ms**, minimum **100 ms**), `grace_period_ms` (the per-runner grace applied to Stage 2 stdin-close wait and Stage 3a SIGTERM-reap wait; default **1000 ms**), and `ε₂` (Stage 3b SIGKILL→reap budget; default **150 ms**, minimum **150 ms**). The total worst-case wall-clock budget is `ε₁ + 2·grace_period_ms + ε₂` (I-3). `GraceWindow` is shorthand for that composite envelope, not a separate configurable parameter; `ε₁`, `grace_period_ms`, and `ε₂` remain independently owned by §3.3 / §5. |
| **WorkSource** | Caduceus's tracker abstraction (the analogue of Symphony's tracker per SPEC §11.5). Owned by the daemon. Read-only from the agent's perspective. |

---

## 3. Normative algorithms

The pseudocode below is illustrative. Implementations **MUST** preserve
ordering, state-transition, and timeout semantics; they **MAY** choose
any concurrency primitive that does.

### 3.1 `spawn_agent(run, workspace, workflow)`

```text
spawn_agent(run, workspace, workflow) -> RunnerProcess:
    // Argv: direct exec by default, no shell wrapper. Caduceus departs
    // from Symphony here, which uses `bash -lc <cmd>` (app_server.ex:
    // start_port). The shell indirection is a footgun that exists in
    // Symphony because workflows often need shell globbing; caduceus
    // makes this opt-in.
    if workflow.shell_wrap == true:
        argv = ["bash", "-lc", workflow.command_string]
    else:
        argv = workflow.argv  // a Vec<String>; MUST NOT be empty

    // Env merge order, lowest precedence to highest. Hooks always win
    // because that is how secrets are sourced (Symphony SPEC §10.1).
    env = empty_map()
    env.merge(daemon_default_env(run))      // sets all §5 I-9 reserved env vars

    // I-9.2 enforcement on the daemon-default layer: validate the
    // daemon-authored value of CADUCEUS_DECLARES_TOOL_USE BEFORE the
    // caller-supplied layers merge in. Caller-supplied attempts to set
    // this key are gated by the I-9 reserved-env-override check below
    // (and so cannot reach the value-validator), which makes this the
    // sole reachable enforcement point for I-9.2's invalid-value path.
    match env.get("CADUCEUS_DECLARES_TOOL_USE") {
        Some("0") | Some("1") => { /* ok; proceed */ }
        _ => {
            emit_diagnostic("reserved_env_invalid_value", run.id,
                            "CADUCEUS_DECLARES_TOOL_USE");
            return Err(SpawnRefused::ReservedEnvInvalidValue);
        }
    }

    // I-9 enforcement on CADUCEUS_RUNNER_UUID: validate the
    // daemon-authored value is present, non-empty, and a syntactically
    // valid UUIDv4. Parallel to the CADUCEUS_DECLARES_TOOL_USE
    // validation above. A missing or malformed value indicates a
    // daemon-side bug (RESERVED_ENV_KEYS injection failed) and MUST
    // refuse spawn rather than silently fall through.
    // `is_valid_uuid_v4(v)` (normative helper): `v` MUST be a
    // 36-character ASCII string in canonical 8-4-4-4-12 lowercase hex
    // form (RFC 4122 §3); the version nibble (first hex digit of the
    // third group, i.e. bits 48–51 of the 128-bit value) MUST equal
    // `4`; the variant bits (top two bits of the first byte of the
    // fourth group, i.e. bits 64–65) MUST equal `0b10` (RFC 4122
    // "DCE 1.1" variant). The nil UUID
    // (`00000000-0000-0000-0000-000000000000`) and the max UUID
    // (`ffffffff-ffff-ffff-ffff-ffffffffffff`) MUST be rejected even
    // if a syntactic check would otherwise accept them. UUIDv1 / v3 /
    // v5 / v7 and any non-RFC-4122 variant MUST be rejected.
    // Acceptance is bit-normative; lexical similarity is insufficient.
    match env.get("CADUCEUS_RUNNER_UUID") {
        Some(v) if is_valid_uuid_v4(v) => { /* ok */ }
        _ => {
            emit_diagnostic("reserved_env_invalid_value",
                            run.id, "CADUCEUS_RUNNER_UUID");
            return Err(SpawnRefused::ReservedEnvInvalidValue);
        }
    }

    // Cache hook-exported env once: `hooks.exported_env(run)` may have
    // side effects (secret materialisation, audit logging) and MUST be
    // invoked exactly once per spawn. Both the merge below and the
    // I-9.2 caller-supplied-override scan reuse this same snapshot.
    let hook_env = hooks.exported_env(run);

    env.merge(workflow.env)                  // workflow YAML
    env.merge(hook_env)                      // hook-exported secrets

    // I-9.2 enforcement: refuse caller-supplied overrides of reserved env keys.
    // Applies to the workflow.env + hook_env layers (caller-supplied);
    // the daemon_default layer is privileged and exempt.
    const RESERVED_ENV_KEYS: &[&str] = &[
        "CADUCEUS_RUN_ID",
        "CADUCEUS_DECLARES_TOOL_USE",
        "CADUCEUS_PROTOCOL_VERSION",
        "CADUCEUS_DAEMON_SOCKET",
        "CADUCEUS_WORKSPACE_PATH",
        "CADUCEUS_AGENT_NAME",
        "CADUCEUS_RUNNER_UUID",
        // any future CADUCEUS_*-prefixed reserved keys
    ];
    let caller_supplied_env = workflow.env.iter().chain(hook_env.iter());
    for (k, _v) in caller_supplied_env {
        if RESERVED_ENV_KEYS.contains(&k.as_str()) {
            emit_diagnostic("reserved_env_override", run.id, k);
            return Err(SpawnRefused::ReservedEnvOverride);
        }
        if k.starts_with("CADUCEUS_") {
            emit_diagnostic("reserved_env_prefix", run.id, k);
            return Err(SpawnRefused::ReservedEnvPrefix);
        }
    }

    // cwd is the workspace path. NEVER anything else (I-1).
    // X-8: defence-in-depth. The runner MUST re-validate the path
    // through spec #3 §3.4 immediately before spawn, even though spec
    // #3's create_workspace already canonicalised it. This narrows the
    // TOCTOU window between create_workspace returning and spawn, and
    // is the runner-side mirror of spec #3 invariant I-1.
    cwd = workspace.path
    match validate_workspace_path(workspace.path, workspace.root):
        Ok(canonical) if canonical == workspace.path:
            // ok; proceed
        Ok(_) =>:
            // path moved under us between create and spawn — refuse.
            emit_event(daemon_event_channel, {
                kind:    "cwd_validation_error",
                run_id:  run.id,
                error:   "path_changed_post_create",
            })
            return SpawnRefused { reason: "cwd_validation_error" }
        Err(e):
            emit_event(daemon_event_channel, {
                kind:    "cwd_validation_error",
                run_id:  run.id,
                error:   e,
            })
            return SpawnRefused { reason: "cwd_validation_error" }
    assert cwd is canonicalised and rooted in workspace.root

    // Spawn with the stdio triple piped. The agent never inherits
    // caduceusd's listening socket (I-2).
    child = os.spawn(
        argv,
        cwd            = cwd,
        env            = env,
        stdin          = Pipe,
        stdout         = Pipe,
        stderr         = Pipe,
        close_fds      = true,
        new_process_group = true,   // so SIGTERM in §3.3 hits children
    )

    // Sample the child's OS-level creation identifier immediately
    // post-spawn. This is the ground-truth reference value for the
    // I-9.1 broker peer-authentication `(pid, process_start_time)`
    // check (§5): without it, the broker has no baseline to compare
    // an incoming peer against. Implementations MUST use the same
    // per-OS API enumerated in I-9.1 (`/proc/<pid>/stat` field 22 on
    // Linux, `proc_pidinfo PROC_PIDTBSDINFO` on macOS,
    // `GetProcessTimes` on Windows). On Linux, implementations
    // SHOULD acquire a `pidfd` for `child.pid` here and retain it on
    // the `RunnerProcess` for the lifetime of the run, so that the
    // creation-id sample and all subsequent peer-auth comparisons
    // are race-free against PID reuse. Any sampling failure MUST
    // refuse spawn (the daemon cannot supervise an agent it cannot
    // re-identify against its broker).
    // `sample_creation_id(pid)` (normative helper): returns the
    // host-local opaque creation identifier paired with `pid` using
    // exactly the same per-OS APIs as the I-9.1 broker peer-auth
    // path (§5): Linux — field 22 (`starttime`) of
    // `/proc/<pid>/stat`; macOS —
    // `proc_pidinfo(pid, PROC_PIDTBSDINFO, ...)` →
    // `(pbi_start_tvsec, pbi_start_tvusec)`; Windows —
    // `GetProcessTimes(handle, &lpCreationTime, ...)` `FILETIME`. The
    // returned token is opaque, host-local, and daemon-boot-scoped
    // (see §5 Normalization rule); it MUST be used only for
    // byte-equal lineage checks against `runner.child_creation_id`
    // and MUST NOT be normalised to wall-clock or compared across
    // hosts/boots. Any retrieval error (read failure, permission
    // denied, peer disappeared, OS API not supported on this
    // host/version) MUST be returned as `Err(CreationIdUnavailable)`
    // and the caller MUST fail closed.
    let child_creation_id = match sample_creation_id(child.pid) {
        Ok(id) => id,
        Err(e) => {
            emit_diagnostic("child_creation_id_unavailable", run.id, e);
            child.kill();
            child.reap();
            return Err(SpawnRefused::CreationIdUnavailable);
        }
    };

    runner = RunnerProcess {
        run_id:           run.id,
        pid:              child.pid,
        // I-9.1 ground-truth reference for broker peer auth.
        // Opaque, host-local, daemon-boot-scoped (see §5
        // Normalization rule).
        child_creation_id: child_creation_id,
        spawned_at:       now(),
        workspace_path:   cwd,
        // From daemon config (cross-spec lock §5); per-runner snapshot at spawn.
        grace_period_ms:  daemon.config.grace_period_ms,
        // Per-RunAttempt UUID minted at spawn and injected into the
        // agent env as CADUCEUS_RUNNER_UUID (I-9). Stored here so the
        // runner can cross-check heartbeat.runner_uuid against the
        // minted value.
        runner_uuid:      env.get("CADUCEUS_RUNNER_UUID")
            .expect("validated at spawn-time gate; see §3.1 reserved-env enforcement")
            .clone(),
        // N-3: initialise to the most-restrictive defaults BEFORE any
        // handshake is read. This ensures that any code path which
        // observes `runner.capabilities` between spawn and handshake
        // (notably stop_cascade in §3.3, which reads
        // accepts_stdin_control) sees a safe value rather than a
        // partially-initialised one. A successful handshake refines
        // these in §3.2; a missing handshake leaves them as set here
        // (which is the T-4 fallback contract).
        capabilities:     Capabilities::most_restrictive(),
        last_reported_tokens: TokenTotals::ZERO,
        seq_high_water:   0,
        // Z-24: first inbound frame MUST carry seq == 1; receiver tracks
        // contiguity via expected_seq, initialised here so §3.1 is
        // self-contained and the §3.2 validate_and_wrap check has a
        // defined initial value.
        expected_seq:     1,
        // declares_tool_use_env: Option<String> — captured from
        // CADUCEUS_DECLARES_TOOL_USE at spawn for §3.2 handshake
        // cross-check (I-9.2). Stashed on the runner so `read_event_loop`
        // does not need access to the spawn-scope `env` map.
        declares_tool_use_env: env.get("CADUCEUS_DECLARES_TOOL_USE").cloned(),
    }

    emit_event(daemon_event_channel, {
        kind:      "spawn",
        run_id:    run.id,
        pid:       child.pid,
        argv:      argv,
        cwd:       cwd,
        timestamp: spawned_at,
    })

    spawn_task(read_event_loop(runner, child.stdout))
    spawn_task(drain_stderr(runner, child.stderr))
    return runner
```

**Notes.**

- The argv default is **direct exec, no shell**. Symphony's
  `bash -lc` indirection (`app_server.ex` `start_port`) is opt-in via
  `workflow.shell_wrap = true`. Workflows that opt in accept the
  associated injection surface.
- `env` merge precedence is **deterministic** and mandated by I-9
  below.
- `cwd` validation is a two-sided contract with spec #3. Spec #3's
  `create_workspace` (§3.5) canonicalises the path and rejects
  symlink-escapes; the runner here re-validates by calling spec #3
  §3.4 `validate_workspace_path(workspace.path, workspace.root)`
  immediately before spawn (X-8). On any error the runner MUST refuse
  to spawn and emit a `cwd_validation_error` event, mirroring
  Symphony's `validate_workspace_cwd` (`app_server.ex` `validate_workspace_cwd`).

**`grace_period_ms` immutability (normative).** The `grace_period_ms`
field on each `RunnerProcess` instance MUST be sampled at spawn time
from `daemon.config.grace_period_ms` (the assignment at the struct
literal above) and MUST NOT be re-read from `daemon.config` thereafter.
All §3.3 callers (StopCascade Stages 2 and 3a) and any other site that
needs the per-runner grace MUST use `runner.grace_period_ms`, never
`daemon.config.grace_period_ms`. This pins per-runner grace-period
semantics across daemon config hot-reload: a long-running runner
spawned under one configured grace MUST continue to use that grace
through its full StopCascade, even if the daemon's live config is
mutated mid-run. Tested by **T-1** / **T-9** (composite-budget bounds
are computed against the snapshotted value).

### 3.2 `read_event_loop(runner, stdout)`

```text
read_event_loop(runner, stdout):
    reader = LineReader(stdout, max_line_bytes = 1_048_576)
    handshake_seen = false

    loop:
        match reader.read_line():
            Eof:
                emit_event(daemon_event_channel, {
                    kind: "stdout_eof", run_id: runner.run_id,
                })
                return

            PartialLineThenEof(bytes):
                // T-2: graceful degradation. Do NOT fatally parse a
                // truncated final line; emit a warning and exit.
                emit_event(daemon_event_channel, {
                    kind:    "stdout_truncated",
                    run_id:  runner.run_id,
                    bytes:   len(bytes),
                })
                return

            LineTooLong:
                // I-4 violation. Treat as protocol error; do NOT try
                // to recover by reading further.
                stop_cascade(runner, reason = "line_too_long",
                             grace_period_ms = runner.grace_period_ms)
                return

            Ok(line):
                event = try_parse_jsonl(line)
                if event is Err:
                    // X-7 / I-4: any unparseable line on the JSONL transport
                    // is a protocol error, not a recoverable hiccup. The line
                    // boundary is `\n` and lines are independent, so a parse
                    // failure means the agent emitted bytes it cannot defend.
                    // Spec #1 §3.x maps this to ExitReason::Abnormal via the
                    // worker exit channel. The PartialLineThenEof branch
                    // above (T-2) is the ONLY exception — a truncated final
                    // line on EOF is graceful, not a protocol violation.
                    emit_event(daemon_event_channel, {
                        kind:   "malformed_jsonl",
                        run_id: runner.run_id,
                        raw:    redact(line),
                        error:  event.err,
                    })
                    stop_cascade(runner, reason = "malformed_jsonl",
                                 grace_period_ms = runner.grace_period_ms)
                    return

                // Run-identity validation (HOISTED above seq check
                // and handshake/T-4 branch). The agent's `run_id`
                // field is schema-required (§4.1) and MUST equal
                // `runner.run_id` (the value passed to the agent via
                // the reserved env var, §5 I-9). A mismatch means
                // either (a) a misconfigured / cross-wired agent
                // emitting frames into the wrong Session, or (b) a
                // malicious / corrupt frame attempting to mutate
                // state on a Session it does not belong to. Either
                // case is fail-closed: the runner MUST emit a
                // `protocol_violation` diagnostic with
                // `reason = "run_id_mismatch"` and trigger
                // `stop_cascade` BEFORE any further state mutation
                // (no seq advance, no handshake-flag flip, no
                // forward_to_daemon).
                if event.run_id != runner.run_id:
                    emit_event(daemon_event_channel, {
                        kind:     "protocol_violation",
                        run_id:   runner.run_id,
                        reason:   "run_id_mismatch",
                        expected: runner.run_id,
                        got:      event.run_id,
                        seq:      event.seq,
                    })
                    stop_cascade(runner, reason = "run_id_mismatch",
                                 grace_period_ms = runner.grace_period_ms)
                    return

                // Sequencing (HOISTED above the handshake/T-4 branch).
                // The agent's `seq` is **strictly increasing** per
                // runner Session (Z-24 — was "monotonic non-decreasing"
                // pre-Z-24; equal seqs are now classified as stutter,
                // not progress, and surfaced via the same
                // seq_regression diagnostic). The runner stamps
                // `runner_seq` (Z-22) on the way out — that is the
                // daemon-facing monotonic counter documented in §4.4;
                // the prior `orchestrator_seq` field has been removed.
                //
                // Z-24 contiguity at validate_and_wrap: the receiver
                // tracks `runner.expected_seq`, initialised to `1` at
                // spawn (§3.1). The first frame on the inbound stream
                // MUST carry `seq == 1`; any other starting value
                // (including `seq = 2`, which would otherwise satisfy
                // the obsolete `seq > seq_high_water` check) is
                // rejected here. Subsequent frames MUST carry
                // `seq == expected_seq` (contiguous; §3.5.1
                // back-pressure-driven gaps occur AFTER this point
                // on the post-coalesce path, not on the wire from the
                // agent).
                //
                // Hoist rule (normative): this seq check MUST run BEFORE
                // both the handshake-parse branch AND the T-4
                // missing-handshake fallback. A bad-seq runtime first
                // frame (e.g. `turn_start` with `seq = 2`) on a stream
                // where no handshake is sent MUST NOT flip
                // `handshake_seen = true` via T-4, because doing so
                // would cause a subsequent legitimate `handshake`
                // frame to trip the `late_handshake` branch and
                // `stop_cascade` — contradicting T-13(c-runtime)
                // "NO stop_cascade". Hoisting restores symmetry
                // between c-handshake and c-runtime: in BOTH
                // sub-cases a bad-seq first frame is dropped with
                // zero side-effects on `runner.capabilities`,
                // `handshake_seen`, `expected_seq`, or
                // `seq_high_water`. See T-13(c-runtime).
                if event.seq != runner.expected_seq:
                    kind_detail =
                        if event.seq == 0
                            // §4.1 reserved seq=0 — non-fatal
                            // stutter-drop. Routes to diagnostic only;
                            // does NOT trip stop_cascade.
                            then "stutter"
                        else if event.seq <= runner.seq_high_water
                            // duplicate (==) or out-of-order replay (<);
                            // both classified as "stutter" per Z-24
                            // (drop + log, no stop_cascade)
                            then "stutter"
                        else
                            // event.seq > expected_seq — agent
                            // skipped. Includes T-13 case (c):
                            // seq == 2 as the first frame, in both
                            // c-handshake and c-runtime sub-cases.
                            then "gap"
                    emit_event(daemon_event_channel, {
                        kind:        "seq_regression",
                        run_id:      runner.run_id,
                        got:         event.seq,
                        expected:    runner.expected_seq,
                        high_water:  runner.seq_high_water,
                        kind_detail: kind_detail,
                    })
                    // Drop the frame with NO mutation of any runner
                    // state: `runner.capabilities` stays at its
                    // current value (most_restrictive() per the
                    // §3.1 spawn init if no handshake has been
                    // seen yet); `handshake_seen` stays as-is
                    // (false in the c-runtime / c-handshake first-
                    // frame cases); `expected_seq` is NOT advanced;
                    // `seq_high_water` is NOT advanced; the
                    // post-Ok `runner_seq` stamp is never reached
                    // for this frame, so the wire counter stays
                    // gap-free. Connection stays live; the next
                    // inbound line is re-evaluated as the first
                    // frame. See T-13.
                    continue
                    // policy: surface but do not crash; downstream
                    // consumers reconstruct ordering by received_at
                    // and runner_seq (which IS strictly increasing —
                    // see §4.4).
                else:
                    runner.seq_high_water = event.seq
                    runner.expected_seq   = event.seq + 1

                if not handshake_seen:
                    if event.kind != "handshake":
                        // T-4: missing handshake. Treat as
                        // most-restrictive defaults; do NOT crash.
                        // C1: handshake_seen MUST be set here so a
                        // later `handshake` line cannot retroactively
                        // mutate capabilities mid-session.
                        // Hoist precondition: by the time we reach
                        // this fallback the hoisted seq check above
                        // has already accepted the frame
                        // (seq == expected_seq); a bad-seq runtime
                        // first frame would have been dropped with
                        // zero state mutation, preserving
                        // handshake_seen = false so a later
                        // legitimate handshake is processed
                        // normally rather than misclassified as
                        // late_handshake. See T-13(c-runtime).
                        runner.capabilities = Capabilities::most_restrictive()
                        handshake_seen = true
                        // fall through to the per-event dispatch loop
                        // and do NOT re-loop on the same line
                    else:
                        // Handshake with seq == expected_seq (==1).
                        // The hoisted seq check above already
                        // verified contiguity, so we proceed
                        // directly to capability parsing without
                        // a duplicate inline check.
                        match parse_capabilities(event.payload):
                            Ok(caps) =>
                                runner.capabilities = caps
                            Err(parse_err) =>
                                // Malformed handshake payload: line was
                                // valid JSONL (otherwise try_parse_jsonl
                                // above would have fired `malformed_jsonl`),
                                // but the capabilities sub-schema is
                                // invalid (wrong type for a known bit —
                                // daemon bug; unknown capability bits
                                // are informational-only per §4.2 and
                                // MUST be ignored —
                                // missing required field, …). Fail-closed:
                                // this is NOT the T-4 missing-handshake
                                // path (T-4 is for *no* handshake frame),
                                // and it is NOT `malformed_jsonl` (the
                                // outer envelope parsed). Cascade-stop
                                // with an explicit `malformed_handshake`
                                // reason so the daemon can distinguish
                                // "agent declined to handshake" (T-4)
                                // from "agent attempted handshake but
                                // sent a payload we cannot honour".
                                emit_event(daemon_event_channel, {
                                    kind:    "malformed_handshake",
                                    run_id:  runner.run_id,
                                    seq:     event.seq,
                                    error:   parse_err,
                                })
                                stop_cascade(runner,
                                             reason = "malformed_handshake",
                                             grace_period_ms = runner.grace_period_ms)
                                return
                        // I-9.2 bidirectional env↔handshake consistency check.
                        let env_declares = runner.declares_tool_use_env.as_deref();
                        match (env_declares, runner.capabilities.declares_tool_use):
                            (Some("0"), true) =>
                                // workflow did not authorise tool use, but the
                                // agent advertises it: refuse Session at handshake.
                                emit_diagnostic("tool_use_declared_but_not_permitted", runner.run_id)
                                stop_cascade(runner,
                                             reason = "tool_use_declared_but_not_permitted",
                                             grace_period_ms = runner.grace_period_ms)
                                return Err(HandshakeRejected::ToolUseNotPermitted)
                            (Some("1"), false) =>
                                // env permits tool use but agent failed to honor
                                // the env hint. The FIRST tool_use_request frame
                                // is fatal "tool_use_without_capability" (N-3 below).
                                /* no-op-by-design; N-3 fail-closed handles first tool frame via capabilities.declares_tool_use=false */
                            (None, _) =>
                                // Daemon-bug guard: under correct daemon
                                // operation CADUCEUS_DECLARES_TOOL_USE is
                                // always set in the runner env (per I-9.2),
                                // so reaching this arm with env==None
                                // indicates a daemon bug ("env present but
                                // handshake absent" / equivalently env
                                // missing entirely). Hard-stop the cascade
                                // — debug_assert! alone would fail open in
                                // release builds and let the run continue.
                                emit_diagnostic("daemon_bug_missing_declares_tool_use_env", runner.run_id)
                                stop_cascade(runner,
                                             reason = "missing_declares_tool_use_env",
                                             grace_period_ms = runner.grace_period_ms)
                                return Err(HandshakeRejected::DaemonBug)
                            _ => {}
                        // C2: tool-using vendors MUST advertise
                        // accepts_stdin_control = true. Without it the
                        // daemon cannot deliver tool_use_response or
                        // permission_elevation_response and the agent
                        // hangs on first denial. Reject at handshake.
                        if runner.capabilities.declares_tool_use
                           and not runner.capabilities.accepts_stdin_control:
                            emit_event(daemon_event_channel, {
                                kind:    "unsupported_bidirectional_control",
                                run_id:  runner.run_id,
                                reason:  "tool_using_vendor_without_stdin_control",
                            })
                            stop_cascade(runner,
                                         reason = "unsupported_bidirectional_control",
                                         grace_period_ms = runner.grace_period_ms)
                            return
                        handshake_seen = true
                        emit_event(daemon_event_channel, {
                            kind: "handshake_ok",
                            run_id: runner.run_id,
                            capabilities: runner.capabilities,
                        })
                        continue
                else:
                    // C1: handshake already concluded (either via a
                    // real handshake line or via the most_restrictive
                    // fallback). A subsequent `handshake` event is a
                    // protocol violation: capabilities are immutable
                    // for the lifetime of the runner Session.
                    if event.kind == "handshake":
                        emit_event(daemon_event_channel, {
                            kind:    "unexpected_handshake",
                            run_id:  runner.run_id,
                            seq:     event.seq,
                        })
                        stop_cascade(runner,
                                     reason = "late_handshake",
                                     grace_period_ms = runner.grace_period_ms)
                        return

                // N-3: runtime enforcement of declares_tool_use.
                // If the agent emits a tool_use_request or a
                // permission_elevation_request without having advertised
                // declares_tool_use = true at handshake (or, equivalently,
                // because no handshake was seen and capabilities are the
                // most_restrictive() default per the runner-init in §3.1),
                // this is a protocol violation. The runner MUST terminate
                // with ExitReason::ProtocolViolation { reason:
                // "tool_use_without_capability" }; the daemon cannot
                // safely deliver responses on a stdin the agent did not
                // contract to read, so any further progress is unsafe.
                if (event.kind == "tool_use_request"
                    or event.kind == "permission_elevation_request")
                   and not runner.capabilities.declares_tool_use:
                    emit_event(daemon_event_channel, {
                        kind:    "protocol_violation",
                        run_id:  runner.run_id,
                        reason:  "tool_use_without_capability",
                        offending_kind: event.kind,
                        seq:     event.seq,
                    })
                    stop_cascade(runner,
                                 reason = "tool_use_without_capability",
                                 grace_period_ms = runner.grace_period_ms)
                    return

                // Closed-set enforcement of `event.kind` (Z-11
                // normative; §4.1 inbound table). The two §4.1
                // tables are jointly the closed set of v1 message
                // kinds. Any inbound `kind` not present in
                // AGENT_TO_DAEMON_KINDS is a protocol violation per
                // §4.1's Z-11 paragraph and MUST trigger
                // `stop_cascade(reason = "unknown_message_kind")`.
                // This guard runs AFTER the handshake / late_handshake
                // branches above (which special-case `kind == "handshake"`)
                // and AFTER the N-3 tool-use-without-capability check
                // (which bypasses delivery for those two kinds when
                // capability is absent), and BEFORE forward_to_daemon
                // so unknown kinds never consume a `runner_seq` slot
                // and never reach daemon-side consumers.
                if event.kind not in AGENT_TO_DAEMON_KINDS:
                    // Note: this guard fires regardless of whether handshake has been
                    // seen. Pre-handshake unknown kinds are rejected here just as
                    // post-handshake unknown kinds are; in both cases stop_cascade
                    // returns before any runner_seq is consumed (Z-22 invariant
                    // intact).
                    emit_event(daemon_event_channel, {
                        kind:           "protocol_violation",
                        run_id:         runner.run_id,
                        reason:         "unknown_message_kind",
                        offending_kind: event.kind,
                        seq:            event.seq,
                    })
                    stop_cascade(runner,
                                 reason = "unknown_message_kind",
                                 grace_period_ms = runner.grace_period_ms)
                    return

                match validate_payload_schema(event.kind, event.payload):
                    Ok => {}
                    Err(field_name) =>
                        emit_event(daemon_event_channel, {
                            kind:       "malformed_payload",
                            run_id:     runner.run_id,
                            frame_kind: event.kind,
                            field:      field_name,
                        })
                        stop_cascade(runner,
                                     reason = "malformed_payload",
                                     grace_period_ms = runner.grace_period_ms)
                        return Err(RuntimeError::MalformedPayload)

                // Heartbeat runner_uuid cross-check (defense-in-depth).
                // This check runs ONLY AFTER payload-schema validation has
                // accepted the payload, so a heartbeat missing
                // `payload.runner_uuid` (or carrying it with the wrong type)
                // is classified as `malformed_payload`, not as
                // `runner_uuid_mismatch`.
                if event.kind == "heartbeat"
                   and event.payload.runner_uuid != runner.runner_uuid:
                    emit_event(daemon_event_channel, {
                        kind:     "protocol_violation",
                        run_id:   runner.run_id,
                        reason:   "runner_uuid_mismatch",
                        expected: runner.runner_uuid,
                        got:      event.payload.runner_uuid,
                        seq:      event.seq,
                    })
                    stop_cascade(runner, reason = "runner_uuid_mismatch",
                                 grace_period_ms = runner.grace_period_ms)
                    return

                // Z-23 (post-backpressure stamp). The runner_seq
                // counter is the daemon-facing strictly-increasing,
                // gap-free wire counter (§4.4). It MUST be consumed
                // ONLY by frames that successfully cross the
                // §3.5.1 bounded inbound queue (i.e.
                // `forward_to_daemon` returned Ok); frames dropped
                // or coalesced under back-pressure MUST NOT consume
                // a runner_seq, otherwise the wire counter would
                // develop gaps that are indistinguishable from a
                // genuine `runner_seq_gap` protocol violation. We
                // therefore wrap the event without a runner_seq
                // first, attempt delivery, and stamp post-Ok.
                //
                // `frame_id` is an internal monotonic diagnostic
                // identifier (separate from `runner_seq`) used for
                // `runner_backpressure` / coalesce diagnostics so
                // dropped frames can still be referenced in logs.
                wrapped = {
                    received_at:     now(),
                    frame_id:        runner.next_frame_id(),
                    runner_seq:      None,   // stamped post-Ok below
                    inner:           event,
                }
                match forward_to_daemon(wrapped):
                    Ok(slot) =>
                        // Single-producer post-backpressure stamp:
                        // the runner is still the sole writer of the
                        // counter and the frame has cleared the
                        // bounded queue, so the gap-free invariant of
                        // §4.4 holds.
                        slot.runner_seq = runner.next_runner_seq()
                        // Side effects MUST only fire once the frame
                        // has been successfully queued; a dropped
                        // frame MUST NOT mutate token state, raise a
                        // permission UI, or record an advertised exit.
                        if event.kind == "token_update":
                            reconcile_tokens(runner, event.payload)
                        if event.kind == "permission_elevation_request":
                            handle_elevation_request(runner, event)
                        if event.kind == "exit":
                            // agent advertised an orderly exit; do NOT race
                            // with reaper, just record.
                            runner.advertised_exit = event.payload
                    Dropped(reason) =>
                        // §3.5.1 back-pressure / coalesce: no
                        // runner_seq consumed, no daemon-visible wrap.
                        // No success-side effects — the dropped-frame
                        // diagnostic below is the only observable.
                        emit_event(daemon_event_channel, {
                            kind:     "runner_backpressure",
                            run_id:   runner.run_id,
                            frame_id: wrapped.frame_id,
                            reason:   reason,
                        })
```

**Notes.**

- `max_line_bytes = 1_048_576` (1 MiB) matches Symphony's
  `@port_line_bytes` ceiling (`app_server.ex` line-buffer constant).
  Caduceus pins this value normatively (I-4a below).
- Stderr is captured by `drain_stderr` for diagnostics only, **never**
  as a control channel (Symphony SPEC §10.2).
- Forwarding to the daemon's event channel is the *only* place
  cross-agent observability happens. Per-agent ordering is preserved;
  cross-agent ordering is best-effort by `received_at` (matching
  Symphony's "no global clock" stance, B.5).

### 3.3 `stop_cascade(runner, reason, grace_period_ms)`

Symphony's port-close cascade (`app_server.ex` `stop_session` →
`stop_port`) collapses into close-then-kill because the BEAM port
abstraction does the work. Caduceus exposes the three stages
explicitly because we run on tokio and need to be precise about
timeouts.

```text
stop_cascade(runner, reason, grace_period_ms):
    deadline_total = now() + ε₁ + (2 * grace_period_ms) + ε₂
    // I-3: this function MUST return by deadline_total.
    // ε₁, ε₂ are the timing-budget tunables defined in §3.3.

    // Stage 1: cooperative cancel via stdin, IF supported.
    // Z-26: Stage 1 MUST NOT write to runner.stdin directly. Direct
    // writes here race with the §3.5.2 outbound queue (which owns the
    // single-writer invariant for runner stdin) and would let a
    // backpressured queue stay full while a fresh `cancel` line slips
    // past it, breaking ordering. Instead, the cascade MUST enqueue
    // the cancel via the runner's outbound control channel; if the
    // queue is at capacity (stdin already wedged), the cascade MUST
    // emit a `stage1_cancel_skipped` diagnostic and proceed to
    // Stage 2 — the cancel cannot be delivered, but Stage 2 (stdin
    // close) and Stage 3 (signals) still run.
    if runner.capabilities.accepts_stdin_control:
        let frame = jsonl({
            kind:      "cancel",
            run_id:    runner.run_id,
            reason:    reason,
            timestamp: now(),
        })
        match enqueue_outbound_control(runner, frame, deadline_ms = ε₁):
            Ok => {}
            Err::QueueFull | Err::WriterWedged | Err::Timeout => {
                emit_diagnostic({
                    kind:    "stage1_cancel_skipped",
                    reason:  reason,
                    cause:   <Err variant>,
                })
                // fall through to Stage 2
            }
    // else: skip stage 1, proceed to stage 2.

    // Stage 2: close stdin, await voluntary exit.
    close(runner.stdin)
    // Note: close(runner.stdin) MAY race a still-flushing §3.5.2 writer
    // task; the writer MUST observe `EBADF`/`EPIPE` and exit cleanly.
    // A partial Stage-1-cancel frame on the agent side is acceptable
    // because Stage 2 is the actual termination signal.
    if wait_for_exit(runner, grace_period_ms) is Exited(code):
        return Reaped { signal: None, exit_code: code,
                        stage: "stdin_close" }

    // Stage 3a: SIGTERM the new process group.
    posix_kill(-runner.pid, SIGTERM)   // negative pid = process group
    if wait_for_exit(runner, grace_period_ms) is Exited(code):
        return Reaped { signal: SIGTERM, exit_code: code,
                        stage: "sigterm" }

    // Stage 3b: SIGKILL. Last resort. Always succeeds; the process
    // group is destroyed by the kernel.
    posix_kill(-runner.pid, SIGKILL)
    code = wait_for_exit(runner, ε₂)   // bounded by sigkill_reap_budget (§3.3)
    return Reaped { signal: SIGKILL, exit_code: code,
                    stage: "sigkill" }
```

**Timing-budget tunables (authoritative definition site).** The shutdown
budget is composed of:

- `ε₁` — Stage-1 enqueue latency budget. ε₁ is the budget for
  enqueuing-or-skipping the Stage-1 `cancel` control message
  (whichever applies under §3.3 cancellation policy) once the daemon
  has decided to cancel the run; it does NOT measure time to first
  SIGTERM. Stage 2 (stdin close) and Stage 3a (SIGTERM grace) are each
  bounded by grace_period_ms; Stage 3b (SIGKILL→reap) is bounded by ε₂.
  Default **100 ms** (minimum **100 ms**), configurable via `shutdown_enqueue_budget_ms`.
- `grace_period_ms` — per-runner SIGTERM→SIGKILL grace. Default **1000 ms**,
  configurable per-daemon via config.
- `ε₂` — Stage-3b reap budget after SIGKILL (time from SIGKILL dispatched
  to `waitpid`/`WaitForSingleObject` resolution and FD reclamation).
  Default **150 ms** (minimum **150 ms**), configurable via `sigkill_reap_budget_ms`. This
  tunable is hereby normatively named **`sigkill_reap_budget`** (alias
  `sigkill_reap_budget_ms`).

**Total bound (normative).** `total_shutdown_budget ≤ ε₁ + 2·grace_period_ms + ε₂`.
Every cite of this bound in
§5 / §6 / I-3 / T-1 / T-9 MUST use this exact expression and refer back to §3.3.

**Stage 1 drain bound (Z-26 normative).** Stage 1 cooperative cancel
MUST drain through the §3.5.2 outbound queue within `ε₁` (default
**100 ms**, the `deadline_ms = ε₁` argument to
`enqueue_outbound_control` above) or the cascade MUST escalate to Stage 2 —
emitting a `stage1_cancel_skipped` diagnostic and proceeding to `close(stdin)`.
The `ε₁` bound is normatively part of the cascade timing math: the
total Stage-1 budget is bounded by *enqueue-or-skip* (≤ `ε₁`),
not by the agent's response to the `cancel` frame, because the
cascade does not wait for the agent to acknowledge — it relies on
Stage 2 (stdin close) and Stage 3 (signals) for guaranteed reap.

**Per-stage decomposition (normative).** Stage 1 ≤ `ε₁`; Stage 2 ≤
`grace_period_ms` (await voluntary exit on stdin close); Stage 3a ≤
`grace_period_ms` (await SIGTERM reap); Stage 3b ≤ `ε₂`. The two
`grace_period_ms` waits (Stage 2 and Stage 3a) are the source of the
factor of 2 in the **Total bound** above. The composite
bound `ε₁ + 2·grace_period_ms + ε₂` is invariant **I-3**. This
decomposition is consumed by §3.5 (back-pressure stage budgets),
§5 (invariants table), and §6 (T-1, T-9 timing assertions).

**Citation.** Symphony's equivalent is the `stop_session` →
`stop_port` cascade in `app_server.ex` (the close-then-grace-then-kill
flow). Caduceus splits it into three explicit stages because the
cooperative `cancel` JSONL frame is a caduceus addition not present in
the Symphony reference; Symphony has no inbound JSON-RPC `cancel`
notification — it relies on closing the port directly.

### 3.4 `acp_adapter`

Some agents speak the **Agent Coding Protocol (ACP)** instead of the
caduceus JSONL transport. The runner contract MUST treat ACP as a
peer transport, not a special case visible to the orchestrator.

**Z-25 — same validation+dispatch path (normative).** ACP frames
translated by the adapter MUST be funnelled through the *same*
read_event_loop validation, sequence-stamping, and forward_to_daemon
machinery defined in §3.2 — the adapter MUST NOT have a parallel
forwarding implementation. Concretely: after step "translate ACP frame
to internal TurnEvent" the adapter MUST hand the synthesised event to
the same `validate_and_wrap` step that the JSONL path uses (the same
`seq_regression` guard from §3.2, the same `runner_seq` post-Ok
stamping rule from Z-22 — `runner_seq` is consumed only after
`forward_to_daemon → Ok` and is NOT consumed on `Dropped(reason)`,
keeping the wire counter gap-free; `frame_id` serves as the internal
correlation id for dropped frames — the same back-pressure queue from
§3.5.1). Likewise,
outbound frames from the daemon MUST go through the *same*
§3.5.2 outbound queue before the adapter translates them to ACP
shape and writes to the agent. This unifies invariants
(seq-monotonicity, queue saturation, stop_cascade visibility) across
both transports — without it I-10's "indistinguishable in shape"
guarantee is unenforceable because the two transports could drift on
which validations fire.

```text
acp_adapter:
    on spawn:
        ACP MUST be explicitly negotiated. If the workflow / engine
        has not requested ACP for this Session, the runner stays on
        JSONL and this adapter is not invoked. If ACP IS requested,
        perform the ACP handshake; if the peer responds with a
        compatible ACP-version frame, the transport is ACP for the
        lifetime of the Session. If the peer fails to respond as
        ACP, or responds with an unsupported version, reject per
        the **Version negotiation** clause below — the runner MUST
        NOT silently fall back to JSONL once ACP has been requested
        (protocol confusion is a refusal-to-run condition).

    on every ACP frame from agent:
        translate to internal TurnEvent (same payload shape as JSONL
        kinds defined in §4.1). Hand to the same validate_and_wrap
        step as §3.2 (Z-25): the seq_regression check fires; on accept
        (Ok path) forward_to_daemon enqueues onto the §3.5.1 inbound
        queue and ONLY THEN is `runner_seq` stamped (post-Ok rule from
        Z-22 / §3.2). On `Dropped(reason)` no `runner_seq` is consumed
        — `frame_id` is the internal correlation id — so the wire
        counter remains gap-free.

    on every internal request from daemon (cancel, elevation
    response, …):
        hand to the §3.5.2 outbound queue (same single-writer
        invariant as JSONL); the adapter dequeues, translates from
        internal shape to ACP frame, writes to agent.

    invariant: the daemon's view (the wrapped event stream from
    §3.2) is indistinguishable in shape between an ACP-speaking
    agent and a JSONL-speaking agent of the same vendor running the
    same task. This is **I-10** below and is the essential guarantee
    that lets spec #1's orchestrator dispatch on `Session` trait
    alone — and Z-25's same-path requirement is what makes it true
    in practice.
```

**Version negotiation.** If the agent advertises an ACP version the
daemon does not implement, the daemon MUST emit an
`unsupported_acp_version` daemon-internal diagnostic and reject the
Session; the daemon MUST NOT silently fall back to JSONL. Rejection
proceeds via StopCascade (§3.3) using
`reason = "acp_version_mismatch"`.

(See also §8.2 — version-pinning policy, Resolved (v1).)

### 3.5 Back-pressure (C4)

`forward_to_daemon` (§3.2) hands events from the per-runner transport
reader to one or more daemon-side consumers (orchestrator state,
event log, engine fan-out, ACP bridge). A bounded queue sits between
the reader and those consumers. Without an explicit back-pressure
contract a misbehaving consumer or a burst of `turn_event` telemetry
can either block the reader (deadlocking the JSONL/ACP transport,
which in turn causes the agent to block on its own stdout write) or
grow the queue without bound (OOM). This subsection is normative.

The runner has **two** distinct frame paths and they have separate
back-pressure regimes; conflating them was a defect in earlier drafts
(gpt-N-3). §3.5.1 specifies the **inbound** path (agent stdout →
daemon consumers), §3.5.2 specifies the **outbound** path (daemon →
agent stdin). Engine reattach is **not** part of either path: it is
an engine→daemon RPC consumed via `Cmd::Reattach` (§4.5, spec #1 §4)
and never traverses these queues.

#### 3.5.0 Frame priority class

To make "higher priority class" precise (it was prose in earlier
drafts), the runner classifies every frame on either path into one of
five ordered classes. Lower numeric value = higher priority. Coalescing
and saturation drops apply **only within the same class**; a frame in
class N can never displace or be displaced by a frame in class M ≠ N.

```text
enum FramePriorityClass {
    Control     = 0,   // handshake, cancel, cancel_ack, shutdown,
                       // permission_elevation_response, tool_use_response
    Heartbeat   = 1,   // heartbeat
    Lifecycle   = 2,   // stage_transition, run_complete, exit, turn_start, turn_end, cross_run_handoff
    Event       = 3,   // turn_event, log, error, tool_use_request, permission_elevation_request
    TokenUpdate = 4,   // token_update
}
```

This enum is the **canonical** priority-class assignment for the runner
contract. §3.5.1 and §3.5.2 MUST reference this enum without restating
the membership; any drift between this enum and downstream sections is
a defect and MUST be reconciled in favour of this enum.

Diagnostic events emitted by the back-pressure policy itself
(`turn_event_backpressure_drop`, `runner_backpressure`,
`outbound_backpressure`, and Z-26's `stage1_cancel_skipped` from
§3.3 stop_cascade Stage 1) are classified as `Control` and MUST NOT
be droppable by the policies in §3.5.1 / §3.5.2.

#### 3.5.1 Inbound queue (agent → daemon)

The inbound bounded queue sits between the per-runner stdout reader
(§3.2) and the daemon-side consumers. Frame kinds that traverse this
queue, exhaustively:

- `turn_start`, `turn_event`, `turn_end`,
- `log`,
- `stage_transition`,
- `run_complete`,
- `cancel_ack`,
- `heartbeat`,
- `tool_use_request`,
- `permission_elevation_request`,
- `token_update`,
- `error`, `exit`,
- `cross_run_handoff` (reserved in v1; MUST be tolerated and forwarded if observed; runners MUST NOT emit it in v1; treated as Lifecycle priority class — see §3.5.0).

`handshake` is **not** in this list: the very first agent stdout line
is consumed by the handshake state machine in §3.2, which
validates capabilities locally and emits a synthesized `handshake_ok`
daemon-internal event. The raw `handshake` frame itself never traverses
the inbound queue — handshake-phase frames are distinct from the
steady-state queued frames enumerated above. A subsequent `handshake`
line after the handshake phase has concluded is a protocol violation
(§3.2 C1) and triggers `stop_cascade(reason = "late_handshake")`
rather than enqueue.

`reattach` is **not** in this list: it is an engine→daemon RPC frame
(§4.5) and does not flow through agent stdout. Daemon→agent frames
(`cancel`, `permission_elevation_response`, `tool_use_response`,
`shutdown`) are covered by §3.5.2.

```text
inbound_back_pressure_policy(queue, event):
    if queue.try_enqueue(event) is Ok:
        return

    // Queue is saturated. Apply differentiated drop policy.
    // Coalescing is permitted ONLY within the same FramePriorityClass.
    //
    // Saturation handling for turn_event:
    if event.kind == "turn_event":
        if event.payload.authoritative == true:
            // Authoritative turn_events are coalescible-by-keep-latest under saturation:
            // we never drop them silently, but we MAY collapse multiple in-flight
            // authoritative events into the latest one if the queue is full.
            queue.coalesce_turn_event_keep_latest(event)
            emit_diagnostic("turn_event_backpressure_drop", run_id, ...)
            return
        // Non-authoritative turn_events: drop or coalesce freely.
        queue.drop_or_coalesce_turn_event(event)
        emit_diagnostic("turn_event_backpressure_drop", run_id, ...)
        return

    // Mandatory frames (MUST NOT be dropped, MUST block on backpressure):
    // every inbound kind OTHER THAN turn_event. This includes
    // token_update, permission_elevation_request, tool_use_request,
    // error, exit, and any frame referenced in the load-bearing
    // invariants I-2..I-9. (handshake is handled by §3.2's handshake
    // state machine and never reaches this queue.)
    deadline = now() + ε₁ + (2 * runner.grace_period_ms) + ε₂
    while now() < deadline:
        if queue.try_enqueue(event) is Ok:
            return
        park(short_backoff)

    // Cannot enqueue a mandatory frame within the §3.3 composite shutdown budget:
    // the consumer side is wedged. Refusal is the only safe option.
    emit_event(daemon_event_channel, {
        kind:    "runner_backpressure",
        run_id:  runner.run_id,
        dropped: event.kind,
    })
    stop_cascade(runner,
                 reason = "runner_backpressure",
                 grace_period_ms = runner.grace_period_ms)
    return
```

Normative rules:

1. The per-runner inbound queue between the transport reader and
   daemon consumers MUST be bounded. The bound is implementation-defined;
   recommended starting value is 1024 events.
2. Under saturation the runner MUST first drop or coalesce
   non-authoritative `turn_event` telemetry. A `turn_event` is
   "non-authoritative" if its payload does not carry the reserved
   key `authoritative: true` (see §4.1).
3. Authoritative `turn_event` frames (`authoritative: true`) are
   **coalescible-by-keep-latest** under saturation: the runner MUST NOT
   silently drop them, but MAY collapse multiple in-flight authoritative
   `turn_event` frames into the latest one if the queue is full. The
   earlier frames are treated as superseded (N-4). Non-authoritative
   `turn_event` frames MAY be dropped or coalesced freely.
4. The runner MUST NOT drop, under any saturation condition,
   `token_update`, `permission_elevation_request`,
   `tool_use_request`, `error`, `exit`, or any frame referenced by
   I-2…I-9. `handshake` is not a member of this set: it is consumed
   by the §3.2 handshake state machine before steady-state queueing
   begins and never traverses the inbound queue. Reattach is also
   not a member: it is an
   engine→daemon RPC frame consumed by `Cmd::Reattach` (spec #1 §4)
   and never traverses the per-runner queue (see §4.5).
5. If a mandatory frame cannot be enqueued within the §3.3 composite shutdown budget (`ε₁ + 2·grace_period_ms + ε₂`), the
   runner MUST emit a `runner_backpressure` event and trigger
   `stop_cascade(reason = "runner_backpressure")`.
6. The ACP adapter (§3.4) inherits these rules unchanged: ACP frames
   are translated into the same internal event shape and pushed
   through the same bounded queue, so the I-10 transparency
   guarantee holds. Saturation behaviour is indistinguishable in
   shape between transports.
7. Coalescing and prioritisation MUST be evaluated within the same
   `FramePriorityClass` (§3.5.0); a frame of one class MUST NOT be
   coalesced with or evicted in favour of a frame of a different
   class.

#### 3.5.2 Outbound queue (daemon → agent)

The outbound bounded queue sits between daemon-side producers (the
orchestrator's cancel path, the permissions broker, the shutdown
sequencer) and the per-runner stdin writer. Frame kinds that traverse
this queue, exhaustively:

- `cancel` (cooperative cancel; FramePriorityClass::Control),
- `permission_elevation_response` (FramePriorityClass::Control),
- `tool_use_response` (FramePriorityClass::Control — daemon-authored
  response to an agent-emitted `tool_use_request`),
- `shutdown` (FramePriorityClass::Control).

Engine reattach is **not** an outbound frame: it never reaches the
agent (§4.5, Y-1). Heartbeats are emitted by the agent only; the
daemon does not push heartbeats on stdin.

Normative rules:

1. The per-runner outbound queue MUST be bounded; recommended
   starting value is 64 frames (the outbound side is far lower-volume
   than inbound).
2. Every outbound frame is `FramePriorityClass::Control` and MUST NOT
   be dropped or coalesced under saturation. If the queue cannot
   accept a Control frame within the §3.3 composite shutdown budget,
   the runner MUST emit an `outbound_backpressure` diagnostic and trigger
   `stop_cascade(reason = "outbound_backpressure")`.

   **Reentrancy exemption.** The Stage-1 `cancel` frame emitted from
   within `stop_cascade` itself (the Stage-1 cancel block in §3.3) is exempt from this
   rule: a recursive `stop_cascade(reason = "outbound_backpressure")`
   trigger from inside an already-running cascade would be ill-formed.
   Stage-1 enqueue uses a short deadline `ε₁` (not `GraceWindow`); on
   `Err::QueueFull | Err::WriterWedged | Err::Timeout` the cascade
   MUST emit `stage1_cancel_skipped` and fall through to Stage 2,
   which is the actual termination signal. This is the sole exemption
   to rule 2.
3. The runner MUST NOT write any other kind on agent stdin. In
   particular: `reattach` is engine→daemon (§4.5); `heartbeat`,
   `turn_event`, `token_update`, `log`, `error`, `exit`, and all
   agent→daemon kinds in §4.1 MUST NOT be written to stdin.
4. The ACP adapter (§3.4) inherits these rules unchanged: outbound
   ACP control frames map onto the same four kinds above and traverse
   the same outbound queue.

---

## 4. Data shapes

### 4.1 JSONL line schema

Every line emitted by the agent on stdout, when the JSONL transport is
in use, MUST conform to:

```json
{
  "kind":      "<one of the required values below>",
  "run_id":    "<string; matches the run_id passed in env>",
  "seq":       <integer; STRICTLY INCREASING within this Session, starting at 1 — Z-24>,
  "timestamp": "<RFC 3339 UTC, 9-digit nanosecond resolution recommended>",
  "payload":   { <kind-specific object> }
}
```

Z-24: `seq` is a per-Session, monotonically-strictly-increasing
64-bit counter on the agent → daemon (inbound) JSONL stream. It is
**strictly increasing**, NOT monotonic-non-decreasing. Equal `seq`
values across two consecutive events are an agent bug ("stutter")
and the runner MUST surface the diagnostic per §3.2's
`seq_regression` handling (with `kind_detail = "stutter"` for any
non-zero `event.seq <= seq_high_water` — covering both equal-to
(duplicate) and strictly-less-than (out-of-order replay) — matching
the §3.2 algorithm classification and the reserved-`seq = 0` rule in
§4.1 below; semantically a stutter is **drop + log diagnostic, NO
`stop_cascade`**). The daemon-facing strictly-increasing counter is
`runner_seq` (§4.4, Z-22) — the runner stamps it **post-Ok only**
(after `forward_to_daemon → Ok`, per §3.2 and §3.5.1); on
`Dropped(reason)` no `runner_seq` is consumed (the internal
`frame_id` serves as correlation id), keeping the wire counter
gap-free regardless of agent-side `seq` behaviour.

**Starting value (Z-24 normative).** The very first frame the agent
emits on stdout — typically the `handshake` (§3.2), or the first
runtime frame when handshake is omitted (T-4 fallback) — MUST carry
`seq = 1`. The runner initialises `runner.expected_seq = 1` (and
`runner.seq_high_water = 0`) at spawn (§3.1) and enforces strict
contiguity at validate_and_wrap: the first frame is accepted iff
`event.seq == 1`, and any other starting value (including
`seq = 2`, see T-13 case (c)) is rejected as a `seq_regression`
diagnostic with `kind_detail = "gap"` and dropped without
forwarding. The value `seq = 0` is **reserved and MUST NOT appear**
on the post-spawn inbound stream; receivers MUST reject any frame
with `seq = 0` as a stutter (drop, log diagnostic, do NOT
`stop_cascade`), classifying it via the §3.2 `seq_regression`
diagnostic with `kind_detail = "stutter"` (since
`0 ≤ seq_high_water = 0` at that point) and dropping the frame
without forwarding it. This prevents the "lost first frame" hazard
where an agent stamping `seq = 0` on its first event would have
that event silently classified as a stutter against the initial
high-water of 0.

**Wire seq vs logical observation seq (Z-22/Z-23/Z-24 normative).**
Two distinct sequence concepts apply on the inbound path and MUST NOT
be conflated:

- **Wire seq** — the `runner_seq` stamped post-Ok in §3.2 (after
  `forward_to_daemon` accepts the wrapped frame). Strictly
  increasing, gap-free, monotone over the lifetime of a single
  connection between runner and daemon consumers. Z-23 fixes the
  stamp point: only frames that clear the §3.5.1 bounded queue
  consume a `runner_seq`; pre-delivery wrap, raw read, and post-
  enqueue daemon-side stamping are all forbidden. The wire
  seq is what the daemon's `runner_seq_high_water` (spec #1 §4)
  tracks.
- **Logical observation seq** — the agent-stamped `seq` field on
  the JSONL line (§4.1). Strictly increasing *as emitted by the
  agent*, but a downstream consumer MAY observe stutter or apparent
  gaps in this field's value because §3.5.1 inbound back-pressure
  policy MAY drop or coalesce non-authoritative `turn_event` frames
  (and only those — see §3.5.1 rule 2) before they reach the
  daemon's snapshot consumers. A gap in the agent-stamped `seq` as
  observed by a downstream consumer is therefore NOT a protocol
  violation; it is the expected signal that a coalesce or drop
  occurred.

Critically: the **wire `runner_seq` is gap-free even when the
agent-stamped `seq` is not** — because `runner_seq` is stamped only
on frames that survive back-pressure (Z-23: stamp at delivery, after
saturation policy has fired). A consumer that wants to detect drops
should compare *agent-stamped `seq` ranges* across consecutive
`runner_seq` values; a consumer that wants to detect daemon-side
loss should check `runner_seq` for gaps (which would be a protocol
violation triggering `runner_seq_gap` per §4.4 rule 2).

**Worked example — backpressure drop on `turn_event`:**

Suppose the agent emits four lines in order:

| agent `seq` | `kind` | `payload.authoritative` | fate |
| --- | --- | --- | --- |
| 17 | `turn_start` | n/a | enqueued |
| 18 | `turn_event` | `false` | **dropped** by §3.5.1 rule 2 (queue saturated, non-authoritative streaming partial) |
| 19 | `turn_event` | `false` | **coalesced** with seq 20 (both non-authoritative) |
| 20 | `turn_event` | `true` | enqueued (authoritative supersedes) |
| 21 | `turn_end` | n/a | enqueued |

The daemon-side consumer observes the wrapped stream:

| `runner_seq` | inner `seq` | `kind` |
| --- | --- | --- |
| 42 | 17 | `turn_start` |
| 43 | 20 | `turn_event` *(authoritative)* |
| 44 | 21 | `turn_end` |

Note that `runner_seq` advances by exactly one per surviving frame
(42 → 43 → 44, gap-free) — this is the wire-seq invariant from §4.4
rule 2. The agent-stamped `seq` jumps 17 → 20 → 21, skipping 18 and
19; the consumer correctly infers that two `turn_event` frames were
dropped or coalesced under back-pressure. A `runner_seq_gap` would
mean daemon-side loss (a protocol violation); a `seq` gap with
contiguous `runner_seq` means back-pressure did its job.

Required `kind` values are split into two tables by direction. Z-11:
the prior single table commingled `agent → daemon` kinds (which
traverse the §3.5.1 inbound queue and carry agent-stamped `seq` /
runner-stamped `runner_seq`) with `daemon → agent` kinds (which
traverse the §3.5.2 outbound queue and carry no agent-side `seq`).
Splitting them makes the validation contract per direction
unambiguous.

**Agent → daemon kinds** (inbound; agent stamps `seq`, runner stamps
`runner_seq` **post-Ok only** — after `forward_to_daemon → Ok` per
§3.2; on `Dropped(reason)` no `runner_seq` is consumed and `frame_id`
serves as the internal correlation id):

| Kind | Required when | Payload fields |
| --- | --- | --- |
| `handshake` | First line on stream SHOULD be `handshake`; if absent, T-4 fallback applies (§3.2 missing-handshake branch / §4.2). | `agent_name`, `agent_version`, `protocol`, `capabilities` (see §4.2) |
| `heartbeat` | every `heartbeat_interval` (spec #3 §4.2) | `pid`, `runner_uuid` (the daemon-minted per-RunAttempt UUID exposed via `CADUCEUS_RUNNER_UUID`) |
| `turn_start` | each new turn | `turn_id`, `prompt_hash` |
| `turn_event` | streaming during turn | free-form; opaque to runner; forwarded to daemon |
| `turn_end` | end of turn | `turn_id`, `result_summary`, `tokens_at_turn_end: TokenTotals` (canonical 5-component struct defined under **Token reporting (canonical)** in §4.1) |
| `stage_transition` | agent crosses an internal lifecycle stage (e.g. `planning → executing`) | `from`, `to`, optional `note` |
| `run_complete` | agent has finished the Run's logical work, before voluntary `exit` | `outcome` ∈ `{success, failure, aborted}`, optional `summary` |
| `cancel_ack` | agent acknowledges a daemon-issued `cancel` (§3.3 Stage 1) | `run_id`, `reason` (echoed from cancel), optional `at_seq` |
| `tool_use_request` | agent wants to call a tool | `request_id`, `tool`, `args` |
| `permission_elevation_request` | tool requires escalated permission | `request_id`, `permission_kind`, `tool`, `args_hash`, `justification` |
| `token_update` | streaming or batched | `mode` ∈ `{absolute, delta}`, **all five components of `TokenTotals`**: `input_tokens`, `output_tokens`, `cache_read`, `cache_write`, `seconds_running` (each a non-negative integer; agents that do not track a given component MUST emit `0` rather than omitting the field, so the per-component watermark / additive rules in §4.3 `reconcile_tokens` are total). See **Token reporting (canonical)** below. |
| `log` | freeform diagnostic emit | `level`, `message` |
| `error` | recoverable error the agent wants logged | `code`, `message`, optional `cause` |
| `exit` | last line before voluntary process exit | `exit_reason`, `final_tokens: TokenTotals` (canonical 5-component struct defined under **Token reporting (canonical)** in §4.1) |
| `cross_run_handoff` | reserved (normative-but-not-emitted-in-v1) | reserved for spec #5 Pattern 3 forward-compat (cross-run agent handoff). Runners MUST NOT emit this kind in v1; daemons MUST tolerate the kind appearing in event tables and parsers without treating it as a protocol error. Payload shape is owned by spec #5. |

**Daemon → agent kinds** (outbound; traverse §3.5.2 outbound queue;
NOT seq-stamped — sequencing for these frames is by enqueue order in
the single-writer outbound queue, not by an agent-stamped `seq`
field):

| Kind | Required when | Payload fields |
| --- | --- | --- |
| `cancel` | stop_cascade Stage 1 (§3.3) | `run_id`, `reason`, `timestamp` |
| `tool_use_response` | response to agent's `tool_use_request` | `request_id`, `result` *or* `error` |
| `permission_elevation_response` | response to agent's `permission_elevation_request` | `request_id`, `decision` ∈ `{allow, deny, allow_once, deny_once}` |
| `shutdown` | daemon-initiated graceful shutdown (Cmd::Shutdown) | `reason` |

**Closed set (Z-11 normative).** The two tables above are jointly the
**closed set** of runner-contract message kinds in v1. Every kind
referenced anywhere else in this spec (§3.x algorithms, §3.5.x queue
lists, §3.5.0 priority classes, §6 test contract, §4.5 reattach RPC
*excepted* — see below) MUST appear in exactly one of the two tables.
Implementations MUST treat any other `kind` value on either direction
as a protocol violation and trigger
`stop_cascade(reason = "unknown_message_kind")`. **Adding, removing,
or renaming a `kind` constitutes a runner-contract revision** and
MUST bump `CADUCEUS_PROTOCOL_VERSION` (§5 I-9 reserved env). Vendor
adapters and integrators MUST NOT introduce new kinds out-of-band.
The two tables are referenced in §3.2 by the symbolic names
`AGENT_TO_DAEMON_KINDS` (inbound table above) and
`DAEMON_TO_AGENT_KINDS` (outbound table above); these names are
spec-level constants and have no on-wire representation.

The `reattach` frame (§4.5) is deliberately **not** in either table:
it is an engine→daemon RPC and never traverses agent stdio. Diagnostic
events emitted by the runner itself (e.g. `runner_backpressure`,
`stage1_cancel_skipped`, `seq_regression`, `protocol_violation`,
`cwd_validation_error`, `token_regression`,
`unknown_permission_request_id`, `duplicate_permission_request`,
`stdout_eof`, `stdout_truncated`, `malformed_jsonl`, `malformed_handshake`, `malformed_payload`,
`reserved_env_override`, `reserved_env_prefix`,
`reserved_env_invalid_value`,
`unsupported_acp_version`,
`unsupported_bidirectional_control`, `unexpected_handshake`,
`turn_event_backpressure_drop`, `outbound_backpressure`,
`spawn`, `handshake_ok`) are **daemon-internal events** published on
`daemon_event_channel` for observability — they do not traverse agent
stdio in either direction and are therefore not part of the
runner-contract wire surface enumerated above.

**Malformed inbound payload (post-handshake) — fail-closed.** When a
runtime inbound frame (i.e. any §4.1 agent → daemon kind other than
`handshake` itself) is missing a required field or carries a
type-mismatched value relative to the schema in §4.1, the runner MUST:

1. Emit a daemon-internal diagnostic with `kind = "malformed_payload"`,
   `run_id = runner.run_id`, `frame_kind = <the violating frame's wire
   kind>`, and `field = <the violating field name>`.
2. Invoke `stop_cascade(runner, reason = "malformed_payload",
   grace_period_ms = runner.grace_period_ms)`.
3. Return `Err(RuntimeError::MalformedPayload)` to the §3.2 read loop.

This is the symmetric runtime-phase counterpart of `malformed_handshake`
(§3.2 handshake-validation branch, pre-handshake). Together they form
the fail-closed validation contract for both phases of the wire
protocol. **Unknown payload fields** (forward-compat) are NOT malformed
and MUST be tolerated and forwarded verbatim per **Schema stability
(v1)** below; only **missing-required** fields and **type-mismatched**
values trigger this path. Unknown `kind` values follow the separate
Z-11 `unknown_message_kind` path (§3.2 / closed-set guard above), not
this one.

**Independence.** Per **I-4**, every line is a complete JSON object
parseable in isolation. There are no continuation frames. There is no
length prefix. There is no multi-byte framing. A reader that has
buffered up to one `\n` has a complete record.

**Size cap.** Per **I-4a**, every JSONL line **MUST NOT** exceed 1 MiB
(`1_048_576` bytes including the trailing `\n`). This matches
Symphony's `@port_line_bytes` (`app_server.ex` line-buffer constant).
Agents that need to emit larger payloads MUST chunk them across
multiple `turn_event` lines.

**Reserved payload keys (N-4).** Payload bodies are otherwise opaque
to the daemon and the runner — they are forwarded verbatim — with
**one** normative exception: every `turn_event` payload MAY include
the reserved key `authoritative: bool` (default `false` if omitted).
This is the single key the runner inspects inside a `turn_event`
payload, and it drives the §3.5.1 coalescing rule:

- `authoritative: false` (default) — the frame is a streaming partial
  that a later `turn_event` or the trailing `turn_end` will fully
  supersede. Default coalescing applies (drop or merge under
  saturation; both frames MAY be retained for stream replay when not
  saturated).
- `authoritative: true` — the frame is a self-contained authoritative
  snapshot. Two consecutive `turn_event`s both carrying
  `authoritative: true` MAY be coalesced by keeping only the latter
  (the earlier is treated as superseded; see §3.5.1 rule 3).

**Schema stability (v1).** `authoritative` is the **only** payload
key reserved by this spec in v1. All other keys inside any payload
remain opaque to the daemon and the runner; the runner MUST forward
them verbatim and MUST NOT make routing or coalescing decisions on
them. Future reserved payload keys MUST be added by extending this
section normatively, not by treaty between vendor and integrator
(parallel to the §4.2 capability-bit stability rule).

**Token reporting (canonical).** The `token_update` frame's payload
is normatively a `TokenTotals` struct extended with a `mode` tag.
This is the single source of truth for the token wire shape; the
schema row above, the `reconcile_tokens` algorithm in §4.3, and the
T-3 test in §6 all refer back to this definition.

```text
TokenTotals := {
    input_tokens:    u64,   // prompt-side tokens consumed
    output_tokens:   u64,   // completion-side tokens emitted
    cache_read:      u64,   // tokens served from prompt cache
    cache_write:     u64,   // tokens written into prompt cache
    seconds_running: u64,   // wall-clock seconds the agent has been
                            // accumulating against this Run
}

token_update.payload := TokenTotals ∪ {
    mode: "absolute" | "delta",
}
```

Normative rules:

1. **All five components are required on every `token_update`
   frame.** Agents that do not track a given component MUST emit
   `0` rather than omitting the field, so `reconcile_tokens` (§4.3)
   applies its per-component watermark (`mode = "absolute"`) or
   additive (`mode = "delta"`) rule totally — no missing-field
   ambiguity, no implicit zero behaviour that would let a buggy
   agent silently roll a high-water back via an undefined field.
2. **Per-component, not scalar.** I-5's monotonicity invariant
   applies independently to each of the five components. There is
   **no** scalar "total tokens" anywhere in the contract; any prior
   prose that read "tokens" as a single counter is shorthand for
   "the relevant `TokenTotals` component" and MUST be interpreted
   per-component.
3. **`mode` interaction with capabilities.** An agent with
   `reports_tokens_absolute = true` (handshake bit, §4.2) SHOULD
   emit `mode = "absolute"`; if it emits `mode = "delta"` the
   daemon ignores the payload (§4.3, per-component watermark rule).
   An agent with `reports_tokens_absolute = false` SHOULD emit
   `mode = "delta"`; if it emits `mode = "absolute"` the daemon
   treats the payload as a new absolute baseline under the
   per-component watermark rule (§4.3 protocol-confusion branch).

### 4.2 Capability handshake

The first line SHOULD be `kind == "handshake"` with a `payload.capabilities`
object containing at least the following bits (default to `false` if
omitted). If the handshake is missing, the daemon executes the T-4
fallback most-restrictive policy described below (and at the
`most_restrictive` block at end of §4.2 / §6 T-4 / §3.2
missing-handshake branch); compliant runners SHOULD emit a handshake
by default.

| Bit | Meaning |
| --- | --- |
| `accepts_stdin_control` | Agent is willing to read JSONL control frames (notably `cancel`, `permission_elevation_response`, `tool_use_response`) on stdin. If `false`, the runner MUST skip Stage 1 of StopCascade. **C2 — required for tool-using vendors:** any agent that MAY emit `tool_use_request` or `permission_elevation_request` (i.e. relies on daemon-authored responses on stdin) MUST advertise `accepts_stdin_control = true`. If such an agent advertises `false`, the runner MUST refuse the runner Session at handshake by emitting an `unsupported_bidirectional_control` event and triggering `stop_cascade(reason = "unsupported_bidirectional_control")`. The capability `declares_tool_use` (next row) is the trigger. |
| `declares_tool_use` | Normative handshake bit. The agent advertises whether it MAY emit `tool_use_request` or `permission_elevation_request` during this runner Session. Default `false`. **Validation (C2):** if `declares_tool_use == true && accepts_stdin_control == false`, the runner MUST reject at handshake with `unsupported_bidirectional_control` (per the previous row). **Runtime check (N-3):** if `declares_tool_use == false` and the agent subsequently emits a `tool_use_request` or a `permission_elevation_request`, the runner MUST treat it as a protocol violation, emit a `protocol_violation` diagnostic with `reason: "tool_use_without_capability"`, and trigger `stop_cascade(reason = "tool_use_without_capability")` (terminating the RunAttempt with `ExitReason::ProtocolViolation`). This rule covers the no-handshake fallback path: when no handshake is seen, capabilities default to `most_restrictive()` (`declares_tool_use = false`), so any tool-use frame from such an agent is automatically rejected by this runtime check. |
| `supports_acp` | Informational capability bit only. Setting `true` in handshake makes the runner ELIGIBLE for ACP, but ACP MUST be selected only by explicit spawn-time negotiation per §3.4 (handshake bit alone does NOT switch transport). MUST NOT switch transports on a follow-up frame; daemon MUST treat capability bit as advertisement, not selection. |
| `streams_partials` | Agent emits `turn_event` lines incrementally during a turn (token-level or paragraph-level). If `false`, the runner SHOULD expect a single large `turn_end` only. |
| `reports_tokens_absolute` | Agent emits `token_update` with `mode == "absolute"`. If `false`, the runner MUST treat `delta` mode as authoritative; see §4.3. |

Caduceus implementations **MUST** treat any handshake bit not listed
above as informational only. The runner's behaviour MUST NOT change
based on undocumented capability bits — that is a forward-compat
hazard. Future bits will be added by extending this table normatively,
not by treaty between vendor and integrator.

If the handshake is **missing** (first line has any other `kind`), the
runner falls back to `Capabilities::most_restrictive()`:

```text
most_restrictive = {
    accepts_stdin_control:     false,
    declares_tool_use:         false,
    supports_acp:              false,
    streams_partials:          false,
    reports_tokens_absolute:   false,
}
```

This is **T-4** in §6 below.

### 4.3 Token accounting

Caduceus's reconciliation rule is taken from Symphony SPEC §13.5,
lines 1304–1328, and lifted to per-component watermarks because the
authoritative daemon-side store is `HashMap<RunId, TokenTotals>`
(spec #1 §4, X-5), not a scalar:

> The orchestrator prefers the absolute counter (`reports_tokens_absolute`
> capability bit). When the agent reports absolute values, the daemon
> applies a per-component watermark rule: for each of `input_tokens`,
> `output_tokens`, `cache_read`, `cache_write`, `seconds_running`,
> `new = max(stored, payload)`. Delta payloads from an absolute-capable
> agent are ignored to avoid double-counting; delta payloads from a
> delta-only agent are added per-component to the stored value.

Concretely:

```text
reconcile_tokens(runner, payload):
    // The daemon's authoritative store is
    // OrchestratorState.last_reported_tokens: HashMap<RunId, TokenTotals>
    // (spec #1 §4). The runner forwards `payload` to the daemon, which
    // applies the per-component rule below and re-publishes the
    // resulting TokenTotals on the snapshot bus. The runner-local
    // cache is informational only.

    let stored = daemon.last_reported_tokens.get(runner.run_id)
                 .unwrap_or(TokenTotals::ZERO)

    if runner.capabilities.reports_tokens_absolute:
        if payload.mode != "absolute":
            // payload was a delta from an absolute-capable agent.
            // Per Symphony SPEC §13.5, ignore it.
            return

        // Per-component watermark (absolute mode).
        let updated = TokenTotals {
            input_tokens:    max(stored.input_tokens,    payload.input_tokens),
            output_tokens:   max(stored.output_tokens,   payload.output_tokens),
            cache_read:      max(stored.cache_read,      payload.cache_read),
            cache_write:     max(stored.cache_write,     payload.cache_write),
            seconds_running: max(stored.seconds_running, payload.seconds_running),
        }

        // I-5 diagnostic: any per-component regression is surfaced
        // (the watermark itself is preserved by the max(...)).
        for field in [input_tokens, output_tokens,
                      cache_read,  cache_write,
                      seconds_running]:
            if payload[field] < stored[field]:
                emit_event(daemon_event_channel, {
                    kind:       "token_regression",
                    run_id:     runner.run_id,
                    field:      field,
                    got:        payload[field],
                    high_water: stored[field],
                })

        daemon.last_reported_tokens.insert(runner.run_id, updated)
        publish_run_token_update(runner.run_id, updated)

    else:
        // Delta-only agent. Add per-component.
        if payload.mode == "absolute":
            // Protocol confusion: agent sent absolute despite
            // capability false. Treat payload as a new absolute
            // baseline under the per-component watermark rule.
            let updated = TokenTotals {
                input_tokens:    max(stored.input_tokens,    payload.input_tokens),
                output_tokens:   max(stored.output_tokens,   payload.output_tokens),
                cache_read:      max(stored.cache_read,      payload.cache_read),
                cache_write:     max(stored.cache_write,     payload.cache_write),
                seconds_running: max(stored.seconds_running, payload.seconds_running),
            }

            // I-5 / T-3 diagnostic: a delta-only agent that sends
            // an absolute payload below the current per-component
            // watermark is still a regression on that component.
            // The watermark itself stays at max(prior, new) per
            // the per-component rule above; the diagnostic surfaces
            // the prior watermark and the offending new value so
            // consumers can correlate the regression with the
            // protocol-confusion event.
            for field in [input_tokens, output_tokens,
                          cache_read,  cache_write,
                          seconds_running]:
                if payload[field] < stored[field]:
                    emit_event(daemon_event_channel, {
                        kind:       "token_regression",
                        run_id:     runner.run_id,
                        field:      field,
                        got:        payload[field],
                        high_water: stored[field],
                        cause:      "delta_only_absolute_baseline",
                    })

            daemon.last_reported_tokens.insert(runner.run_id, updated)
            publish_run_token_update(runner.run_id, updated)
            return

        let updated = TokenTotals {
            input_tokens:    stored.input_tokens    + payload.input_tokens,
            output_tokens:   stored.output_tokens   + payload.output_tokens,
            cache_read:      stored.cache_read      + payload.cache_read,
            cache_write:     stored.cache_write     + payload.cache_write,
            seconds_running: stored.seconds_running + payload.seconds_running,
        }
        daemon.last_reported_tokens.insert(runner.run_id, updated)
        publish_run_token_update(runner.run_id, updated)
```

Daemon-side, the per-Run high-water is owned by spec #1 §4 as
`OrchestratorState.last_reported_tokens: HashMap<RunId, TokenTotals>`
(X-5). The runner forwards each `token_update` payload to the daemon
unmodified; the daemon applies the per-component rule above and
re-publishes the resulting `TokenTotals` on the snapshot bus. Any
runner-local cache is informational only — the daemon's value is the
authoritative watermark. This spec owns the rule for how each
component of the watermark moves; spec #1 owns the storage and spec
#4 reads it as `state.last_reported_tokens` (never
`state.token_totals`).

### 4.4 `runner_seq` (per-Run monotonic) — X-2

`runner_seq` is a `u64` counter that the runner emits with every event
it forwards to the daemon. The counter is **per-Run**, not
per-RunAttempt: it survives across attempts of the same Run because
the daemon's `RunningEntry.runner_seq_high_water` (spec #1 §4) is the
durable store, and a freshly spawned RunAttempt for an already-known
Run resumes at the daemon-side high-water rather than restarting at 0.

Normative rules:

1. The counter starts at `0` for the first event the runner ever sends
   for a `(run_id)` that the daemon has no record of.
2. Each subsequent event the runner forwards via `forward_to_daemon`
   (§3.2) MUST increment the counter by exactly one. There MUST NOT
   be gaps in the per-Run sequence; the daemon treats a gap as a
   protocol violation and triggers `stop_cascade(reason =
   "runner_seq_gap")`.

   Z-23: the runner stamps `runner_seq` **only after
   `forward_to_daemon` returns `Ok`** — i.e. only on frames that
   have successfully cleared the §3.5.1 bounded inbound queue
   (post-backpressure-survival). Stamping at any other point is
   forbidden:
   in particular, the runner MUST NOT pre-stamp `runner_seq` at the
   moment the agent's stdout line is parsed (because back-pressure
   policy in §3.5.1 may drop or coalesce frames *before* delivery,
   which would produce gaps the daemon cannot distinguish from a
   genuine protocol violation), AND the runner MUST NOT pre-stamp
   inside the validate+wrap step before delivery is confirmed
   (a `Dropped(reason)` outcome from `forward_to_daemon` would then
   consume a `runner_seq` for a frame the daemon never sees, again
   producing an unprovable gap), AND the runner MUST NOT lazily stamp
   on the daemon side after enqueue (because once the wrapped event
   sits in the queue, the runner is no longer the single producer
   of the counter and the gap-free invariant is unprovable).
   "Stamp post-Ok in the runner, never at read; never pre-delivery;
   never on the daemon side after enqueue" is the normative rule.
   Implementations MAY use an internal `frame_id` (separate from
   `runner_seq`) before delivery for diagnostic correlation; only the
   `runner_seq` stamp is governed by this rule.
3. On a fresh RunAttempt for a Run the daemon already has a
   `runner_seq_high_water` for, the runner MUST seed its counter from
   the daemon's stored high-water (passed in via spawn-time
   environment / argv per spec #1 §3.3) and continue incrementing
   from there. The runner MUST NOT reset to `0` on RunAttempt
   boundaries within the same Run. Engine reattach (§4.5) does not
   touch the runner-side counter — it only re-binds the engine to
   the daemon's existing snapshot subscription.
4. The counter is process-local for emission but Run-durable in
   identity: the daemon's `RunningEntry.runner_seq_high_water` (spec
   #1 §4) is the authoritative store across runner-process death and
   restart.
5. The counter is **distinct from** the per-event `seq` field on the
   JSONL wire shape (§4.1). The wire `seq` is a within-Session
   monotonicity check used by §3.2 step "seq_regression"; the
   `runner_seq` is the cross-attempt durable cursor used for daemon
   reconciliation and snapshot delta gap detection (spec #4 I-8).
   Spec #4's transport-level `stream_seq` is daemon-scoped and is yet
   a third counter; consumers MUST track all three independently.

### 4.5 Reattach handshake — X-3

Reattach is an **engine→daemon RPC frame**, NOT a daemon→agent stdin
control frame. The scenario it serves is "the engine restarted (or
otherwise lost its in-memory snapshot subscription) while a worker
process is still alive"; the surviving worker continues to own its own
stdout/stderr unchanged, and the engine's reattach call asks the daemon
to re-establish snapshot subscription and replay the buffered event
tail. The daemon consumes the frame via `Cmd::Reattach { run_id,
runner_seq, session_id }` (spec #1 §4) on its main mailbox — it does
**not** flow through the per-runner stdio queue defined in §3.2 / §3.5.

Wire shape (engine→daemon RPC payload):

```json
{
  "kind":    "reattach",
  "run_id":  "<RunId>",
  "payload": {
    "runner_seq":  <u64>,
    "session_id":  "<SessionId>"
  }
}
```

Internal Rust shape (matches `Cmd::Reattach`):

```rust
struct ReattachFrame {
    run_id:     RunId,
    runner_seq: u64,
    session_id: SessionId,
}

/// Z-21: Daemon reply to a `ReattachFrame`. The daemon MUST send
/// exactly one of these in response to every reattach RPC; the engine
/// MUST NOT proceed to consume buffered tail events until it has
/// observed the response. The response is delivered on the same
/// engine→daemon RPC channel as the request (it does NOT travel
/// over the snapshot bus).
enum ReattachResponse {
    /// Reattach accepted. The daemon has re-bound this engine to the
    /// run's snapshot subscription and will begin streaming buffered
    /// tail events with `runner_seq > supplied_runner_seq`. `boot_id`
    /// echoes spec #1 / Z-6 so the engine can detect daemon-restart
    /// epoch changes (same semantics as spec #4 §3.4 SubscribeAck).
    Ok {
        boot_id:                  Uuid,                  // 16-byte raw per spec #4 §3.4 SubscribeAck / Z-9
        current_runner_seq:       u64,                   // daemon's high-water at reattach time
        replay_starts_at_seq:     u64,                   // == supplied_runner_seq + 1
    },

    /// Rejected. The daemon's prior binding (if any) is left intact;
    /// the engine MUST NOT retry without first observing a fresh
    /// `boot_id` via spec #4 §3.4 subscribe (i.e. retrying with the same
    /// supplied state is futile and would loop).
    Err(ReattachError),
}

/// Z-21: Closed enum of reattach failure modes. Adding a variant
/// bumps the engine-daemon protocol version.
enum ReattachError {
    /// Supplied `runner_seq` is greater than the daemon's high-water
    /// — the engine claims to have observed events the daemon never
    /// produced (clock-skew lie, or wrong run_id). Per Z-21
    /// rule 2 the daemon MUST refuse and leave any existing binding
    /// intact.
    SeqAhead { daemon_high_water: u64 },

    /// `run_id` is unknown to the daemon (already finished and
    /// evicted from `recent_history_ring`, or never dispatched).
    UnknownRun,

    /// `session_id` does not match the daemon's record for this
    /// `run_id`. The daemon MUST forward `session_id` to the engine
    /// subsystem without inspection (per Z-21 rule 3) for *valid*
    /// reattaches; this variant covers the case where spec #8's
    /// engine subsystem reports the supplied `session_id` is
    /// inconsistent with the run's lineage. v1 implementations MAY
    /// always accept (treat as Ok) and reserve this variant for v2.
    SessionMismatch,

    /// The daemon is shutting down (Cmd::Shutdown observed); reject
    /// new reattaches.
    DaemonShuttingDown,

    /// The run is known to the daemon but has already reached a
    /// terminal state (`Completed`, `Failed`, `Cancelled` per spec #1
    /// §4) and the daemon has finalised its snapshot. Distinct from
    /// `UnknownRun` (which means the daemon has no record at all,
    /// typically post-eviction from `recent_history_ring`): here the
    /// run *did* exist, but reattach is no longer meaningful because
    /// no further events will be produced. The `final_runner_seq`
    /// lets the engine fetch the terminal snapshot via the normal
    /// spec #4 §3.4 subscribe path rather than reattach.
    RunTerminated {
        terminal_state:    TerminalState,   // Completed | Failed | Cancelled
        final_runner_seq:  u64,
    },

    /// The reattach window has elapsed: the daemon retains buffered
    /// tail events for at most `reattach_window_ms` (spec #1 §4)
    /// after the engine's last observed `runner_seq`, and the engine's
    /// supplied `runner_seq` is older than the oldest still-buffered
    /// event. The daemon CANNOT replay the gap because the events
    /// have been evicted from the per-run tail buffer. The run itself
    /// MAY still be live; only the lossless-replay guarantee is gone.
    ReattachWindowExpired {
        oldest_buffered_seq:  u64,
        daemon_high_water:    u64,
    },
}
```

**Per-variant orchestrator next-action (Z-21 normative).** The engine
(spec #1's caller of `Cmd::Reattach`) MUST react to each variant
exactly as follows; implementations MAY add diagnostics but MUST NOT
deviate from the action:

- **`Ok { boot_id, current_runner_seq, replay_starts_at_seq }`** —
  the engine MUST begin consuming buffered tail events at
  `replay_starts_at_seq` and MUST treat any `runner_seq` it has
  already observed as authoritative for dedup. If `boot_id` differs
  from any prior observed `boot_id` for the same run, the engine
  MUST discard local snapshot state and re-seed from the daemon (the
  daemon restarted between bindings).
- **`Err(SeqAhead { daemon_high_water })`** — the engine has
  *fabricated* progress (clock-skew lie or wrong run_id). The engine
  MUST NOT retry with the same supplied `runner_seq`. It MUST drop
  the local in-memory cursor and re-subscribe via the spec #4 §3.4 subscribe
  path, accepting whatever snapshot the daemon publishes as the new
  ground truth.
- **`Err(UnknownRun)`** — the run is not (or no longer) in the
  daemon's record set. The engine MUST surface this to its caller as
  a non-retryable error and MUST NOT reattempt against the same
  `run_id`. If the engine's caller still wants this work performed,
  it MUST dispatch a fresh Run via spec #1's normal dispatch path.
- **`Err(SessionMismatch)`** — the engine subsystem (spec #8) has
  determined the supplied `session_id` is inconsistent with the
  run's lineage. The engine MUST NOT retry with the same
  `session_id`; it MUST either obtain a fresh `session_id` from
  spec #8 and reattach with it, or surface the mismatch to its
  caller as a non-retryable error. v1 implementations that always
  accept (per Z-21 rule 3) will never observe this variant.
- **`Err(DaemonShuttingDown)`** — the daemon will not accept new
  reattaches. The engine MUST treat this as a transient terminal
  condition for the current daemon process: do not retry against the
  same daemon, defer reattach until a fresh daemon `boot_id` is
  observed via spec #4 §3.4 subscribe (which is the engine's signal that the
  daemon has come back).
- **`Err(RunTerminated { terminal_state, final_runner_seq })`** —
  the engine MUST NOT reattach. It MUST instead call the spec #4 §3.4
  subscribe path to fetch the terminal snapshot (the daemon is
  guaranteed to retain it for `recent_history_ring`'s retention
  window per spec #1 §4) and surface `terminal_state` to its caller.
  The `final_runner_seq` allows the engine to verify it has not
  silently missed events past its last observed cursor.
- **`Err(ReattachWindowExpired { oldest_buffered_seq,
  daemon_high_water })`** — the engine has lost lossless-replay. It
  MUST treat the local snapshot as stale, re-subscribe via spec #4 §3.4 to
  obtain a fresh full snapshot at `daemon_high_water`, and surface a
  non-fatal "snapshot resync" diagnostic to its caller. The engine
  MUST NOT re-attempt reattach with the same supplied `runner_seq`.

Daemon-side validation rules:

1. The daemon MUST validate the engine-supplied `runner_seq` against
   the value stored in `RunningEntry.runner_seq_high_water` (spec #1
   §4). On a successful match (supplied ≤ daemon's high-water) **and**
   the supplied `runner_seq` is ≥ the oldest still-buffered tail event
   (`oldest_buffered_seq`, i.e. lossless replay is still possible),
   the daemon MUST return `ReattachResponse::Ok { boot_id,
   current_runner_seq, replay_starts_at_seq }` and replay any
   buffered tail with `runner_seq > supplied_runner_seq`.
2. If the supplied `runner_seq` exceeds the daemon's high-water for
   the run, the daemon MUST refuse the reattach with
   `ReattachResponse::Err(ReattachError::SeqAhead { daemon_high_water
   })` and leave the run's existing engine binding (if any) intact.
2a. If the supplied `runner_seq` is ≤ daemon's high-water but **strictly
   less than** `oldest_buffered_seq` (the per-run tail buffer has
   evicted the events the engine would need replayed — either because
   `reattach_window_ms` has elapsed since the engine's last observed
   event, or because the buffer's bounded retention has rolled past
   the supplied cursor), the daemon MUST refuse the reattach with
   `ReattachResponse::Err(ReattachError::ReattachWindowExpired {
   oldest_buffered_seq, daemon_high_water })`. The run itself MAY
   still be live and tracked; only the lossless-replay guarantee is
   gone. The daemon MUST NOT silently return `Ok` with a truncated
   replay in this case.
3. The `session_id` is opaque to the daemon's reattach handling and
   is forwarded to the engine subsystem (spec #8) without inspection;
   the daemon MUST NOT attempt semantic validation of it. Spec #8
   MAY reject the session asynchronously and the daemon MAY surface
   that as `ReattachResponse::Err(ReattachError::SessionMismatch)`
   in v2.
4. Other specs MUST cite this section by reference rather than
   restating the frame shape. Spec #1 §8.7 and spec #4 I-6 are the
   current consumers (X-3).
5. **Idempotency (Z-21).** A reattach RPC for the same `(run_id,
   runner_seq, session_id)` tuple MUST be **idempotent**: a second
   identical call (e.g. due to engine retry on a dropped TCP
   connection) MUST observe the same response shape as the first
   (`Ok` reuses the now-current high-water; the engine treats
   `replay_starts_at_seq` as advisory and dedupes by `runner_seq`).
   The daemon MUST NOT charge a per-call cost (no counter mutation,
   no fresh `boot_id`, no fresh `Cmd::Reattach` re-issuance to the
   main mailbox) for a duplicate RPC observed within the engine's
   reasonable retry window. Implementations MAY achieve this by
   deduping at the RPC layer or by making `Cmd::Reattach`'s handler
   a no-op when the run is already bound to the calling engine.

> **Note (out of scope, v2).** A *daemon-restart-with-surviving-worker*
> scenario — where the daemon process restarts but the worker OS
> process is still running and needs to be re-seated against its
> stdio — would require a daemon→agent stdin signal to inform the
> worker of its new parent. That mechanism is **not** part of v1; it
> is a distinct feature that MAY be specified in v2. The §3.5
> mandatory-frame list, the §4.1 events table, and the JSONL
> transport here are all scoped to v1 and do not include such a
> stdin signal.

---

## 5. Invariants (MUST)

These are the load-bearing invariants of the runner contract. Each is
testable; §6 enumerates the test obligations.

- **I-1 — `cwd` is the workspace, always.** The AgentProcess MUST be
  spawned with `cwd == workspace.path` and the path MUST already have
  been canonicalised and symlink-escape-checked by spec #3. No
  exception, no environment override, no workflow override. Symphony
  parallel: `app_server.ex` `validate_workspace_cwd` →
  `:symlink_escape` rejection.

- **I-2 — No daemon-socket leak.** The AgentProcess MUST NOT inherit
  any file descriptor that connects to `caduceusd`'s control socket,
  approval-broker IPC, WorkSource RPC, or any other privileged daemon
  surface. The agent's only channels into the daemon are stdio (the
  JSONL/ACP transport) and explicitly granted MCP servers.
  Implementations MUST set `close_fds = true` (or equivalent) on
  spawn.

- **I-3 — StopCascade is bounded.** `stop_cascade` MUST return within
  the §3.3 composite bound: `total_shutdown_budget ≤ ε₁ + 2·grace_period_ms + ε₂`
  wall-clock, where `ε₁`, `grace_period_ms`, and `ε₂` are defined in §3.3.
  The daemon MUST NOT block indefinitely on agent death. Implementations
  MUST NOT hardcode a single scalar ε; the bound is composite. Tested by **T-1**.

- **I-4 — Lines are independent.** Every JSONL line MUST be
  parseable in isolation. No multi-line records. No continuation
  frames. No length prefix.

  - **I-4a — Line size cap.** Every JSONL line MUST NOT exceed 1 MiB
    (`1_048_576` bytes including the trailing `\n`). Receivers MUST
    treat oversize lines as a protocol error and trigger StopCascade.
    *(C5: corrected polarity — the intent is an upper bound on line
    size, consistent with the §3.2 `LineTooLong` branch which
    triggers `stop_cascade(reason = "line_too_long")`. The earlier
    "no single line MUST exceed" phrasing inverted the modal.)*

- **I-5 — Tokens are monotonic non-decreasing per Run, per component.**
  Each component of `last_reported_tokens` for a given `run_id`
  (`input_tokens`, `output_tokens`, `cache_read`, `cache_write`,
  `seconds_running`) MUST never decrease independently. If an agent
  reports an absolute value below the current per-component
  high-water mark on any one component, the runner MUST keep that
  component's high-water mark, emit a `token_regression` diagnostic
  event naming the offending `field`, and continue (other components
  in the same payload are still merged via `max(...)` per the
  watermark rule). Tested by **T-3**. The canonical struct is
  `TokenTotals { input_tokens, output_tokens, cache_read, cache_write,
  seconds_running }` — see **Token reporting (canonical)** in §4.1.

- **I-6 — Permission elevation is single-shot per `request_id`,
  daemon-authored response.** Each agent-emitted
  `permission_elevation_request` has exactly one daemon-authored
  `permission_elevation_response` carrying the same `request_id`
  (direction is daemon → agent per §4.1). The agent MUST NOT re-issue
  a `request_id` whose response has already been delivered; if it
  does, the runner MUST emit a `duplicate_permission_request`
  diagnostic event, ignore the duplicate, and remain running. On the
  daemon-side reply path, if the runner's outstanding-request table
  has no record of the `request_id` for which a response is being
  prepared (e.g. stale state, mis-routed reply), the runner MUST emit
  an `unknown_permission_request_id` diagnostic and write **no**
  response line to the agent's stdin. The daemon, if it needs to
  retry a denied elevation under different policy, MUST mint a fresh
  `request_id`. Tested by **T-5**.

- **I-7 — Exit code semantics are minimal.** The runner MUST
  interpret AgentProcess exit codes as exactly two values:
  `0 == normal`, anything else `== abnormal`. The runner MUST NOT
  encode further meanings (e.g., "exit 2 means retry" or "exit 137
  means OOM"). All semantic state lives in the WorkSource (see I-8).
  Symphony parallel: SPEC §10.7 / `agent_runner.ex` exit handler;
  the orchestrator's continuation logic depends only on
  `Normal | Abnormal`.

- **I-8 — Agents do not write to WorkSource.** The AgentProcess MUST
  NOT have any direct write path to the WorkSource. WorkSource
  mutations are an orchestrator responsibility (Symphony SPEC §11.5:
  "the service remains a scheduler/runner and tracker reader"). The
  agent MAY *read* WorkSource state through a daemon-mediated
  surface (a tool exposed via MCP or an explicit JSONL request),
  but writes — comment, label, transition, PR open — flow through
  the orchestrator only. This is the load-bearing isolation property
  that makes Pattern 1/2 collab work (see B.2 of the source
  collaboration document).

- **I-9 — Env merge precedence is fixed; reserved-variable set is closed.**
  Environment variables MUST be merged in the order
  `daemon_default < workflow_declared < hook_exported`, with later
  entries shadowing earlier ones, **except** that the reserved
  daemon-default set below MUST NOT be shadowable by either workflow
  or hook layers. The reserved set, exhaustive in v1:

  | Variable | Meaning |
  | --- | --- |
  | `CADUCEUS_WORKSPACE_PATH` | The agent's `cwd` binding (mirrors the spawn-time cwd, per X-9 from Phase A). Read-only contract surface for tools that need to discover the workspace root without parsing argv. |
  | `CADUCEUS_RUN_ID` | The `RunId` of the current RunAttempt. Stable across the runner Session lifetime. |
  | `CADUCEUS_AGENT_NAME` | The agent vendor identifier (e.g. `claude-code`, `codex`, `gemini-cli`). Mirrors the `agent_name` field of the handshake payload. |
  | `CADUCEUS_DAEMON_SOCKET` | Z-27: locator for the **runner-scoped MCP broker** the daemon exposes for *this* run (UDS on Linux/macOS, named pipe on Windows). Not the daemon's privileged control socket; agent reaches it only via daemon-mediated MCP shims. Full broker contract (address format, peer authentication, file mode, failure modes) in **I-9.1** below. |
  | `CADUCEUS_DECLARES_TOOL_USE` | Z-28: mirrors the `declares_tool_use` capability bit (§4.2) so the no-handshake fallback path can observe it. Allowed values are exactly `"0"` or `"1"`. Full fail-closed contract (allowed values, mutation rules, env↔handshake mismatch handling) in **I-9.2** below. |
  | `CADUCEUS_PROTOCOL_VERSION` | The runner-contract protocol version this daemon implements (e.g. `"v1"`). Allows agents to refuse a runner that speaks an unsupported version without first emitting a malformed handshake. |
  | `CADUCEUS_RUNNER_UUID` | Per-RunAttempt UUID minted by the daemon at spawn. Stable for the AgentProcess lifetime; agents MUST echo it in `heartbeat.runner_uuid`. |

  Implementations MUST refuse the spawn if the workflow or any hook
  attempts to set, override, or unset any variable in the reserved
  set above. The merge MUST NOT silently drop an attempted override;
  it MUST fail-closed with a `reserved_env_override` diagnostic and
  return `SpawnRefused`.

  **Closed namespace.** The daemon MUST NOT pass any environment
  variable beginning with the prefix `CADUCEUS_` to a runner other
  than those enumerated in the reserved set above. Workflow- and
  hook-declared variables MUST NOT use the `CADUCEUS_` prefix; the
  merge MUST refuse the spawn (with `reserved_env_prefix`) if they
  do. This keeps the `CADUCEUS_` namespace owned by this spec and
  prevents drift where vendor adapters silently inject new
  daemon-shaped env vars.

  - **I-9.1 — `CADUCEUS_DAEMON_SOCKET` broker contract (Z-27).**

    *Address format (normative).* On Linux/macOS the value MUST be an
    **absolute filesystem path** to a `SOCK_STREAM` Unix-domain socket.
    The socket name MUST be **unique per RunAttempt**; deriving it from
    `run_id` alone is forbidden because `run_id` is stable across retries.
    A compliant shape is `/run/caduceus/runs/<run_id>/<runner_uuid>.sock`
    or `$XDG_RUNTIME_DIR/caduceus/runs/<run_id>/<runner_uuid>.sock`.
    Abstract sockets (Linux `\0`-prefixed names) are NOT permitted in v1
    because they bypass filesystem permissions. On Windows the value MUST
    be a named-pipe path unique per RunAttempt, for example
    `\\.\pipe\caduceus-run-<run_id>-<runner_uuid>`. The daemon MUST
    allocate a fresh endpoint for every RunAttempt and MUST revoke it
    (socket unlinked / pipe closed) when that RunAttempt terminates; a
    later retry of the same `run_id` MUST NOT reuse a prior attempt's
    socket or pipe name.

    *File mode (normative).* The UDS path (Linux:
    `/run/caduceus/runs/<run_id>/<runner_uuid>.sock` or equivalent under
    `$XDG_RUNTIME_DIR`; macOS: same form) MUST be created with mode
    `0600` owned by the daemon's UID, in a parent directory hierarchy
    that is not world-traversable (`0700` on each daemon-owned path
    segment that is not a shared system runtime dir). On Windows the
    named-pipe security descriptor MUST grant access only to the
    daemon's user SID and deny everyone else.

    *Rationale (file mode vs PID-lineage).* File mode `0600` (socket)
    and parent directory mode `0700` defend against **different-UID**
    and world access. Same-UID-different-process attacks are NOT
    blocked by file mode (a same-UID peer can `connect(2)` regardless
    of mode); the **PID-lineage check** is the load-bearing mechanism:
    the broker validates that the connecting peer's PID is the
    AgentProcess PID (or a descendant of it) for this `run_id` via a
    parent-PID-chain walk — `/proc/<pid>/stat` field 4 (`ppid`) on
    Linux, `proc_pidinfo(..., PROC_PIDTBSDINFO, ...)` `pbi_ppid` on
    macOS, `NtQueryInformationProcess(ProcessBasicInformation)`
    `InheritedFromUniqueProcessId` on Windows — combined with per-link
    `(pid, process_start_time)` verification against the spawn-time
    `runner.child_creation_id`. Note that `getpgid(2)` returns the
    process-group ID, not the parent chain, and is NOT used for
    lineage; a child can `setpgid` out of its group and a non-descendant
    can `setpgid` into it. File mode is defense-in-depth (closes
    cross-UID and world holes), NOT the primary control against
    same-UID adversaries.

    *Authentication (peer credentials, normative).* Every connection accepted on the broker socket MUST be authenticated using `SO_PEERCRED` (Linux), `getpeereid(...)` + `getsockopt(..., LOCAL_PEERPID, ...)` (macOS), or `GetNamedPipeClientProcessId` + token query (Windows). The broker MUST verify both of the following, and neither is sufficient alone: (i) the peer's effective UID matches the UID the daemon used to spawn the AgentProcess (GID is informational), and (ii) the peer PID is the AgentProcess PID or a descendant of it, with lineage bound to `(pid, process_start_time)` (or the OS-equivalent creation identifier), not PID alone. Lineage bound to `(pid, process_start_time)` is the load-bearing same-UID control; UID match only constrains cross-UID access. If the creation identifier cannot be obtained or validated, the broker MUST fail closed: reject the connection, emit `broker_peer_rejected` with `reason = "creation_id_unavailable"`, and MUST NOT fall back to PID-only lineage checks. Connections that fail either check MUST be closed immediately and a `broker_peer_rejected` diagnostic MUST be emitted on `daemon_event_channel`.

    *Descendant lineage walk (normative).* `runner_pid_set` is
    defined as the set of `runner.pid` values for all
    `RunnerProcess` instances currently owned by this `caduceusd`
    instance (including the target `runner.pid` for the run this
    auth pertains to). To verify that a peer PID
    is a descendant of the AgentProcess for this `run_id`, the broker
    MUST walk the parent-PID chain upward from the peer. At each
    step, the broker reads the parent PID and samples
    `(pid, process_start_time)` for that parent via the per-OS API
    enumerated below. The walk terminates with **accept** when it
    reaches `runner.pid` AND the sampled `process_start_time` matches
    `runner.child_creation_id` exactly (byte-equal opaque token; see
    Normalization rule). The walk terminates with **reject** under
    each of the following conditions, with the bound normative
    `broker_peer_rejected` `reason` string given inline:

    - (a) **Read failure at any link** — the parent-PID read or the
      `(pid, process_start_time)` sample for any link in the chain
      fails (peer disappears, permission denied, malformed `/proc`
      entry, `proc_pidinfo` error, Windows `ERROR_ACCESS_DENIED`):
      `reason = "lineage_walk_failed"`.
    - (b) **Non-descendant terminus** — the walk reaches PID 1
      (`init` / `launchd`) or PID 0 before matching `runner.pid`,
      OR the walk reaches a PID that is in the daemon's
      `runner_pid_set` but is not this run's `runner.pid` (i.e. the
      chain crossed into a different RunAttempt's lineage):
      `reason = "non_descendant"`. Note: intermediate links that are
      NOT in `runner_pid_set` are legitimate descendants of
      `runner.pid` (the agent's own children, grandchildren, …) and
      MUST NOT terminate the walk; the walk continues upward through
      them. `runner_pid_set` is consulted only to detect crossing
      into another runner's tree.
    - (c) **Per-link `(pid, start_time)` inconsistency** — at any
      link, the just-sampled `(pid, process_start_time)` does not
      internally agree with the parent PID just read from the prior
      link's record (indicating PID reuse mid-walk):
      `reason = "lineage_pid_reuse"`.
    - (d) **Depth cap exceeded** — the walk traverses more links
      than the configured maximum (MUST be configurable; recommended
      default: 16 links): `reason = "lineage_depth_exceeded"`.

    Per-OS parent-PID source: Linux — field 4 (`ppid`) of
    `/proc/<pid>/stat`; macOS —
    `proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info))`
    `pbi_ppid`; Windows —
    `NtQueryInformationProcess(handle, ProcessBasicInformation, ...)`
    `InheritedFromUniqueProcessId` on a handle opened with
    `PROCESS_QUERY_LIMITED_INFORMATION`. PID-only fallback during the
    walk is FORBIDDEN.

    *Per-OS retrieval API for `process_start_time` (normative).* Because
    the broker's same-UID defence is load-bearing on
    `(pid, process_start_time)`, implementations MUST obtain the peer's
    creation identifier through the OS-supported APIs below and MUST NOT
    derive it from any agent-supplied or non-kernel source:

    - **Linux.** Read field 22 (`starttime`, clock ticks since boot, opaque) from `/proc/<pid>/stat` for the peer PID returned by `SO_PEERCRED`. `(pid, starttime)` is the canonical creation identifier on Linux. Implementations **SHOULD** use `pidfd_open(2)` (kernel ≥ 5.3) when available and **MUST** keep the pidfd alive for the full authentication step (`SO_PEERCRED` → `/proc/<pid>/stat` sample → completion of credential check) to neutralise PID-reuse races. On kernels without `pidfd_open`, implementations MUST issue `SO_PEERCRED` and the `/proc/<pid>/stat` read consecutively with NO intervening `.await` point, no blocking channel send/recv, no `sleep`, and no other operation that could yield to the async executor or block on an unbounded wait between the two syscalls (so that the PID cannot be recycled undetected between the two samples), and MUST fail closed (per the rule below) on any read error or `(pid, starttime)` inconsistency.
    - **macOS.** Obtain the peer EUID/EGID via `getpeereid(...)` and the
      peer PID via `getsockopt(..., LOCAL_PEERPID, ...)`. Then call
      `proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info))` and
      use `(pbi_start_tvsec, pbi_start_tvusec)` as the creation identifier
      paired with that PID. If `LOCAL_PEERPID` is unavailable or returns
      `ENOPROTOOPT` / `ENOTSUP` (including pre-10.14 hosts), or
      `proc_pidinfo` fails, broker authentication MUST fail closed at
      the retrieval site: reject the connection, emit
      `broker_peer_rejected` with `reason = "creation_id_unavailable"`,
      and MUST NOT fall back to `getpeereid`-only (UID-only) acceptance,
      MUST NOT fall back to PID-only lineage, and MUST NOT accept the
      connection on any other basis.
    - **Windows.** Open the peer process via the PID returned by
      `GetNamedPipeClientProcessId` and call
      `GetProcessTimes(handle, &lpCreationTime, ...)`; the
      `lpCreationTime` `FILETIME` (100-nanosecond intervals since
      1601-01-01 UTC) is the canonical creation identifier paired with
      the peer PID.

    **Normalization rule (normative).** `process_start_time` /
    `creation_id` values are **host-local opaque identity tokens**.
    Implementations MUST use them only for same-host, same-daemon-boot
    `(pid, creation_id)` lineage checks. Implementations MUST NOT
    convert them to a common wall-clock epoch for any security
    decision, MUST NOT compare creation identifiers across hosts, and
    MUST NOT compare across `caduceusd` restarts (any cross-boot peer
    MUST re-authenticate via the spawn-time handshake).

    Implementations MUST sample `process_start_time` (or its OS-equivalent
    creation identifier) atomically with peer credential lookup — i.e.
    in the same authentication step, against the same peer reference —
    so that the `(uid, pid, process_start_time)` tuple is internally
    consistent. Where the OS exposes a race-free peer reference (Linux `pidfd`, Windows process handle), implementations MUST hold that reference across both credential and creation-id retrieval. Any failure during retrieval — `/proc/<pid>/stat` read failure on Linux, `proc_pidinfo` error on macOS, `GetProcessTimes` failure or access-denied on Windows, peer disappearance before sampling completes, or detection of a pidfd-vs-PID mismatch on Linux — MUST be treated as `creation_id_unavailable` and broker peer authentication MUST fail closed per the rule above. PID-only fallback is FORBIDDEN.

    *Failure modes (normative).* If the env var is missing at agent
    spawn, the daemon MUST set it to the broker path it allocated for
    this run — i.e. the env var being unset inside the AgentProcess
    is itself a daemon bug (the daemon-default layer of I-9
    establishes it). If the env var is present but points to a
    non-existent or non-listening socket/pipe (e.g. broker crashed),
    MCP shims MUST fail their connect with a `broker_unreachable`
    error code and MUST NOT fall back to any other endpoint; the
    broker outage is fatal to tool use for that RunAttempt and the
    agent SHOULD propagate the failure as a tool error rather than
    retrying. The agent never inherits a pre-opened fd to this socket
    (per I-2); the daemon's privileged control surface is a separate,
    unreachable endpoint.

    Tested by **T-10**.

  - **I-9.2 — `CADUCEUS_DECLARES_TOOL_USE` fail-closed (Z-28).**

    *Allowed values (case-sensitive, normative).* Exactly `"0"`
    (false) or `"1"` (true). Any other value (including `"true"`,
    `"True"`, `"TRUE"`, `"false"`, empty string, or unset) is
    invalid. Two paths can produce an invalid value, and both
    fail-closed at spawn:
    (a) **caller-supplied** — a workflow-config error in which the
    caller's `env` overlay attempts to set this reserved key; this is
    intercepted by the I-9 reserved-env-override gate before
    value-validation runs (the caller never reaches the value check);
    and
    (b) **daemon-default** — a daemon-bug error in which
    `daemon_default_env` itself emits a value outside `{"0","1"}`;
    this is the sole reachable path to value-validation and surfaces
    as a `reserved_env_invalid_value` diagnostic with
    `SpawnRefused::ReservedEnvInvalidValue` (see T-11(d)).
    In either case the daemon MUST refuse the spawn. The daemon MUST
    set this exactly once at spawn and MUST NOT mutate it during the
    run.

    *Read point (normative).* The orchestrator reads the workflow's
    tool-use posture at dispatch time (spec #1 §3) and the daemon
    writes the env var at the start of `spawn_agent` (§3.1, before
    the merge in I-9 fires). The agent MAY inspect it at any time
    during the run, but its value is fixed for the lifetime of the
    AgentProcess.

    *Pre-handshake fail-closed (normative).* The binding
    `CADUCEUS_DECLARES_TOOL_USE="1"` is a *daemon-side declaration
    that this RunAttempt is permitted to use tools*; it does NOT
    itself grant the agent the right to emit tool frames. The agent
    must still successfully advertise `declares_tool_use=true` (and
    therefore `accepts_stdin_control=true` per C2) at handshake
    before the runner will accept any `tool_use_request` or
    `permission_elevation_request`.

    *Mismatch handling (both directions).* If
    `CADUCEUS_DECLARES_TOOL_USE="1"` but the agent's handshake
    advertises `declares_tool_use=false` (or the handshake is
    missing, leaving capabilities at `most_restrictive()`), and the
    agent subsequently emits a `tool_use_request` or
    `permission_elevation_request`, the runner MUST emit a
    `protocol_violation` diagnostic with `reason:
    "tool_use_without_capability"` and trigger
    `stop_cascade(reason = "tool_use_without_capability")`; the
    RunAttempt terminates with `ExitReason::ProtocolViolation`.
    Conversely, if `CADUCEUS_DECLARES_TOOL_USE="0"` but the agent
    advertises `declares_tool_use=true` at handshake, the runner
    MUST refuse the runner Session at handshake with a
    `tool_use_declared_but_not_permitted` diagnostic and trigger
    `stop_cascade(reason = "tool_use_declared_but_not_permitted")` —
    the workflow has not authorised tool use for this run. The
    runtime check N-3 in §3.2 is the load-bearing enforcement point
    for the agent-side direction; the handshake check above is the
    load-bearing enforcement point for the env-var direction.

    Tested by **T-11**.

- **I-10 — ACP transparency.** The wrapped event stream the daemon
  observes (per §3.2 `forward_to_daemon`) MUST be indistinguishable
  in shape between the JSONL and ACP transports for the same agent
  performing the same task. Tested by **T-6**.

**Ownership of timing tunables (cross-spec lock).** `grace_period_ms`,
`ε₁` (`shutdown_enqueue_budget_ms`), `ε₂` / `sigkill_reap_budget`
(`sigkill_reap_budget_ms`) are **defined and owned by spec #2 §3.3 / §5**.
Spec #1 §8.7 (Tunables) MUST cross-reference spec #2 §5 and MUST NOT
redefine numeric defaults. If spec #1 §8.7 lists these tunables, it lists
them as references with the note "see spec #2 §3.3, authoritative".

---

## 6. Test contract

The runner implementation MUST carry the following tests (or
behavioural equivalents) before a release. Numbering matches the
invariants in §5.

- **T-1 — StopCascade timing under unresponsive agent.** Spawn an
  agent that traps SIGTERM and ignores stdin closure. Trigger
  `stop_cascade(reason="test", grace_period_ms=1000)`. With defaults
  `ε₁ = 100 ms`, `grace_period_ms = 1000 ms`, `ε₂ = 150 ms`, the total
  bound is `ε₁ + 2·grace_period_ms + ε₂ = 100 + 2·1000 + 150 = 2250 ms`.
  T-1 asserts that, under the synthetic "child ignores SIGTERM" load,
  observed wall time from `Cmd::Shutdown` dispatch (or equivalent
  cascade-trigger) to `RunFinished` MUST be ≤ 2250 ms ± 50 ms
  (measurement jitter), the agent is reaped, and the final stage recorded is
  `sigkill`. Validates **I-3**.

- **T-2 — stdout closed mid-line.** Spawn an agent that emits
  `{"kind":"turn_start"...` then closes stdout without a trailing
  `\n`. Assert the read loop returns gracefully with a
  `stdout_truncated` event, no parse-error fatality, and the runner
  proceeds to reap normally. Validates the partial-line branch in
  §3.2.

- **T-3 — Absolute token report after delta-only history (per-component
  watermark).** Replay a recorded session where the agent emits five
  `mode=delta` updates whose `output_tokens` sum to 1,000 (other
  components 0), then a single `mode=absolute` update with
  `output_tokens = 950` (other components ≥ stored). Assert: (a)
  `last_reported_tokens[run_id].output_tokens` ends at 1,000, not 950,
  per the per-component watermark rule of §4.3 `reconcile_tokens`;
  (b) a `token_regression` diagnostic is emitted with
  `field = "output_tokens"`, `got = 950`, `high_water = 1000`,
  and `cause = "delta_only_absolute_baseline"` (the classifier
  field that flags a first-`mode=absolute` frame whose value falls
  below the delta-summed watermark — distinct from a true mid-stream
  absolute regression);
  (c) no panic. Validates **I-5** (per-component monotonicity) and
  the canonical `TokenTotals` struct defined under **Token reporting
  (canonical)** in §4.1.

- **T-4 — Capability handshake missing.** Spawn an agent whose first
  line is a `turn_start`, not a `handshake`. Assert the runner
  proceeds with `most_restrictive` capabilities, that StopCascade
  for this runner skips Stage 1 (because `accepts_stdin_control =
  false`), and that token reconciliation treats the agent as
  delta-only. Validates the handshake-missing fallback.

- **T-5 — Permission elevation duplicate `request_id`.** Drive the
  agent to emit a `permission_elevation_request` with `request_id =
  "A"`. After the daemon delivers its
  `permission_elevation_response` for `"A"`, have the agent re-emit
  a second `permission_elevation_request` with the same
  `request_id = "A"`. Assert: (a) the runner emits a
  `duplicate_permission_request` diagnostic; (b) no second
  `permission_elevation_response` is written to the agent's stdin;
  (c) the runner remains in `Running`. Separately, inject a
  daemon-side reply path call carrying `request_id = "B"` for which
  the runner has no outstanding record; assert the runner emits
  `unknown_permission_request_id` and writes no response line.
  Validates **I-6**.

- **T-6 — ACP adapter round-trip.** Run the same agent vendor against
  the same task twice: once via JSONL, once via ACP. Capture the
  daemon's wrapped event stream in both runs. Assert that, modulo
  `received_at` and `runner_seq`, the two streams are equal. Per §4.4 rule 2:
  `runner_seq` is consumed only on `forward_to_daemon → Ok`; Dropped
  frames consume no seq (the internal `frame_id` serves as the
  correlation id for any Dropped diagnostics). It is a per-Run
  monotonic counter (per-Run, NOT per-RunAttempt; initialised to `0`
  for a brand-new Run and seeded from the daemon's
  `runner_seq_high_water` on RunAttempt resume per §4.4 rules 1 & 3 —
  never reset on RunAttempt boundaries within the same Run), gap-free
  on the wire, and differs across distinct Runs by construction
  (pre-Z-22 the field was named `orchestrator_seq`).
  Validates **I-10**.

- **T-7 — Symlink-escape attempt at cwd boundary.** Configure a
  workspace whose `path` resolves through a symlink to a target
  outside `workspace.root`. Assert that `spawn_agent` rejects the
  spawn and emits a `cwd_validation_error` event without launching
  any process. Cite spec #3 invariant equivalent to Symphony's
  `:symlink_escape`. Validates the I-1 backstop.

- **T-8 — Reattach variant matrix.** Drive each of the 7 `ReattachResponse`
  variants and assert the engine takes the §4.5 normative per-variant
  next-action. The test MUST cover all 7 variants explicitly:
  (a) **`Ok`** — engine begins replay at `replay_starts_at_seq`; assert
  idempotent replay on duplicate `Ok` (same `(run_id, runner_seq,
  session_id)` tuple) per §4.5 rule 5; assert local snapshot discard +
  re-seed when `boot_id` differs from any prior observed value.
  (b) **`SeqAhead`** — assert the engine MUST NOT retry with the same
  supplied `runner_seq`; assert it drops the local cursor and
  re-subscribes via spec #4 §3.4.
  (c) **`UnknownRun`** — assert no retry against the same `run_id`;
  assert the engine surfaces a non-retryable error to its caller.
  (d) **`SessionMismatch`** — assert the engine MUST NOT retry with the
  same `session_id`; assert it either obtains a fresh `session_id` from
  spec #8 and reattaches with it, or surfaces a non-retryable error to
  its caller (v1 stub-or-skip permitted per §4.5, since v1 daemons MAY
  always accept).
  (e) **`DaemonShuttingDown`** — assert the engine does NOT retry against
  the same daemon process; assert it defers reattach until a fresh
  daemon `boot_id` is observed via spec #4 §3.4 subscribe.
  (f) **`RunTerminated`** — assert no reattach retry; assert the engine
  fetches the terminal snapshot via the spec #4 §3.4 subscribe path and
  surfaces `terminal_state` to its caller; assert `final_runner_seq` is
  used to verify no silently-missed events past the last observed cursor.
  (g) **`ReattachWindowExpired`** — assert the engine MUST NOT re-attempt
  reattach with the same supplied `runner_seq`; assert the subscriber
  MUST resubscribe via spec #4 §3.4 to obtain a fresh full snapshot at
  `daemon_high_water`; assert a non-fatal "snapshot resync" diagnostic
  is surfaced to the caller.

- **T-9 — Disconnect-during-shutdown is bounded by the §3.3 composite budget.**
  Inject a runner-side disconnect concurrent with `Cmd::Shutdown` dispatch.
  Assert that the orchestrator drives the run to `RunFinished` within
  `ε₁ + 2·grace_period_ms + ε₂` wall-clock (per §3.3, defaults
  `100 + 2·1000 + 150 = 2250 ms`, ± 50 ms measurement jitter),
  regardless of the disconnect's relative
  timing to the SIGTERM dispatch. The test enumerates three timings:
  (i) disconnect BEFORE SIGTERM, (ii) disconnect DURING the grace window,
  (iii) disconnect AFTER SIGKILL but BEFORE reap. All three MUST satisfy
  the same composite bound. Additionally, when the §3.5.2 outbound queue
  is saturated, assert `stage1_cancel_skipped` diagnostic fires and
  Stage 2 still executes within the same composite bound.

- **T-10 — Broker peer authentication.** Four sub-cases:
  (a) **wrong-UID peer** — connect to the broker from a process running
  under a different UID; assert `broker_peer_rejected`.
  (b) **same-UID non-descendant PID** — connect from a same-UID process
  that is NOT a descendant of the daemon's `spawn_agent` for this
  `run_id`; assert `broker_peer_rejected`.
  (c) **same-UID recycled PID** — after the original AgentProcess exits,
  connect from a new process that reuses the same PID but has a different
  `process_start_time` / creation identifier; assert `broker_peer_rejected`.
  This sub-case normatively validates the I-9.1 `(pid, process_start_time)`
  rule.

  This sub-case MUST be exercised mechanically (no reliance on natural
  PID wraparound). Implementations MUST provide a test hook that
  injects peer-credential results into the broker's authentication
  path. The canonical strategy is a **synthetic peer-credential
  provider (mock interface)** that, for an authenticated session
  originally established with peer `(pid = P, process_start_time = T₁)`,
  returns `(pid = P, process_start_time = T₂)` where `T₂ ≠ T₁` on a
  subsequent re-authentication (or on a re-connect against the same
  broker socket). An equally acceptable strategy on Linux is a
  **pidfd-handle test fixture** that retains an old `pidfd` and
  presents it as the peer reference against a recycled PID, exercising
  the same `(pid, process_start_time)` mismatch through the real
  `pidfd_open` / `/proc/<pid>/stat` path. Implementations MUST expose
  these injection points (e.g. a trait-based `PeerCredentialProvider`
  with a test-only constructor, or a build-time `cfg(test)` shim) so
  that T-10(c) is deterministic in CI; production code MAY bind the
  same interface to the real OS APIs enumerated in §I-9.1.
  (d) **creation_id_unavailable** — mock or simulate an OS-level failure
  to retrieve `process_start_time` (or the OS-equivalent creation
  identifier) for the connecting peer; assert `broker_peer_rejected` with
  `reason = "creation_id_unavailable"` and confirm no fallback to PID-only
  lineage. Validates the I-9.1 fail-closed rule.
  (e) **macOS `LOCAL_PEERPID` unavailable** — on a macOS test fixture (or
  via the `PeerCredentialProvider` shim defined in sub-case (c)),
  simulate `getsockopt(..., LOCAL_PEERPID, ...)` returning
  `ENOPROTOOPT` or `ENOTSUP` (the pre-10.14 capability gap). Assert
  `broker_peer_rejected` with `reason = "creation_id_unavailable"`,
  assert NO fallback to `getpeereid`-only (UID-only) acceptance, and
  assert NO fallback to PID-only lineage. Validates the I-9.1 macOS
  retrieval-site fail-closed rule.
  (f) **lineage walk failure / non-descendant chain** — connect from a
  same-UID process whose parent-PID chain (a) cannot be read at some
  link (simulate a `/proc/<pid>/stat` read error on Linux, a
  `proc_pidinfo` error on macOS, or `ERROR_ACCESS_DENIED` on Windows)
  → assert `broker_peer_rejected` with `reason = "lineage_walk_failed"`;
  (b) reaches `init` / `launchd` / PID 0 before matching
  `runner.pid`, OR reaches a PID in `runner_pid_set` that is NOT this
  run's `runner.pid` (cross-RunAttempt lineage crossing) → assert
  `reason = "non_descendant"`. The test MUST also include a
  positive control: a multi-link descendant chain (peer →
  non-runner-pid intermediate → `runner.pid`) where every
  intermediate link is a legitimate non-runner child of the agent;
  assert this case is **accepted** (no rejection) so that the
  `runner_pid_set` consultation is not over-strict;
  (c) exhibits per-link
  `(pid, process_start_time)` inconsistency mid-walk (use the
  `PeerCredentialProvider` shim from sub-case (c) to inject a
  parent-link mismatch) → assert `reason = "lineage_pid_reuse"`;
  (d) exceeds the configured depth cap → assert
  `reason = "lineage_depth_exceeded"`. Validates the I-9.1 normative
  descendant-walk algorithm reason taxonomy.

- **T-11 — Env/handshake fail-closed (both directions).** In a runner
  test harness (daemon-authored env layer, NOT workflow/hook input —
  workflow/hook caller-supplied `CADUCEUS_*` keys are refused at §3.1
  spawn-time per the I-9 reserved-env-override gate):
  (a) set daemon-authored `CADUCEUS_DECLARES_TOOL_USE="1"` and use a
  handshake declaring `declares_tool_use=false`; emit a
  `tool_use_request`; assert `protocol_violation` /
  `tool_use_without_capability` exit with StopCascade.
  (b) Repeat with daemon-authored env `"0"` + handshake declaring
  `true`; assert handshake-time refusal with diagnostic
  `tool_use_declared_but_not_permitted`.
  (c) Spawn with workflow-supplied `CADUCEUS_DECLARES_TOOL_USE="true"`
  (lowercase string, caller-side); assert
  `SpawnRefused::ReservedEnvOverride` and no AgentProcess started
  (verifies caller-supplied `CADUCEUS_*` attempts are gated by I-9
  reserved-env-override before I-9.2 value-validation runs; I-9.2
  `reserved_env_invalid_value` is therefore reachable only through the
  daemon-default layer).
  (d) Spawn with the daemon-default layer producing an
  invalid/unset `CADUCEUS_DECLARES_TOOL_USE` (e.g. `"true"`, empty
  string, or absent — simulating a bug in `daemon_default_env`); assert
  `SpawnRefused::ReservedEnvInvalidValue` with a
  `reserved_env_invalid_value` diagnostic and no AgentProcess started
  (covers the I-9.2 invalid-value path on the daemon-default layer,
  which per (c) is the sole reachable enforcement point).

- **T-12 — Reserved env override.** Workflow attempts to set
  `CADUCEUS_RUN_ID` (reserved) and a hook attempts to set a
  `CADUCEUS_FOO` (reserved-prefix); assert `reserved_env_override` and
  `reserved_env_prefix` SpawnRefused diagnostics respectively.

- **T-12a — Spawn-time `CADUCEUS_RUNNER_UUID` UUIDv4 validation.**
  Configure the daemon-default layer to produce, in turn, each of
  the following malformed values: (a) key absent; (b) empty string
  `""`; (c) a syntactically canonical UUID with the version nibble
  ≠ 4 — e.g. `"00000000-0000-3000-8000-000000000000"` (version 3),
  `"00000000-0000-1000-8000-000000000000"` (version 1),
  `"00000000-0000-7000-8000-000000000000"` (version 7); (d) a
  syntactically canonical UUID with version 4 but a non-RFC-4122
  variant — e.g. `"00000000-0000-4000-0000-000000000000"` (variant
  bits `00`, NCS-reserved); (e) a non-canonical / non-UUID string
  (`"not-a-uuid"`, `"deadbeef"`, a UUID without dashes, or a UUID
  with mixed-case-but-otherwise-valid form, if the implementation's
  `is_valid_uuid_v4` definition is case-strict per the §3.1
  normative comment); (f) the nil
  UUID `"00000000-0000-0000-0000-000000000000"`; (g) the max UUID
  `"ffffffff-ffff-ffff-ffff-ffffffffffff"`. For every case, assert
  that `spawn_agent` returns `SpawnRefused::ReservedEnvInvalidValue`
  with a `reserved_env_invalid_value` diagnostic naming
  `CADUCEUS_RUNNER_UUID`, and that NO AgentProcess is started (no
  `child.pid` is allocated, no `spawn` event is emitted). Validates
  the §3.1 UUIDv4 spawn-time gate at bit-level granularity.

- **T-12b — Spawn-time `child_creation_id` sampling fail-closed.**
  Configure the test harness to make `sample_creation_id(child.pid)`
  return an error immediately after `os.spawn` (Linux: simulate
  `/proc/<pid>/stat` read error; macOS: simulate `proc_pidinfo`
  failure; Windows: simulate `GetProcessTimes` failure or
  `ERROR_ACCESS_DENIED`). The injection point MUST be the same
  per-OS API enumerated in §I-9.1, exercised via either the
  `PeerCredentialProvider` shim defined in T-10(c) or a build-time
  `cfg(test)` hook on the creation-ID sampling call site. Assert in
  every case: (a) the spawned child is killed AND reaped (no leaked
  PID, no zombie); (b) `spawn_agent` returns
  `SpawnRefused::CreationIdUnavailable`; (c) a
  `child_creation_id_unavailable` diagnostic is emitted on
  `daemon_event_channel` naming the `run_id`; (d) NO `spawn` event
  is emitted and NO `RunnerProcess` is constructed; (e) the
  RunAttempt does NOT transition into `Running`. Validates the §3.1
  spawn-time creation-ID gate, which is the source of the
  ground-truth value compared by I-9.1 broker peer auth.

- **T-13 — Sequence violation taxonomy: stutter, gap, and bad-first-frame are non-fatal.**
  Inject frames whose wire `seq` violates the Z-24 contiguity rule:
  (a) **stutter (duplicate)** — `seq` equal to the previous accepted
  `seq` (i.e. `seq == seq_high_water`);
  (b) **stutter (out-of-order replay)** — `seq` strictly less than
  the previous accepted `seq` (i.e. `seq < seq_high_water`,
  non-zero); per Z-24 / §3.2 this is also classified as `"stutter"`
  (drop + log, no `stop_cascade`), NOT as a separate `"regression"`
  term;
  (c) **bad first frame** — the very first frame on the inbound stream
  carries `seq = 2` (or any value other than `1`), which the
  `expected_seq`-based check in §3.2 rejects as a `gap` even though it
  would have satisfied the obsolete `seq > seq_high_water = 0` test.
  This case has TWO sub-cases that MUST both be exercised:
  - **(c-handshake)** — the bad-first-frame is a `handshake` line
    (e.g. `seq = 2` on a kind=handshake frame). Assert the
    handshake-success block in §3.2 runs the seq check BEFORE any
    capability mutation, so the diagnostic fires, the frame is
    dropped, and `runner.capabilities` remains
    `most_restrictive()` (NOT the values that would have been
    parsed from the rejected handshake's payload). `handshake_seen`
    remains `false`; the next inbound line is re-evaluated as the
    first frame.
  - **(c-runtime)** — the bad-first-frame is a runtime kind (e.g.
    `turn_start` with `seq = 2`) on a stream where no handshake is
    sent. Per the §3.2 hoist rule, the seq check is HOISTED above the T-4
    missing-handshake fallback, so the frame is dropped with zero
    state mutation BEFORE the fallback would otherwise run. Assert
    after the drop that `runner.capabilities` remains
    `most_restrictive()` (per §3.1 spawn init), `handshake_seen`
    remains `false`, `runner.expected_seq` remains `1`, and
    `runner.seq_high_water` remains `0`. Then inject a subsequent
    legitimate `handshake` frame with `seq = 1` and assert it is
    processed normally as the first frame: the daemon emits
    `handshake_ok` (NOT `unexpected_handshake` / `late_handshake`)
    and does NOT trigger `stop_cascade`. This is the regression
    test for the pre-hoist control-flow bug (where the pre-hoist
    code flipped `handshake_seen = true` via T-4 on the dropped
    frame and then mis-routed the subsequent legitimate handshake
    into the `late_handshake` branch).
  Assert in all four cases ((a), (b), (c-handshake), (c-runtime))
  that the daemon emits a `seq_regression` diagnostic (per §3.2)
  with the appropriate `kind_detail`
  (`"stutter"` / `"stutter"` / `"gap"` / `"gap"` respectively) AND continues
  processing the connection (NO disconnect, NO `stop_cascade`, NO
  `protocol_violation`). The frame itself is dropped from delivery
  but the connection remains live; `runner.expected_seq` is NOT
  advanced and the post-Ok `runner_seq` stamp is never reached, so
  the wire `runner_seq` counter remains gap-free. This verifies §3.2
  graceful-skew tolerance and Z-24's "wire `seq` strictly-increasing,
  first-frame-must-be-1" constraint as a soft (diagnosed-but-tolerated)
  wire-property, NOT a connection-killing one.

- **T-14 — Run-identity mismatch is fail-closed and HOISTED.** Spawn an agent emitting frames with a `run_id` that differs from `runner.run_id`. Assert: (a) runner emits `protocol_violation` with `reason = "run_id_mismatch"`, `expected`, `got`, and `seq`; (b) `stop_cascade(reason = "run_id_mismatch")` fires; (c) no `runner_seq` consumed; (d) `handshake_seen`, `expected_seq`, and `seq_high_water` unchanged. Sub-case: inject mismatch as the very first frame (pre-handshake) and assert HOIST order — run_id check fires BEFORE the seq check and BEFORE the handshake / T-4 branch. Validates the run-identity invariant in §3.2.

- **T-15 — Malformed runtime payload is fail-closed.** Emit a post-handshake runtime frame with the correct `kind` and envelope but a missing required payload field (and separately a type-mismatched field). Assert `malformed_payload`, `stop_cascade(reason = "malformed_payload")`, and `Err(RuntimeError::MalformedPayload)`, with no `runner_seq` consumed.

- **T-16 — Heartbeat `runner_uuid` validation is schema-first and fail-closed.** Spawn an agent and exercise two sub-cases:
  (a) inject a well-formed `heartbeat` frame whose `runner_uuid` differs from `runner.runner_uuid` (the value minted by the daemon at spawn and injected via `CADUCEUS_RUNNER_UUID`). Assert: `protocol_violation` with `reason = "runner_uuid_mismatch"`, `expected`, `got`, and `seq`; `stop_cascade(reason = "runner_uuid_mismatch")`; and no `runner_seq` consumed past the rejected frame.
  (b) inject a `heartbeat` frame with a malformed payload (missing `runner_uuid`, and separately `runner_uuid` of the wrong type). Assert `malformed_payload`, `stop_cascade(reason = "malformed_payload")`, no `runner_uuid_mismatch` diagnostic, and no `runner_seq` consumed past the rejected frame. This pins the §3.2 ordering rule that payload-schema validation runs before the heartbeat UUID cross-check.

---

## 7. Out of scope

- Specific MCP server protocols (covered by `spec-m-permissions.md`
  and any forthcoming MCP wire spec).
- Specific agent vendor implementations (Claude Code, OpenAI Codex,
  Gemini CLI, Aider, etc.). The contract here is intentionally
  vendor-agnostic; vendor adapters live above this contract.
- The orchestrator's dispatch decision: which run to start, when, how
  many in parallel, when to retry, how to back off (→ spec #1).
- The workspace filesystem layout, snapshotting, branch isolation,
  and cleanup (→ spec #3).
- The user-facing Engine B Session that wraps multiple runner
  Sessions across model turns. This contract is internal to the
  daemon's runner subsystem only.
- The approval-card UI rendering for `permission_elevation_request`
  (→ `spec-m-ui-approval-card.md`).

---

## 8. Open questions

Items marked Resolved (v1) are closed and normative for v1.

### 8.1 Is `accepts_stdin_control` required for v1? — Resolved (v1)

**Resolution (v1, normative).** Any agent that MAY emit
`tool_use_request` or `permission_elevation_request` MUST advertise
`accepts_stdin_control = true`; otherwise the daemon cannot deliver
the corresponding `tool_use_response` / `permission_elevation_response`
on stdin and the agent deadlocks on first denial. The runner MUST
enforce this at handshake per §4.2 (`accepts_stdin_control` row, C2),
§3.2 (handshake reject path), and **I-6**.

For pure read-only / non-tool agents (those that never advertise
`declares_tool_use = true`), `accepts_stdin_control` remains optional
in v1; such agents fall back to the `most_restrictive` capability set
under T-4, and StopCascade for them skips Stage 1 (cooperative cancel)
and proceeds directly to Stage 2 (stdin close) per §3.3.

The narrower question of whether to mandate `accepts_stdin_control =
true` even for non-tool agents is deferred to v1.1 once vendor
roadmaps are known; it is tracked as a future-revision item, not an
open question for v1.

Symphony's reference implementation does not require the bit (it
relies on closing the port to cancel); Caduceus diverges from
Symphony here because the cooperative `cancel` JSONL frame is a
caduceus addition that requires bidirectional stdio (§3.3 citation).

### 8.2 ACP version pinning policy

**§8.2 Resolved (v1).** ACP requested but version unsupported:
the daemon MUST emit an `unsupported_acp_version` daemon-internal
diagnostic and reject the Session; the daemon MUST NOT silently
fall back to JSONL. Fallback to JSONL is permitted ONLY when the
runner did not request ACP at all. This was an open question pre-v1;
v1 binds the answer normatively per §3.4 negotiation rules.

### 8.3 Streaming vs end-of-turn token reporting

Pre-v1 §8.3 conflated two independently-tunable cadence questions
sitting on opposite sides of the daemon boundary. Splitting clarifies
that the runner→daemon decision and the daemon→engine decision are
not the same knob. Both remain open for v1.

#### 8.3.1 Runner→daemon token-update forwarding cadence

This is the **runner→daemon** cadence question: whether the runner
forwards each agent-emitted partial `token_update` frame as it arrives
on the JSONL transport, or aggregates and forwards only at `turn_end`.

(This is distinct from the §4.2 `streams_partials` capability bit,
which advertises whether the agent emits streaming `turn_event`
partials. The two knobs are independent.)

This spec makes no normative requirement on token-update forwarding
cadence — both end-of-turn-only and streaming modes are
contract-valid. Spec #1 MUST handle slower liveness signals
(e.g. `token_update` only at `turn_end`) as a first-class case in its
stall-sweep policy.

#### 8.3.2 `token_update` cadence: should the orchestrator's `EngineUpdate::TokenUpdate` fire on every partial or only on aggregated checkpoints?

This is the **daemon→engine** cadence question. Independent of how the
runner forwards inbound token frames (§8.3.1), the orchestrator MAY
fan `EngineUpdate::TokenUpdate` out to engine subscribers either
per-frame or on aggregated checkpoints (e.g. coalesced over a small
debounce window, or only on `turn_end`). This spec takes no position;
the answer interacts with spec #1's subscriber backpressure model and
with the engine-side UI tick budget. Left open for v1.

### 8.4 Boundary with spec #1: retry ownership on `Abnormal` exit

Per **I-7** and §9, the runner MUST NOT encode retry policy in exit-code
interpretation and MUST NOT emit any structured "recoverable" or retry-hint
signal derived from stderr, exit code, or local heuristics. The runner emits
raw `exit_code`, `exit_reason`, and daemon-local diagnostics only. Spec #1's
orchestrator is the sole owner of retry classification and decides retry from
WorkSource state plus those raw terminal facts. This matches Symphony's
tracker-owned retry model.

### 8.5 Boundary with spec #3: who canonicalises `workspace.path`?

This spec says "spec #3 must have already canonicalised and
symlink-escape-checked the path before `spawn_agent` is called".
Spec #3 must, in its invariant table, mirror this with a "path
handed to runner is canonical and rooted" guarantee. This is a
two-sided contract — confirm phrasing during spec #3 authoring.

---

## 9. Cross-references

- **Spec #1 — `spec-caduceus-orchestrator-algorithm.md`** (forthcoming).
  Owns: dispatch decision, retry policy, stall sweep, the
  `RunnerState` struct (specifically its `last_reported_tokens`
  field, which this spec writes to but does not own). Owns the
  `Session` trait that the JSONL transport and ACP adapter both
  implement.

- **Spec #3 — `spec-multi-repo-workspace-model.md`** (forthcoming). Owns:
  the workspace path lifecycle, canonicalisation, symlink-escape
  rejection (Symphony parallel: `validate_workspace_cwd`), and the
  cwd handed to `spawn_agent`. This spec's I-1 is enforced by spec
  #3; this spec assumes the path arrives already-validated.

- **`spec-m-permissions.md`** (existing). Owns: the
  `PermissionsConfig`, the approval-broker, the per-entity
  permission resolution pipeline, and the envelope into which
  `permission_elevation_request` plugs. The runner contract here
  defines only the wire shape of the request/response on the agent
  stdio; the resolution is a permissions-spec concern. The
  `request_id` in `permission_elevation_request` corresponds to the
  permission-broker request id used in `spec-m-permissions.md` §3
  (Per-Entity Permission Resolution).

- **`spec-m-session-lifecycle.md`** (existing). Disambiguates the
  **engine Session** from the **runner Session** defined here. The
  runner Session is the lifetime of a single AgentProcess; the
  engine Session is the user-facing chat lifetime. One engine
  Session may span zero, one, or many runner Sessions.

---

*Source attribution.* The seven-part agent-expose contract this
document is built around (process invocation, transport, handshake,
streaming events, stop semantics, token accounting, failure
surface) is derived from Symphony SPEC §10 and `app_server.ex`.
The three-stage StopCascade extends Symphony's two-stage
close-then-kill (`stop_session` / `stop_port`) with an explicit
cooperative cancel frame, which is a caduceus addition. The
isolation invariants (I-2, I-8) are direct ports of Symphony SPEC
§11.5. The token-accounting reconciliation rule (I-5, §4.3) is a
verbatim port of Symphony SPEC §13.5.
