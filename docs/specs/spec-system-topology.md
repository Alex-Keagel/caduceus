# spec-system-topology

> **Status:** Draft (P-tier)
> **Author:** caduceus team (drafted via Copilot)
> **Last-updated:** 2025
> **Priority:** P0 (foundational — referenced by every other P-tier spec)
> **Scope-locked decision:** Topology is **C-hybrid** (separate `caduceusd` daemon owns
> orchestrator state; per-host `caduceus` engine owns per-thread chat state; the two
> join on `run_id`). Process model is **multi-process** (parent host + engine library
> in-host + out-of-host daemon + sub-runner + sub-agents + sub-MCP + sub-LSP).
> Deviations from C-hybrid are **out of scope** for this spec and require a new
> scope-locked decision document before any implementation diverges.
>
> **Attribution:**
> - Material derived from upstream Symphony (zed-industries/zed) is governed by
>   Apache-2.0 (process-topology fragments, ACP framing, RunSnapshot pubsub idea).
> - Material derived from M's reverse-engineered E2E architecture notes is **cleanroom
>   re-statement** of process / IPC / lifecycle invariants — no source code copied.
> - Material specific to caduceus (daemon split, multi-repo workspace, autonomy
>   budget, DAG topology) is original and contributed under the caduceus repo's
>   own license terms.
>
> **Reading order:**
> 1. §1 Scope, §2 Terms — what this spec promises and the vocabulary it uses.
> 2. §3 Process inventory + the ASCII topology diagram in §3.1 — the single picture
>    every engineer is expected to carry in their head.
> 3. §4–§10 — normative chapters: IPC, lifecycle, host handshake, sandbox boundary,
>    multi-repo topology, failure domains, network topology.
> 4. §11 Invariants (Z-1 … Z-N) — the testable rules. Every PR that touches process
>    boundaries MUST cite the Z-numbers it preserves or re-affirms.
> 5. §12 Acceptance criteria — what an integration test suite has to demonstrate
>    before a release branch is allowed to claim conformance to this spec.
> 6. §13 Out of scope, §14 Open questions, §15 Cross-references, Appendix A glossary.

---

## 1. Scope

### 1.1 In scope

This spec is normative for:

- **The set of OS processes** that exist when a caduceus run is in flight, their
  parent/child relationships, and their lifetime classes.
- **The IPC channels** between those processes: transport (stdio/Unix-domain
  socket/TCP/loopback), encoding (JSONL/length-prefixed/protobuf), framing
  rules (max line length, multi-line forbidden, half-close semantics), and the
  layer at which authentication or capability negotiation occurs.
- **Lifecycle ownership** — for every process P in the inventory, exactly one
  parent process is named as P's supervisor (spawns, monitors, restarts where
  policy permits, and reaps).
- **Host capability negotiation** — what bits a host (`caduceus-zed`,
  `caduceus-cli`, future `caduceus-cloud-host`) MUST advertise on connect to the
  engine, what fallback the engine takes when a bit is missing, and how
  capability changes mid-session are propagated.
- **Sandbox / permission boundary** — which process holds authority for each
  capability (filesystem write, shell exec, network egress, MCP-tool dispatch,
  approval prompt rendering), and the cross-process trust assumptions made.
- **Multi-repo topology** — how the workspace registry is sharded across the
  daemon and per-engine processes, and where the working directory for any
  given `run_id` lives on disk.
- **Failure domains** — for each process P, what surface area is lost when P
  crashes, who notices, who reaps, and what the user-visible recovery is.
- **Network topology** — the default zero-network local-only mode, the
  exceptions (cloud-agent handoff, model-provider HTTPS egress), and how
  caduceus stays usable on an air-gapped host.
- **Deployment surface** — a complete enumeration of the binaries, install
  locations, OS-level entitlements, and bootstrap order for a clean install.

This spec is **the** authoritative description of the system's process model.
Any other spec that describes a process or pipe MUST cite this spec or
contradict it explicitly with a scope-locked override.

### 1.2 Out of scope

The following are deliberately out of scope and are owned by other specs (see
§15 Cross-references):

- The **algorithmic** behaviour of the orchestrator (run dispatch, retry
  policy, autonomy budget, DAG self-pause) — owned by
  `spec-caduceus-orchestrator-algorithm.md`.
- The **wire format** of agent/runner messages — owned by
  `spec-caduceus-agent-runner-contract.md`.
- The **9-step permission evaluate pipeline** — owned by
  `spec-m-permissions.md` (this spec only fixes which process houses the
  pipeline and which processes call into it).
- The **per-thread session state machine** (idle / awaiting-input /
  awaiting-tool / running / stopped) — owned by
  `spec-m-session-lifecycle.md`.
- The **status-snapshot pubsub schema** — owned by
  `spec-orchestrator-status-snapshot.md` (this spec only fixes that the
  channel exists, who publishes, who subscribes, and the lifetime guarantee).
- The **DAG orchestration semantics** (vendor-tag fan-out, F7b autonomy
  budget, F7c spawn-count threshold, PB6 self-pause) — owned by
  `dag-orchestration-design.md`'s eventual P-tier successor.
- The **multi-repo workspace path scheme** beyond what is needed to fix
  process ownership — owned by `spec-multi-repo-workspace-model.md`.
- **Build / CI / packaging** of the binaries themselves.
- **UI / chrome / panel layout** of the host applications.

### 1.3 Audience

Engineers implementing or modifying:

- The `caduceusd` daemon binary.
- The caduceus engine library that hosts embed.
- Any host (`caduceus-zed`, `caduceus-cli`, future cloud host).
- Any sidecar that crosses a process boundary owned by caduceus
  (RunnerProcess, AgentProcess, MCP server adapter, LSP shim).
- Reviewers of PRs whose diff touches an `os::Command::spawn`, a Unix-domain
  socket bind, a JSONL framer, or a capability-bit comparison.

### 1.4 Conformance

The keywords **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, **MAY** in
this document are to be interpreted as in RFC 2119. A non-MUST clause that
describes intent is annotated as `(rationale)` to make obvious that violating
it is not a conformance break.

A binary MAY claim conformance to this spec at version `v1` only if every
`MUST` and every `Z-N` invariant in §11 is preserved, and every acceptance
criterion in §12 has at least one passing automated test in a CI lane that is
required for the release branch.

---

## 2. Terms

This is the working glossary. The full glossary is in Appendix A; this
section gives just-enough vocabulary to read the rest of the spec without
a forward reference.

- **caduceusd** — the long-lived OS daemon that owns orchestrator state. One
  per user account, per machine. Listens on a Unix-domain socket (Linux/macOS)
  or a named pipe (Windows). Restarts independently of any host.
- **caduceus engine** — the in-process library that lives inside every host.
  Owns per-thread chat state (transcript, MemoryBlocks, broadcast emitter,
  gen-bound cancel token). Speaks **upward** to the host via in-process Rust
  API and **downward** to `caduceusd` via the daemon socket.
- **host** — any process that links the caduceus engine. Today: `caduceus-zed`
  (the Zed-fork GUI host) and `caduceus-cli`. Tomorrow: `caduceus-cloud-host`
  (Copilot Coding Agent / cloud delegation surface).
- **RunnerProcess** — a sub-process spawned by the engine to isolate the
  agent's stdio / signal surface. Parent of one or more **AgentProcess**
  children. Speaks ACP/JSONL over its stdio to the engine.
- **AgentProcess** — the actual model client (e.g. claude-code-cli, codex,
  goldeneye-cli). A child of RunnerProcess. Speaks the vendor's native wire
  protocol (often ACP, sometimes a vendor-specific superset) over its stdio
  to RunnerProcess.
- **MCP server** — a child process spawned per granted workflow to expose
  Model Context Protocol tools. Owned by the engine, not by the runner. Wire
  protocol is MCP over stdio.
- **LSP** — language server. Spawned by the host (not by caduceus) and
  exposed to the engine via a thin shim. Out of scope for lifecycle except
  insofar as MCP servers and LSPs share the host's "child-process" budget.
- **run** — the unit of orchestration. Owned by the daemon. Identified by a
  `run_id`. Has a workspace path, an autonomy budget, a retry map, an
  optional parent run, and a current status snapshot.
- **thread** — the unit of chat. Owned by the engine. Identified by a
  `thread_id`. May be associated with at most one in-flight run; many threads
  may have a history of completed runs. Joins to runs via `run_id`.
- **ACP** — Agent Coding Protocol. Line-oriented JSONL. 1 MiB max line.
  No multi-line records. Half-close (EOF on stdin) means "no more requests";
  reply EOF means "done". The wire-shape spec is
  `spec-caduceus-agent-runner-contract.md`.
- **RunSnapshot** — the immutable, monotonically-versioned status object the
  daemon publishes for each in-flight run. Replaces Symphony's 20 ms tick
  render delay with explicit pubsub. Schema owned by
  `spec-orchestrator-status-snapshot.md`.
- **C-hybrid** — the locked-in topology decision: orchestrator state out of
  process, chat state in process, joined on `run_id`.

---

## 3. Process inventory

### 3.0 Inventory at a glance

Every running caduceus deployment consists of **at most** the following
kinds of OS process. (A given user session may not exercise all of them;
e.g. an air-gapped CLI invocation never spawns a cloud-handoff process.)

| # | Process kind        | Cardinality                          | Lifetime class      | Parent (supervisor)  | Owns                                                         |
|---|---------------------|--------------------------------------|---------------------|----------------------|--------------------------------------------------------------|
| 1 | `caduceusd`         | 1 per user, per machine              | machine-long        | OS init / launchd / systemd | run registry, retry map, snapshot pubsub, workspace registry |
| 2 | host (`caduceus-zed`, `caduceus-cli`, …) | 1 per user-launched UI/CLI invocation | session-long | OS shell / desktop launcher | UI, panel state, in-process engine library                   |
| 3 | engine (in-process inside host) | 1 per host process              | = host              | host                 | thread store, transcript, MemoryBlocks, MCP supervisor       |
| 4 | RunnerProcess       | 1 per active agent dispatch          | run-long            | engine               | ACP framing, agent stdio, signal cascade                     |
| 5 | AgentProcess        | 1 per RunnerProcess (typically)      | ≤ run-long          | RunnerProcess        | model client / vendor CLI                                    |
| 6 | MCP server          | 1 per granted workflow tool          | ≤ thread-long       | engine               | MCP tool surface                                             |
| 7 | LSP                 | 1 per language, host's choice        | host-long           | host                 | language analysis (out of caduceus's lifecycle)              |
| 8 | cloud-handoff (`caduceus-cloud-host` peer) | 0 or 1 per delegated run | run-long (remote)   | caduceusd (logically)| remote agent run                                             |

The remainder of this section gives, for each kind, a normative description of
its purpose, the executable that backs it, the surface area it owns, and the
**non-goals** that make explicit what each kind deliberately does *not* own.

### 3.1 ASCII process-topology diagram

The following diagram is **the** canonical picture. PRs that change the
process model MUST update it.

