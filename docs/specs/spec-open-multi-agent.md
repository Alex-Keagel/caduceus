# Behavioral Specification: open-multi-agent (Clean-Room)

## Provenance
- Source repository: `/Users/alexkeagel/caduceus-reference/open-multi-agent`
- Source commit: `f1c7477a262fc7a029b0db7e370eab9a901851f5`
- Scope analyzed: all TypeScript files under `src/` and `examples/` (50 files total)
- Clean-room statement: this document describes observable behavior, contracts, and runtime semantics only. No copyrightable source expression is carried forward.

---

## 0) System Model Overview

This framework is a coordinator-driven multi-agent runtime with:
1. A top-level orchestrator API (single-agent, explicit-task, and auto-decomposed team modes)
2. Stateful agents running iterative model/tool conversation loops
3. A dependency-aware task queue with failure/skip propagation and retries
4. Team collaboration primitives (message bus + shared memory)
5. A provider-agnostic LLM adapter layer
6. Tool registration, validation, filtering, and concurrent execution
7. Multi-layer concurrency control and trace instrumentation

The system is event-rich, fault-tolerant by default (favoring error results over thrown failures), and optimized for progressive completion even when decomposition/parsing fails.

---

## 1) Types and Contracts (`src/types.ts`)

### Message/content contract
- Conversation messages are role-tagged (`user` or `assistant`) and carry ordered content blocks.
- Content blocks support four modalities:
  - plain text
  - model-requested tool invocation (name + structured input + per-call id)
  - tool result (referencing invocation id, string payload, optional error marker)
  - inline image (base64 + MIME)

### LLM response contract
- Unified provider response shape:
  - provider response id
  - normalized content blocks
  - model id used
  - normalized stop reason
  - token usage pair (input/output)

### Stream event contract
- Stream emits typed events including incremental text, tool request/result events, loop alerts, budget alerts, terminal completion, and terminal error.
- Terminal invariant: stream ends with exactly one terminal event (success or error).

### Tool contract
- A tool definition has: name, description, input schema, async execute function.
- Execution context carries:
  - invoking agent identity and role metadata
  - optional team context (team name, roster, shared memory reference)
  - cancellation primitives
  - optional working directory hint
  - arbitrary metadata
- Tool result is string output + error boolean (never non-string payload).

### Agent configuration contract
- Includes model/provider settings, prompt policy, tool controls, turn/token ceilings, timeout, sampling controls, loop detection settings, optional structured-output schema, and lifecycle hooks.
- Supports allow/deny lists and named tool presets.

### Result contracts
- Agent result includes success flag, final output text, all messages produced during run, aggregate token usage, tool call records (name/input/output/duration), optional structured output, and loop/budget flags.
- Team result includes overall success, per-agent results map, and total token usage.
- Task result surface includes status transitions, outputs/errors, dependencies, retry metadata, and timestamps.

### Trace contract
- Span categories: model call, tool call, task execution, agent execution.
- Shared span fields: run id, timing (start/end/duration), actor identity, optional task linkage.

---

## 2) Orchestrator (`src/orchestrator/`)

### Top-level API modes

#### A. `runAgent()` (single agent)
Behavior:
- Creates an ephemeral agent runtime with a fresh tool registry/executor.
- Registers built-in tools.
- Resolves effective token budget as minimum of orchestrator-level and agent-level limits when both exist.
- Emits progress events for start/complete, and budget event when exceeded.
- Runs one objective and returns agent result directly.

Inputs:
- agent config
- user goal/prompt
- optional run options (abort, tracing)

Outputs:
- one agent run result with accumulated usage/tool records

#### B. `runTasks()` (explicit task DAG)
Behavior:
- Skips decomposition.
- Accepts explicit tasks (with optional assignees and dependency references).
- Creates queue, resolves dependencies, auto-assigns unassigned tasks via scheduler.
- Executes tasks via agent pool with retry/backoff and cascade semantics.
- Returns aggregate team run result (no synthesis step).

Inputs:
- team
- task list

Outputs:
- team-level success + per-agent aggregated results + total tokens

#### C. `runTeam()` (coordinator pattern)
Behavioral phases:
1. **Simple-goal short-circuit**: if objective appears simple and roster exists, bypass coordinator and route directly to best-matching specialist.
2. **Decomposition phase**: coordinator agent is created with roster/context + required structured task format and asked to produce task plan.
3. **Task extraction/parsing**: parse structured task list; validate minimal fields; resolve assignees/dependencies.
4. **Fallback if parse fails**: synthesize one task per agent using original goal.
5. **Queue execution**: run dependency-aware task pipeline.
6. **Synthesis phase**: coordinator aggregates completed/failed/skipped outcomes + shared memory summary into final response.

