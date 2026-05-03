# Caduceus Behavioral Specification — Repo-Owned Workflow Contract

> **Spec ID.** `spec-repo-owned-workflow-contract`
> **Status.** Draft (Wave A, P1).
> **Depends on.** Independent of `spec-orchestrator-loop` (#1),
> `spec-agent-runner` (#2), `spec-workspace-fs` (#3); may ship after them.
> **Consumed by.** #1 (validation hook + hot-reload), #2 (agent invocation +
> prompt shape), #3 (`workspace.root`, hook lifecycle).

## 0. Header / Provenance

This specification documents the workflow file contract that a `caduceusd`
daemon discovers, validates, and reloads at runtime. The behavioural model is
adapted from Symphony's `WORKFLOW.md` worked example and the elixir reference
implementation at `openai/symphony` commit `58cf97d`.

- **Source attribution.** Symphony source material referenced by this spec —
  including SPEC.md prose, the elixir reference implementation
  (`config.ex`, `agent_runner.ex`, `orchestrator.ex`), and the
  `WORKFLOW.md` worked example — is Copyright (c) 2025 OpenAI, licensed
  under the Apache License, Version 2.0. A copy of the license is
  available at <http://www.apache.org/licenses/LICENSE-2.0>. Pseudocode,
  schema sketches, and design rationale in this document are derivative
  works under the same terms; line-level citations to Symphony files are
  retained verbatim so the derivation is auditable.
- **Cleanroom posture.** This spec lifts behaviour, schema shape, and
  invariants only. No source code, identifier names, error strings,
  comments, or other copyrightable expression are reproduced. Concrete
  Rust types named below are caduceus-side names, not ports of
  Symphony's elixir module names.

The C-hybrid topology is **locked in**: the workflow file lives **in the
repository under analysis**. There is no global / user-level / daemon-level
fallback workflow. `caduceusd` discovers the workflow at workspace creation
time relative to the repo cwd, watches the file for edits, and atomically
swaps the in-memory configuration on change. In-flight runs continue with
the workflow they started with (Symphony's posture, SPEC §11).

---

## 1. Scope

### In scope (this spec is normative for the following)

- The workflow YAML schema (top-level keys, types, defaults, version field).
- The discovery rule: workflow path is repo-relative, single-file, fixed name.
- The `load_workflow` algorithm: read → schema-validate → env interpolate →
  resolve relative paths → fail-fast on missing required fields.
- The `validate_dispatch_config` startup gate (`orchestrator.ex:67`).
- Hot-reload semantics: file-watch trigger, atomic swap, in-flight
  immutability.
- The `build_turn_prompt` first-turn-vs-continuation contract
  (`agent_runner.ex:133–145`) — load-bearing because workflow authors
  ship templates against it.
- The closed placeholder catalog and substitution rules.
- The hook execution model: shell vs direct exec, env merge order,
  cwd-by-lifecycle-point, timeout enforcement.
- `workspace.root` resolution (relative-to-workflow-file vs absolute).
- `agent.command`, `agent.args`, `agent.env` shape.
- `agent.max_concurrent_agents` (the orchestrator's slot ceiling).
- `work_source` discriminator + minimal binding to `tracker_query`.

### Out of scope (deferred to other specs)

- The orchestrator polling loop and reconcile algorithm — see
  `spec-orchestrator-loop` (#1).
- Agent-runner internals (transport framing, session handshake, token
  accounting, stall detection) — see `spec-agent-runner` (#2).
- Workspace filesystem layout, sanitisation, and cleanup — see
  `spec-workspace-fs` (#3).
- MCP tool servers, ACP protocol negotiation, permission cards.
- Secrets management beyond `${ENV}` interpolation (no in-repo secret
  store in v1).
- Per-WorkSource adapter sub-schemas (Linear, GitHub Issues,
  local-file, etc.). This spec defines only the discriminator field
  (`work_source.type`) and the contract that adapter sub-schemas attach
  under `work_source.*`.

---

## 2. Terms

| Term | Definition |
|---|---|
| **Workflow** | The repo-owned configuration that tells `caduceusd` how to dispatch, how to invoke the agent, and what the agent's first-turn / continuation prompt shape is. |
| **WorkflowFile** | The on-disk YAML file that encodes a Workflow. Fixed name `WORKFLOW.yaml` at the repository root in v1. |
| **WorkflowVersion** | The `version` field at the top of WorkflowFile. v1 of this spec only accepts `version: "1"`. |
| **Hook** | A shell snippet (or argv list) declared under `hooks.*` that runs at a workspace lifecycle point: `before_create`, `after_create`, `before_cleanup`, `after_cleanup`. |
| **AgentCommand** | The triple `(agent.command, agent.args, agent.env)` that `caduceusd` uses to spawn the agent process for one attempt. |
| **BuildTurnPrompt** | The pure function `(workflow, run, turn_history, workspace_state) → PromptText` that produces the prompt for one turn. |
| **TurnContext** | The structured record passed to `BuildTurnPrompt` containing the run's invariants (issue, workspace, branch) and turn-local state (turn number, prior summary, event-log tail). |
| **FirstTurn** | The first turn of an attempt; `BuildTurnPrompt` MUST emit `agent.first_turn_template` rendered with the FirstTurn placeholder set. |
| **ContinuationTurn** | Any turn after the first within the same attempt; `BuildTurnPrompt` MUST emit `agent.continuation_template` rendered with the ContinuationTurn placeholder set. |
| **WorkflowReload** | The act of re-reading WorkflowFile after a filesystem change event, validating it, and swapping it in. |
| **WorkflowValidation** | The synchronous schema + semantic check applied to a Workflow before it is allowed to drive any dispatch. Run at startup, on every reload, and on every poll tick (Symphony posture; SPEC §11). |

---

## 3. Normative algorithms

### 3.1 `load_workflow(path)` → `Result<Workflow, ValidationError>`

```text
fn load_workflow(path: PathBuf) -> Result<Workflow, ValidationError>:
    bytes  = fs::read(path)?                      # I/O error → ValidationError::Io
    raw    = yaml::parse(bytes)?                  # YAML syntax error → ValidationError::Syntax
    schema_validate(raw)?                         # see §3.1.1; structured field-path errors

    # env-var interpolation — process env only; never in-repo plaintext
    interpolated = interpolate_env(raw)?          # see §3.1.2

    # relative-path resolution — relative paths resolve against the
    # workflow file's parent directory, NOT cwd of the daemon.
    base_dir = path.parent()
    workflow = resolve_paths(interpolated, base_dir)?

    # required-field check (post-interpolation, post-resolution)
    require!(workflow.agent.command.is_some(),    field = "agent.command")
    require!(workflow.workspace.root.is_some(),   field = "workspace.root")
    require!(workflow.version == "1",             field = "version")

    Ok(workflow)
```

#### 3.1.1 Schema validation

Schema validation rejects unknown top-level keys (closed-world: a typo at
the top level MUST fail loud, not silently degrade). Unknown keys *within*
adapter-specific sub-trees (`work_source.<adapter>.*`) are passed through
opaquely so this spec does not block adapter evolution.

#### 3.1.2 Env-var interpolation

The only interpolation form recognised in WorkflowFile is `${VAR}` (curly
braces required). `$VAR` (no braces) is **not** recognised and MUST be
preserved verbatim — this protects shell snippets in `hooks.*` from being
mangled by the loader.

`${VAR}` resolves against the daemon process's environment at load time
(load-time, not dispatch-time, so reload semantics are explicit). If `VAR`
is unset and the field is required, validation fails with
`ValidationError::EnvUnset { var, field_path }`. If the field has a default
in this spec (see §4.1), the default is used.

### 3.2 `validate_dispatch_config(workflow)` — fail-fast at startup

Cite Symphony `orchestrator.ex:67` — this MUST run **before** the first
poll tick.

```text
fn validate_dispatch_config(wf: &Workflow) -> Result<(), DispatchError>:
    # 1. AgentCommand is present and non-empty.
    require!(!wf.agent.command.is_empty(),            DispatchError::NoAgentCommand)

    # 2. workspace.root canonicalises and is a directory the daemon may write to.
    canonical = canonicalize(&wf.workspace.root)?     # follows symlinks
    require!(canonical.is_dir(),                      DispatchError::WorkspaceRootNotDir)
    require!(is_writable(&canonical),                 DispatchError::WorkspaceRootNotWritable)

    # 3. max_concurrent_agents in [1, MAX_AGENT_CEILING].
    require!(wf.agent.max_concurrent_agents >= 1 &&
             wf.agent.max_concurrent_agents <= MAX_AGENT_CEILING,
             DispatchError::ConcurrencyOutOfRange)

    # 4. work_source.type is a known discriminator.
    require!(WORK_SOURCE_REGISTRY.contains(&wf.work_source.type),
             DispatchError::UnknownWorkSource)

    # 5. Templates contain only placeholders from the closed catalog (§4.3).
    for (name, tmpl) in [("first_turn", &wf.agent.first_turn_template),
                         ("continuation", &wf.agent.continuation_template)]:
        for ph in scan_placeholders(tmpl):
            require!(PLACEHOLDER_CATALOG[name].contains(&ph),
                     DispatchError::UnknownPlaceholder { template: name, ph })

    # 6. Hook timeouts within bounds.
    for hook in wf.hooks.iter():
        require!(hook.timeout_ms <= HOOK_TIMEOUT_MAX_MS,
                 DispatchError::HookTimeoutTooLarge)

    Ok(())
```

`MAX_AGENT_CEILING` and `HOOK_TIMEOUT_MAX_MS` are caduceus daemon constants
(default `64` and `600_000` respectively) and are **not** workflow-tunable.
They are absolute upper bounds enforced for safety; the workflow may set
lower values.

### 3.3 `hot_reload(workflow_path)` — runs on file change

> **Cite.** Symphony posture, SPEC §11; verbatim adoption.

```text
fn hot_reload(daemon: &mut Daemon, path: PathBuf):
    new = match load_workflow(path):
        Ok(w):   w
        Err(e):  log_warn("workflow reload rejected", error = e)
                 return                                # keep old workflow, do NOT crash

    if validate_dispatch_config(&new).is_err():
        log_warn("workflow reload rejected at validate")
        return

    # Atomic swap: writers observe either the old or new Workflow; never partial.
    daemon.workflow.store(Arc::new(new))               # caduceus: ArcSwap

    # In-flight runs hold their own Arc<Workflow> snapshot taken at run_attempt
    # entry; they continue to completion using that snapshot.
    # Next dispatch (poll tick) reads the new Arc.
```

Filesystem watch is debounced at 200 ms to coalesce editor multi-write
patterns (vim's write-rename, VS Code's atomic save). A single reload event
fires per debounce window.

If reload fails validation, the daemon logs a structured warning **and
keeps serving with the previous workflow**. This is deliberate: a partial
edit on disk MUST NOT take the daemon down. The dashboard surface
(out of scope here) MUST display the most recent reload error.

### 3.4 `build_turn_prompt(workflow, run, turn_history, workspace_state)` → `PromptText`

> **Cite.** `agent_runner.ex:133–145`. THIS IS THE LOAD-BEARING CONTRACT.

```text
fn build_turn_prompt(wf: &Workflow, run: &Run, history: &[Turn],
                     ws: &WorkspaceState) -> PromptText:
    if history.is_empty():
        # FIRST TURN
        ctx = TurnContext {
            issue_identifier: run.issue.identifier,
            issue_title:      run.issue.title,
            issue_body:       run.issue.body,
            issue_url:        run.issue.url,
            workspace_path:   ws.path.display(),
            branch:           run.branch.clone(),
            attempt:          run.attempt,
        }
        render(&wf.agent.first_turn_template, &ctx, &PLACEHOLDER_CATALOG.first_turn)
    else:
        # CONTINUATION TURN
        last = history.last().unwrap()
        ctx = TurnContext {
            turn_number:        history.len() + 1,
            prior_turn_summary: last.summary.clone(),
            event_log_tail:     tail(history, n = wf.agent.continuation_tail_n),
            workspace_path:     ws.path.display(),
            branch:             run.branch.clone(),
            attempt:            run.attempt,
        }
        render(&wf.agent.continuation_template, &ctx, &PLACEHOLDER_CATALOG.continuation)
```

The first-turn-vs-continuation distinction is **stable** (Invariant I-3).
The full Liquid-style render of the workflow body with the issue context
happens on turn 1; the much shorter "continuation guidance" prompt is used
on every subsequent turn. Workflow authors rely on this — they put the
expensive context (workflow body, repository conventions, success
criteria) in `first_turn_template`, and the short re-grounding context
(prior summary, what happened last turn) in `continuation_template`.

`render()` is a pure substitution. Unknown placeholders (anything not in
the catalog for this template) MUST raise `RenderError::UnknownPlaceholder`
and abort the turn — they MUST NOT silently pass through as literal text
(see Invariant I-4 and Test T-4).

### 3.5 `run_hook(hook_name, env_overlay)` → `Result`

```text
fn run_hook(wf: &Workflow, name: HookName, ws: Option<&WorkspaceState>,
            env_overlay: HashMap<String, String>) -> Result<HookOutcome, HookError>:
    hook = wf.hooks.get(name).ok_or(NotConfigured)?

    # cwd by lifecycle point
    cwd = match name:
        BeforeCreate:  wf.workspace.root.clone()      # workspace doesn't exist yet
        AfterCreate:   ws.unwrap().path.clone()
        BeforeCleanup: ws.unwrap().path.clone()
        AfterCleanup:  wf.workspace.root.clone()      # workspace already removed

    # env merge order (each later layer overrides earlier):
    #   1. caduceusd process env (filtered allow-list; PATH, HOME, USER, LANG)
    #   2. workflow.agent.env (post-interpolation)
    #   3. workflow.hooks.env  (block-level; common to all hooks)
    #   4. hook.env            (per-hook)
    #   5. env_overlay         (caller-supplied; e.g. CADUCEUS_WORKSPACE_PATH, CADUCEUS_ATTEMPT)
    env = merge_envs([daemon_filtered_env(), wf.agent.env, wf.hooks.env,
                      hook.env, env_overlay])

    # exec model
    if hook.no_shell:
        spawn(argv = hook.command_argv, cwd, env)     # direct exec; argv is a list
    else:
        spawn(argv = ["bash", "-lc", hook.command_str], cwd, env)

    # bounded execution
    timeout_ms = hook.timeout_ms.unwrap_or(60_000)    # default 60s; cap HOOK_TIMEOUT_MAX_MS
    wait_with_timeout(child, timeout_ms)?
        .or_else_on_timeout(|| {
            send_signal(child, SIGTERM)
            wait_grace(child, 5_000)
            send_signal(child, SIGKILL)
            Err(HookError::Timeout)
        })
```

Hook stdout is captured into the daemon's run log; hook stderr is captured
into the daemon's diagnostic log. Neither stream is treated as a control
channel (consistent with the agent-process contract; see #2).

---

## 4. Data shapes

### 4.1 YAML schema (canonical form)

```yaml
version: "1"

workspace:
  root: ./.caduceus/workspaces        # relative paths resolve from WORKFLOW.yaml's dir

agent:
  command: codex
  args: [exec, "--ask-for-approval=never"]
  env:
    OPENAI_API_KEY: "${OPENAI_API_KEY}"
    CODEX_PROFILE:  "agent"
  max_concurrent_agents: 10           # [1, MAX_AGENT_CEILING=64]
  continuation_tail_n: 50             # how many prior events feed {event_log_tail}
  first_turn_template: |
    Resolve issue {issue_identifier}: {issue_title}

    {issue_body}

    Workspace: {workspace_path}
    Branch:    {branch}
  continuation_template: |
    Continue. Turn {turn_number}.
    Prior summary: {prior_turn_summary}

    Recent activity:
    {event_log_tail}

work_source:
  type: linear                        # discriminator; adapter schema attaches under same key
  query: "is:open team:eng"
  # adapter-specific keys (e.g. linear.team_id, linear.api_key) live under work_source.*
  # and are validated by the adapter, not by this spec.

hooks:
  env:                                # block-level env, merged into every hook below
    REPO_URL: "https://github.com/owner/repo"
  before_create:
    command_str: |                    # shell form — `bash -lc <command_str>`
      mkdir -p "$CADUCEUS_WORKSPACE_PATH"
    timeout_ms: 30000
  after_create:
    command_str: |
      git clone --depth 1 "$REPO_URL" .
      git checkout -b "$CADUCEUS_BRANCH"
    timeout_ms: 120000
  before_cleanup:
    no_shell: true                    # direct exec form
    command_argv: ["./scripts/save-artifacts.sh"]
    timeout_ms: 60000
  after_cleanup:
    command_str: |
      echo "cleaned $CADUCEUS_WORKSPACE_PATH (removed=$CADUCEUS_WORKSPACE_REMOVED)"

observability:
  refresh_ms: 1000
  log_path: ./.caduceus/log
```

#### Schema-level rules

- `version` is a string. v1 accepts only `"1"`.
- `workspace.root` is required.
- `agent.command` is required.
- `agent.args` defaults to `[]`.
- `agent.env` defaults to `{}`.
- `agent.max_concurrent_agents` defaults to `1`.
- `agent.continuation_tail_n` defaults to `50`.
- Either `first_turn_template` and `continuation_template` are both present,
  or both absent (in which case caduceus's spec-default templates are
  used; spec defaults are pinned in §4.4).
- `work_source.type` is required. `work_source.query` is required and is a
  string the adapter interprets; adapters MAY accept structured queries
  via additional sub-keys.
- All `hooks.*` entries are optional. Each is either `{ command_str,
  timeout_ms?, env? }` (shell form) or `{ command_argv: [...],
  no_shell: true, timeout_ms?, env? }` (exec form).
- `observability.refresh_ms` defaults to `1000`. `observability.log_path`
  defaults to `./.caduceus/log` (relative to workflow file).

### 4.2 In-memory shape (Rust-flavored)

```rust
pub struct Workflow {
    pub version:       String,                    // "1"
    pub workspace:     WorkspaceConfig,
    pub agent:         AgentConfig,
    pub work_source:   WorkSourceConfig,
    pub hooks:         HooksConfig,
    pub observability: ObservabilityConfig,
    pub source_path:   PathBuf,                   // absolute path of WORKFLOW.yaml
}

pub struct WorkspaceConfig {
    pub root: PathBuf,                            // absolute, post-resolution
}

pub struct AgentConfig {
    pub command:               String,
    pub args:                  Vec<String>,
    pub env:                   BTreeMap<String, String>,
    pub max_concurrent_agents: u32,
    pub continuation_tail_n:   u32,
    pub first_turn_template:   String,
    pub continuation_template: String,
}

pub struct WorkSourceConfig {
    pub r#type: String,                           // discriminator
    pub query:  String,
    pub extra:  serde_yaml::Value,                // adapter-opaque
}

pub struct HooksConfig {
    pub env:            BTreeMap<String, String>,
    pub before_create:  Option<Hook>,
    pub after_create:   Option<Hook>,
    pub before_cleanup: Option<Hook>,
    pub after_cleanup:  Option<Hook>,
}

pub enum Hook {
    Shell { command_str:  String, timeout_ms: Option<u64>, env: BTreeMap<String,String> },
    Exec  { command_argv: Vec<String>, timeout_ms: Option<u64>, env: BTreeMap<String,String> },
}

pub struct ObservabilityConfig {
    pub refresh_ms: u64,
    pub log_path:   PathBuf,
}
```

The daemon holds the live workflow as `ArcSwap<Workflow>` so reload is a
pointer swap and readers see a consistent snapshot.

### 4.3 Placeholder catalog (NORMATIVE)

The placeholder set is **closed**. Adding a placeholder requires bumping
WorkflowVersion and amending this spec. Unknown placeholders MUST be
rejected (see §3.4 and T-4).

#### First-turn placeholders

| Token | Expansion | Source |
|---|---|---|
| `{issue_identifier}` | Tracker-side ID (`PROJ-123`, `gh#42`, `LIN-7`). | `run.issue.identifier` |
| `{issue_title}` | Title as fetched from the WorkSource. | `run.issue.title` |
| `{issue_body}` | Body / description text, untruncated. | `run.issue.body` |
| `{issue_url}` | Canonical URL to the issue, if the WorkSource exposes one; empty string otherwise. | `run.issue.url` |
| `{workspace_path}` | Absolute filesystem path of the per-issue workspace. | `WorkspaceState.path` |
| `{branch}` | Git branch name caduceus has prepared for this attempt. | `run.branch` |
| `{attempt}` | 1-based attempt counter for this issue (resets only on issue close). | `run.attempt` |

#### Continuation-turn placeholders

| Token | Expansion | Source |
|---|---|---|
| `{turn_number}` | 1-based turn counter within this attempt. Always ≥ 2 in continuation context. | `history.len() + 1` |
| `{prior_turn_summary}` | The agent-emitted summary of the immediately preceding turn (empty string if the agent did not emit one). | `history.last().summary` |
| `{event_log_tail}` | The last `agent.continuation_tail_n` events from the agent stream, formatted one per line. | `tail(history, n)` |
| `{workspace_path}` | Same as first-turn. | `WorkspaceState.path` |
| `{branch}` | Same as first-turn. | `run.branch` |
| `{attempt}` | Same as first-turn. | `run.attempt` |

#### Hook placeholders (substituted into hook env, not into command text)

Hooks receive the following caduceus-injected env variables in addition
to merged env (§3.5):

| Variable | Value |
|---|---|
| `CADUCEUS_WORKSPACE_PATH` | Absolute workspace path (`{workspace_path}` equivalent). For `before_create` this is the planned-but-not-yet-existing path (the daemon-side fd-anchored `mkdirat` from spec #3 §3.5 step 5b has NOT yet run when `before_create` fires; for `after_create` and later phases the leaf is materialized). Z-30: previously named `CADUCEUS_WORKSPACE`; the rename harmonises with spec #2 §5 I-9 and spec #3 §3.5 step 6 / §3.6 step 7. |
| `CADUCEUS_WORKSPACE_REMOVED` | Set to `"1"` *only* on `after_cleanup` invocations that follow a successful step-6 leaf removal in spec #3 §3.6; absent / unset otherwise. Hooks checking "did the leaf get removed" MUST consult this flag rather than `stat`-ing `CADUCEUS_WORKSPACE_PATH`. (Z-13 echo from spec #3.) |
| `CADUCEUS_BRANCH` | Branch name. |
| `CADUCEUS_ISSUE_IDENTIFIER` | Tracker ID. |
| `CADUCEUS_ATTEMPT` | Attempt number, decimal. |
| `CADUCEUS_LIFECYCLE` | One of `before_create`, `after_create`, `before_cleanup`, `after_cleanup`. |

Hook command text (`command_str` or `command_argv`) is **not**
placeholder-substituted by caduceus. Workflow authors interpolate via the
shell (`$CADUCEUS_WORKSPACE_PATH`) or via argv-level env-var consumption in
their script.

### 4.4 Spec-default templates

If the workflow omits both templates, caduceus uses these defaults:

```text
# default first_turn_template
Resolve issue {issue_identifier}: {issue_title}

{issue_body}

Workspace: {workspace_path}
Branch:    {branch}
Attempt:   {attempt}

# default continuation_template
Continue. Turn {turn_number}.

Prior turn summary:
{prior_turn_summary}

Recent activity:
{event_log_tail}
```

Workflow authors who provide one template MUST provide the other —
a "fall back to default for the missing one" rule was rejected because
it would let a small typo silently mix author-intent with caduceus-default
prompt shape (Invariant I-3).

---

## 5. Invariants (MUST)

- **I-1.** Workflow validation is fail-fast at startup. The daemon MUST
  call `validate_dispatch_config` before its first poll tick. A
  misconfigured workflow (missing `agent.command`, unsanitisable
  `workspace.root`, unknown placeholder, unknown `work_source.type`)
  causes startup failure, **not** a deferred runtime error. (Cite
  `orchestrator.ex:67`.)

- **I-2.** Hot-reload is atomic. A reload either succeeds and is
  observed in full by the next dispatch, or fails and leaves the
  previous workflow active in full. There is no observable partial
  state. In-flight Runs use the workflow they started with, captured by
  reference (or `Arc<Workflow>` snapshot) at `run_attempt` entry.

- **I-3.** `build_turn_prompt`'s first-turn-vs-continuation distinction
  is stable. Workflow authors and agents MAY depend on the contract:
  *first turn → `first_turn_template` rendered with the FirstTurn
  catalog; every later turn → `continuation_template` rendered with the
  ContinuationTurn catalog.* Caduceus MUST NOT reorder, conditionally
  swap, or interleave these template selections.

- **I-4.** The placeholder set is closed. The `PLACEHOLDER_CATALOG`
  enumerated in §4.3 is the complete set of tokens caduceus
  substitutes. Unknown tokens in a template MUST cause
  `validate_dispatch_config` to reject the workflow at load time
  (preferred), or `build_turn_prompt` to raise `RenderError` at turn
  time (fallback). Silent pass-through of literal `{...}` text is
  prohibited. Adding a placeholder bumps WorkflowVersion.

- **I-5.** Hook execution is bounded. Every hook has a finite
  `timeout_ms` (default 60 000; cap `HOOK_TIMEOUT_MAX_MS = 600_000`).
  On timeout the daemon SIGTERMs the hook process, waits 5 seconds,
  and SIGKILLs. The cascade applies to the entire process group
  (the daemon launches hooks in a fresh process group via `setsid`
  on Unix).

- **I-6.** Workflow files are repo-owned. `caduceusd` discovers
  `WORKFLOW.yaml` at a fixed location relative to the repository root;
  the daemon does **not** accept a `--workflow` global path argument
  in v1, does **not** read a user-level workflow at
  `~/.config/caduceus/workflow.yaml`, and does **not** synthesise a
  default workflow when the file is missing. A repo without
  `WORKFLOW.yaml` is not a caduceus repo. This is what makes the
  contract repo-owned and reviewable through the same code-review
  surface as any other source change.

- **I-7.** Secrets are interpolated from process environment, never
  from in-repo plaintext. If a YAML scalar inside `agent.env`,
  `hooks.*.env`, or `hooks.env` looks like a secret (matches a
  configurable regex of secret-like patterns: long high-entropy
  strings, common prefixes such as `sk-`, `ghp_`, `xoxb-`,
  `AKIA[0-9A-Z]{16}`) **and** is not enclosed in `${...}`, validation
  MUST reject it with `ValidationError::PlaintextSecret { field_path }`.
  Workflow authors who genuinely want a literal value matching this
  pattern can opt out per-field with a `# caduceus: literal` line
  comment trailer; the loader honours the comment if present.

---

## 6. Test contract

| ID | Test | Expected |
|---|---|---|
| **T-1** | Start `caduceusd` with a `WORKFLOW.yaml` that has no `agent.command`. | Daemon exits non-zero before first poll tick; stderr contains a structured `ValidationError` naming `agent.command` as the missing field. |
| **T-2** | Start a Run on issue I (turn 1 in flight); edit `WORKFLOW.yaml` to change `agent.first_turn_template`; let Run continue to turn 2 and beyond. | Run I uses the **old** `first_turn_template` (since turn 1 already rendered) and the **old** `continuation_template` for turn 2+. A *new* Run started after the reload uses the new templates. |
| **T-3** | With a workflow that sets distinct `first_turn_template` and `continuation_template`, observe `build_turn_prompt` output for turns 1, 2, 3. | Turn 1 output matches `first_turn_template` with FirstTurn catalog substituted; turns 2 and 3 match `continuation_template` with ContinuationTurn catalog substituted. |
| **T-4** | Author a workflow whose `first_turn_template` references `{not_a_real_placeholder}`. | `validate_dispatch_config` rejects the workflow at load time. The literal `{not_a_real_placeholder}` is **not** present in any rendered prompt. |
| **T-5** | Configure `hooks.after_create.command_str` to `sleep 9999` with `timeout_ms: 2000`. | After 2 seconds the hook receives SIGTERM; after a further 5 seconds it receives SIGKILL; the daemon records `HookError::Timeout` and aborts the workspace creation for this Run. |
| **T-6** | Author `agent.env: { OPENAI_API_KEY: "sk-abcdefghijklmnop1234567890ABCDEF" }` (literal secret pattern, no `${...}`). | Validation rejects with `ValidationError::PlaintextSecret`. Re-author with `"${OPENAI_API_KEY}"` and the literal exported in process env: validation passes. |
| **T-7** | Author a workflow with `version: "2"`. | `load_workflow` rejects with `ValidationError::UnsupportedVersion` naming the supported set (`["1"]`) and the observed value. |
| **T-8** | Edit `WORKFLOW.yaml` to introduce a YAML syntax error mid-Run. | The reload fails; the daemon logs a warning; the previously-loaded workflow remains active; in-flight Runs are unaffected; subsequent dispatches use the previously-loaded workflow. Fixing the syntax error and saving causes a successful reload on the next debounce tick. |
| **T-9** | Specify `workspace.root: ./ws` in a workflow at `/repo/WORKFLOW.yaml`. | After load, `workflow.workspace.root` is the absolute path `/repo/ws`, not `<daemon-cwd>/ws`. |
| **T-10** | Set `agent.max_concurrent_agents: 999`. | Validation rejects with `DispatchError::ConcurrencyOutOfRange { observed: 999, max: 64 }`. |

---

## 7. Out of scope

- **Per-WorkSource adapter sub-schemas.** Each adapter (Linear, GitHub
  Issues, local-file, etc.) ships its own sub-schema attached under
  `work_source.*`. This spec defines only the discriminator
  (`work_source.type`) and the contract that adapter-validated keys are
  passed through opaquely by the workflow loader.

- **Hook security hardening beyond timeout.** Sandboxing hook processes
  (chroot, cgroup limits, seccomp filters, network egress controls) is
  the workflow author's responsibility. Caduceus enforces only the
  timeout and the cwd / env merge contract. A workflow that runs
  `rm -rf /` in `after_create` will succeed in destroying everything
  the daemon's UID can reach; this is out of scope to defend against.

- **Workflow-level secret stores.** v1 has no `workflow.secrets:`
  block. Secrets are sourced from process env via `${VAR}`. A future
  spec may add a `secrets:` block bound to OS keychains / Vault /
  cloud KMS, but it is not on the v1 critical path.

- **Multiple workflow files / per-branch workflows.** v1 fixes the
  workflow at `WORKFLOW.yaml` at repo root. Branch-conditional or
  layered workflows are a v2 question (see §8).

- **Schema migration tooling.** When WorkflowVersion bumps, caduceus
  rejects the old version (T-7); it does not auto-migrate. A separate
  `caduceus migrate-workflow` CLI is a future surface.

- **Telemetry contract for hook outcomes.** Hooks emit log lines today;
  structured emission to OpenTelemetry is owned by the orchestrator
  spec (#1).

---

## 8. Open questions

1. **`workflow.version` field policy.** Three candidates:
   - String semver (`"1.0.0"`) — most familiar but encourages spurious
     patch bumps.
   - Monotonic integer-as-string (`"1"`, `"2"`) — current preference;
     forces deliberate version events.
   - Named codename (`"chiron"`, `"centaur"`) — fun, hostile to tooling.
   Current spec text uses option 2; revisit before v2.

2. **Multiple workflow files.** Do we want a per-branch overlay?
   Possible shapes:
   - `WORKFLOW.yaml` + optional `WORKFLOW.<branch>.yaml` overlay
     (rejected for v1: branch-aware loading complicates reload
     semantics and code review).
   - `WORKFLOW.yaml` with explicit `branches:` block where the workflow
     declares per-branch overrides (preferred for a future v2; keeps
     one file under one review).
   - No overlay ever (current v1 stance).

3. **Schema validator implementation.** Three candidates:
   - Hand-rolled validator (current preference; simpler, fewer deps,
     better error messages with field paths).
   - JSON Schema (declarative, widely supported, but YAML→JSON
     coercion has edge cases around tagged scalars).
   - Cue (powerful constraint language, large dep surface).
   Decision deferred until #1 lands; the validator implementation is
   internal to the daemon and not normative here.

4. **Per-hook concurrency.** May two hooks at different lifecycle
   points run simultaneously (e.g., `before_cleanup` for Run A
   concurrent with `after_create` for Run B)? Current spec text:
   yes — hooks are scoped to a single Run's lifecycle, so cross-Run
   concurrency is governed by `agent.max_concurrent_agents`. Confirm
   with #1 / #3 before merge.

5. **`continuation_tail_n` semantics under abnormal exit.** When an
   attempt is resumed after an abnormal exit (with a continuation
   retry, see #1), is the tail computed from *all* prior turns of the
   issue across attempts, or only from the current attempt's history?
   Current spec text: current attempt only (turn history is
   per-attempt; cross-attempt context lives on the tracker). Revisit
   if Pattern-3 (shared-context multi-agent) ever lands.

6. **Hook failure semantics.** Current spec text leaves it to #1 / #3
   to decide whether `before_create` failure aborts the Run, retries
   it, or marks the issue blocked. This spec only guarantees the hook
   *executes* per its declared timeout/env contract.

---

## 9. Cross-references

- **Spec #1 — `spec-orchestrator-loop`.** Calls
  `validate_dispatch_config` at startup (§3.2) and on every poll
  tick. Subscribes to the workflow's `ArcSwap` for hot-reload (§3.3).
  Reads `agent.max_concurrent_agents` for slot accounting.
- **Spec #2 — `spec-agent-runner`.** Reads `agent.command`,
  `agent.args`, `agent.env` to build the AgentCommand. Calls
  `build_turn_prompt` (§3.4) once per turn. Honours the closed
  placeholder catalog (§4.3).
- **Spec #3 — `spec-workspace-fs`.** Reads `workspace.root`. Invokes
  `run_hook` (§3.5) at workspace lifecycle points with the
  caduceus-injected env (§4.3 hook table).
- **Future — WorkSource adapter specs.** Each adapter spec attaches a
  sub-schema under `work_source.<adapter>.*` and defines the semantics
  of `work_source.query` for that adapter. This spec defines only the
  discriminator and is forward-compatible with arbitrary adapter
  schemas.

---

*End of `spec-repo-owned-workflow-contract.md`.*