```
                            ┌────────────────────────┐
                            │       end user         │
                            │ (keyboard / shell tty) │
                            └───────────┬────────────┘
                                        │ user input
                                        ▼
   ┌────────────────────────────────────────────────────────────────┐
   │                          host process                          │
   │            (caduceus-zed  |  caduceus-cli  |  …)               │
   │                                                                │
   │   ┌─────────────────────┐         ┌────────────────────────┐   │
   │   │   host UI / chrome  │ ◀────▶  │   in-process engine    │   │
   │   │  (panel, kbd, term) │         │   (caduceus crate)     │   │
   │   └─────────────────────┘         │                        │   │
   │                                   │   • thread store       │   │
   │                                   │   • transcript         │   │
   │                                   │   • MemoryBlocks       │   │
   │                                   │   • broadcast emitter  │   │
   │                                   │   • gen cancel token   │   │
   │                                   │   • MCP supervisor     │   │
   │                                   └─────────┬──────────────┘   │
   │                                             │                  │
   └─────────────────────────────────────────────┼──────────────────┘
                                                 │ daemon socket
                                                 │ (UDS / named pipe)
                                                 │ JSONL control RPC
                                                 ▼
                              ┌──────────────────────────────────┐
                              │           caduceusd              │
                              │   (long-lived per-user daemon)   │
                              │                                  │
                              │   • run registry (run_id → …)    │
                              │   • retry map                    │
                              │   • RunSnapshot pubsub bus       │
                              │   • workspace registry           │
                              │   • approval-broker dispatch     │
                              │   • cloud-handoff coordinator    │
                              └────────────────┬─────────────────┘
                                               │ spawn / supervise
                                               ▼
                              ┌──────────────────────────────────┐
                              │         RunnerProcess            │  (1 per active run)
                              │      (parent of agent stdio)     │
                              │                                  │
                              │   stdio: ACP / JSONL framing     │
                              │   signal cascade owner           │
                              └────────────────┬─────────────────┘
                                               │ spawn / pipe (stdio)
                                               ▼
                              ┌──────────────────────────────────┐
                              │          AgentProcess            │  (1 per runner)
                              │  claude-code-cli | codex | …     │
                              │                                  │
                              │   speaks vendor wire protocol    │
                              └──────────────────────────────────┘

   Sidecars spawned by the engine (NOT by the runner):

       engine ──spawn──▶ MCP server  (1 per granted workflow tool)
       engine ──spawn──▶ MCP server
       engine ──spawn──▶ MCP server   …

   Sidecars spawned by the host (NOT by caduceus):

       host ──spawn──▶ LSP (rust-analyzer | gopls | …)
       host ──spawn──▶ LSP …

   Optional cloud-handoff (only when a run is delegated to a cloud agent):

       caduceusd ──HTTPS──▶ caduceus-cloud-host (remote)
                            └─▶ remote RunnerProcess
                                └─▶ remote AgentProcess
       caduceusd ──HTTPS◀── RunSnapshot stream from remote
```

The diagram is intentionally redundant with the table in §3.0; the table is the
authoritative inventory and the diagram is the navigation aid.

### 3.2 Process kind 1 — `caduceusd`

**Purpose.** `caduceusd` is the only process in the system that owns
**durable** orchestrator state (run registry, retry counters, workspace
registry, snapshot pubsub fan-out). It exists so that hosts can come and go
— the user can quit `caduceus-zed`, switch to `caduceus-cli`, reboot the
machine — without losing track of in-flight runs (where "in flight" is
extended in §9.4 to include the post-crash recovery window).

**Backing executable.** `caduceusd` is a single static binary. It is
distributed alongside the host binaries and lives at one of:

- `/usr/local/libexec/caduceusd` (system-wide install)
- `~/.local/libexec/caduceusd` (per-user install)
- `<host.app>/Contents/Helpers/caduceusd` (macOS bundled-with-host install)

The host MUST resolve `caduceusd` via a documented search path (§17.2 in
`spec-multi-repo-workspace-model.md` for the canonical search order).

**Surface owned.**

- The **daemon socket** (Unix-domain socket on Linux/macOS, named pipe on
  Windows). One per user. Path:
  `${XDG_RUNTIME_DIR:-/tmp}/caduceus-${uid}/daemon.sock` on Linux,
  `~/Library/Caches/com.caduceus/daemon.sock` on macOS,
  `\\.\pipe\caduceus-${user-sid}` on Windows.
- The **run registry** (in-memory + on-disk WAL): the authoritative mapping
  `run_id → RunRecord`.
- The **RunSnapshot pubsub bus**: the channel any host subscribes to to
  receive `RunSnapshot` updates. Replaces Symphony's 20 ms render-tick
  poll with an explicit pub/sub.
- The **retry map**: per-run, per-failure-class retry counters and budgets
  consumed by the orchestrator algorithm.
- The **workspace registry**: the mapping
  `(repo_slug, run_id) → workspace_path`.
- The **approval-broker dispatch**: when a runner emits an approval
  request, the daemon decides which host(s) currently render the prompt
  (since the user may have closed the originating host).
- The **cloud-handoff coordinator**: outbound HTTPS to a remote
  `caduceus-cloud-host`, including the credential blob and the snapshot
  stream pull.

**Non-goals (what `caduceusd` does NOT own).**

- It MUST NOT hold per-thread chat state (no transcripts, no MemoryBlocks,
  no broadcast emitters). Those live in the engine. The daemon is **stateless
  about chat**.
- It MUST NOT render UI. It has no terminal, no GUI surface, no notification
  center integration. Approval prompts are *dispatched* by the daemon but
  *rendered* by hosts.
- It MUST NOT call into model providers directly. All vendor traffic flows
  through a runner+agent pair.
- It MUST NOT spawn LSPs. LSPs are host-owned.

**Lifetime class.** Machine-long. The daemon survives any host crash. It is
brought up by `launchd` (macOS), `systemd --user` (Linux), or the Windows
Service Control Manager. On first connect from a host, if the daemon is not
already running, the host MAY auto-start it via the OS launcher (it MUST NOT
fork it directly — see Z-3).

### 3.3 Process kind 2 — host

**Purpose.** A host is the user-facing process. It owns the rendering
surface (terminal, GUI panel, web socket bridge) and embeds the caduceus
engine as an in-process library.

**Backing executables (today).**

- `caduceus-zed` — the Zed-fork GUI host. Multi-platform native app.
- `caduceus-cli` — the headless CLI host. Used for scripting, CI, and
  air-gapped testing.

**Backing executables (planned).**

- `caduceus-cloud-host` — a peer host that runs in the cloud and is driven
  by the local daemon over HTTPS. Used for the agent-handoff workflow.

**Surface owned.**

- All UI rendering.
- The host's own configuration store (e.g. Zed settings, CLI flags).
- The host's LSP fleet (host-spawned, host-supervised).
- The in-process engine library (see §3.4).
- The connection to `caduceusd` (one daemon socket connection per host
  process).

**Non-goals.**

- A host MUST NOT hold authoritative orchestrator state. If a host caches
  `RunSnapshot`s for UI smoothing, it MUST treat the daemon as the source
  of truth on reconnect.
- A host MUST NOT spawn RunnerProcess directly. Runner spawn is owned by
  the daemon (Z-5).

**Lifetime class.** Session-long. Lives as long as the user keeps the host
window/CLI invocation open. Crashing a host MUST NOT crash the daemon, MUST
NOT crash any RunnerProcess (because runners are children of the daemon, not
of the host — Z-5), and MUST NOT lose any in-flight `run_id`'s state.

### 3.4 Process kind 3 — engine (in-process inside the host)

**Purpose.** The engine is the in-process library that hosts link. It owns
per-thread chat state. It is **not** a separate process; it shares an
address space with whichever host process embeds it.

**Surface owned.**

- The **thread store** (in-memory, persisted via the host's local storage):
  `thread_id → ThreadRecord`. Each `ThreadRecord` includes the transcript,
  the MemoryBlocks, the active broadcast emitter, and the gen-bound cancel
  token.
- The **transcript** for each thread: the ordered append-only log of
  user/assistant/tool messages.
- The **MemoryBlocks**: structured long-lived context that survives across
  turns within a thread.
- The **broadcast emitter** for the active turn: where new tokens are
  pushed for downstream rendering.
- The **gen-bound cancel token**: a per-generation abort handle so a new
  turn cleanly cancels the previous turn's stragglers.
- The **MCP supervisor**: the engine spawns and reaps MCP servers per
  granted workflow.
- The **`run_id` join**: a thread that dispatches an agent run holds a
  reference to the `run_id` returned by the daemon.

**Non-goals.**

- The engine MUST NOT persist orchestrator state (no retry counters, no
  workspace registry, no cross-host run dispatch). Those are daemon-owned.
- The engine MUST NOT render UI. It exposes a Rust API the host calls.

**Lifetime class.** Same as host. When the host process exits, the engine's
in-memory state is gone — but the *runs the engine launched* survive,
because the runners are children of the daemon, and the daemon persists
the run records to its WAL (Z-5, Z-7).

### 3.5 Process kind 4 — RunnerProcess

**Purpose.** RunnerProcess isolates the agent from the engine and from the
daemon, providing a stable JSONL/ACP wire boundary. It is the parent of
the actual model client (AgentProcess).

**Surface owned.**

- **stdio framing.** RunnerProcess speaks ACP — line-oriented JSONL,
  1 MiB max line length, no multi-line records, half-close semantics.
  Wire shape is `spec-caduceus-agent-runner-contract.md`.
- **signal cascade.** When the daemon decides a run must stop (user
  cancel, autonomy-budget exhaustion, parent run terminated), it sends
  the runner a stop signal. The runner is responsible for cleanly
  cascading that to its child AgentProcess (SIGTERM, then SIGKILL after
  a documented grace).
- **stdout/stderr separation.** Runner stdout is the ACP wire; runner
  stderr is treated as a diagnostic stream and tagged into the run's
  log artifact.

**Non-goals.**

- RunnerProcess MUST NOT initiate network I/O. All network I/O is the
  AgentProcess's privilege.
- RunnerProcess MUST NOT see the user's filesystem authority directly.
  When a tool call requires filesystem write, the runner forwards the
  ACP request upward; the *engine* (or, for autonomous tools, the
  daemon) consults the permission pipeline.
- RunnerProcess MUST NOT have persistent state. If it crashes, the
  daemon may respawn it from the run record (subject to retry policy).

**Lifetime class.** Run-long. One RunnerProcess per active run. Reaped
by the daemon when the run terminates (success, fail, cancel, or budget
exhausted). The daemon MUST collect the runner's exit status and
fold it into the final RunSnapshot.

### 3.6 Process kind 5 — AgentProcess

**Purpose.** AgentProcess is the actual model client. It is the only
process in the system that holds long-lived credentials for a model
provider (vendor token, OAuth blob, or local-model file path).

**Backing executables.** Whatever the vendor ships:

- `claude-code-cli` (Anthropic).
- `codex` (OpenAI).
- `goldeneye-cli` (Microsoft internal).
- A local llama.cpp-based binary, etc.

caduceus does not ship these binaries; it discovers them via the user's
PATH or a pinned per-vendor path in config.

**Surface owned.**

- The vendor's own wire protocol (often ACP, sometimes a vendor
  superset, occasionally something else entirely with a thin shim).
- The model-provider HTTPS connection.
- The sandbox the vendor implements (which caduceus does NOT
  re-implement — see Z-12).

**Non-goals.**

- AgentProcess MUST NOT spawn further child processes for tool dispatch.
  Tool dispatch (filesystem write, shell exec) is request-routed via
  ACP back up to the engine, which consults the permission pipeline,
  which delegates to the appropriate executor process. (Some vendors
  do spawn sub-tools internally for their own reasoning; that is the
  vendor's affair, but the *user-visible* tool surface flows through
  the runner.)
- AgentProcess MUST NOT persist state across runs. If a vendor needs
  cross-run memory, it does so via MemoryBlocks in the engine, surfaced
  to the agent via prompts.

**Lifetime class.** ≤ run-long. Typically the agent dies before the
runner (the runner waits for `wait()` and reports exit). On a hung
agent, the runner kills it (SIGTERM-then-SIGKILL with a grace).

### 3.7 Process kind 6 — MCP server

**Purpose.** An MCP server exposes Model Context Protocol tools (search,
file-fetch, ticket-system bridge, etc.) to the agent's tool surface.

**Backing executables.** Anything the user has configured. Discovered
via the per-workflow MCP grant in user config.

**Surface owned.**

- One MCP wire connection over stdio.
- Whatever sandbox the MCP server itself implements (caduceus does
  not constrain it beyond the OS-level child-process isolation).

**Non-goals.**

- MCP servers MUST NOT be spawned by the runner or the agent. They
  are children of the **engine**. (Rationale: the engine is the
  authority that decides which workflows are granted, so the engine
  controls the lifetime.)
- MCP servers MUST NOT outlive the host process that spawned them
  (because the engine is in-host and dies with the host). On host
  crash, the engine's MCP children are reaped by the OS via the
  parent-death signal (Linux: `prctl(PR_SET_PDEATHSIG)`; macOS: a
  watchdog thread; Windows: job objects).

**Lifetime class.** ≤ thread-long, scoped to host. An MCP server may
serve many turns within a thread but does not survive across hosts.

### 3.8 Process kind 7 — LSP

**Purpose.** Language analysis. caduceus does not own LSP lifecycle
(the host does) but the engine consumes LSP results via host-mediated
APIs.

**Lifetime class.** Host-long. Reaped on host exit.

**This spec's only constraint on LSPs:** the engine MUST NOT spawn
LSPs directly. The host's LSP fleet is the host's affair.

### 3.9 Process kind 8 — cloud-handoff peer