Budget guardrails are enforced between phases and after queue activity.

### Goal decomposition rules
- Decomposition output expected as machine-readable array of task objects with title, description, assignee, and dependency references.
- Dependency references may be by title; resolved to internal IDs in load phase.
- Parsing strategy is resilient (fenced block extraction, then bracket-range fallback).

### Simplicity heuristic
- Uses text-length threshold plus complexity-pattern detection.
- Patterns detect explicit sequencing, multi-step coordination language, parallel directives, and multi-deliverable conjunctions.

### Queue execution loop semantics (orchestrator-side)
Per round:
1. Handle cancellation by skipping pending work.
2. Auto-assign newly pending tasks.
3. Batch pending tasks; execute as parallel batch (pool enforces real concurrency cap).
4. For each task: mark running, validate assignee, build prompt with memory/messages, run with retry, update budget, complete/fail task.
5. If budget exceeded, skip remaining tasks.
6. Optional approval gate: if callback denies/throws, skip remaining tasks.
7. Repeat until no runnable work remains.

### Retry delay function
- Exponential growth with configurable base and multiplier.
- Clamped to an upper bound.
- Attempt indexing ensures first retry waits base delay.

---

## 3) Agent Runtime (`src/agent/`)

### Lifecycle state machine
- States: idle → running → completed | error
- Reset returns to idle and clears stored conversation state.

### Execution entry points
- one-shot run (fresh history)
- persistent multi-turn prompt (appends to retained history)
- streaming run (event iterator)

### Conversation loop behavior
- Iterative turn loop until stop condition:
  - aborted
  - max turns reached
  - token budget exceeded
  - model returns no further tool calls
  - loop-detection policy terminates
- Each turn:
  1. call LLM adapter with current transcript + available tools
  2. append assistant response
  3. emit text deltas
  4. account tokens
  5. detect and process tool-use blocks
  6. execute tool calls in parallel
  7. append tool results as next user message

### Tool dispatch behavior
- Tool availability pipeline:
  1. optional preset reduction
  2. optional allowlist intersection
  3. optional denylist removal
  4. framework safety denylist
- Runtime-added tools bypass static filters.
- Contradictory filtering config warns but does not abort.

### Multi-turn management
- Persistent prompt mode retains transcript across prompts.
- One-shot mode starts with provided initial messages only.
- Role alternation is preserved when adding tool results and structured-output retry feedback.

### Token accumulation
- Per-turn token usage accumulates to run total.
- Structured-output retries merge token usage across attempts.
- Budget checks compare total input+output against limit after each model response.

### Structured output validation + retry
When schema is configured:
1. schema instructions injected into system guidance
2. run normally
3. extract JSON-like payload from output
4. validate against schema
5. on failure: append corrective user feedback describing validation issue
6. rerun once
7. merge messages/tokens/tool logs from both attempts
8. if second validation still fails, mark run unsuccessful

### Hooks
- pre-run hook can rewrite prompt text content
- post-run hook can transform final result
- hook exceptions fail the run

### Loop detection integration
- Signature-based detection on repeated tool-call patterns and repeated text outputs.
- Policy options:
  - warn/inject guidance first, then terminate if repetition persists
  - immediate terminate
  - custom callback returning continue/inject/terminate

---

## 4) Task System (`src/task/`)

### Task model
- Mutable task entity with id, textual definition, status, assignee, dependency list, result/error field, timestamps, retry config.

### Dependency readiness
- Runnable iff status is pending and all dependencies are completed.
- Missing dependency references are treated as unresolved (not runnable).

### Queue behaviors

#### Add
- Newly added task becomes blocked if unresolved dependencies exist, else pending/ready.

#### Complete
- Marks completed.
- Rechecks blocked tasks; auto-unblocks those now satisfied.

#### Fail
- Marks failed.
- Cascades failure recursively to dependent pending/blocked tasks.

#### Skip
- Similar cascade behavior for skip semantics.

#### Skip remaining
- Converts all non-terminal tasks to skipped.

#### Completion detection
- Queue finishes when all tasks terminal (completed/failed/skipped) or empty.

### Validation/topology utilities
- Dependency validator checks unknown refs, self-dependency, cycles.
- Topological ordering algorithm returns orderable subset if cycles prevent full ordering.

### Retry semantics
- Attempts = maxRetries + 1
- Retry triggered by thrown error or explicit unsuccessful result
- Exponential backoff with floor/ceiling safeguards
- Retry callback receives attempt metadata
- Final failure returns last observed failure payload

---

## 5) Team System (`src/team/`)

### Team composition
- Team owns roster, message bus, task queue, optional shared memory.
- Team emits translated task lifecycle events to external listeners.