**Purpose.** When a run is delegated to a cloud-hosted agent (the
"agent-handoff" workflow), a remote `caduceus-cloud-host` peer process
runs in the cloud and is driven by the local daemon.

**Surface owned.**

- A remote runner+agent pair (mirror of §3.5/§3.6 but on the cloud
  host).
- An outbound RunSnapshot stream (cloud → local) so the local user
  sees progress.
- Inbound approval-broker forwarding (local user is still the
  decision-maker, even though the run executes remotely).

**Non-goals.**

- The cloud peer MUST NOT speak to the user's host directly. All
  cloud traffic goes through `caduceusd`. The host sees the cloud run
  the same way it sees a local run: as a `run_id` and a snapshot
  stream.

**Lifetime class.** Run-long, remote. Reaped by the cloud-host's
own supervisor when the run terminates. The local daemon notices via
either an explicit terminal RunSnapshot or a heartbeat-timeout policy.

---

## 4. IPC channels

This section enumerates every inter-process communication channel in the
system. Each channel has a **transport**, an **encoding**, a **framing**
rule, an **authentication** rule, and a **flow direction**. The same
channel may carry multiple logical sub-protocols (e.g. the daemon socket
carries control RPCs *and* RunSnapshot pubsub on the same connection,
multiplexed by message kind).

### 4.0 Channel summary

| ID    | Channel                            | Transport                   | Encoding   | Framing                | Auth                              | Direction                     |
|-------|------------------------------------|-----------------------------|------------|------------------------|-----------------------------------|-------------------------------|
| C-1   | host ↔ engine                      | in-process Rust function calls | n/a    | n/a                    | shared address space              | bidirectional                 |
| C-2   | engine ↔ daemon (control + pubsub) | UDS / named pipe            | JSONL      | LF-delimited, 1 MiB    | OS uid (UDS peer cred / pipe ACL) | bidirectional, multiplexed    |
| C-3   | daemon ↔ runner                    | OS pipes (stdin/stdout/stderr) | JSONL   | ACP framing            | parent/child trust                | bidirectional via stdin/stdout|
| C-4   | runner ↔ agent                     | OS pipes                    | vendor-specific (often JSONL) | vendor-specific (often ACP) | parent/child trust                | bidirectional                 |
| C-5   | engine ↔ MCP server                | OS pipes                    | JSON-RPC   | MCP framing            | parent/child trust                | bidirectional                 |
| C-6   | host ↔ LSP                         | OS pipes / TCP              | JSON-RPC   | LSP framing            | host's affair                     | bidirectional                 |
| C-7   | daemon ↔ cloud-handoff peer        | TCP / TLS                   | HTTP/2 or HTTPS+SSE | per cloud-host spec    | OAuth2 + signed run handle        | bidirectional                 |
| C-8   | agent ↔ model provider             | TCP / TLS                   | vendor wire protocol | vendor framing         | vendor token (held by agent only) | outbound (with streamed reply)|
| C-9   | approval-broker dispatch           | layered on C-2              | JSONL      | LF-delimited           | derived from C-2                  | daemon → engine, reply engine → daemon |
| C-10  | RunSnapshot pubsub                 | layered on C-2              | JSONL      | LF-delimited           | derived from C-2                  | daemon → engine (fan-out)     |

The C-N IDs are referenced by the invariants in §11 and the acceptance
criteria in §12.

### 4.1 C-1 — host ↔ engine (in-process)

**Transport.** Direct Rust function calls. The engine is a library, not
a process.

**Encoding.** Native Rust types. There is no serialization boundary.

**Framing.** N/A.

**Authentication.** Implicit: shared address space implies the host
trusts the engine and vice versa. There is no defence against a
compromised host process — caduceus's trust model treats the host
process as part of the user's TCB.

**Flow direction.** Bidirectional. The host calls into the engine
(e.g. "submit user message", "cancel turn"); the engine calls back via
callbacks/channels (e.g. "render a token", "request approval").

**Invariant link.** Z-1 (engine never escapes its host process).

### 4.2 C-2 — engine ↔ daemon (daemon socket)

**Transport.**

- **Linux/macOS:** Unix-domain socket (SOCK_STREAM).
- **Windows:** named pipe (`\\.\pipe\caduceus-${user-sid}`).

The socket is bound 0700 and the parent directory is 0700. UDS peer
credentials (`SO_PEERCRED` on Linux; `LOCAL_PEERCRED` on macOS) MUST
match the user's uid; the daemon MUST reject connections with mismatched
peer uid. On Windows the named pipe ACL MUST grant only the user's
SID.

**Encoding.** JSONL. One JSON object per line. UTF-8.

**Framing.** Line-feed delimited (`\n`). 1 MiB max per line. Multi-line
JSON records are forbidden (a record that does not parse on a single
LF-delimited line is a protocol violation; the receiving side MUST
close the connection).

**Authentication.** OS-level uid match (UDS peer credential or pipe
ACL). There is no token-based auth on this channel — the OS already
gates access. (Rationale: the daemon is per-user; an attacker who can
reach the socket has already compromised the user account.)

**Flow direction.** Bidirectional. Multiplexed by message kind:

- Control RPCs (engine → daemon, daemon → engine).
- RunSnapshot pubsub broadcasts (daemon → engine).
- Approval-broker dispatch (daemon → engine; engine replies inline).
- Workspace-registry queries (engine → daemon).

**Sub-protocols carried.** C-9, C-10 (see §4.9, §4.10).

**Half-close semantics.** Either side closing its write half signals
"no more messages from me." The daemon MUST treat an engine close as a
voluntary unsubscribe (any in-flight runs continue; the engine's
RunSnapshot subscription is cancelled). The engine MUST treat a daemon
close as a daemon shutdown signal and surface it to the host as a
"daemon-disconnected" status.

### 4.3 C-3 — daemon ↔ runner

**Transport.** OS pipes. The daemon spawns RunnerProcess via
`posix_spawn` (Unix) or `CreateProcess` (Windows), inheriting stdin /
stdout / stderr.

**Encoding.** JSONL.

**Framing.** ACP framing per
`spec-caduceus-agent-runner-contract.md`: line-oriented JSONL, 1 MiB
max, no multi-line records. The runner MUST treat any line over 1 MiB
as a fatal protocol error and exit non-zero with a diagnostic on
stderr.

**Authentication.** Parent/child trust. The daemon spawned the
runner, set its environment, set its cwd to the workspace path, and
holds its handle.

**Flow direction.** Bidirectional via stdin/stdout. stderr is a
diagnostic stream (consumed by the daemon and folded into the run's
log artifact, see §9.5).

**Stop semantics.** The daemon stops a runner by:
1. Closing the runner's stdin (half-close → ACP "no more requests").
2. Waiting up to a documented grace (default 2 s) for the runner to
   exit cleanly.
3. Sending SIGTERM.
4. After a further grace (default 2 s) sending SIGKILL.

The runner MUST cascade each step to its child agent.

### 4.4 C-4 — runner ↔ agent

**Transport.** OS pipes (parent runner spawns child agent).

**Encoding.** Vendor-specific. Most vendors today speak JSONL/ACP;
some require a thin shim.

**Framing.** Vendor-specific. The runner MUST translate to/from ACP
on its upstream side (C-3) so that anything north of the runner sees
a uniform wire.

**Authentication.** Parent/child.

**Stop semantics.** The runner is the only process authorized to
terminate the agent. Cascading from C-3's stop steps.

### 4.5 C-5 — engine ↔ MCP server

**Transport.** OS pipes (engine spawns MCP server as a child).

**Encoding.** JSON-RPC over stdio per the MCP spec.

**Framing.** MCP's framing (Content-Length-prefixed messages).

**Authentication.** Parent/child trust + the engine's per-workflow
grant policy. The engine MUST NOT spawn an MCP server unless the
current workflow's MCP grant explicitly includes that server (Z-9).

**Flow direction.** Bidirectional. The engine calls tools; the
MCP server may emit notifications.

**Lifetime.** The engine reaps the MCP server when:
- the host shuts down (Z-7), or
- the workflow grant is revoked, or
- the MCP server itself exits non-zero (in which case the engine MUST
  surface a "tool unavailable" status and not silently restart unless
  policy permits).

### 4.6 C-6 — host ↔ LSP

**Transport.** OS pipes or TCP, host's choice.

**Encoding / framing.** Standard LSP.

This spec only fixes that **the engine does not spawn LSPs**. Any
analysis the engine consumes flows through host-mediated APIs (C-1).

### 4.7 C-7 — daemon ↔ cloud-handoff peer

**Transport.** TCP/TLS. HTTPS or HTTP/2.

**Encoding.** Per the cloud-host spec (out of scope here).

**Authentication.** OAuth2 bearer token + a signed `run_handle`
issued by the daemon. The cloud peer MUST verify both. The local
daemon stores the OAuth refresh token in the OS keychain.

**Flow direction.** Bidirectional:
- Outbound: dispatch RPCs ("start run R with prompt P, budget B").
- Inbound: RunSnapshot stream (over SSE or HTTP/2 push).
- Inbound: approval-broker requests (forwarded to local engine via
  C-9).

**Failure mode.** Network partition is treated as a soft failure;
the daemon retains the run record and resumes the snapshot stream
on reconnect within a documented heartbeat budget. After heartbeat
exhaustion the run is marked `unreachable` and surfaced as such.

### 4.8 C-8 — agent ↔ model provider

**Transport.** TCP/TLS. The vendor's own endpoint.

**Encoding.** Vendor wire protocol.

**Authentication.** Vendor-issued credentials, held only inside
AgentProcess (Z-11).

**Caduceus's role:** none. This channel is opaque to caduceus. The
agent owns it. Caduceus only observes the agent's stdio (C-4).

### 4.9 C-9 — approval-broker dispatch (layered on C-2)