### Message bus
- Supports directed messages and broadcast.
- Broadcast excludes sender.
- Stores full history.
- Tracks per-recipient read state.
- Provides unread query and pairwise conversation query.
- Subscriber callbacks are synchronous; unsubscribe is idempotent.

### Shared memory binding
- Namespaced key-value storage per agent namespace.
- Metadata records provenance by writer.
- Summarization returns grouped markdown-like digest with truncation of long values.

### Prompt injection for collaboration
When executing a team task, prompt construction includes:
1. task definition
2. shared-memory summary
3. inbound team messages for assignee

After successful completion, task output is persisted into shared memory under task-scoped key in assignee namespace.

---

## 6) Tool System (`src/tool/`)

### Tool definition API
- Factory requires human description + schema + execute function.
- Schema drives runtime validation and LLM-facing JSON-schema projection.

### Registry
- Name-keyed registration with duplicate rejection.
- Supports remove/list/get.
- Tracks runtime-added tools separately from static registrations.

### Schema conversion behavior
- Recursive schema conversion supports primitive, structural, enum, union/intersection, wrapper, nullable/optional/default, tuple/record/object forms.
- Required-field derivation respects optional/default/nullable wrappers.

### Tool executor
- Single-call execute pipeline:
  1. lookup by name
  2. cancellation check
  3. schema validation
  4. second cancellation check
  5. invoke execute function
  6. normalize thrown errors into error results
- Batch execution:
  - parallel dispatch through bounded semaphore
  - returns complete map keyed by call id
  - never throws for per-tool failures

### Built-ins
1. **shell command tool**: executes shell command with timeout/cancellation; nonzero exits become errors; combines stdout/stderr.
2. **file read tool**: paginated read with line numbering and range validation.
3. **file write tool**: create/overwrite with parent-dir creation and size/line reporting.
4. **file edit tool**: literal substring replacement with uniqueness constraint unless replace-all enabled.
5. **search tool**: regex search using fast external utility when present, else recursive internal scan; supports glob filter, result cap, cancellation.

### Tool presets and filtering
- Presets represent fixed capability bundles (read-only, read-write, full shell).
- Filtering is deterministic, layered, and applied before model call.

---

## 7) LLM Adapters (`src/llm/`)

### Adapter interface
- Two methods:
  - non-streaming chat request
  - streaming event iterator
- Inputs include model selection, tools, token/temperature controls, system guidance, cancellation signal.

### Adapter factory
- Chooses provider implementation by provider key.
- Uses lazy module loading so unused provider SDKs are not eagerly loaded.

### Canonical message mapping
- Internal content model is converted to provider-native wire format and back.
- Stop reasons normalized to shared vocabulary.
- Token usage normalized to shared shape.

### Provider-specific behaviors

#### Anthropic-style provider
- Native support for structured content blocks including tool-use and tool-result.
- Streaming reconstructs partial tool-input fragments before emitting final tool-use block.

#### OpenAI-compatible provider
- Maps assistant tool requests to native function/tool call representation.
- Maps tool results to dedicated tool-role messages.
- Includes fallback extraction from plain text when model emits pseudo-tool calls in text.
- In fallback case, stop reason can be rewritten to continue tool loop.

#### Copilot provider
- OpenAI-compatible protocol with provider-specific auth/token exchange and headers.
- Multi-stage auth: explicit token, env tokens, or interactive device flow.
- Session-token refresh serialized to avoid duplicate refresh races.
- Exposes model multiplier metadata utility for usage/cost labeling.

#### Grok provider
- Thin OpenAI-compatible specialization with different default endpoint/credential source.

#### Gemini provider
- Uses provider-specific function-call schema and role mapping.
- Generates local call ids when provider omits them.
- Inference of tool-use stop reason when function calls exist even if provider stop reason indicates ordinary stop.
- Streaming aggregates chunked content/tokens manually.
- Does not use text-based fallback extraction path.

### Text tool-call fallback extractor
- Intended for local/open-source model outputs that embed tool calls as text.
- Supports tagged-call formats and freeform JSON object scanning.
- Validates candidate object shapes and tool-name allowlist before acceptance.
- Handles nested JSON/string-encoded argument fields.

---

## 8) Memory (`src/memory/`)

### Storage abstraction
- Async key-value interface supports get/set/list/delete/clear.
- Default implementation is in-memory map.

### Entry semantics
- Entry holds key, value, metadata, creation time.
- Upsert preserves original creation timestamp.

### Shared memory wrapper
- Adds namespace discipline and provenance metadata.
- Supports list-by-agent and summarized export.