When the runner emits an approval request (e.g. "the agent wants to
shell-exec `rm -rf X`"), the request flows up via C-3 to the daemon.
The daemon MUST:
1. Run the 9-step permission evaluate pipeline (delegated to the
   engine since the engine is the holder of per-thread context).
2. If the pipeline yields `prompt`, dispatch a prompt to the
   *currently-rendering* host(s).
3. Wait for the user's reply (relayed via C-2).
4. Forward the reply back down C-3 to the runner.

The dispatch decision (which host renders the prompt) is daemon-owned:
a single user may have multiple hosts open; the daemon picks the host
that owns the originating thread, with a fallback ordering documented
in `spec-m-permissions.md`.

### 4.10 C-10 — RunSnapshot pubsub (layered on C-2)

The daemon publishes a `RunSnapshot` for every state-changing event in
the run lifecycle (start, tool-call, tool-reply, autonomy-budget tick,
self-pause, terminal). Hosts subscribed via C-2 receive the broadcast.

**Backpressure.** A slow subscriber MUST NOT slow the publisher. The
daemon maintains a bounded per-subscriber queue (default 64 messages);
on overflow, the daemon drops the oldest **non-terminal** snapshot for
that subscriber and notes a `dropped: N` counter in the next
delivered snapshot. Terminal snapshots are *never* dropped.

**Ordering.** Within a single `run_id`, snapshots are strictly ordered
by a monotonic version. Across different `run_id`s there is no ordering
guarantee.

**Subscription model.** A host subscribes via an explicit RPC on
C-2: `subscribe_run_snapshots { filter: { run_ids: [...] | all } }`.
Subscriptions are per-connection; closing C-2 cancels them.

### 4.11 Wire boundary table

The following is the canonical wire-boundary table referenced by
acceptance criterion AC-IPC-1 in §12.

| From       | To         | Channel | Crosses process boundary? | Crosses uid? | Crosses host? |
|------------|------------|---------|---------------------------|--------------|---------------|
| host       | engine     | C-1     | no                        | no           | no            |
| engine     | daemon     | C-2     | yes                       | no           | no            |
| daemon     | runner     | C-3     | yes                       | no           | no            |
| runner     | agent      | C-4     | yes                       | no           | no            |
| engine     | MCP server | C-5     | yes                       | no           | no            |
| host       | LSP        | C-6     | yes                       | no           | no            |
| daemon     | cloud peer | C-7     | yes                       | maybe        | yes           |
| agent      | provider   | C-8     | yes (network)             | yes (remote) | yes           |

---

## 5. Lifecycle ownership

This section gives, for every process kind in §3, the **single supervisor**
that spawns, monitors, restarts, and reaps it. Every process kind has
**exactly one** supervisor; "everyone supervises everyone" is the path to
double-spawns and orphan zombies.

### 5.0 Supervisor matrix

| Process kind          | Spawned by         | Monitored by       | Restartable?            | Reaped by          | On supervisor crash |
|-----------------------|--------------------|--------------------|-------------------------|--------------------|--------------------|
| `caduceusd`           | OS launcher        | OS launcher        | yes (launcher policy)   | OS launcher        | n/a (launcher)     |
| host                  | OS shell/desktop   | none (user-facing) | n/a (user re-launches)  | OS                 | n/a                |
| engine                | host               | host (in-process)  | n/a (panic ⇒ host dies) | host               | n/a                |
| RunnerProcess         | daemon             | daemon             | yes (retry policy)      | daemon             | runner orphaned then OS-reaped (Linux: PR_SET_PDEATHSIG); daemon respawns on restart |
| AgentProcess          | runner             | runner             | no (run terminates)     | runner             | agent orphaned then OS-reaped |
| MCP server            | engine             | engine             | yes (policy-bounded)    | engine             | MCP server reaped via OS parent-death signal |
| LSP                   | host               | host               | host's affair           | host               | n/a                |
| cloud-handoff peer    | daemon (remote)    | daemon (remote)    | yes (remote policy)     | remote supervisor  | local daemon marks `unreachable` |

### 5.1 caduceusd

**Spawn.** OS launcher.

- macOS: a `launchd` LaunchAgent at `~/Library/LaunchAgents/com.caduceus.daemon.plist`,
  KeepAlive=true, RunAtLoad=true, with the daemon's stdout/stderr redirected to
  `~/Library/Logs/caduceus/daemon.log`.
- Linux: a `systemd --user` unit `~/.config/systemd/user/caduceusd.service`,
  Restart=on-failure, RestartSec=2.
- Windows: a per-user Service Control Manager service.

**Auto-start on host connect.** If the host opens C-2 and `connect()` fails
with ENOENT/ECONNREFUSED, the host MAY ask the OS launcher to bring the
daemon up (e.g. `launchctl kickstart` on macOS, `systemctl --user start
caduceusd` on Linux). The host MUST NOT `fork()` the daemon directly
(Z-3 — the daemon must always be reaped by the OS launcher).

**Restart policy.** The OS launcher decides. Default: restart-on-crash with
exponential backoff capped at 60 s. The daemon's WAL allows it to recover
in-flight runs on restart (§9.4).

### 5.2 Host

**Spawn.** User action: opening the GUI, running the CLI binary, or
launching from a desktop shortcut.

**Restart policy.** None automatic. The user re-launches. The host's exit
MUST NOT take down `caduceusd` or any RunnerProcess (Z-7).

### 5.3 Engine

**Spawn.** In-process, when the host calls `caduceus::Engine::new(...)`.

**Restart.** N/A. An engine panic propagates as a host process failure; the
host's own crash policy applies.

### 5.4 RunnerProcess

**Spawn.** Always by the daemon, in response to a run-dispatch RPC on
C-2. The daemon:
1. Allocates a fresh `run_id`.
2. Resolves the workspace path via the workspace registry.
3. Sets the runner's cwd to the workspace path.
4. Sets the runner's environment to the curated whitelist documented
   in `spec-caduceus-agent-runner-contract.md` (no inheritance of the
   host's full environment — see Z-10).
5. Calls `posix_spawn` (Unix) / `CreateProcess` (Windows).

**Monitor.** The daemon `wait()`s on the runner via an async waitpid
shim (Linux/macOS) or a JobObject completion port (Windows).

**Restart.** Subject to the orchestrator's retry policy (see
`spec-caduceus-orchestrator-algorithm.md`). The daemon decides; the
host has no role.

**Reap.** The daemon. Exit status is folded into the terminal
RunSnapshot.

### 5.5 AgentProcess

**Spawn.** Always by the RunnerProcess, never by the daemon directly,
never by the engine, never by the host.

**Monitor.** The runner.

**Reap.** The runner.

**Stop cascade.** When the daemon stops the runner (§4.3), the runner
MUST cascade SIGTERM-then-SIGKILL to the agent within a documented
grace window. If the runner crashes before reaping the agent, the OS
parent-death signal SHOULD prevent an orphan; on platforms where this
is unreliable, the daemon SHALL run a periodic orphan sweep
(documented as a follow-up; see §14 OQ-3).

### 5.6 MCP server

**Spawn.** The engine, in response to a granted workflow that names
the MCP server.

**Monitor.** The engine.

**Restart.** Bounded by per-server policy: at most N restarts in a
sliding window of T seconds (defaults N=3, T=60). Exhausted MCP
servers are surfaced as "tool unavailable" in the next agent prompt
construction.

**Reap.** The engine on workflow revocation, host shutdown, or policy
exhaustion. The OS parent-death signal MUST be configured (Z-9b) so
that an engine crash does not orphan MCP servers.

### 5.7 LSP

Out of caduceus's lifecycle. The host owns LSPs entirely.

### 5.8 cloud-handoff peer

**Spawn.** Remote. The local daemon dispatches a "start run" RPC over
C-7; the cloud-host peer spawns a remote runner+agent pair.

**Monitor.** Remote. The local daemon observes via the inbound
RunSnapshot stream (§4.10) and a heartbeat.

**Reap.** Remote.

**Heartbeat-timeout.** If no snapshot arrives for HBT_MAX seconds
(default 90), the local daemon marks the run `unreachable`, drops it
from the dispatch queue, and surfaces the status to subscribers. On
reconnect, the cloud peer's authoritative state wins; the local
daemon reconciles.

### 5.9 Bootstrap order (clean install)

For a freshly installed system:

1. User installs the host package (which includes `caduceusd` as a
   helper binary).
2. The host package's post-install script installs the OS launcher
   manifest (LaunchAgent / systemd unit / SCM service).
3. The OS launcher starts `caduceusd` on next user login (or
   immediately, if RunAtLoad=true).
4. The user launches the host (`caduceus-zed` or `caduceus-cli`).
5. The host opens C-2; the daemon accepts.
6. The user submits a prompt; the engine dispatches a run on C-2; the
   daemon spawns a RunnerProcess; the runner spawns AgentProcess.

The bootstrap order is normative for installer authors and integration
tests (AC-LIFE-1 in §12).

### 5.10 Shutdown order (clean exit)

1. User closes the host.
2. The host calls `engine.shutdown()`. The engine reaps its MCP
   children via the documented mechanism, drops its C-2 connection
   (half-close), and returns.
3. The host process exits.
4. The daemon notices the C-2 close, marks any subscriptions as
   cancelled, but **does not** stop runs. Runs continue under daemon
   supervision (Z-7).
5. The user (or the OS shutdown sequence) sends `caduceusd` a
   `SIGTERM`.
6. `caduceusd` writes a final WAL checkpoint, broadcasts a "daemon
   shutdown imminent" terminal snapshot to all in-flight runs (which
   may then be marked `paused-by-shutdown` for resume on next boot),
   stops accepting new C-2 connections, drains C-3 stop cascades,
   waits for runners to exit (with a hard cap), then exits.

---

## 6. Host capability negotiation

This section is normative for any host that links the engine. It describes
the handshake that runs on every fresh C-1 connection (host <-> engine, in
process) and on the C-2 connection the engine then opens to the daemon.

### 6.0 Why a handshake exists

Hosts differ. `caduceus-zed` can render arbitrarily-styled approval cards
in a panel; `caduceus-cli` can render only a textual prompt on stderr; a
future cloud host may render nothing locally and instead route prompts
to the user via a notification service. The engine adapts to the host's
capabilities; therefore the host MUST advertise them.

A handshake also enables forward compatibility: a newer engine can detect
that the host is older and degrade gracefully; an older engine can detect
that the host advertises capability bits it does not know about and
politely ignore them.

### 6.1 Capability bits

The host advertises a `HostCapabilities` struct on engine init:

```
struct HostCapabilities {
    proto_version: SemVer,
    can_render_markdown: bool,
    can_render_diff: bool,
    can_render_approval_card: bool,
    can_render_progress_bar: bool,
    can_accept_streaming_input: bool,
    can_accept_keyboard_interrupt: bool,
    has_panel_ui: bool,
    has_terminal_tty: bool,
    has_notification_center: bool,
    can_render_approval_locally: bool,
    approval_priority: u32,
    can_keep_alive_when_minimized: bool,
    can_run_headless: bool,
    host_name: String,
    host_version: SemVer,
    host_pid: u32,
    host_uid: u32,
}
```

The engine forwards (a subset of) these bits to `caduceusd` on C-2 so the
daemon's approval-broker can pick the right host (see section 4.9).

### 6.2 Handshake sequence

```
1. host   -> engine: Engine::new(HostCapabilities { ... })
2. engine -> host:   EngineHandle (in-process)
3. engine -> daemon (C-2): { kind: "hello", engine_version, host{...}, subscriptions:[] }
4. daemon -> engine (C-2): { kind: "hello-ack", daemon_version, wire_version, session_id, active_runs }
5. engine -> host: HandshakeReport { daemon_reachable, wire_version_ok, active_runs, warnings }
```

If step 3's wire-version mismatch is fatal, the daemon MUST reject the
connection with `kind: "hello-rej"` and a documented reason; the engine
MUST surface the rejection to the host as a structured error and refuse
to dispatch runs (read-only fallback to inspect existing run records is
permitted).

### 6.3 Capability degradation rules

For each capability bit the host advertises false, the engine MUST take
the documented fallback:

- `!can_render_markdown` => engine emits plaintext.
- `!can_render_diff` => engine emits unified-diff text.
- `!can_render_approval_card` => engine emits a textual yes/no prompt
  (fallback path documented in `spec-m-permissions.md`).
- `!can_render_approval_locally` => engine SHALL set `approval_priority`
  to 0 and explicitly tell the daemon "do not route approvals here". The
  daemon picks another host or, if no host can render, applies the policy
  default for unrouted approvals (deny if forcePrompt, otherwise stall
  the run with a status of `awaiting-approval` until a capable host
  connects).
- `!has_panel_ui && !has_terminal_tty` => engine refuses to dispatch any
  run that would require human input (full-headless mode).

### 6.4 Capability changes mid-session

A host MAY upgrade or downgrade a capability mid-session. The host
signals this via an in-process API call; the engine forwards a
`host-caps-changed` record on C-2; the daemon updates its routing table.

### 6.5 Multi-host coexistence

A single user MAY have multiple hosts open concurrently (a `caduceus-zed`
window plus a `caduceus-cli` invocation in another terminal). Each host
opens its own C-2 connection; the daemon treats each as a separate
subscriber.

The daemon's approval-broker MUST pick a single host for a given prompt
(Z-13). Tie-breaking order:

1. If the run was dispatched by host H and H is still connected and
   `can_render_approval_locally`, route to H.
2. Otherwise pick the connected host with the highest `approval_priority`
   that `can_render_approval_locally`.
3. Otherwise stall (see section 6.3).

### 6.6 Engine <-> host event surface

The engine emits structured events to the host via an in-process broadcast
(the `broadcast emitter` in section 3.4). Event kinds include:

- `transcript_appended` (per token / per message).
- `tool_call_started` / `tool_call_finished`.
- `approval_requested` / `approval_resolved`.
- `run_dispatched` / `run_terminated`.
- `mcp_status_changed`.
- `daemon_disconnected` / `daemon_reconnected`.

The host renders whichever events its capability bits permit.

### 6.7 Wire-version compatibility

The engine and daemon negotiate a single `wire_version` SemVer. The
compatibility rule is:

- Same major: compatible.
- Different major: refuse (hello-rej).
- Engine minor > daemon minor: engine MUST adapt.
- Daemon minor > engine minor: daemon MUST adapt.

PRs that bump the major version MUST update this section and add an
acceptance criterion in section 12 demonstrating cross-version refusal.

---

## 7. Sandbox / permission boundary

This section is normative for which process holds authority over each
capability the agent might exercise. The policy (the 9-step evaluate
pipeline) lives in `spec-m-permissions.md`; this section fixes the
process boundaries.

### 7.0 Capability inventory

| Capability             | Authority process | Rationale                                                                 |
|------------------------|-------------------|---------------------------------------------------------------------------|
| Filesystem read        | engine            | reads are deterministic under the per-thread allow-list                   |
| Filesystem write       | engine            | write authority is per-thread by design (multi-repo workspace)            |
| Shell exec             | engine            | shell exec policy is per-workflow; engine has the workflow context        |
| Network egress (tools) | engine            | tool-driven egress is bounded by workflow grants                          |
| Network egress (model) | agent             | the agent already holds the vendor token (Z-11)                           |
| MCP tool dispatch      | engine            | the engine spawned the MCP server (section 3.7)                           |
| Approval rendering     | host              | host owns the UI surface (section 6)                                      |
| Run dispatch           | daemon            | run identity (run_id) is daemon-owned                                     |
| Cross-run state        | daemon            | retry maps, autonomy budgets, snapshot fan-out                            |
| User secrets (model)   | agent             | secret blast-radius is one process: AgentProcess                          |
| User secrets (other)   | host              | the host's own keychain (Zed settings, shell env)                         |

### 7.1 The 9-step pipeline runs in the engine

The 9-step evaluate pipeline (tenant-deny -> tenant-forcePrompt ->
reads-always -> server-disabled -> auto-approve -> patterns ->
background-deny -> prompt -> final-decision) runs **inside the engine**.

The engine consults per-thread state (workflow grant, per-tenant
overrides, MemoryBlocks-derived context). The daemon does NOT reimplement
the pipeline; when an approval flows up via C-3, the daemon forwards it
to the originating engine via C-2 (Z-13b), which runs the pipeline and
either:

- auto-resolves and replies down C-2 -> C-3, or
- decides to prompt and asks the daemon's approval-broker (C-9) to route
  a prompt to a host capable of rendering.

### 7.2 Approval-card surface

The approval card is a structured record with at minimum:

- the originating run_id,
- the proposed action (read N bytes / write N bytes / shell-exec / tool-call),
- a human-readable summary,
- a tenant identifier,
- the suggested decision (allow / deny / forcePrompt),
- a free-form rationale.

The host renders the card per its capability bits (section 6.1). The
user's reply is encoded as `{ verdict: allow|deny, scope: once|session|forever }`
and flows back via C-9.

### 7.3 Runner is not authority for any user-visible permission

The runner sees the agent's tool requests but holds **no permission
authority**. A request to write `/etc/hosts` is forwarded up the wire;
the runner does not pre-filter. Pre-filtering at the runner is explicitly
forbidden because:

1. the runner has no per-thread context (workflows, MemoryBlocks,
   per-tenant overrides), and
2. duplicating pre-filtering risks divergence from the canonical
   pipeline.

This is invariant Z-12.

### 7.4 OS-level sandbox

caduceus does **not** apply a syscall-level sandbox to AgentProcess
beyond what the OS already provides. The trust model is:

- The AgentProcess is the vendor's binary; the user installed it; the
  user trusts it to the extent of the vendor's reputation.
- The runner is the boundary that observes and translates; it does not
  sandbox.
- The user's authority gates every privileged tool call via the 9-step
  pipeline.

A future spec MAY add seatbelt / sandbox-exec / AppContainer constraints;
that work is out of scope here (section 13).

### 7.5 Multi-tenant boundary

Because the engine is per-thread and the daemon is per-machine, two
threads in the same host process share the engine's address space.
Per-tenant policy (where tenant means a workspace+repo+identity triple)
is enforced **inside the engine** via the pipeline. The daemon does not
enforce per-tenant policy on its own, but it MUST include a tenant
identifier in every C-3 dispatch so that an audit log can attribute each
tool call (Z-14).

---

## 8. Multi-repo topology

Cross-reference: `spec-multi-repo-workspace-model.md` is the authoritative
source. This section fixes only the process-level ownership.

### 8.1 Workspace path scheme

Per `spec-multi-repo-workspace-model.md`, the on-disk workspace path for
a run is:

```
${workspace_root}/${repo_slug}/${run_id}/
```

The `${workspace_root}` is per-user, configurable, and defaults to:

- macOS:   `~/Library/Caches/com.caduceus/workspaces`
- Linux:   `~/.cache/caduceus/workspaces`
- Windows: `%LOCALAPPDATA%\caduceus\workspaces`

`${repo_slug}` is a deterministic, sanitised projection of the upstream
remote URL (e.g. `github.com_owner_repo`).

`${run_id}` is the daemon-allocated run identifier (UUIDv7).

### 8.2 Daemon owns the workspace registry

The mapping `(repo_slug, run_id) -> workspace_path` is held in the
daemon. When the engine dispatches a run on C-2, the request includes
the `repo_slug` (computed by the host's git logic and passed through the
engine); the daemon allocates the path, creates the directory (0700),
and replies with the absolute path.

The daemon also owns:

- garbage collection of expired workspaces (configurable retention,
  default 30 days after run terminal),
- disk-quota enforcement (default unlimited; admins MAY set a per-user
  cap),
- cross-host shared access: the daemon MAY hand the same workspace path
  to different hosts that re-attach to the same run.

### 8.3 Engine and runner see only the path

The engine receives the absolute workspace path and passes it to the
runner as the runner's cwd (section 5.4). Neither the engine nor the
runner is aware of the `${workspace_root}` parent or the registry; the
abstraction is "a directory the daemon told me to use".

### 8.4 Cross-repo pinning

A run dispatched in the context of one repo MAY pin to a different repo's
workspace if the orchestrator policy allows (rare; documented in
`spec-caduceus-orchestrator-algorithm.md`). The daemon is the authority
for the pin; the engine merely observes.

### 8.5 Symbolic links and cross-volume layout

Workspaces are real directories, not symbolic links (Z-16). Some
distributions place `${workspace_root}` on a different volume (e.g. a
faster SSD, or a tmpfs); this is permitted and transparent to the
engine.

Sub-runs in a DAG (parent run R0, child runs R1..Rn) MAY share
sub-paths under `${workspace_root}/${repo_slug}/R0/.children/`. The
exact layout is owned by the DAG orchestration spec (section 13
OoS-3).

---

## 9. Failure domains

This section enumerates each plausible failure and identifies, for each,
(a) which process notices, (b) what the user-visible effect is, (c) who
recovers, and (d) what guarantees survive.

### 9.0 Failure-domain summary

| Failure                         | Noticed by             | User-visible              | Recovery owner    | Guarantee preserved                                  |
|---------------------------------|------------------------|---------------------------|-------------------|------------------------------------------------------|
| Daemon crashes                  | hosts (C-2 EOF)        | "daemon disconnected"     | OS launcher       | run records preserved via WAL (Z-7, Z-15)            |
| Host crashes                    | daemon (C-2 EOF)       | run keeps running         | none              | runner survives; resumes when host re-attaches       |
| Runner crashes                  | daemon (waitpid)       | RunSnapshot terminal      | daemon (retry)    | retry policy applied; agent reaped                   |
| Agent crashes                   | runner (waitpid)       | RunSnapshot terminal      | runner reports up | run terminal; retry per orchestrator policy          |
| MCP server crashes              | engine                 | "tool unavailable"        | engine (bounded)  | up to N restarts; then surfaced                      |
| LSP crashes                     | host                   | host's affair             | host              | n/a                                                  |
| Cloud peer unreachable          | daemon (heartbeat)     | "unreachable"             | daemon            | run remains in registry; reconciles on reconnect     |
| Network partition (model)       | agent                  | agent surfaces error      | agent             | retry per agent policy; runner relays                |
| Disk full (workspace)           | daemon / runner        | dispatch refused          | admin / GC        | dispatch fails before runner spawn (Z-15a)           |
| OS reboot                       | all                    | new launch                | OS launcher       | WAL replay; in-flight runs marked paused-by-shutdown |

### 9.1 Daemon crashes

**Detection.** Each connected host's C-2 socket signals EOF. The host's
engine surfaces a `daemon_disconnected` event.

**Effect on running runs.** RunnerProcess instances are children of the
daemon. Without the daemon, they are reparented to PID 1 (Linux/macOS)
or become orphans (Windows). Per Z-7, the OS launcher restarts the
daemon; on restart, the daemon performs WAL replay (Z-15) and reconciles
the runner table by:

1. Reading the WAL'd `run_id -> pid` map.
2. Probing `/proc/<pid>` (or platform equivalent) for liveness.
3. For live runners, attempting re-attachment: the daemon does NOT
   re-spawn but does NOT have a usable stdio (the runner's stdio was
   attached to the dead daemon). Therefore the daemon MUST mark such
   re-attached runners as `stranded` and terminate them with a
   documented diagnostic; the orchestrator retry policy decides whether
   to start a fresh run.

**User-visible.** The host shows a banner; the orchestrator status panel
shows runs as `paused-by-daemon-restart`. After daemon restart, the runs
are surfaced as `stranded` and either resumed (fresh runner) or
terminated per policy.

**Guarantee.** Run records, retry counters, and workspace registry
survive (WAL). Per-thread chat state (transcript, MemoryBlocks) is
unaffected because it is engine-local.

### 9.2 Host crashes

**Detection.** The daemon notices C-2 EOF.

**Effect.** None on running runs. RunnerProcess is a daemon child; the
runner keeps running. RunSnapshot pubsub continues; the daemon retains
the snapshots in its bounded buffer (per-subscriber-or-orphan policy:
on host disconnect, the daemon retains the most recent snapshot for each
in-flight run that the host had subscribed to, so a re-attaching host
can catch up).