### Injection pattern
- Memory summary is not implicit global context; it is explicitly appended to execution prompts by orchestrator in team/task flows.

---

## 9) Concurrency Model

Three semaphore layers interact:

1. **Tool execution layer**
   - Bounded parallel tool calls per agent turn.
2. **Agent pool layer**
   - Global pool limit controls total concurrent agent runs.
   - Per-agent mutex prevents concurrent reuse of same mutable agent instance.
   - Lock ordering avoids wasting pool slots on same-agent contention.
3. **Orchestrator dispatch layer**
   - Orchestrator may submit many pending tasks concurrently; effective execution rate is shaped by pool semaphore.

Result: high throughput across distinct agents, serialization for same-agent state safety, and bounded tool fan-out per turn.

---

## 10) Scheduler (`src/orchestrator/scheduler.ts`)

Supports four assignment strategies for unassigned tasks:

1. dependency-priority first (default): prioritize tasks unlocking most downstream work
2. round-robin: cyclic assignment across roster
3. least-busy: assign by current in-progress load with intra-batch load simulation
4. capability match: keyword affinity between task text and agent profile text

Auto-assignment updates live queue tasks and skips tasks that changed state since snapshot.

---

## 11) Loop Detection (`src/agent/loop-detector.ts`)

### Detection mechanism
- Maintains sliding windows of normalized fingerprints for:
  - tool invocation sets
  - assistant text outputs
- Fingerprint normalization includes deterministic ordering of tool calls and object keys.
- Detection triggers when tail contains threshold count of identical fingerprints.

### Configurable response
- warning-inject cycle (default): inject caution guidance; terminate on repeated recurrence
- immediate terminate
- callback-driven policy returning continue/inject/terminate

### Streaming observability
- Emits explicit loop-detected event before applying response policy.

---

## 12) Observability (`src/utils/trace.ts` + callsites)

### Trace callback behavior
- Trace callback can be sync or async.
- Any callback failure is swallowed (observability must not break execution).

### Span emission points
- per model request
- per tool invocation
- per task execution (including retries as one span scope)
- per whole agent run

### Span payload
- run id correlation
- timing fields
- actor identity
- event-specific metadata (e.g., token usage, retry count, tool error flag)

---

## 13) Error and Cancellation Semantics

- Tool failures are converted to tool-result errors, not thrown up-stack.
- Agent execution catches exceptions and returns structured failure results.
- Pool parallel execution captures per-task failures into result map.
- Queue failure/skip propagates downstream to dependents.
- Approval callback deny/error causes skip of remaining work.
- Cancellation is polled at orchestration and tool/agent boundaries; behavior is graceful shutdown/skip, not process crash.

---

## 14) Example Programs (`examples/`)

The examples collectively define intended usage patterns:

1. Single-agent run + direct streaming + multi-turn conversation APIs.
2. Team auto-orchestration with specialized roles and progress events.
3. Explicit dependency pipeline (design/implement/test/review pattern).
4. Mixed-provider teams under unified orchestrator API.
5. Provider-specific single-agent run for Copilot endpoint.
6. Local-model interoperability via OpenAI-compatible endpoint override.
7. Fan-out then aggregate workflow using explicit dependencies.
8. Hard local-model test comparing explicit-task mode vs coordinator mode.
9. Structured-output schema enforcement path.
10. Task retry behavior with retry progress events.
11. Trace callback consumption and span logging.
12. Alternate provider team orchestration (Grok).
13. Alternate provider single-agent run (Gemini).
14. Multi-perspective review with coordinated sub-results.
15. Research aggregation workflow (parallel specialists + synthesizer dependency).

Common demonstrated contracts:
- event-driven progress UI integration
- per-agent tool scopes
- optional shared memory for cross-task context transfer
- per-task dependency and retry controls
- provider/base-endpoint configurability

---

## 15) Reimplementation Guidance for Rust (Behavioral)

To preserve behavior in a Rust implementation:
1. Keep a provider-neutral internal message/tool schema and normalize provider I/O at adapter boundaries.
2. Preserve role alternation and tool-result-as-user-message semantics.
3. Implement task queue transitions exactly: blocked→pending auto-unblock, and recursive dependent fail/skip propagation.
4. Preserve retry accounting: token/cost totals include failed attempts.
5. Preserve three-layer concurrency controls (pool, per-agent lock, tool fan-out).
6. Implement coordinator fallback paths (simple-goal bypass and parse-failure fallback) to guarantee progress.
7. Keep observability side-effect-safe (trace failures never fail business logic).
8. Keep loop detector deterministic (stable fingerprint normalization).
9. Maintain strict input-schema validation before tool execution.
10. Keep robust local-model text-to-tool-call extraction for compatibility with non-native tool calling.