**Recovery.** The user re-launches the host. The handshake reports
`active_runs`; the host re-subscribes; the engine restores its in-memory
thread state from local persistent storage (host's own storage, e.g.
Zed's local DB) and re-joins its threads to the daemon's runs by
`run_id`.

**Guarantee.** Z-7. Host crash never affects daemon-owned runs.

### 9.3 Runner crashes

**Detection.** Daemon's `waitpid` returns abnormal status.

**Effect.** The agent child becomes an orphan; the OS parent-death
signal SHOULD reap it (Z-9b for MCP servers; the same mechanism is
applied for AgentProcess, see Z-9c). The daemon publishes a terminal
RunSnapshot with cause `runner_crashed`.

**Recovery.** The orchestrator retry policy decides. By default, up to
two restarts on `runner_crashed`. Beyond that the run terminates fatally.

### 9.4 Daemon WAL & recovery

The daemon's WAL captures, append-only, every state-changing
orchestrator event:

- run dispatched (run_id, repo_slug, prompt_hash, cwd, pid).
- run state-transition (run_id, new status, snapshot version).
- run terminal (run_id, exit cause).

On daemon restart, WAL replay yields the run table; live runners are
probed (section 9.1). The WAL is bounded and rotated; rotation order
matters for replay determinism. The WAL spec is owned by a sibling
document (see section 15 cross-references).

### 9.5 Diagnostic logging

stderr from RunnerProcess and from MCP servers is captured by their
respective supervisors (daemon for runner; engine for MCP) and folded
into a per-run log artifact (`${workspace_path}/.caduceus/run.log`).
These artifacts are part of the post-mortem surface.

The daemon's own stderr is captured by the OS launcher
(`~/Library/Logs/caduceus/daemon.log` on macOS; `journald` on Linux;
`Application` event log on Windows).

### 9.6 Failure interaction matrix

The following matrix gives, for each pair of simultaneous failures,
the outcome.

| Concurrent failures              | Outcome                                                                 |
|----------------------------------|-------------------------------------------------------------------------|
| Daemon + host                    | Both restart independently; runner orphaned then OS-reaped; runs lost.  |
| Daemon + runner                  | Runner orphaned; agent reaped; daemon WAL knows the run; on restart marks `stranded`. |
| Host + MCP server                | Host crash reaps MCP server (parent-death); engine and host both gone; daemon unaffected; run continues. |
| Runner + agent                   | Both reaped; daemon notices; retry policy applies.                      |
| Cloud unreachable + daemon       | Daemon restart re-establishes C-7; bounded by remote heartbeat budget.  |

### 9.7 Defence-in-depth: reaper of last resort

On daemon startup, the daemon SHALL run a one-shot orphan sweep that
detects any process whose argv[0] matches the runner binary and whose
parent is PID 1 (i.e. orphaned), and which is not in the WAL's live-run
set. Such orphans are terminated with a documented diagnostic. This
defends against the rare case where the OS parent-death signal failed
to fire.

---

## 10. Network topology

caduceus is **local-first**. The default operating mode requires zero
network reachability beyond what the agent's vendor demands. This
section fixes the network surface and the air-gap policy.

### 10.0 Default mode: local-only

In default mode, every IPC channel except C-7 (cloud-handoff) and C-8
(model provider) is local. C-1 is in-process; C-2/C-3/C-4/C-5/C-6 are
OS pipes or UDS / named pipes that never leave the machine.

C-8 (agent <-> model provider) is the only mandatory outbound
connection, and even that is optional if the user is running a local
model.

### 10.1 Cloud-handoff mode

When a run is delegated via the agent-handoff workflow, C-7 (daemon
<-> cloud peer) becomes active. This is the only caduceus-owned channel
that traverses the public internet.

C-7 traffic includes:

- the original prompt (user-authored content),
- the workspace's git diff or relevant subset,
- inbound RunSnapshot stream,
- approval-broker dispatch round-trips.

C-7 is encrypted with TLS 1.3+; the cloud-host's certificate MUST chain
to a public root or a user-pinned root (config). Bearer tokens are
stored in the OS keychain and never written to disk in plaintext.

### 10.2 Air-gap policy

caduceus MUST be usable on an air-gapped machine, subject to:

- the user has installed an offline-capable agent (e.g. a local
  llama.cpp build), and
- the user has not enabled cloud-handoff workflows.

In this configuration, every channel C-1..C-6 is exercised and no
external connection is opened.

The CI lane MUST include an air-gap test that asserts no syscall opens
a public-internet socket during a representative set of agent runs
(AC-NET-1 in section 12).

### 10.3 Loopback assumptions

caduceus does NOT use loopback TCP for internal IPC. The daemon socket
is UDS or named pipe; runners are stdio. Rationale: avoiding loopback
TCP defends against (a) port-collision with user services and (b) the
macOS firewall popping prompts on every cold start.

### 10.4 Proxy / corporate-MITM compatibility

C-7 and C-8 MUST honour the user's HTTPS proxy environment
(`HTTPS_PROXY`, `NO_PROXY`, system proxy on Windows). The local internal
channels do not need proxy support.

### 10.5 No daemon-to-daemon discovery

Two `caduceusd` instances on the same network do NOT discover each
other. There is no clustering, no leader election, no shared distributed
state. If a future feature requires distributed orchestrators, it MUST
be designed around an explicit broker, not an emergent peer-to-peer
mesh.

### 10.6 IPv4 / IPv6

C-7 and C-8 inherit the OS's default address family. Daemon IPC is not
IP at all. No IP-version constraints.

### 10.7 Telemetry

caduceus does not emit telemetry to external endpoints by default. Any
optional telemetry feature MUST be off by default, opt-in, and
documented; this spec does not enumerate the telemetry endpoints because
none exist in scope today.

---

## 11. Invariants (normative)

This section enumerates the **MUST** rules of the topology. Every PR that
touches a process boundary, an IPC channel, or a lifecycle edge MUST
cite the Z-numbers it preserves or re-affirms. A PR that violates a
Z-number MUST be blocked unless an explicit scope-locked override is
attached.

The Z-numbers are stable identifiers; they MUST NOT be reused or
renumbered. New invariants are appended (Z-N+1, Z-N+2, ...). Retired
invariants are marked `(retired in <PR>)` but their numbers are
preserved.

### Z-1 - Engine never escapes its host process

The caduceus engine library MUST run in-process inside its host. The
engine MUST NOT fork, MUST NOT spawn a daemon-equivalent, MUST NOT
expose a network listener. Rationale: chat state stays inside the host;
the daemon is the only out-of-host process owned by caduceus.

### Z-2 - Single-daemon-per-user

There MUST be at most one `caduceusd` process per user account per
machine, bound to a uid-private UDS / named pipe. A second daemon
attempt MUST fail at bind() time with EADDRINUSE / ERROR_PIPE_BUSY,
and the duplicate MUST exit 0 with a "daemon already running"
diagnostic.

### Z-3 - Hosts MUST NOT fork the daemon

A host MAY ask the OS launcher (launchd / systemd / SCM) to start the
daemon. A host MUST NOT call `fork()` + `exec("caduceusd")` directly
to bring the daemon up. Rationale: the OS launcher is the canonical
supervisor; double-supervision yields zombies and crash loops.

### Z-4 - All durable orchestrator state lives in the daemon

The run registry, retry map, workspace registry, RunSnapshot pubsub
log, and approval-broker dispatch table MUST be daemon-owned. No host
or engine MAY hold authoritative copies of these. Caches for UI
smoothing are permitted; on reconnect, the daemon's state wins.

### Z-5 - Runners are spawned by the daemon, never by the host

RunnerProcess MUST be a direct child of `caduceusd`. Host code MUST
NOT invoke `posix_spawn` / `CreateProcess` for a runner. The engine
dispatches a run via C-2; the daemon spawns the runner. Rationale:
host crash MUST NOT take down a runner, and the only way to ensure
that is for the runner to be a child of a process that survives the
host.

### Z-6 - RunnerProcess holds no permission authority

The runner MUST NOT pre-filter agent tool calls based on permission
policy. Tool calls flow up via C-3; the engine consults the 9-step
pipeline; only the engine's verdict (relayed back through C-3) gates
execution. See section 7.3.

### Z-7 - Daemon survives host crash

A host crash MUST NOT cause the daemon, any RunnerProcess, or any
AgentProcess to terminate. Specifically:

- The daemon MUST detect C-2 EOF and treat it as a subscription
  cancellation, not a run cancellation.
- The daemon MUST NOT propagate any signal from the host to runners.
- Reattachment of a fresh host instance MUST be transparent (handshake
  yields `active_runs`).

### Z-8 - RunSnapshot terminal is durable

The terminal RunSnapshot for a run_id MUST be written to the WAL
before being broadcast on C-10, AND MUST be redelivered to any host
that subscribes after the broadcast (subject to a retention window
documented by the orchestrator-status-snapshot spec).

### Z-9 - MCP servers are children of the engine, not the runner

MCP servers MUST be spawned by the engine in response to a granted
workflow. The runner MUST NOT spawn MCP servers. The agent MAY request
MCP tool dispatch, but the dispatch flows: agent -> runner (C-4) ->
daemon (C-3) -> engine (C-2) -> MCP server (C-5), with the engine being
the only process that holds the MCP server handle.

### Z-9b - MCP servers reaped on engine death

The engine MUST configure parent-death reaping for MCP children
(Linux: `prctl(PR_SET_PDEATHSIG, SIGTERM)`; macOS: a watchdog thread;
Windows: a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`). On
host crash, no MCP server MAY remain alive.

### Z-9c - AgentProcess reaped on runner death

By analogy with Z-9b, agents MUST be reaped on runner death. Runner
SHOULD set the same parent-death mechanism for its agent child.

### Z-10 - Curated runner environment

The runner's environment variables on spawn MUST be the documented
whitelist (see `spec-caduceus-agent-runner-contract.md`). The runner
MUST NOT inherit the daemon's full environment; in particular, secrets
in the daemon's environment MUST NOT leak.

### Z-11 - Vendor secrets only in AgentProcess

Vendor model-provider credentials (API keys, OAuth tokens) MUST be
visible only to the AgentProcess. The runner MUST NOT see them. The
engine MUST NOT see them. The daemon MUST NOT see them. Rationale:
blast-radius minimisation.

### Z-12 - Runner does not sandbox the agent

The runner MUST NOT impose a syscall-level sandbox on the agent beyond
the OS-level sandbox the agent itself implements. Future work MAY
tighten this; that is a scope-locked future spec.

### Z-13 - One host renders an approval prompt

For a given approval prompt, the daemon's approval-broker MUST dispatch
the prompt to **exactly one** host. Multiple hosts MUST NOT receive the
same prompt. Tie-breaking rules: section 6.5.

### Z-13b - Approval pipeline runs in the engine

The 9-step evaluate pipeline MUST run inside the engine that originated
the run. The daemon MUST NOT reimplement the pipeline. On approval
requests, the daemon forwards to the engine via C-2.

### Z-14 - Tool calls are tenant-attributed

Every C-3 dispatch from daemon to runner MUST include a tenant
identifier; every C-9 approval round-trip MUST preserve it. This
enables auditable per-tenant logs.

### Z-15 - WAL durability before user-visible commitment

The daemon MUST persist a run-state transition to the WAL before
acknowledging the transition over C-2 (so a daemon crash mid-ack does
not leave a host believing a state that no longer exists in the daemon).

### Z-15a - Workspace pre-allocation before runner spawn

The daemon MUST create the workspace directory on disk before spawning
the runner. If creation fails (disk full, permission error), the
dispatch MUST fail before any process is created.

### Z-16 - Workspaces are real directories

Workspace paths MUST resolve to real directories, not to symbolic links.
The daemon MUST refuse to use a workspace path whose resolved type is
not a directory.

### Z-17 - Local-only by default

A fresh install MUST NOT open any caduceus-owned outbound network
connection (C-7) until the user explicitly enables a cloud-handoff
workflow. C-8 (agent <-> provider) is the agent's affair; caduceus
neither opens it nor inhibits it.

### Z-18 - Idempotent daemon connect

The host MAY open and close C-2 any number of times within a host
session; the daemon's view of the host is fully reconstructed from the
most recent handshake. The daemon MUST NOT accumulate stale
subscriptions across host disconnects.

### Z-19 - No loopback TCP for internal IPC

Internal IPC channels (C-2 through C-6) MUST NOT use loopback TCP.
UDS / named pipes / OS pipes only.

### Z-20 - Runner cwd is the workspace path

The runner's cwd on spawn MUST be the workspace path returned by the
daemon's workspace registry. This is the only path the runner or the
agent is permitted to assume as a working directory.

### Z-21 - Half-close means "no more requests"

On any JSONL channel (C-2, C-3, C-4 where ACP-shaped), closing the
write half MUST mean "I will send no more requests; please drain
in-flight replies and close your write half." It MUST NOT mean "abort
everything immediately."

### Z-22 - 1 MiB max line on JSONL channels

JSONL channels (C-2, C-3) MUST enforce a 1 MiB max-line. A line longer
than 1 MiB MUST be treated as a fatal protocol error.

### Z-23 - Snapshot ordering per run_id

RunSnapshots for a single run_id MUST be delivered in monotonic version
order on every subscriber's channel. Across run_ids, no ordering
guarantee is offered.

### Z-24 - No daemon-to-daemon discovery

caduceus MUST NOT implement peer-to-peer daemon discovery. Two daemons
on the same network are independent.

### Z-25 - Capability handshake is mandatory

A host MUST advertise `HostCapabilities` on engine init. The engine MUST
forward the relevant subset to the daemon on hello. No run dispatch MAY
occur before hello-ack.

### Z-26 - Backpressure drops only non-terminal snapshots

If the daemon's per-subscriber RunSnapshot queue overflows, the daemon
MUST drop the oldest **non-terminal** snapshot. Terminal snapshots MUST
never be dropped.

### Z-27 - Engine never listens on a network socket

The engine library MUST NOT open a listener on any address family that
is reachable from outside the host process (i.e. neither TCP/UDP nor a
publicly-named socket). The only listeners caduceus operates are: (a)
the daemon's UDS / named pipe, and (b) the cloud-host peer's HTTPS
endpoint, and (c) nothing else.

### Z-28 - Per-thread state never leaves the engine

Transcript, MemoryBlocks, broadcast emitter, and gen cancel token MUST
stay inside the engine's address space. Mirroring of these to the
daemon is forbidden. Rationale: keeps the chat blast-radius small and
the daemon protocol narrow.

### Z-29 - Run dispatch is daemon-allocated

run_id MUST be allocated by the daemon, not the engine. The engine sees
a run_id only in dispatch responses.

### Z-30 - Stop cascade is monotonic

A stop signal applied at any layer (host -> engine -> daemon -> runner
-> agent) MUST monotonically progress: once a layer has been told to
stop, it MUST NOT spawn new work in subsequent ACP messages.

### Z-31 - Snapshot subscriptions are connection-scoped

A subscription created on a C-2 connection MUST NOT outlive that
connection. On C-2 close, the daemon drops all subscriptions for the
peer. (Idempotency Z-18 means reconnecting hosts re-subscribe.)

### Z-32 - Runner stderr never reaches hosts directly

stderr from RunnerProcess MUST be consumed by the daemon and folded
into the per-run log artifact. Hosts MUST NOT receive runner stderr
verbatim. Rationale: hosts may be stale; the run's authoritative log
is the artifact.

### Z-33 - No silent restart of terminal runs

Once a run is in a terminal state (success / fail / cancel), the
daemon MUST NOT silently restart it. A new dispatch from the engine
yields a new run_id.

### Z-34 - Workspace path is absolute

The path returned to the engine on dispatch MUST be absolute and
fully resolved (no `..`, no `~`, no environment variable expansion
required by the consumer).

### Z-35 - Daemon refuses connections from a different uid

The daemon's accept loop MUST verify peer credentials and refuse any
connection whose uid does not match the daemon's own uid.

---

## 12. Acceptance criteria

This section enumerates the testable conformance claims. Each AC-N MUST
have at least one passing automated test in a CI lane that is required
on the release branch.

### AC-PROC-1 - Process inventory

A representative end-to-end run on a fresh install MUST produce a
process tree exactly matching the §3 inventory (modulo lifetime: at the
moment the run terminates, agent and runner are both reaped; daemon and
host remain).

Test: spawn a host, dispatch a run, snapshot `ps -ef --forest` (or
platform equivalent) at three points (T0=before dispatch, T1=mid-run,
T2=after terminal), and assert the topology.

### AC-PROC-2 - Single daemon

Two hosts started in parallel MUST share exactly one daemon. Test:
launch two hosts; assert exactly one `caduceusd` PID across both.

### AC-IPC-1 - No internal loopback TCP

Run a representative workload; assert no listening TCP port belongs to
caduceus binaries (parses `lsof -nP` / `ss` / `netstat`).

### AC-IPC-2 - Daemon socket peer-cred enforcement

Connect to the daemon socket from a different uid; assert connection is
refused.

### AC-IPC-3 - 1 MiB JSONL guard

Send a 2 MiB single-line JSON message on C-2; assert daemon closes the
connection with a documented error.

### AC-LIFE-1 - Bootstrap order

Fresh install -> launch host -> first run; assert daemon is auto-started
via OS launcher and not via a host-side fork (test inspects parent PID
of `caduceusd`; MUST be PID 1 on Unix or the SCM controller on Windows,
not the host's PID).

### AC-LIFE-2 - Host crash does not reap runner

Mid-run, terminate the host with SIGKILL. Assert: runner PID still
alive; agent PID still alive; daemon PID still alive; run continues to
completion; replacement host re-attaches and sees the terminal
RunSnapshot.

### AC-LIFE-3 - Daemon crash recovery

Mid-run, terminate the daemon with SIGKILL. Assert: OS launcher
restarts the daemon within the documented backoff; the daemon performs
WAL replay; the orphaned runner is detected as `stranded` and either
restarted or terminated per policy; the host's snapshot stream resumes.

### AC-LIFE-4 - MCP server reaped on host crash

Spawn an MCP server (mock); host crash; assert MCP server is reaped
within 1 second.

### AC-CAP-1 - Capability degradation

Run a host that advertises `!can_render_markdown`; assert engine emits
plaintext rendering on C-1.

### AC-CAP-2 - Approval routing across hosts

Two hosts connected; one with `can_render_approval_locally=false`;
assert approval prompts are routed only to the capable host.

### AC-PERM-1 - Pipeline runs in engine

Mock daemon; assert the daemon does not call into any permission-policy
path on its own (verified by code search and runtime tracer in CI).

### AC-PERM-2 - Vendor secret containment

Inspect the daemon's and the runner's environment via `/proc/<pid>/environ`
during a representative run; assert vendor secret env-vars are NOT
present. They MAY be present only in AgentProcess.

### AC-MR-1 - Workspace path is absolute and unique

Dispatch two runs; assert each workspace path is absolute, unique, and
under `${workspace_root}/<repo_slug>/<run_id>/`.

### AC-FAIL-1 - Snapshot terminal durability

Mid-run, mark the run as terminal; assert the terminal snapshot is
persisted to the WAL **before** broadcast on C-10 (verify by killing
the daemon between persist and broadcast and confirming the post-restart
snapshot is still terminal, not replayed as in-flight).

### AC-FAIL-2 - Backpressure drops only non-terminal

Connect a slow subscriber; flood snapshots; assert dropped snapshots
counter increments and final terminal snapshot is delivered.

### AC-NET-1 - Air-gap conformance

Run a representative workload with a local model and the network
disabled; assert the workload completes; assert no caduceus-owned
syscall opened a public-internet socket (CI uses an `iptables`-based
deny-all egress rule).

### AC-NET-2 - Cloud handoff opt-in

Fresh install with default config; dispatch a run; assert no outbound
HTTPS connection from `caduceusd`. Then enable cloud-handoff workflow;
dispatch a delegated run; assert the connection is opened.

### AC-INV-1 - Z-numbered invariant grep

CI lints every PR's commit message body for `Z-N` citations when the
diff touches files matching the topology paths. PRs that touch process
boundaries without a Z-N citation are flagged for human review.

### AC-INV-2 - Z-2 enforcement

Launch two `caduceusd` processes in parallel; assert the second exits
with the documented "daemon already running" message and a 0 exit code.

### AC-INV-3 - Z-19 enforcement

Bind any internal channel to TCP loopback in a test build; assert CI
fails (a static lint catches `tcp::Listener` use in the daemon /
engine / runner crates).

### AC-INV-4 - Z-27 enforcement

Static + runtime check: assert no listener exists in the host process
beyond the in-process broadcast channel.

### AC-INV-5 - Z-9b/c enforcement

On Linux, assert `prctl(PR_SET_PDEATHSIG, ...)` is called by the
runner before exec of agent and by the engine before exec of MCP
server (eBPF or strace-based assertion).

### AC-INV-6 - Z-30 stop-cascade monotonicity

Dispatch a run; cancel mid-stream; assert no further tool calls are
emitted by the agent after the cancel ack flows back from the runner.

---

## 13. Out of scope

The following are explicitly out of scope for this spec. Each item names
the spec that owns it (see section 15 for the cross-reference list), or
is flagged as future work.

- **OoS-1.** The exact RunSnapshot schema (field-by-field), retention
  policy, and replay semantics. Owned by
  `spec-orchestrator-status-snapshot.md`.
- **OoS-2.** The 9-step permission evaluate pipeline's per-step
  semantics, allow-list/deny-list grammars, and the
  Adapter/ApprovalBroker/PermissionCardManager three-layer architecture.
  Owned by `spec-m-permissions.md`.
- **OoS-3.** DAG sub-run topology (parent run R0 spawning child runs
  R1..Rn), F7b autonomy budget enforcement, F7c spawn-count threshold,
  PB6 self-pause semantics. This spec only fixes that **the daemon** is
  the supervisor of any runner involved (whether top-level or DAG
  child); the rest is owned by the eventual P-tier successor of
  `dag-orchestration-design.md`.
- **OoS-4.** The wire format of agent <-> runner messages (envelope
  schema, content-type policy, capability bit list at the ACP layer).
  Owned by `spec-caduceus-agent-runner-contract.md`.
- **OoS-5.** Per-thread session state machine (idle, awaiting-input,
  awaiting-tool, running, stopped). Owned by
  `spec-m-session-lifecycle.md`.
- **OoS-6.** Multi-repo workspace semantics beyond process ownership
  (slug derivation, workspace lifecycle policies, GC heuristics). Owned
  by `spec-multi-repo-workspace-model.md`.
- **OoS-7.** Cloud-host peer protocol details (HTTP/2 vs SSE, retry
  semantics, signed-handle issuance algorithm). Owned by a future
  `spec-cloud-host.md`.
- **OoS-8.** OS-level sandbox tightening for AgentProcess
  (seatbelt / sandbox-exec / AppContainer / seccomp / Landlock).
  Future scope-locked spec; not addressed here.
- **OoS-9.** UI surface design (panel layout, status indicator
  semantics, approval card visual design). Host-owned.
- **OoS-10.** Build, packaging, signing, and notarisation of the
  caduceus binaries. Owned by the release-engineering documentation.
- **OoS-11.** Telemetry endpoint enumeration. There are none in scope
  today (Z-17, section 10.7).
- **OoS-12.** WAL on-disk format and rotation policy. Owned by a
  separate `spec-caduceusd-wal.md` (planned).
- **OoS-13.** Per-vendor model client compatibility shims (claude-code,
  codex, goldeneye-cli). The runner relies on each agent honouring
  ACP at its stdio; the per-vendor adaptation is owned by per-vendor
  shims documented elsewhere.
- **OoS-14.** Web embedding (running caduceus inside a browser
  process via WASM). Future work; out of scope.
- **OoS-15.** Multi-machine team workspaces (a "shared" daemon owned
  by a team rather than a user). Out of scope; would require an
  explicit broker per Z-24.

---

## 14. Open questions

These are items the spec deliberately does not resolve; they are
tracked for follow-up. PRs that resolve them MUST update this spec and
add or modify Z-numbers and AC-numbers as required.

- **OQ-1.** Should `caduceusd` expose a richer admin API (e.g. for an
  ops dashboard) over a separate authenticated channel, or are the
  current C-2 control RPCs sufficient for all admin needs?
- **OQ-2.** What is the exact retention window for terminal snapshots
  (Z-8) before they are GC'd from the WAL? Today the cross-ref spec
  says "configurable, default 14 days"; this should be ratified here
  to anchor AC-FAIL-1's longevity test.
- **OQ-3.** On platforms where `PR_SET_PDEATHSIG` is unreliable (some
  containerised Linux distributions; Windows without Job Objects), is
  the orphan-sweep on daemon startup (section 9.7) sufficient, or do
  we need a periodic sweep?
- **OQ-4.** Should the engine treat C-2 disconnect as transient (keep
  retrying for N minutes) or terminal (surface as fatal immediately)?
  Current default: transient with 60s exponential backoff; should be
  promoted to a Z invariant.
- **OQ-5.** What is the grace period for a "stranded" runner detected
  on daemon restart (section 9.1) before forced termination? Today
  default is "immediate"; some runs may benefit from a re-attach
  attempt via a sidecar control fifo.
- **OQ-6.** Should `caduceusd` support multiple concurrent versions
  (i.e. `caduceusd-v1` + `caduceusd-v2` co-resident, talking to
  hosts of different generations) for staged rollout, or is the
  Z-2 single-daemon-per-user rule absolute? Likely absolute, but
  this should be ratified.
- **OQ-7.** Per-vendor agents differ in their reaction to `stdin` EOF.
  Some treat it as "drain and exit"; others treat it as "panic and
  exit non-zero". The runner's stop cascade (section 4.3) currently
  assumes "drain and exit"; vendors that don't honour this need a
  documented pre-EOF "exit" message.
- **OQ-8.** When two hosts both have `can_render_approval_locally=true`
  and the run was dispatched by H1, but H1 is currently iconified /
  backgrounded while H2 is foregrounded, should the broker still route
  to H1, or to whichever host is "user-attended"? Currently we route
  to H1 (origin); UX research may move us to H2.

---

## 15. Cross-references

Sibling specs cited above. (Some are in this repo; some are in a
sibling repo. The `repo:` prefix is omitted when in the same repo as
this spec.)

- `spec-caduceus-orchestrator-algorithm.md` - run dispatch, retry
  policy, autonomy budget. Cited from sections 1, 5, 8, 13.
- `spec-caduceus-agent-runner-contract.md` - ACP wire shape, runner
  environment whitelist, stop cascade. Cited from sections 2, 4, 5,
  11.
- `spec-m-permissions.md` - 9-step evaluate pipeline. Cited from
  sections 1, 7, 13.
- `spec-m-session-lifecycle.md` - per-thread state machine. Cited
  from sections 1, 13.
- `spec-orchestrator-status-snapshot.md` - RunSnapshot schema and
  pubsub semantics. Cited from sections 4, 11, 13.
- `spec-multi-repo-workspace-model.md` - workspace path scheme,
  registry semantics. Cited from sections 5, 8, 13.
- `dag-orchestration-design.md` (pre-P-tier) - DAG sub-runs, F7b/F7c,
  PB6. Cited from sections 13.
- `m-e2e-architecture.md` (cleanroom-derived source notes) - process
  topology baseline; this spec is the canonical successor.
- `symphony-orch-collab.md` - collab/agent expose surface; relevant
  to Z-1 (engine never escapes host).
- `symphony-fit-analysis.md` - 12-dimension fit analysis; relevant
  to portability cross-checks.
- `native-loop-design.md` - per-session vs per-turn harness lifetime;
  informs section 3.4 (engine ownership of `CaduceusSessionState`).

External references:

- RFC 2119 - keyword conventions.
- The Apache 2.0 license, for material derived from upstream
  Symphony / zed-industries/zed.
- The Model Context Protocol specification, for C-5.
- The Language Server Protocol specification, for C-6.

---

## Appendix A - Glossary

The full glossary expands and disambiguates every term used in this
spec. Terms are alphabetised; cross-references to the section that
introduces the term are in parentheses.

- **ACP** - Agent Coding Protocol. The line-oriented JSONL wire format
  used over C-3 and (by translation) often over C-4. 1 MiB max line.
  No multi-line records. Half-close means "no more requests" (Z-21).
  Wire shape owned by `spec-caduceus-agent-runner-contract.md`.
  (section 2, section 4.3)

- **AgentProcess** - The vendor-shipped model client subprocess.
  Holds vendor secrets (Z-11). Speaks the vendor's wire to the model
  provider over C-8. Reaped by RunnerProcess. (section 3.6)

- **air-gap** - The default-supported deployment mode where the
  machine has no public-internet egress. caduceus MUST be usable in
  this mode given a local agent (Z-17, section 10.2, AC-NET-1).

- **approval-broker** - The dispatch component inside `caduceusd`
  that picks which host renders an approval prompt. (sections 4.9,
  6.5, 7.2)

- **approval-card** - The structured record describing a proposed
  privileged action that requires user consent. Rendered by the host
  per its capability bits. (section 7.2)

- **autonomy budget** - A per-run resource counter governing how
  long an unattended run is allowed to continue. Owned by the
  orchestrator algorithm spec. Mentioned here only insofar as the
  daemon is the holder of the counter. (section 4, section 13 OoS-3)

- **broadcast emitter** - The per-thread channel inside the engine on
  which token deltas, tool-call events, etc. are published to the
  host. Stays in the engine's address space (Z-28). (section 3.4)

- **`caduceusd`** - The long-lived per-user daemon binary that owns
  durable orchestrator state. Single instance per uid (Z-2).
  (section 3.2)

- **caduceus engine** - The in-process library each host links. Owns
  per-thread chat state. Never escapes the host (Z-1). (section 3.4)

- **caduceus-cli** - The CLI host. Headless. Used for scripting, CI,
  and air-gapped testing. (sections 2, 3.3)

- **caduceus-cloud-host** - The (planned) cloud peer that runs a run
  on behalf of a delegating local daemon. (sections 3.9, 4.7, 10.1)

- **caduceus-zed** - The Zed-fork GUI host. Multi-platform native app.
  Reference host implementation. (sections 2, 3.3)

- **C-N** - Stable identifier for an IPC channel as catalogued in
  section 4. C-1 through C-10 are normative; new channels are
  appended. (section 4.0)

- **C-hybrid** - The locked-in topology decision: orchestrator state
  out of process (in `caduceusd`); chat state in process (in the
  engine); joined on `run_id`. (Header)

- **cloud-handoff** - The optional workflow that delegates a run to
  a remote `caduceus-cloud-host`. Off by default (Z-17). (sections
  3.9, 4.7, 10.1)

- **control RPC** - A daemon-bound request/response over C-2; a
  sub-protocol multiplexed alongside snapshot pubsub. (section 4.2)

- **dispatch** - The act of starting a run. Allocated by the daemon
  (Z-29). (sections 3.2, 5.4)

- **DAG sub-run** - A run spawned as a child of another run. Daemon
  is the supervisor (section 3.2); semantics owned elsewhere (OoS-3).

- **engine** - See "caduceus engine".

- **gen-bound cancel token** - The per-generation cancellation
  handle held by the engine; ensures a new turn cleanly cancels the
  previous turn's stragglers. (section 3.4)

- **handshake** - The hello / hello-ack exchange on C-2 that
  establishes wire-version and host-capabilities (section 6.2).

- **host** - Any process that links the engine. (section 3.3)

- **`HostCapabilities`** - The struct a host advertises on engine
  init (section 6.1).

- **hello / hello-ack / hello-rej** - The three-message handshake on
  C-2. (section 6.2)

- **invariant** - A normative MUST rule numbered Z-N. (section 11)

- **JSONL** - JSON-lines; one JSON object per line, LF-delimited,
  UTF-8. (sections 2, 4.2)

- **launchd / systemd / SCM** - The OS launchers that `caduceusd`
  registers with. (section 5.1)

- **LSP** - Language server. Host-owned. caduceus does not spawn
  LSPs. (sections 2, 3.8, 4.6)

- **MCP** - Model Context Protocol. The wire format spoken to the
  engine's MCP server children. (sections 2, 3.7, 4.5)

- **MemoryBlocks** - Structured long-lived per-thread context held
  inside the engine. Stays in the engine (Z-28). (section 3.4)

- **multi-host** - Two or more host processes connected to the same
  daemon. Permitted; approvals routed to one (Z-13). (section 6.5)

- **named pipe** - The Windows analogue of a Unix-domain socket;
  used for the daemon socket on Windows. (section 4.2)

- **non-terminal snapshot** - A `RunSnapshot` whose status is not a
  terminal status. May be dropped under backpressure (Z-26). (section
  4.10)

- **orphan sweep** - The one-shot reaper that the daemon runs on
  startup to detect runner processes whose parent is PID 1.
  (section 9.7)

- **out of scope** - See section 13.

- **parent-death signal** - The OS mechanism (PR_SET_PDEATHSIG on
  Linux; equivalent on macOS/Windows) that ensures a child is
  reaped when its parent dies. (Z-9b, Z-9c)

- **per-tenant** - The per-(workspace, repo, identity) policy axis
  enforced inside the engine. (section 7.5)

- **permission pipeline** - The 9-step evaluate pipeline. Lives
  inside the engine (Z-13b). (section 7.1)

- **`prctl(PR_SET_PDEATHSIG)`** - Linux-specific syscall for parent-
  death signal. (Z-9b)

- **proto_version** - The SemVer of the host <-> engine in-process
  API. (section 6.1)

- **pubsub** - The publish/subscribe pattern used for `RunSnapshot`
  fan-out on C-10. (section 4.10)

- **reaper of last resort** - The orphan-sweep mechanism on daemon
  startup (section 9.7).

- **`repo_slug`** - Sanitised projection of the upstream remote URL
  used as a directory component in workspace paths. (section 8.1)

- **retry map** - Per-run, per-failure-class counters held by the
  daemon. Owned by orchestrator algorithm spec. (section 3.2)

- **`run_id`** - The globally-unique identifier of a run. Allocated
  by the daemon (Z-29). UUIDv7. (sections 2, 8.1)

- **run record** - The persistent on-WAL representation of a run.
  (section 9.4)

- **run registry** - The in-memory plus on-disk mapping `run_id ->
  RunRecord` held by the daemon. (sections 3.2, 9.4)

- **`RunSnapshot`** - The immutable, versioned status object
  published per state-changing event in a run's lifecycle. Schema
  owned elsewhere (OoS-1). (sections 2, 4.10)

- **RunnerProcess** - The subprocess that owns the agent's stdio,
  ACP framing, signal cascade, and stderr separation. Daemon-spawned
  (Z-5). (section 3.5)

- **scope-locked** - A spec-level decision that requires an explicit
  override document to change. (Header)

- **sandbox** - The OS-level isolation a process imposes on itself.
  caduceus does not impose its own (Z-12, section 7.4).

- **session_id** - An opaque identifier assigned at handshake for
  log correlation. (section 6.2)

- **`SO_PEERCRED` / `LOCAL_PEERCRED`** - Linux/macOS APIs for reading
  the uid of the peer of a Unix-domain socket. Used by the daemon to
  enforce Z-35. (section 4.2)

- **status snapshot** - See `RunSnapshot`.

- **stop cascade** - The monotonic stop sequence (host -> engine ->
  daemon -> runner -> agent). (Z-30, sections 4.3, 5.5)

- **stranded** - The daemon's term for a runner that survives a
  daemon crash but no longer has usable stdio. (section 9.1)

- **subscriber** - A C-2 peer that has called `subscribe_run_snapshots`.
  (section 4.10)

- **TCB** - Trusted Computing Base; here, the host process is part of
  the user's TCB by construction. (section 4.1)

- **tenant** - A `(workspace, repo, identity)` triple used as a
  policy key. (section 7.5)

- **terminal RunSnapshot** - A `RunSnapshot` whose status is one of
  `success`, `fail`, `cancel`, `budget_exhausted`, `runner_crashed`,
  `unreachable`, `paused-by-shutdown`. Never dropped (Z-26).
  (sections 4.10, 9.0)

- **thread** - The unit of chat. Engine-owned. Joined to runs by
  `run_id`. (section 2)

- **transcript** - The append-only ordered log of user/assistant/tool
  messages on a thread. Engine-local (Z-28). (section 3.4)

- **UUIDv7** - Time-ordered UUID variant used for `run_id`.
  (section 8.1)

- **wire_version** - SemVer negotiated on C-2 handshake.
  (section 6.7)

- **WAL** - Write-Ahead Log; the daemon's on-disk durability surface.
  Format owned elsewhere (OoS-12). (sections 3.2, 9.4)

- **workspace path** - The absolute on-disk directory at
  `${workspace_root}/${repo_slug}/${run_id}/`. (section 8.1, Z-20,
  Z-34)

- **workspace registry** - Daemon-owned map `(repo_slug, run_id) ->
  workspace_path`. (sections 3.2, 8.2)

- **workflow** - The per-MCP grant unit. (sections 2, 3.7)

- **Z-N** - Stable identifier for an invariant (section 11). Never
  reused, never renumbered.

- **zero-trust** - Not used by caduceus's local IPC (the OS uid
  boundary is the trust boundary). Used only on C-7 (cloud peer)
  and C-8 (model provider).

---

## Appendix B - Topology decision history

The locked-in C-hybrid decision was reached after considering four
alternatives. They are summarised here for context; this is
non-normative.

- **A. Single-process.** Engine + orchestrator + runner all in the
  host process. Rejected because host crash takes down all in-flight
  runs, violating the "host crash MUST NOT lose runs" requirement
  (now Z-7).

- **B. Two-process (engine + daemon, runner inside daemon).**
  Considered; rejected because the daemon would have to demultiplex
  per-thread chat state (transcript / MemoryBlocks) across many
  threads, growing its protocol surface unboundedly. Keeping chat
  state in the engine (where it is naturally local) keeps the daemon
  protocol narrow.

- **C-hybrid (chosen).** Engine owns per-thread chat state in the
  host; daemon owns durable orchestrator state out-of-host; runners
  are daemon children. Joined on `run_id`. Selected because it is
  the minimal split that preserves Z-1, Z-4, Z-7, and Z-28
  simultaneously.

- **D. Distributed (multiple daemons, peer-discovered).** Rejected
  via Z-24. Future feature would require an explicit broker.

---

## Appendix C - Conformance checklist for reviewers

A reviewer assessing a PR that touches process boundaries SHOULD
verify, in order:

1. Does the PR modify any C-N channel? If so, are framing, encoding,
   transport, and auth still consistent with section 4?
2. Does the PR add or remove any process kind? If so, is the
   inventory in section 3 updated, the diagram regenerated, and the
   supervisor matrix in section 5 amended?
3. Does the PR change a lifecycle edge? If so, does it cite the
   relevant Z-numbers in the commit body?
4. Does the PR introduce a new capability bit? If so, is it added to
   `HostCapabilities` (section 6.1) and is the degradation rule in
   section 6.3 specified?
5. Does the PR introduce a new failure mode? If so, is it added to
   the matrix in section 9.0?
6. Does the PR open a new outbound network connection? If so, does
   it preserve Z-17 (off-by-default)?
7. Does the PR add a new acceptance criterion or modify an existing
   one? If so, is the corresponding test added to the required CI
   lane?

Reviewers MAY consult the cross-references in section 15 for
context.

---

## Appendix D - Notation

- **RFC 2119 keywords** in **bold** when a sentence's normative
  intent could otherwise be ambiguous.
- **Process names** in plain monospace (`caduceusd`, `caduceus-zed`).
- **Channel identifiers** as `C-N`.
- **Invariant identifiers** as `Z-N`.
- **Acceptance criterion identifiers** as `AC-CATEGORY-N`.
- **Out-of-scope identifiers** as `OoS-N`.
- **Open-question identifiers** as `OQ-N`.
- **Cross-references** as `<spec-name>.md` (paths assumed sibling
  unless prefixed `repo:other/...`).

---

*End of spec-system-topology.md.*
