# Spec: Skill / Agent Autocomplete (caduceus + caduceus-zed)

> **Status:** Draft (P-tier specification)
> **Owners:** caduceus core (engine indexing, RPC, manifest loader); caduceus-zed `agent_ui` crate (composer, completion provider).
> **Audience:** caduceus engineers; caduceus-zed UI engineers; future authors of alternate front-ends (VS Code IDE host, Copilot CLI host).
> **Document scope:** Normative behavior of the chat composer's "type a trigger character to get a popup of skills/agents" affordance, end-to-end, from filesystem manifest discovery on the engine side through the wire protocol to popup rendering and selection in the editor.
> **Document non-scope:** The marketplace browse experience, the SkillsPanel enable/disable UX, the runtime semantics of `@agent` invocation (delegated to the agent runner contract), and the rendering of skill bodies into the system prompt.

---

## §0. Provenance

This specification is a **cleanroom rewrite** of behavior originally implemented in the Microsoft EMU "Clawpilot" (M) codebase, specifically:

- `M:docs/architecture/09-skills-and-marketplace.md` (skill loader, two-root precedence, YAML frontmatter)
- `M:docs/architecture/06-ui-layer.md` §UI-C (composer trigger state machine, completion provider integration, keyboard contract)
- `M:docs/architecture/04-tools-and-permissions.md` (permission gating of skill visibility — referenced normatively, not duplicated)

The behavior described herein has been independently re-derived from publicly documented design intents, behavioral observation of the running system, and the in-tree reference specs `spec-m-session-lifecycle.md` and `spec-caduceus-agent-runner-contract.md`. **No source code, identifiers, struct layouts, or protobuf field numbers from M have been transcribed into this specification.** All RPC names, payload shapes, error codes, state-machine state names, and invariant identifiers in this document are caduceus-native and may be freely implemented in caduceus / caduceus-zed without copyright concern.

### Cleanroom Statement

> The author of this specification did not have read access to M source code at any point during authorship. All M references in §0 are by-document-title only and were drawn from a public architecture index. The implementation team building against this spec MUST NOT consult M source code while implementing the features described. Any implementer who has had prior exposure to M source MUST self-disclose to the reviewing maintainer before opening a PR that touches `caduceus/src/skills/`, `caduceus/src/agents/`, `caduceus/src/rpc/autocomplete*`, or `caduceus-zed/crates/agent_ui/src/{message_editor,completion_provider}.rs`.

### Relationship to other specs

| Spec | Relationship |
|------|--------------|
| `spec-m-permissions.md` | **Normatively cited.** Permission gating of skill / agent visibility in the popup defers entirely to that spec. Visibility = `permission_decision == Allow` for the calling principal in the calling session. |
| `spec-caduceus-agent-runner-contract.md` | **Normatively cited** for what happens *after* the user selects an `@agent` token. Selection only inserts text; runner-contract owns invocation. |
| `spec-m-session-lifecycle.md` | **Style reference + normatively cited** for session boundaries: the autocomplete index is per-session-context but its underlying manifest cache is process-wide. |
| `spec-caduceus-collab-patterns.md` | **Non-normative cross-reference.** Composer agents may also surface in autocomplete; precedence rules in §6 cover the overlap. |
| `spec-cross-cutting-wiring.md` | **Normative** for the RPC plumbing layer and version negotiation. §9 of this spec assumes the wiring layer's framing semantics. |

### Conformance language

The key words **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, **MAY** are to be interpreted as described in RFC 2119. Where this document uses the word "host" without qualification, it refers to any of: caduceus-zed, the VS Code Copilot Chat host, or the Copilot CLI host. Where it uses "engine," it refers to the caduceus daemon process providing the autocomplete index over RPC.

---

## §0.1 Table of Contents

- §1. System Overview
- §2. User-Facing Trigger (Composer State Machine)
- §3. Source of Truth (Filesystem Manifest Discovery)
- §4. Indexing & Refresh Model
- §5. Ranking
- §6. Scope Disambiguation
- §7. Error & Empty States
- §8. Cross-Runtime Parity
- §9. Wire Format (RPC between Host and Engine)
- §10. Acceptance Criteria & Invariants (Z-numbered)
- §11. Testable Scenarios
- §12. Glossary
- §13. Out of Scope / Deferred
- §14. Open Questions

---

## §1. System Overview

Skill/Agent Autocomplete is the chat composer affordance that lets a user type a single trigger character — `/` for skills, `@` for agents — at a word boundary in the message editor and receive a filtered, ranked, navigable popup of available skills or agents matching the prefix typed after the trigger. Selecting an item from the popup inserts a token (e.g. `/python-pro ` or `@code-reviewer `) into the editor; it does **not** invoke the skill or agent. Invocation happens later, when the message is submitted, and is owned by the engine (skills) or by the agent runner contract (agents).

### 1.1 Two trigger characters, one mechanism

The system treats `/skill` autocomplete and `@agent` autocomplete as two **instances of one mechanism** with different *catalog sources*. The composer state machine, the keyboard contract, the popup rendering, the wire format, and the ranking algorithm are identical. The only differences are:

1. The trigger character (`/` vs `@`).
2. The catalog source (skills index vs agents index, see §3).
3. The token format inserted on selection (`/<name> ` vs `@<name> `).
4. The downstream invocation handler (skill activation vs agent runner contract — both **out of scope** here).

This shared-mechanism design is load-bearing for **Z-12 (cross-trigger semantic parity)**: a host MUST NOT implement two parallel state machines, ranking algorithms, or popup widgets — one widget driven by one state machine handles both, parameterized by trigger.

### 1.2 Process model

```
+----------------------------------+         RPC        +------------------------------------+
|   Host (e.g. caduceus-zed)       | <----------------> |   caduceus engine (daemon)         |
|   - composer / message editor    |   autocomplete.*   |   - manifest loader (skills)       |
|   - completion_provider          |                    |   - manifest loader (agents)       |
|   - popup widget                 |                    |   - index (debounced fs-watch)     |
|   - keyboard handler             |                    |   - permission gate (per spec-m-   |
|   - selection -> token insertion |                    |     permissions)                   |
+----------------------------------+                    +------------------------------------+
```

The host owns: the trigger state machine, popup rendering, keyboard, caret tracking, token insertion. The engine owns: filesystem scan, manifest parse, permission filtering, ranking signal computation, and serving the index over RPC.

The host MUST NOT scan the filesystem for skills or agents itself. The engine MUST NOT render UI. This separation is **Z-1 (separation of concerns)** and is necessary for cross-runtime parity (§8).

### 1.3 Where this lives in the codebases

In **caduceus** (engine):

- `caduceus/src/skills/loader.rs` — filesystem scan + YAML parse + cache.
- `caduceus/src/agents/loader.rs` — filesystem scan + YAML/Markdown parse + cache.
- `caduceus/src/index/autocomplete.rs` — merged, ranked, permission-filtered view onto both caches.
- `caduceus/src/rpc/autocomplete.rs` — RPC handlers (`autocomplete.query`, `autocomplete.refresh`, `autocomplete.select`).

In **caduceus-zed** (host):

- `caduceus-zed/crates/agent_ui/src/message_editor.rs` — owns the trigger state machine.
- `caduceus-zed/crates/agent_ui/src/completion_provider.rs` — owns the popup widget and RPC client.

### 1.4 Lifecycle summary

1. Engine starts; manifest loaders scan global root then workspace root (§3); cache is populated.
2. Engine starts a debounced filesystem watcher on both roots (§4).
3. Host connects to engine; performs RPC handshake (covered in `spec-cross-cutting-wiring.md`).
4. User opens a chat session; host renders the composer.
5. User types text; the composer's trigger state machine sits in `Closed` state.
6. User types `/` or `@` at a word boundary; state transitions to `Open`; host sends `autocomplete.query` with empty prefix.
7. As the user types subsequent characters, state stays in `Filtering`; each keystroke (debounced ~30ms client-side) sends a fresh `autocomplete.query` with the updated prefix.
8. User presses Enter or Tab; selected item's token is inserted into the editor; state transitions to `Closed`.
9. User submits the message; engine resolves `/skill` and `@agent` tokens during turn construction (out of scope for this spec — see runner contract for `@agent` semantics).

### 1.5 Design pillars (non-normative, motivational)

- **The trigger is a signal, not a macro.** Selection inserts text only. The skill/agent is not activated until the message is sent and the engine resolves tokens. This decouples the popup UX from invocation semantics and lets users freely edit/delete tokens before submission. This is reified in **Z-2 (selection-inserts-token-only)**.
- **The popup is a view onto an index, not a search.** The engine's index is authoritative; the popup just paginates through it. There is no "deep search" mode that escapes the index.
- **Cross-runtime parity is non-negotiable.** A user moving between caduceus-zed, the VS Code Chat host, and the Copilot CLI host MUST observe the same trigger semantics, the same ranking, the same scope disambiguation. Visual chrome may differ; behavior MUST NOT.
- **The slash and the at-sign are sacred at word boundaries only.** Mid-word `/` (e.g. inside a path like `src/main.rs`) MUST NOT trigger the popup. This is **Z-3 (word-boundary trigger discipline)**.
- **Empty states are informative, not silent.** A popup that opens and shows nothing tells the user *why* (no skills installed, all permission-denied, malformed manifests skipped, etc.).

---

## §2. User-Facing Trigger (Composer State Machine)

### 2.1 States

The composer's trigger logic is a four-state state machine owned by `agent_ui::message_editor` (or its analog on other hosts):

| State | Description |
|-------|-------------|
| `Closed` | No popup visible. The composer is in normal text-editing mode. This is the initial and most common state. |
| `Open` | Popup is visible, no characters typed after trigger yet (empty prefix). The full enabled-and-visible catalog is shown, ranked. |
| `Filtering` | Popup is visible, ≥1 characters typed after the trigger. Catalog is filtered by prefix (and ranked). |
| `Selected` | Transient. A selection was just committed; token has been inserted; state immediately transitions to `Closed` on the next event-loop tick. (Reified as a state to make the transition auditable in tests.) |

### 2.2 Transition diagram

```
                     +---------------------------+
                     |                           |
                     v                           |
              +------------+   trigger-char      |
   +--------> |  Closed    |---------------+     |
   |          +------------+               |     |
   |              ^   ^                    v     |
   |   esc/space  |   | backspace-thru-/   +-----+--+
   |   submit/    |   +--------------------|  Open  |
   |   click-out  |                        +--------+
   |              |                            |
   |              |                            | typed-char
   |              |                            v
   |              |                       +-----------+
   |              +-----------------------|Filtering  |
   |   esc/space/click-out                +-----------+
   |                                            |
   |                                            | enter/tab/click
   |                                            v
   |                                      +-----------+
   +--------------------------------------|Selected   |
              auto-tick                   +-----------+
```

### 2.3 Trigger conditions (Closed → Open)

The state transitions from `Closed` to `Open` if and only if **all** of the following are true at the moment the user types the trigger character:

1. The character typed is exactly `/` (skills) or `@` (agents). Other characters MUST NOT open the popup.
2. The character is typed at a **word boundary**. A word boundary is defined as: position 0 of the editor, or the position immediately after a whitespace character (` `, `\t`, `\n`), or the position immediately after a syntactic punctuation that is not a word character (`,`, `;`, `(`, `[`, `{`, `<`, `"`, `'`, backtick).
3. The composer is not currently in an inline-code or code-block region as determined by the host's lightweight tokenizer. Hosts MAY skip this check if they do not perform inline-code tokenization, but SHOULD warn the user that triggers will fire inside backticks if they do.
4. The composer is not in a read-only or disabled state (e.g. during message-send-in-flight).

Mid-word `/` and `@` MUST NOT open the popup. This is **Z-3 (word-boundary trigger discipline)** and is critical because users routinely type paths (`src/main.rs`) and email addresses (`alice@example.com`) inside chat messages.

### 2.4 Filter conditions (Open → Filtering, Filtering → Filtering)

When the popup is `Open` or `Filtering` and the user types any character that is a valid identifier character — `[a-zA-Z0-9_-]` plus `:` (the scope qualifier, see §6.4) — the state advances to `Filtering` and the prefix is updated.

The host MUST debounce its `autocomplete.query` RPCs by approximately 30ms (recommended; not normative) to avoid spamming the engine on rapid typing. The popup MUST visually update on every keystroke even if the RPC has not yet returned, by client-side filtering of the most recent received page (best-effort) — this is a UX nicety, not a correctness requirement.

### 2.5 Dismissal conditions (any open state → Closed)

The state transitions from `Open` or `Filtering` back to `Closed` if any of the following occur:

1. User presses `Esc`. The popup closes; no token is inserted. The trigger character itself is **left in the editor** (the user typed it; the host does not delete it). The user may immediately retype to re-trigger.
2. User presses `Space`. The popup closes; trigger character and any prefix typed are left as plain text.
3. User backspaces past the trigger character. The popup closes; the trigger character has now been deleted.
4. User clicks outside the popup (on the editor or elsewhere in the host UI). The popup closes; trigger and prefix are left as-is.
5. User submits the message (Cmd/Ctrl+Enter). The popup closes; submission proceeds with the literal text.
6. The host loses focus on the composer (e.g. switches to another pane).
7. The engine returns an error other than `E_NO_RESULTS` (see §7); the popup closes and an error toast is shown.

The host MUST NOT close the popup on Tab when there is at least one selectable item — Tab is the secondary "select highlighted item" key (see §2.7). If the popup is open with zero items, Tab MAY close.

### 2.6 Selection (any open state → Selected → Closed)

The popup commits a selection when the user:

1. Presses `Enter` while an item is highlighted, **or**
2. Presses `Tab` while an item is highlighted, **or**
3. Clicks an item with the primary mouse button.

The host MUST insert the token corresponding to the selected item — `/<name> ` for skills, `@<name> ` for agents — at the position where the trigger character was typed, replacing both the trigger character and any prefix the user has typed since. The trailing space is mandatory; it terminates the token and re-establishes a word boundary. The host MUST NOT insert any additional content (no skill body, no help text, no metadata).

This is **Z-2 (selection-inserts-token-only)** and is the most behaviorally load-bearing invariant in the entire spec. Violating it conflates the *signal* of selection with the *semantics* of activation.

### 2.7 Keyboard contract

| Key | State: Closed | State: Open / Filtering | State: Selected (transient) |
|-----|---------------|--------------------------|------------------------------|
| `/` | Open popup if word-boundary | Treat as filter character (reach into prefix) | n/a |
| `@` | Open popup if word-boundary | Treat as filter character | n/a |
| `Enter` | Default (newline or submit per host config) | Commit highlighted item; insert token | n/a |
| `Tab` | Default (insert tab or focus-cycle per host config) | Commit highlighted item; insert token | n/a |
| `Esc` | Default (clear focus or no-op) | Close popup; leave text | n/a |
| `Space` | Default (insert space) | Close popup; leave text + space | n/a |
| `↑` / `↓` | Default (caret movement) | Move highlight in popup; consume event | n/a |
| `←` / `→` | Default (caret movement) | Default (caret movement) **and** update prefix if cursor moves out of trigger context — close popup if cursor leaves the prefix region | n/a |
| `Home` / `End` | Default | Close popup (cursor leaves prefix region) | n/a |
| `Backspace` | Default | Update prefix; close if backspaced past trigger | n/a |
| Printable identifier char | Default | Append to prefix; refilter | n/a |
| Other printable char | Default | Close popup; insert character | n/a |
| `PageUp` / `PageDown` | Default | Page through popup items by visible-page-size | n/a |

The contract is normative for caduceus-zed. Other hosts MUST observe the same Enter/Tab/Esc/Space/Arrow semantics; modifier-key behavior (e.g. Shift+Enter, Cmd+Enter) is host-defined.

### 2.8 Caret-relative popup positioning

The popup MUST anchor visually to the caret position at the moment the trigger was typed, not to the current caret position as it moves through the prefix. Hosts SHOULD reposition the popup if the editor scrolls or resizes, keeping the anchor relative to the original trigger character's screen position. If the popup would render off-screen below the caret, the host SHOULD render it above the caret instead.

### 2.9 Multi-character prefix and partial matching

The prefix is the substring between (exclusive) the trigger character and (exclusive) the current caret position. It updates on every keystroke. The prefix MAY be empty (state `Open`) or non-empty (state `Filtering`). The prefix is normalized to lowercase before being sent to the engine.

If the prefix contains a `:`, it is interpreted as a fully-qualified scope reference (see §6.4): the substring before `:` is the scope hint (`workspace`, `user`, `repo`, `builtin`), and the substring after is the name prefix. Empty scope before `:` (e.g. user types `/:foo`) is treated as a syntax error; the popup MUST display the empty-state error "Scope required before colon."

### 2.10 Concurrent-typing race

If the user types faster than RPCs return, the host MUST associate each in-flight `autocomplete.query` with a monotonically-increasing request sequence number and MUST discard responses whose sequence number is older than the most recent committed response. The popup MUST never flicker backwards in time (i.e. show a result for prefix `py` after already showing a result for prefix `python`).

This is **Z-4 (monotonic response ordering)**.

---

## §3. Source of Truth (Filesystem Manifest Discovery)

### 3.1 Roots

The engine discovers skills and agents from exactly two filesystem roots, in this precedence order (highest to lowest):

| Root | Skills path | Agents path | Scope label | Visibility default |
|------|-------------|-------------|-------------|--------------------|
| Workspace | `<workspace>/.github/skills/<name>/SKILL.md` | `<workspace>/.github/agents/<name>/AGENT.md` (or `<workspace>/.github/agents/<name>.agent.md` flat form) | `workspace` | Visible to sessions whose CWD is within `<workspace>` |
| User (global) | `~/.copilot/skills/<name>/SKILL.md` | `~/.copilot/agents/<name>/AGENT.md` (or `~/.copilot/agents/<name>.agent.md`) | `user` | Visible to all sessions for the current OS user |

The engine MUST NOT scan any other roots for autocomplete purposes. In particular, the engine MUST NOT walk arbitrary repository subdirectories looking for `SKILL.md`. The two roots above are exhaustive.

Two notional additional sources are recognized but **not** discovered by filesystem scan:

- `repo` scope: skills/agents shipped *inside* a repository under a path other than `.github/skills/`, surfaced via an explicit manifest declaration in a repo-level config file (out of scope for this document; see `spec-repo-owned-workflow-contract.md`).
- `builtin` scope: skills/agents compiled into the engine binary. Enumerated by a static const table; not filesystem-derived.

### 3.2 Manifest formats

#### 3.2.1 SKILL.md (skills)

A skill manifest is a Markdown file with a YAML frontmatter block. The frontmatter is the **machine-readable** portion; the Markdown body is the *human-readable* skill instructions injected into the system prompt at activation time (out of scope).

Example minimal skill manifest:

```markdown
---
name: python-pro
description: Pythonic-style code review and refactor suggestions for Python files.
tools:
  - shell
  - file_read
---

# Python Pro

When the user is working in Python, prefer ...
```

**Required frontmatter fields:**

| Field | Type | Notes |
|-------|------|-------|
| `name` | string | MUST match `[a-z][a-z0-9-]*` (lowercase, alphanumeric, hyphen). MUST equal the directory name. MUST be unique within scope. |
| `description` | string | One-line description. SHOULD be < 200 chars. Rendered in popup secondary text. |

**Optional frontmatter fields:**

| Field | Type | Notes |
|-------|------|-------|
| `tools` | string[] | List of permission scopes the skill requires. Used by permission gating (§3.5). Empty/absent → `[]`. |
| `aliases` | string[] | Alternate names. Each MUST satisfy the same regex as `name`. Aliases are matched in autocomplete prefix matching but the inserted token always uses the canonical `name`. |
| `enabled` | boolean | Default `true`. If `false`, the skill is excluded from the popup unless "show disabled" is toggled (out of scope; lives in SkillsPanel spec). |
| `priority` | number | Optional ranking boost; integer; higher = higher rank. Default 0. Range -1000..1000. |

**Forbidden frontmatter fields:** any field beginning with `_` (reserved for engine internals). Unknown fields are silently ignored (forward-compat).

#### 3.2.2 AGENT.md (agents)

Agent manifests are structurally identical to skill manifests, with these differences:

- `tools` field is **required**, not optional. An agent with no declared tools is a no-op and is rejected at load time with a diagnostic.
- An additional optional `model` field (string) names the preferred model. Default `null` → engine default.
- An additional optional `triggers` field (string[]) lists prefix phrases that hint when the agent is relevant for ranking purposes (§5). Not used for direct invocation.

The flat form `<scope>/agents/<name>.agent.md` is accepted as an alternative to `<scope>/agents/<name>/AGENT.md`. The flat form is preferred for agents that are pure prompt-only (no companion files); the directory form is preferred when the agent ships with helper scripts or templates. Both forms produce identical autocomplete catalog entries.

### 3.3 Discovery algorithm

For each scope root in (workspace, user):

1. Open the scope directory (`.github/skills/` or `~/.copilot/skills/`). If it does not exist or is not readable, treat as empty (no error).
2. Enumerate immediate children (one level deep only; the engine MUST NOT recurse).
3. For each child entry:
   - If it is a directory and contains `SKILL.md` (resp. `AGENT.md`), it is a candidate.
   - If the path is the agent flat form (a regular file matching `*.agent.md` directly under `~/.copilot/agents/` or `<workspace>/.github/agents/`), it is a candidate.
   - Other entries are ignored without diagnostic.
4. For each candidate: read the file, parse YAML frontmatter, validate required fields. On validation failure, emit a diagnostic (§7.4) and **skip** the entry; do not abort the scan.
5. After the scan, deduplicate within scope (two manifests claiming the same `name` within one scope is an error: keep the first encountered alphabetically and emit a diagnostic for the duplicate).
6. Cross-scope duplicates are resolved by the precedence rule in §6.

### 3.4 Manifest validation rules

A manifest is **valid** if and only if:

1. The file is UTF-8 decodable.
2. The frontmatter block is delimited by `^---$` lines and is parseable as YAML.
3. All required fields are present and of the correct type.
4. `name` matches the regex and equals the directory name (for directory form) or the filename stem before `.agent.md` (for agent flat form).
5. `tools[]` (if present) contains only known tool scope names (the engine ships a closed set; unknown tool scopes invalidate the manifest).
6. The Markdown body is at least 1 byte (empty bodies are allowed but trigger a `WARN`-level diagnostic).

Invalid manifests are **skipped**, not fatal. The autocomplete index proceeds with whatever validated successfully. This is **Z-5 (skip-not-fail manifest loading)** and is essential because a single corrupted skill on disk MUST NOT take down autocomplete for all skills.

### 3.5 Permission gating

The engine MUST filter the autocomplete catalog by the calling session's permission decisions before returning results to the host. Concretely:

- For each candidate skill/agent in the catalog, the engine consults the permission system (per `spec-m-permissions.md`) using the calling session's principal and the manifest's declared `tools[]` list.
- If the permission decision for any required tool is `Deny`, the skill/agent is **omitted** from the response. It MUST NOT appear in the popup at all — not greyed out, not labeled, not present. The user has no way to discover, via autocomplete, that a skill exists if they lack permission to use it.
- If the permission decision is `Prompt` or `AllowOnce`, the skill/agent is **included** in the popup; the prompt is deferred until the message is submitted.
- If the permission decision is `Allow`, the skill/agent is included.

This is **Z-6 (permission-denied items hidden, not shown-disabled)** and is a deliberate design choice: surfacing names of permission-denied skills would leak existence information that the permission system has already decided to withhold. The autocomplete popup is not a permission-discovery affordance.

### 3.6 Symbolic links and edge cases

- Symbolic links inside the scope roots ARE followed, but the engine MUST detect cycles (depth > 32 or reentry to a previously visited inode) and skip with diagnostic.
- Files larger than 1 MiB are skipped with diagnostic (a SKILL.md bigger than that is almost certainly not a skill).
- Files with mode bits forbidding read access are silently skipped (no diagnostic — this is a permission boundary, not a malformed-manifest case).


### 3.7 Manifest field summary table

| Field | SKILL.md | AGENT.md | Required | Used by autocomplete? |
|-------|----------|----------|----------|------------------------|
| `name` | yes | yes | yes | yes (token + filter target) |
| `description` | yes | yes | yes | yes (popup secondary text) |
| `tools[]` | yes | yes | optional / **required** for AGENT.md | yes (permission gate) |
| `aliases[]` | yes | yes | optional | yes (filter target only; canonical name still inserted) |
| `enabled` | yes | yes | optional, default true | yes (excluded if false) |
| `priority` | yes | yes | optional, default 0 | yes (ranking) |
| `model` | n/a | yes | optional | no (consumed by runner contract) |
| `triggers[]` | n/a | yes | optional | yes (ranking signal hint) |

---

## §4. Indexing & Refresh Model

### 4.1 In-memory index

The engine maintains an **in-memory autocomplete index** structured as:

```
AutocompleteIndex {
  skills:   Map<scope, Map<name, SkillEntry>>,
  agents:   Map<scope, Map<name, AgentEntry>>,
  by_name:  Map<(trigger, name), Vec<(scope, ref)>>,   // for collision detection
  loaded_at: Instant,
  generation: u64,
}
```

Each `Entry` is the parsed, validated manifest plus a denormalized fast-path subset (lowercase-name, lowercase-aliases, scope, priority).

The index generation number is incremented monotonically on every successful refresh (full or partial). Hosts MAY include a `since_generation` field in `autocomplete.query` requests as an ETag-like hint (§9.2); the engine MAY return a 304-equivalent (`E_INDEX_UNCHANGED`) to let the host skip re-rendering.

### 4.2 When the index is built

The index is built (re-built) on these triggers:

1. **Engine startup.** The first scan happens before the engine accepts its first RPC. RPCs received before the initial scan completes block until completion or fail with `E_NO_INDEX` after a 5-second timeout.
2. **Filesystem watch fires.** A debounced filesystem-watcher (§4.3) triggers partial or full re-scan.
3. **Explicit refresh RPC.** A host MAY send `autocomplete.refresh` (§9.4) to force a full rescan. This is intended for use after the user has just installed a new skill via a marketplace UI.
4. **Workspace change.** When the active workspace root changes (e.g. the user opens a new folder in caduceus-zed), the workspace-scope cache is invalidated and re-scanned.

### 4.3 Filesystem watching (debouncing)

The engine watches both scope roots (workspace and user) using the OS-native filesystem-watch API (kqueue / inotify / ReadDirectoryChangesW). Watched paths:

- The scope root directory itself (for create/delete of skill subdirectories).
- Each skill subdirectory's `SKILL.md` (for content changes).
- Each agent subdirectory's `AGENT.md` and the agents-flat-form pattern.

The watcher MUST debounce events with a window of 250ms (recommended; not strictly normative). The engine MUST coalesce events within the window and perform a single re-scan covering all affected entries.

A re-scan triggered by a watcher event MAY be **partial** (re-validating only the affected subdirectories) or **full** (re-scanning the entire root). Partial re-scans are an optimization; the engine MUST produce identical results from partial vs full re-scans (modulo timing).

### 4.4 Cold-start budget

The initial scan at engine startup MUST complete in less than 200ms wall-clock for a "typical" installation (defined as ≤ 50 skills + ≤ 50 agents combined across both scope roots). Implementations SHOULD parallelize file reads but MUST NOT spawn a thread pool larger than 4 workers (latency-vs-fork-cost tradeoff). If the scan exceeds 1000ms, the engine MUST log a `WARN`-level diagnostic with the per-entry timing breakdown.

This is **Z-7 (cold-start ≤ 200ms typical)** and is acceptance-tested in T-7.

### 4.5 Refresh semantics during in-flight queries

If a refresh (watcher-triggered or explicit) completes while an `autocomplete.query` RPC is in-flight, the in-flight query MUST observe one of:

- The pre-refresh index (acceptable; the host's next query will see the new state).
- The post-refresh index (acceptable).
- A consistent mix is **not** acceptable.

Implementations SHOULD use a copy-on-write or generation-pinned read of the index per query to guarantee consistency.

### 4.6 Cache invalidation rules

The cache for a given entry is invalidated when:

- The file's mtime changes (watcher-detected).
- The file's inode changes (watcher-detected; also handles "delete then recreate" as edit).
- The directory containing the file is renamed or deleted.
- The user invokes `autocomplete.refresh`.
- A workspace change makes the entry's scope root no longer current.

The cache MAY persist across engine restarts if the engine is configured for fast restart, but SHOULD validate persisted entries against current mtimes on startup. Treating the cache as ephemeral (rebuilt on every start) is acceptable and recommended for simplicity.

### 4.7 Refresh RPC vs implicit refresh

The host SHOULD NOT call `autocomplete.refresh` on every popup open. The engine's filesystem watcher is the primary mechanism. `autocomplete.refresh` is reserved for:

1. After the user has just performed an out-of-band install (e.g. cloning a skill repo into `~/.copilot/skills/` from a terminal).
2. After the user clicks an explicit "Refresh skills" button in the host UI.
3. As a recovery mechanism if the host detects index staleness via some other channel (e.g. `E_INDEX_UNCHANGED` returned for a generation that the host knows is stale).

Excessive `autocomplete.refresh` calls SHOULD be rate-limited by the engine to no more than 1 per 500ms; excess calls return `E_RATE_LIMITED`.

### 4.8 Per-session vs process-wide caching

The autocomplete index is a **process-wide** resource — there is one per engine process, not one per session. Multiple sessions may share the same workspace and observe the same workspace-scope entries. The permission-gated **view** onto the index (what each session sees in its popup) IS per-session, because the permission decisions are per-principal-per-session.

Concretely:

- The **catalog cache** (parsed manifests) is process-wide.
- The **rank-and-filter results** for a given (session, prefix) tuple are computed on demand per `autocomplete.query` and not cached.

Hosts MUST NOT assume that two sessions in the same engine process will return identical autocomplete results for the same prefix; permission gating may differ.

---

## §5. Ranking

### 5.1 Ranking signals

The engine ranks candidate matches using a small, deterministic, fixed set of signals. The signals are evaluated in priority order: a higher-priority signal entirely dominates a lower-priority one.

Signals (highest priority first):

1. **Match class** (categorical):
   - `prefix` — the candidate's `name` (or canonical alias) starts with the user's prefix (case-insensitive). Highest.
   - `word-prefix` — any whitespace- or hyphen-delimited word inside `name` or `description` starts with the prefix.
   - `substring` — the prefix occurs as a substring anywhere in `name`, `description`, or `aliases`. Lowest.
2. **Scope precedence**: `workspace` > `user` > `repo` > `builtin`. (This is the same ordering as §6's resolution precedence.)
3. **Manifest `priority` field** (integer, higher first).
4. **Recency boost**: candidates the user has selected in the last 7 days (per a per-host persisted MRU list) get a fixed +1 priority effective bonus. Recency MAY be disabled by host configuration (see §5.4).
5. **Conversation-context boost** (optional, if enabled): if the engine has access to a coarse intent classification of the current session/turn (e.g. "this conversation is about Python"), and a candidate's `description` or `triggers[]` matches the intent, it receives a fixed +0.5 priority bonus. This signal is *advisory*; engines MAY omit it. If implemented, it MUST be deterministic given the same inputs.
6. **Lexicographic tiebreak**: within ties, sort `name` ascending (case-insensitive ASCII).

### 5.2 Evaluation order

When two candidates differ in any higher-priority signal, the lower-priority signals are not consulted. This guarantees that ranking is a **total order** and that small changes to the lower-priority signals (e.g. recency) cannot reorder candidates that differ in match class.

### 5.3 Ranking is deterministic

For a fixed (catalog, prefix, principal, time-of-query, MRU list, conversation-context), the ranking output MUST be byte-identical across runs. Floating-point arithmetic is forbidden in the ranking implementation (use integer arithmetic only). Hash-randomized iteration over maps must not leak into the result; sort stable orders before returning.

This is **Z-8 (deterministic ranking)** and is acceptance-tested by replay tests in T-8.

### 5.4 Host-configurable signal weights

Hosts MAY pass a `ranking_profile` field in `autocomplete.query` (§9) drawn from a closed set:

| Profile | Description |
|---------|-------------|
| `default` | The signal ordering described in §5.1. |
| `no-recency` | Identical to `default` except the recency signal is disabled. |
| `alphabetical` | Match class still applies, but within match class, results are purely lexicographic; manifest priority and recency are ignored. Useful for deterministic UI screenshots. |

The set is closed: unknown profiles return `E_BAD_REQUEST`. New profiles require a spec amendment. This is **Z-9 (closed-set ranking profiles)**.

### 5.5 Pagination & page size

The engine returns up to `limit` results per query (default 32, max 100). For prefixes that match more candidates, the engine MUST return results in rank order and MAY include a `next_cursor` field for pagination. The popup is not expected to be infinite-scroll; hosts SHOULD render at most ~20 items at a time. The cursor is opaque to the host and tied to the index `generation` — if the generation changes between calls, the cursor MUST be discarded by the host (the engine returns `E_CURSOR_STALE`).

### 5.6 Empty-prefix ranking

When the prefix is empty (state `Open`), the popup shows the full visible-and-enabled catalog, ranked. The match-class signal is constant (all are "no-prefix-match"), so ranking falls through to scope precedence, manifest priority, recency, and lexicographic tiebreak in that order.

Hosts MAY truncate the empty-prefix popup to the top N candidates (recommended: 20) to avoid overwhelming the user. The ranking algorithm MUST still produce a deterministic full ordering; truncation is a UI concern.

### 5.7 Aliases and ranking

When a candidate matches via an alias, the *alias* is what was matched but the *canonical name* is what is displayed and inserted. The match class is computed against the alias that matched. Aliases never affect lexicographic tiebreak (only canonical `name` does).

If a candidate has multiple aliases that all match the prefix, the best (highest match class) one is used; the others are ignored.

### 5.8 Trigger-character isolation

A `/skill` query MUST never return agents, and an `@agent` query MUST never return skills. The trigger character is part of the query, not a free-text disambiguator. This is **Z-10 (trigger-isolated catalog)** and is essential for predictability.

### 5.9 Score is not surfaced

The engine MUST NOT include a numeric score in the response payload. Surfacing the score would cause hosts to invent secondary sort behavior, breaking determinism guarantees. Only the rank order is communicated.

---

## §6. Scope Disambiguation

### 6.1 Scope precedence (resolution)

When two manifests across scopes claim the same `name` (e.g. a workspace-scope `python-pro` and a user-scope `python-pro`), the user-facing default is **workspace wins**. The full precedence ladder (highest to lowest):

1. `workspace` — `<workspace>/.github/skills/...`
2. `user` — `~/.copilot/skills/...`
3. `repo` — repo-shipped (out of scope; per `spec-repo-owned-workflow-contract.md`)
4. `builtin` — engine-baked-in

The lower-precedence entries with the colliding name are **shadowed** but not deleted from the index. They remain accessible via fully-qualified-name selection (§6.4).

### 6.2 Visual disambiguation in the popup

When the popup has two or more candidates with the same `name` (across scopes), the secondary text in the popup MUST display the scope label in a deterministic location (e.g. trailing parenthetical):

```
python-pro          Pythonic-style code review (workspace)
python-pro          User's preferred Python style (user) [shadowed]
```

The `[shadowed]` affordance is a host-side UX recommendation; the engine returns `is_shadowed: bool` in each result.

When there is no collision, the scope label MAY be omitted from the popup for visual cleanliness; it is still available in tooltips / hover.

### 6.3 Token format on selection

For a non-collision case, selecting `python-pro` inserts `/python-pro `.

For a colliding case where the user selects the **shadowing** entry (top of the popup), the inserted token is the bare `/python-pro ` — the shadowing entry is, by definition, what `/python-pro` resolves to.

For a colliding case where the user selects the **shadowed** entry, the inserted token MUST be the fully-qualified form `/<scope>:<name> ` — e.g. `/user:python-pro `. This is **Z-11 (FQN required for shadowed selection)**.

### 6.4 Fully-qualified-name (FQN) selection by typing

Users MAY type a fully-qualified prefix to disambiguate without first opening the unqualified popup:

- `/workspace:py` → filters to workspace-scope skills with name prefix `py`.
- `/user:py` → filters to user-scope skills with name prefix `py`.
- `/repo:py` → filters to repo-scope skills (if any).
- `/builtin:py` → filters to builtin skills.

The same syntax applies to `@agent` triggers: `@workspace:code-reviewer`, etc.

If the scope qualifier is unrecognized, the popup MUST show the empty-state error "Unknown scope qualifier" (see §7.5). The set of valid scope qualifiers is closed: `workspace`, `user`, `repo`, `builtin`.

### 6.5 Insertion of FQN tokens

When a user selects an item that was matched via a fully-qualified prefix, the inserted token MUST be the FQN form (`/<scope>:<name> `), regardless of whether the entry is shadowed or not. The user typed the FQN; preserve their intent.

When a user selects an item that was matched via an unqualified prefix, and the entry is the shadowing (highest-precedence) entry for its name, the inserted token is the unqualified form (`/<name> `). When the entry is shadowed, the inserted token MUST be the FQN form (per §6.3). This is **Z-11** restated.

### 6.6 Engine resolution of tokens (post-submission)

Although out of scope for autocomplete proper, the engine resolves submitted tokens during message construction as follows (informative; not normative for this spec):

1. Tokens of the form `/<name>` resolve to the highest-precedence scope's entry by §6.1.
2. Tokens of the form `/<scope>:<name>` resolve to that exact scope's entry.
3. Tokens that fail to resolve produce a per-token diagnostic and are treated as plain text in the message.

The autocomplete subsystem does not perform this resolution; it only inserts text. The runner contract (for `@agent`) and the skill activation pipeline (for `/skill`) are responsible for resolution at submission time.

### 6.7 Cross-scope collisions on alias

If two manifests in different scopes share an alias (but not a canonical name), the alias-collision is resolved by the same precedence rule. The popup MAY display the alias-collision warning to surface the ambiguity to the user.

If two manifests **in the same scope** share a canonical name, this is a load-time error (§3.3 step 5) and one of them is dropped from the index. There is therefore no in-scope name collision case at autocomplete time.

---

## §7. Error & Empty States

The popup is an **opinionated** UI: when it has nothing to show, it tells the user *why*. The engine returns a structured error/empty-state code and a human-readable hint; the host renders both.

### 7.1 No skills installed (empty catalog)

Triggered when: the autocomplete catalog for the requested trigger character is empty for this principal in this session (no matching scopes have any visible entries).

- Engine returns: `result.items = []`, `result.empty_reason = "no_catalog"`.
- Host renders: a short popup with no list and the message: "No skills installed. See `<host-help-url>` for how to install skills." (Equivalent message for agents; help URL is host-specific.)
- The popup MUST remain open (not auto-close); the user can dismiss with Esc.

### 7.2 Permission-denied filtering produced empty result

Triggered when: the catalog is non-empty but every candidate was filtered out by §3.5's permission gate.

- Engine returns: `result.items = []`, `result.empty_reason = "all_denied"`.
- Host renders: "No skills available with current permissions. Open the permissions panel to review."
- The host MAY include a clickable affordance to open the permissions UI.

### 7.3 No matches for prefix

Triggered when: catalog is non-empty, permission filter passes some entries, but no entry matches the typed prefix.

- Engine returns: `result.items = []`, `result.empty_reason = "no_match"`, `result.suggested_prefix = <closest_prefix_with_results>` (optional).
- Host renders: "No skills match `<prefix>`." The host MAY display the suggested prefix as a clickable link.
- This is **not** an error; it is a normal zero-result state.

### 7.4 Malformed manifest at scan time

Triggered when: the manifest loader encounters a YAML parse error, missing required field, name regex mismatch, etc.

- The malformed manifest is **skipped** (Z-5). It does NOT cause the popup to fail.
- The engine logs a `WARN`-level diagnostic with the file path, line/column of the parse error if available, and a short description.
- The host MAY surface a "1 manifest skipped — see logs" toast at the host's discretion. This is non-normative; hosts that wish to be silent about manifest errors are conformant.
- The malformed manifest is NOT included in the popup, even with a "broken" annotation. Surfacing broken entries clutters the popup with non-actionable items.

### 7.5 Unknown scope qualifier in prefix

Triggered when: the user types `/foo:bar` where `foo` is not a recognized scope qualifier.

- Engine returns: `result.items = []`, `result.empty_reason = "unknown_scope"`, `result.unknown_scope = "foo"`.
- Host renders: "Unknown scope `foo`. Valid scopes: `workspace`, `user`, `repo`, `builtin`."

### 7.6 Filesystem unavailable

Triggered when: the engine's manifest loader cannot read either scope root (e.g. permission denied on `~/.copilot`, or workspace directory has been deleted).

- Engine logs `ERROR`-level diagnostic with the failed path.
- The corresponding scope is treated as empty; other scopes proceed normally.
- If both scopes fail, the catalog is empty and §7.1 applies.

### 7.7 Engine RPC error

Triggered when: the host's RPC to the engine returns an error other than the empty-state cases above (e.g. transport failure, version mismatch, internal error).

- Host MUST close the popup and display a host-level error toast: "Autocomplete unavailable: `<error_code>`."
- Host MUST NOT attempt to silently swallow the error; users need to know that autocomplete is broken.
- Host SHOULD retry the RPC at most once with exponential backoff before declaring failure.

### 7.8 Engine not yet ready (cold start)

If the host's first `autocomplete.query` arrives before the engine's initial manifest scan completes, the engine MUST block the RPC up to a per-implementation timeout (recommended 5 seconds). On timeout, the engine returns `E_NO_INDEX`; the host renders the popup as in §7.7.

### 7.9 Index unchanged (304-equivalent)

If the host's `autocomplete.query` includes a `since_generation` field equal to the current generation, **and** the result set would be byte-identical to the prior result, the engine MAY return `E_INDEX_UNCHANGED` instead of the result. The host treats this as "keep the current popup contents." This is an optional optimization; conformant engines MAY always return the full result.

### 7.10 Rate-limited refresh

If the host calls `autocomplete.refresh` too frequently (more than 1/500ms), the engine returns `E_RATE_LIMITED`. The host SHOULD NOT retry; the refresh will happen via the watcher anyway.

### 7.11 Empty-state error code summary

| `empty_reason` | Trigger | Host UX |
|----------------|---------|---------|
| `no_catalog` | No items in catalog | Suggest installing skills |
| `all_denied` | All items permission-denied | Suggest reviewing permissions |
| `no_match` | Prefix doesn't match anything | Show suggestion if available |
| `unknown_scope` | FQN scope unrecognized | Show valid scope list |

| Error code | Trigger | Host UX |
|------------|---------|---------|
| `E_NO_INDEX` | Engine initial scan not complete | Show "Loading..." then error |
| `E_INDEX_UNCHANGED` | 304-equivalent | Reuse cached items |
| `E_RATE_LIMITED` | Refresh called too often | Silent no-op |
| `E_BAD_REQUEST` | Malformed query payload | Internal error toast |
| `E_CURSOR_STALE` | Pagination cursor from old generation | Restart pagination from page 0 |
| `E_PERMISSION_DENIED` | Caller cannot use autocomplete at all (rare) | Disable popup entirely |


---

## §8. Cross-Runtime Parity

### 8.1 Hosts in scope

This specification is normative for at least three host implementations:

1. **caduceus-zed** — the Zed-fork host. `agent_ui::message_editor` + `agent_ui::completion_provider`.
2. **VS Code Copilot Chat host** — the Visual Studio Code extension. The completion provider integrates with VS Code's chat UI primitives.
3. **Copilot CLI host** — the terminal-based host. Trigger detection is on a single-line prompt buffer; popup rendering is via terminal-positioned overlay.

Future hosts (e.g. a JetBrains plugin, a web-based host) MUST also conform.

### 8.2 What MUST be identical across hosts

The following behaviors are normative parity requirements. Conformance tests in §11 SHOULD be runnable as a black-box test suite that exercises any host:

| Behavior | Parity requirement |
|----------|---------------------|
| Trigger characters | `/` and `@` only. |
| Word-boundary trigger discipline | Exactly the rules in §2.3. |
| Filter prefix character class | `[a-zA-Z0-9_:\-]`. |
| Keyboard contract | Per §2.7. |
| Selection inserts token only | Z-2. No body injection in any host. |
| Token format | `/<name> ` or `/<scope>:<name> ` (skills); `@<name> ` or `@<scope>:<name> ` (agents). Trailing space mandatory. |
| Ranking output | Z-8 deterministic ranking; identical bytes for identical inputs across hosts. |
| Permission gating | Z-6 hidden, not shown-disabled. |
| Empty-state messages | Same `empty_reason` codes; visual presentation MAY differ but messages MUST convey the same information. |
| Wire format | §9 — identical RPC names, payload shapes, error codes. |

### 8.3 What MAY differ across hosts

| Aspect | Permitted variation |
|--------|---------------------|
| Popup visual chrome (font, color, border, animation) | Free; subject to host's design system. |
| Popup width / max-height | Free, subject to readability. |
| Truncation of long descriptions | Free. |
| Whether `[shadowed]` is shown as text, badge, or icon | Free. |
| Mouse interaction (hover preview, right-click menu) | Free; not specified. |
| Scrollbar styling, arrow-key animation | Free. |
| Error toast styling | Free. |
| Page size truncation for empty-prefix popup | Free, recommended ~20. |

### 8.4 Engine is the same

All hosts connect to the **same** caduceus engine binary (or a host-bundled copy thereof, of the same version). The engine implementation is shared; per-host engine forks are **forbidden** and would be a conformance failure.

This single-engine constraint is what makes parity tractable: the catalog source, the manifest parser, the permission gate, the ranker, and the wire-format serializer are one implementation. The host only renders.

### 8.5 Version negotiation

Hosts and engines negotiate a wire-format version at handshake (per `spec-cross-cutting-wiring.md`). Both sides MUST refuse to operate if they cannot agree on a version. The autocomplete RPCs are versioned together with the rest of the wire format.

A host running an older wire-format version than the engine supports MAY still get autocomplete results, but newer features (e.g. `triggers[]` ranking signal, FQN selection) MAY be unavailable. The engine MUST gracefully degrade by silently dropping fields the host cannot understand.

### 8.6 Conformance test suite

The caduceus repository MUST publish a host-conformance test harness that:

1. Spins up a known-good engine with a fixed-content manifest fixture.
2. Drives the host under test through a scripted set of keystrokes.
3. Compares the popup contents (both items and empty-state messages) against a golden file.
4. Asserts the inserted token byte-for-byte after selection.

The harness is normative for what to test; the implementation language is host-specific (the caduceus-zed harness is in Rust, the VS Code harness in TypeScript). A new host's implementation is conformant when it passes the host-conformance test suite at version N of the suite.

### 8.7 Host-specific implementation notes (informative)

#### 8.7.1 caduceus-zed

- Trigger state machine lives in `agent_ui::message_editor` as a substate of the editor.
- Popup is a Zed `Popover` widget rendered by `agent_ui::completion_provider`.
- The completion provider is **not** the generic Zed code-completion provider — chat-composer autocomplete is its own subsystem to avoid conflicts with code completion.

#### 8.7.2 VS Code Copilot Chat host

- The completion provider hooks into the VS Code Chat input via the chat extension API.
- VS Code's completion-provider trigger characters are configured to include `/` and `@`.
- The popup uses VS Code's native suggestion widget; this constrains some of the visual freedom.

#### 8.7.3 Copilot CLI host

- The CLI's prompt is a single-line readline-style buffer.
- The popup is rendered as a terminal overlay using ANSI cursor positioning.
- Arrow-key handling is more constrained than GUI hosts; PageUp/PageDown MAY be unavailable.

---

## §9. Wire Format (RPC between Host and Engine)

### 9.1 Transport assumptions

This spec assumes the host and engine communicate over the framing transport defined in `spec-cross-cutting-wiring.md`. Concretely:

- Each RPC is a request-response pair.
- Messages are serialized as JSON (canonical caduceus encoding) by default; alternate encodings (CBOR, MessagePack) MAY be negotiated at handshake.
- Field names are `snake_case`.
- Unknown fields are silently ignored on receive (forward-compat).
- The transport guarantees in-order delivery within a connection but does NOT guarantee in-order completion (the engine MAY process queries concurrently and return out of order; the host's monotonic-sequence handling per §2.10 covers this).

### 9.2 Method: `autocomplete.query`

**Direction:** host → engine.

**Request payload:**

```jsonc
{
  "session_id": "sess-abc-123",        // string, required; identifies the session for permission gating
  "trigger": "/",                      // string, required; "/" or "@"
  "prefix": "py",                      // string, required; may be empty for state Open; lowercase
  "scope_hint": "workspace",           // string, optional; one of "workspace"|"user"|"repo"|"builtin"
                                       //   if present, only this scope is queried (FQN behavior)
  "limit": 32,                         // integer, optional; default 32, max 100
  "cursor": "opaque-base64",           // string, optional; pagination
  "since_generation": 142,             // integer, optional; ETag-equivalent
  "ranking_profile": "default",        // string, optional; one of "default"|"no-recency"|"alphabetical"
  "context_hints": {                   // object, optional; may be omitted
    "active_file_extension": ".py",
    "active_repo": "github.com/user/repo",
    "recent_topics": ["python", "testing"]
  },
  "request_seq": 17                    // integer, required; monotonically increasing per host connection
}
```

**Response payload (success):**

```jsonc
{
  "request_seq": 17,                   // echoed; host uses to drop stale responses
  "generation": 143,                   // integer; engine's index generation at time of query
  "items": [
    {
      "name": "python-pro",
      "scope": "workspace",
      "description": "Pythonic-style code review and refactor suggestions for Python files.",
      "match_class": "prefix",         // "prefix" | "word-prefix" | "substring"
      "matched_via": "name",           // "name" | "alias" | "description"
      "is_shadowed": false,
      "fqn_required_on_select": false, // host: if true, insert "/scope:name " not "/name "
      "trigger": "/",
      "token": "/python-pro "          // engine-canonical token to insert; host MUST use this verbatim
    },
    // ... up to `limit` items
  ],
  "next_cursor": "opaque-base64",      // string, optional; absent if no more results
  "empty_reason": null                 // string|null; one of "no_catalog"|"all_denied"|"no_match"|"unknown_scope"|null
}
```

**Response payload (empty but not error):**

`items` is `[]`; `empty_reason` is non-null per §7.

**Response payload (error):**

```jsonc
{
  "request_seq": 17,
  "error": {
    "code": "E_NO_INDEX",              // see §7.11
    "message": "Initial manifest scan in progress.",
    "retry_after_ms": 200              // integer, optional
  }
}
```

### 9.3 Method: `autocomplete.select`

**Optional, advisory.** A host MAY notify the engine that the user has selected an item, so the engine can update its MRU list (used for the recency boost in §5.1).

**Direction:** host → engine.

**Request payload:**

```jsonc
{
  "session_id": "sess-abc-123",
  "trigger": "/",
  "name": "python-pro",
  "scope": "workspace",
  "selected_at_ms": 1730000000000      // wall-clock ms since epoch
}
```

**Response payload (success):**

```jsonc
{
  "ok": true
}
```

The host MUST NOT block popup-close on this RPC's response. Send-and-forget semantics are appropriate. If the response indicates an error, the host SHOULD log it and continue; selection has already happened in the editor.

This RPC is OPTIONAL because not all hosts maintain a per-host MRU; engines that don't implement recency MAY treat the RPC as a no-op.

### 9.4 Method: `autocomplete.refresh`

**Direction:** host → engine.

**Request payload:**

```jsonc
{
  "scope": "user"                      // string, optional; if absent, refresh all scopes
}
```

**Response payload (success):**

```jsonc
{
  "ok": true,
  "new_generation": 144,
  "skills_count": 23,
  "agents_count": 11,
  "skipped_count": 2                   // count of malformed manifests skipped
}
```

**Response payload (error):**

```jsonc
{
  "error": {
    "code": "E_RATE_LIMITED",
    "message": "Refresh rate-limited; try again later.",
    "retry_after_ms": 500
  }
}
```

### 9.5 Method: `autocomplete.subscribe` (server-push, optional)

**Direction:** engine → host (server-initiated push within an established host-side subscription).

A host MAY subscribe to be notified of catalog generation changes. Useful when the host wants to refresh the popup if it is currently open and the underlying catalog has changed (e.g. user edited a SKILL.md in another window).

**Subscription request (host → engine):**

```jsonc
{
  "method": "autocomplete.subscribe",
  "session_id": "sess-abc-123"
}
```

**Push payload (engine → host):**

```jsonc
{
  "method": "autocomplete.generation_changed",
  "new_generation": 144,
  "session_id": "sess-abc-123"
}
```

The host SHOULD, on receiving this push, re-issue its most recent `autocomplete.query` with `since_generation = old_generation` if the popup is still open. If the popup is closed, the host MAY ignore the push.

### 9.6 Field invariants

| Field | Invariant |
|-------|-----------|
| `session_id` | MUST be a session known to the engine; otherwise return `E_BAD_REQUEST`. |
| `trigger` | Closed set: `/`, `@`. Other values return `E_BAD_REQUEST`. |
| `prefix` | MUST be lowercase ASCII (host normalizes before send). Unicode is reserved for future use. |
| `scope_hint` | Closed set: `workspace`, `user`, `repo`, `builtin`. Unknown values return `E_BAD_REQUEST`. |
| `limit` | 1 ≤ limit ≤ 100. Out-of-range returns `E_BAD_REQUEST`. |
| `request_seq` | MUST monotonically increase per-connection. Engine MAY (but is not required to) detect non-monotonic requests and reject with `E_BAD_REQUEST`. |
| `token` (in response) | Host MUST insert this verbatim. Host MUST NOT compose its own token from `name`+`scope`. |

### 9.7 Why `token` is engine-determined

The engine returns the canonical token text to insert because:

1. It encodes the FQN-vs-unqualified decision (§6.5) — the host shouldn't replicate this logic.
2. It encodes the trailing-space convention.
3. It allows future evolution (e.g. quoted names with special characters) without host changes.

This is **Z-13 (engine-canonical token)**. Hosts that compose their own token from `name`+`scope` are non-conformant.

### 9.8 Connection lifecycle

The autocomplete subsystem assumes a long-lived host↔engine connection. If the connection drops:

- Any in-flight `autocomplete.query` is failed at the host with a transport error.
- The host MUST close any open popup and display the standard "Autocomplete unavailable" toast (§7.7).
- Reconnection logic is owned by the wiring layer (`spec-cross-cutting-wiring.md`).

### 9.9 Backwards-compatibility policy

When a new field is added to a request or response payload:

- Engines MUST silently ignore unknown request fields (forward-compat from old hosts).
- Hosts MUST silently ignore unknown response fields (forward-compat from old engines).
- New empty-state codes added to `empty_reason` MUST be mappable to `no_match` by old hosts (i.e. always include `no_match`-equivalent semantics in any new code, or accept that old hosts will display a generic message).
- New error codes MUST be mappable to a generic transport error by old hosts.

When a field's *meaning* changes, the wire-format version MUST be bumped (per `spec-cross-cutting-wiring.md`).

---

## §10. Acceptance Criteria & Invariants

This section enumerates the **load-bearing properties** of the system that conformance tests MUST verify. Each invariant has a stable identifier (Z-N) and is tied to one or more test obligations (T-N) in §11.

### 10.1 Z-numbered invariants

The Z prefix is the project's namespace for testable load-bearing properties shared with `spec-caduceus-agent-runner-contract.md` and `spec-m-session-lifecycle.md`. Z-IDs in this spec do not collide with those of other specs; the autocomplete spec uses Z-1..Z-30 (reserved range), with currently allocated IDs Z-1..Z-15.

| ID | Invariant | Tied test |
|----|-----------|-----------|
| **Z-1** | **Separation of concerns.** The host MUST NOT scan the filesystem for skill or agent manifests. The engine MUST NOT render UI. | T-1 |
| **Z-2** | **Selection inserts token only.** Popup selection inserts exactly the engine-returned `token` string (e.g. `/python-pro `) into the editor; it MUST NOT inject the skill body, agent prompt, or any other content. | T-2 |
| **Z-3** | **Word-boundary trigger discipline.** `/` and `@` MUST open the popup only at word boundaries (per §2.3). Mid-word triggers (e.g. `src/main.rs`, `alice@example.com`) MUST NOT open the popup. | T-3 |
| **Z-4** | **Monotonic response ordering.** The popup MUST never display a result for an older prefix after a result for a newer prefix has been displayed. The host's request-sequence-number protocol enforces this. | T-4 |
| **Z-5** | **Skip-not-fail manifest loading.** A malformed (unparseable, missing-field, regex-violating, etc.) manifest MUST be skipped with a diagnostic. It MUST NOT cause the catalog scan to fail or be incomplete for unrelated entries. | T-5 |
| **Z-6** | **Permission-denied items hidden, not shown-disabled.** A skill or agent for which the calling principal lacks permission MUST be omitted from `items[]`. It MUST NOT appear greyed out, labeled, or otherwise present. | T-6 |
| **Z-7** | **Cold-start ≤ 200ms typical.** Initial manifest scan at engine startup completes in ≤ 200ms wall-clock for a typical (≤50 skills + ≤50 agents) installation. | T-7 |
| **Z-8** | **Deterministic ranking.** For fixed (catalog, prefix, principal, time, MRU, context), the ranked output is byte-identical across runs. No hash randomization, no floating-point. | T-8 |
| **Z-9** | **Closed-set ranking profiles.** `ranking_profile` accepts only `default`, `no-recency`, `alphabetical`. Unknown profiles return `E_BAD_REQUEST`. | T-9 |
| **Z-10** | **Trigger-isolated catalog.** A `/skill` query MUST NEVER return agents; an `@agent` query MUST NEVER return skills. | T-10 |
| **Z-11** | **FQN required for shadowed selection.** When the user selects a shadowed entry, the inserted token MUST be the fully-qualified form (`/<scope>:<name> `). When the user selects via FQN-typed prefix, the inserted token MUST be FQN regardless of shadow status. | T-11 |
| **Z-12** | **Cross-trigger semantic parity.** The composer MUST implement a single trigger state machine, ranking algorithm, and popup widget; `/` and `@` are parameterizations of the same code path, not parallel implementations. | T-12 |
| **Z-13** | **Engine-canonical token.** The host MUST insert the `token` field from the response verbatim. The host MUST NOT synthesize its own token from `name` + `scope`. | T-13 |
| **Z-14** | **Local-over-global precedence.** When two manifests share a `name` across `workspace` and `user` scopes, the workspace entry shadows the user entry. Both remain in the catalog; only the workspace entry resolves for the unqualified `/<name>` token. | T-14 |
| **Z-15** | **Empty-state messages distinguishable.** The four empty-reason codes (`no_catalog`, `all_denied`, `no_match`, `unknown_scope`) MUST be returned distinctly by the engine and MUST be rendered with messaging that distinguishes them in the host. | T-15 |

### 10.2 I-numbered implementation invariants

The I prefix is reserved for internal implementation invariants — properties that are non-observable from the host but constrain the engine's internal structure. They are testable via unit tests in the engine.

| ID | Invariant |
|----|-----------|
| **I-1** | The autocomplete index data structure is immutable from the perspective of in-flight queries. Generations are bumped atomically; queries take a snapshot. |
| **I-2** | The manifest loader uses the same YAML parser configuration for SKILL.md and AGENT.md. There is exactly one parser instance. |
| **I-3** | The permission gate is invoked with the manifest's declared `tools[]` list verbatim. The autocomplete subsystem MUST NOT add or remove tools from the list before consulting the permission system. |
| **I-4** | The filesystem watcher uses a single shared instance for both scope roots, with debouncing implemented in user-space (not relying on OS coalescing). |
| **I-5** | The per-session MRU is stored in volatile session state (per `spec-m-session-lifecycle.md`); it does NOT persist across engine restarts unless the host opts in. |
| **I-6** | The ranking algorithm is implemented as a pure function: `(items, prefix, profile, mru, context) → ordered_items`. It MUST NOT have side effects. |
| **I-7** | The token assembly logic (deciding bare vs FQN) is a single pure function consulted by both `autocomplete.query` (to populate `token`) and the engine's submission-time token resolver (to validate that submitted tokens match the catalog). |
| **I-8** | The autocomplete subsystem holds NO mutable state across RPCs other than: the index itself (generation-bumped), the per-session MRU (selection-event-driven), and rate-limiting counters. |

### 10.3 Acceptance criteria for shippability

The autocomplete feature is **shippable** when the following are all true:

1. All Z-1..Z-15 invariants pass their corresponding T-N tests on caduceus and caduceus-zed.
2. The host-conformance test suite passes for caduceus-zed.
3. Cold-start performance (Z-7) is verified on at least one developer-class workstation per supported OS (macOS, Linux, Windows).
4. The empty-state UX has been reviewed by a designer for tone and clarity (non-normative; this is a courtesy gate, not a correctness one).
5. No `panic!` / unwrap-on-untrusted-input paths exist in the manifest loader (audited via static analysis).
6. The wire format is documented in the public engine reference at the version being shipped.


---

## §11. Testable Scenarios

This section enumerates the test obligations T-1..T-15 corresponding to invariants Z-1..Z-15. Each test is described as a Given/When/Then scenario suitable for translation into a deterministic test in the host or engine test suite.

### T-1: Separation of concerns

**Z-1.** Verifies host does not scan filesystem; engine does not render UI.

| Step | Detail |
|------|--------|
| Given | The engine is started with a fixture catalog (3 skills, 2 agents). |
| And | A network-monitoring shim is interposed between host and engine to observe RPCs. |
| And | An fs-monitoring shim is interposed in the host to observe filesystem reads. |
| When | The host opens a chat session and the user types `/`. |
| Then | The host MUST NOT have read any path under `~/.copilot/skills/` or `<workspace>/.github/skills/`. |
| And | The host MUST have issued exactly one `autocomplete.query` RPC. |
| And | The engine MUST NOT have rendered any UI primitives (the engine is headless; trivially satisfied — verified by code review, not runtime). |

### T-2: Selection inserts token only

**Z-2.** Verifies popup selection inserts only the canonical token.

| Step | Detail |
|------|--------|
| Given | Catalog contains skill `python-pro` with body "When the user is working in Python, prefer pep8..." |
| And | The composer is empty. |
| When | User types `/py`, popup opens, highlights `python-pro`, presses Enter. |
| Then | The composer text MUST be exactly `/python-pro ` (trailing space). |
| And | The composer text MUST NOT contain any of the skill body. |
| And | The composer text MUST NOT contain the description string. |
| And | The composer's caret MUST be positioned immediately after the trailing space. |

### T-3: Word-boundary trigger discipline

**Z-3.** Verifies mid-word `/` and `@` do not trigger.

| Step | Detail |
|------|--------|
| Given | Composer contains the text `Look at src` with the caret at end. |
| When | User types `/main.rs`. |
| Then | The popup MUST NOT open at any point during this typing. |
| And | The composer text is `Look at src/main.rs`. |
| Given | Composer contains the text `Email alice` with the caret at end. |
| When | User types `@example.com`. |
| Then | The popup MUST NOT open. |
| Given | Composer contains the text `Run ` with the caret at end (note trailing space). |
| When | User types `/`. |
| Then | The popup MUST open. |
| Given | Composer is empty. |
| When | User types `/`. |
| Then | The popup MUST open. |

### T-4: Monotonic response ordering

**Z-4.** Verifies stale responses are dropped.

| Step | Detail |
|------|--------|
| Given | The engine is configured to artificially delay responses: `prefix=p` delays 100ms, `prefix=py` delays 10ms. |
| When | User rapidly types `p` then `y` (within ~5ms). |
| Then | The popup MUST display the result for `py` only. |
| And | The popup MUST NOT briefly flash the result for `p` after `py`'s result has rendered. |
| And | The host's request-sequence-number tracker MUST have discarded the late `p` response. |

### T-5: Skip-not-fail manifest loading

**Z-5.** Verifies one bad manifest does not poison the catalog.

| Step | Detail |
|------|--------|
| Given | `~/.copilot/skills/good/SKILL.md` is a valid manifest with name `good`. |
| And | `~/.copilot/skills/bad/SKILL.md` has malformed YAML (unterminated string). |
| And | `~/.copilot/skills/also-good/SKILL.md` is valid with name `also-good`. |
| When | Engine starts and scans. |
| Then | The catalog MUST contain `good` and `also-good`. |
| And | The catalog MUST NOT contain `bad`. |
| And | A `WARN`-level diagnostic citing the bad manifest's path and parse-error line MUST be present in the engine log. |
| When | User types `/`. |
| Then | The popup MUST show `good` and `also-good`. |

### T-6: Permission-denied items hidden

**Z-6.** Verifies permission-denied skills are absent from popup.

| Step | Detail |
|------|--------|
| Given | Catalog contains `safe-skill` (no tools) and `dangerous-skill` (`tools: [shell, network]`). |
| And | Permission system is configured to deny `network` for the calling principal. |
| When | User types `/`. |
| Then | The popup MUST contain `safe-skill`. |
| And | The popup MUST NOT contain `dangerous-skill`. |
| And | The popup MUST NOT show any "permission denied" indicator for `dangerous-skill`. |
| And | A user looking at the popup MUST be unable to discover the existence of `dangerous-skill` from the popup alone. |

### T-7: Cold-start performance

**Z-7.** Verifies initial scan completes within budget.

| Step | Detail |
|------|--------|
| Given | A fixture installation with 50 skill manifests + 50 agent manifests across both scope roots. |
| And | Each manifest is a "typical" size (1–5 KiB). |
| And | The host machine is a developer-class workstation (Apple M1 / Intel i5 / equivalent). |
| When | The engine starts. |
| Then | The first `autocomplete.query` RPC issued ≥10ms after engine start MUST receive a non-`E_NO_INDEX` response within 200ms wall-clock from engine-start. |
| And | The engine log MUST NOT contain a `WARN`-level "slow scan" diagnostic. |

### T-8: Deterministic ranking

**Z-8.** Verifies ranking is byte-identical across runs.

| Step | Detail |
|------|--------|
| Given | A fixture catalog of 25 skills with mixed match-classes for prefix `te`. |
| And | A fixed MRU list and a fixed conversation context. |
| When | The host issues `autocomplete.query` 100 times in sequence. |
| Then | All 100 responses MUST have byte-identical `items[]` arrays (up to the response-level `request_seq` and `generation` fields, which are expected to differ). |
| When | The engine is restarted and the same 100 queries are issued. |
| Then | The 100 responses MUST be byte-identical to the prior run's responses (with the same sequence/generation caveat). |

### T-9: Closed-set ranking profiles

**Z-9.** Verifies unknown ranking profiles are rejected.

| Step | Detail |
|------|--------|
| Given | The engine is running. |
| When | The host issues `autocomplete.query` with `ranking_profile = "frecency"` (not in the closed set). |
| Then | The engine MUST return `E_BAD_REQUEST` with a message naming the invalid field. |
| When | The host issues `autocomplete.query` with `ranking_profile = "default"`, `"no-recency"`, or `"alphabetical"`. |
| Then | The engine MUST return a normal response. |

### T-10: Trigger-isolated catalog

**Z-10.** Verifies skills and agents do not bleed across triggers.

| Step | Detail |
|------|--------|
| Given | Catalog has skill `cool` and agent `cool` (same name, different trigger). |
| When | User types `/co`. |
| Then | The popup MUST contain skill `cool`. |
| And | The popup MUST NOT contain agent `cool`. |
| When | User types `@co`. |
| Then | The popup MUST contain agent `cool`. |
| And | The popup MUST NOT contain skill `cool`. |

### T-11: FQN required for shadowed selection

**Z-11.** Verifies shadowed entries get FQN tokens.

| Step | Detail |
|------|--------|
| Given | Workspace catalog has `python-pro` (description "Workspace pro"). |
| And | User catalog has `python-pro` (description "User pro"). |
| When | User types `/py`. |
| Then | The popup shows two entries: `python-pro (workspace)` (top, not shadowed) and `python-pro (user) [shadowed]`. |
| When | User selects the workspace entry. |
| Then | Inserted token is `/python-pro `. |
| When | User again types `/py` and selects the user (shadowed) entry. |
| Then | Inserted token is `/user:python-pro `. |
| When | User types `/user:py` and selects `python-pro`. |
| Then | Inserted token is `/user:python-pro ` (FQN preserved). |
| When | User types `/workspace:py` and selects `python-pro`. |
| Then | Inserted token is `/workspace:python-pro ` (FQN preserved even though it would resolve unqualified). |

### T-12: Cross-trigger semantic parity

**Z-12.** Verifies single-implementation discipline.

| Step | Detail |
|------|--------|
| Given | The host's `agent_ui` crate (or equivalent in another host). |
| When | A code review or static analysis examines the trigger state machine, ranking client logic, and popup widget. |
| Then | There MUST be a single implementation parameterized by trigger character. |
| And | There MUST NOT be two parallel state machines (one for `/`, one for `@`). |
| And | The popup widget MUST handle both triggers without a `match trigger { '/' => ..., '@' => ... }` branch on rendering logic (configuration data, like the trigger character itself, is fine). |

This test is **structural** rather than runtime; it is a code-review / lint check.

### T-13: Engine-canonical token

**Z-13.** Verifies host uses engine-returned token verbatim.

| Step | Detail |
|------|--------|
| Given | The engine is configured (in test) to return a token field with an unusual format, e.g. `/python-pro\n` (trailing newline instead of space). |
| When | User selects the entry in the popup. |
| Then | The inserted text MUST be exactly `/python-pro\n`. |
| And | The host MUST NOT have synthesized its own token. |

This is a regression-guard: in production, the engine never returns weird tokens, but the host MUST be willing to pass through whatever it gets.

### T-14: Local-over-global precedence

**Z-14.** Verifies workspace shadows user.

| Step | Detail |
|------|--------|
| Given | `~/.copilot/skills/foo/SKILL.md` has description "User foo". |
| And | `<workspace>/.github/skills/foo/SKILL.md` has description "Workspace foo". |
| When | User types `/fo` and selects the top entry. |
| Then | Inserted token is `/foo `. |
| When | The user submits the message and the engine resolves `/foo`. |
| Then | The resolved entry's description is "Workspace foo". |
| And | The user-scope `foo` entry is still present in the catalog as a shadowed entry, retrievable via `/user:foo`. |

### T-15: Empty-state distinguishability

**Z-15.** Verifies four empty-reason codes are visually distinct.

| Step | Detail |
|------|--------|
| Given | Four test scenarios, one per `empty_reason`. |
| **Scenario A** (`no_catalog`) | Engine started with empty scope roots. User types `/`. Popup shows "No skills installed" message. |
| **Scenario B** (`all_denied`) | Catalog has 3 skills, all permission-denied for principal. User types `/`. Popup shows "No skills available with current permissions" message. |
| **Scenario C** (`no_match`) | Catalog has skills `apple`, `banana`. User types `/zzz`. Popup shows "No skills match `zzz`" message. |
| **Scenario D** (`unknown_scope`) | User types `/foo:bar`. Popup shows "Unknown scope `foo`" message with valid scope list. |
| Then | Each scenario's rendered popup text MUST be distinguishable from each other scenario's by simple substring inspection. |

### 11.1 Property-based test obligations

In addition to the scenario tests above, conformant implementations SHOULD include property-based tests for the following:

- **Manifest loader fuzzing.** Random byte sequences fed to the manifest parser MUST never panic the engine; they MUST produce either a valid parse, a `WARN` diagnostic, or a malformed-skip outcome.
- **Ranking idempotence.** Ranking the same input twice MUST produce identical output (Z-8).
- **Trigger-state-machine fuzzing.** Random keystroke streams (drawn from a realistic distribution) MUST never leave the state machine in `Selected` for more than 1 event-loop tick, never open the popup mid-word, and never insert content other than canonical tokens on selection.
- **FQN round-trip.** For any `(scope, name)` in the catalog, typing `/<scope>:<name>` MUST resolve to that entry; selecting it MUST insert exactly `/<scope>:<name> `.

### 11.2 Test fixture conventions

Test fixtures used by the conformance suite live in `caduceus/tests/fixtures/autocomplete/`:

- `catalog-typical/` — 50 skills + 50 agents, diverse names.
- `catalog-empty/` — empty roots.
- `catalog-malformed/` — mix of valid and malformed manifests.
- `catalog-collisions/` — entries colliding across scopes.
- `catalog-permissions/` — entries requiring various tool scopes for permission-gate tests.

Each fixture is a hermetic directory tree consumed by the test harness via a temp-root mechanism (no global state).

---

## §12. Glossary

| Term | Definition |
|------|------------|
| **Autocomplete index** | The engine's in-memory data structure holding parsed, validated skill and agent manifests, used to serve `autocomplete.query`. |
| **Catalog** | Synonym for autocomplete index; the set of available skills + agents. |
| **Composer** | The chat input editor in the host UI where users type messages. |
| **Engine** | The caduceus daemon process providing autocomplete (and other) RPCs. |
| **FQN** (fully-qualified name) | A skill/agent name written with an explicit scope qualifier, e.g. `/user:python-pro` or `@workspace:reviewer`. |
| **Generation** | A monotonically-increasing integer identifying the version of the autocomplete index. Bumped on every refresh. |
| **Host** | A user-facing application that connects to the engine over RPC and renders the popup. Examples: caduceus-zed, VS Code chat extension, Copilot CLI. |
| **Manifest** | A `SKILL.md` or `AGENT.md` file with YAML frontmatter declaring the skill/agent's metadata. |
| **Match class** | The categorical signal in ranking: `prefix`, `word-prefix`, or `substring`. |
| **MRU** (most-recently-used) | A per-host, per-principal list of recently-selected skills/agents, used as a recency boost in ranking. |
| **Popup** | The list widget rendered by the host when the user types a trigger character. |
| **Prefix** | The substring typed by the user after the trigger character; used to filter the catalog. |
| **Principal** | The identity of the user/session for which permission decisions are made. Drawn from `spec-m-permissions.md`. |
| **Ranking profile** | A named ranking configuration, drawn from a closed set: `default`, `no-recency`, `alphabetical`. |
| **Scope** | The provenance of a manifest: `workspace`, `user`, `repo`, `builtin`. |
| **Shadowed** | A catalog entry that is hidden from default resolution because a higher-precedence entry has the same name. Still present in the catalog; reachable via FQN. |
| **Token** | The text inserted into the composer when the user selects an entry: `/<name> ` or `@<name> ` (or the FQN form). |
| **Trigger character** | `/` (skills) or `@` (agents) — the character that opens the popup at a word boundary. |
| **Trigger state machine** | The four-state machine (`Closed`, `Open`, `Filtering`, `Selected`) governing the composer's autocomplete behavior. |
| **Word boundary** | Position 0 of the editor, or position immediately after whitespace or non-word punctuation. The set of word-boundary positions is defined in §2.3. |
| **Wire format** | The JSON (or alternate-encoded) RPC messages exchanged between host and engine for autocomplete, defined in §9. |

---

## §13. Out of Scope / Deferred

The following topics are explicitly **NOT** part of this specification. They are owned elsewhere or deferred:

### 13.1 Owned by other specs

| Topic | Owning spec |
|-------|-------------|
| Permission decision engine (Allow/Deny/Prompt semantics) | `spec-m-permissions.md` |
| Agent invocation runtime (what happens when `@agent` is in a submitted message) | `spec-caduceus-agent-runner-contract.md` |
| Skill activation runtime (how `/skill` becomes part of the system prompt) | (forthcoming `spec-skill-activation.md`) |
| Session lifecycle (what counts as a "session") | `spec-m-session-lifecycle.md` |
| RPC framing, version negotiation, transport reconnect | `spec-cross-cutting-wiring.md` |
| Marketplace browse / install UX | (forthcoming `spec-skill-marketplace.md`) |
| SkillsPanel (per-session enable/disable UI) | (forthcoming `spec-skills-panel.md`) |
| Repo-shipped skills (manifest declaration in repo config) | `spec-repo-owned-workflow-contract.md` |

### 13.2 Deliberately deferred to future revisions

| Topic | Rationale |
|-------|-----------|
| Unicode-aware prefix matching | v1 is ASCII-only for predictable lowercase normalization. Unicode requires NFC normalization, locale-aware case folding, and bidi handling — substantial work for marginal user value. |
| Fuzzy matching (typo tolerance) | Adds non-determinism and is hard to make cross-runtime-stable. Revisit when there is concrete user demand. |
| Inline preview of skill body | Conflicts with Z-2 in spirit (popup as signal-only). May be added as an explicit "preview pane" affordance in the popup, not as auto-injection on selection. |
| Multi-skill selection | Selecting two skills at once is not supported in v1; users compose multiple `/skill1 /skill2` tokens manually. |
| Server-pushed catalog updates while popup closed | The `autocomplete.subscribe` push (§9.5) is optional in v1 and only useful for open popups. A "skill installed in another window" notification toast is out of scope. |
| Cross-skill conflict warnings | "These two skills both modify the system prompt and may conflict" — out of scope; covered by skill activation, not autocomplete. |
| Per-skill keyboard shortcuts | E.g. Ctrl+Shift+P for `python-pro`. Out of scope; a global-shortcut feature, not an autocomplete feature. |
| Recency persistence across engine restarts | Currently MRU is volatile per session. Persistent MRU is a future opt-in. |
| Conversation-context boost across multiple turns | Currently the boost (§5.1 signal 5) considers only the current session's coarse intent. Cross-turn signal aggregation is deferred. |
| Drag-and-drop selection | E.g. drag a skill from a sidebar into the composer. Not specified; hosts MAY implement but MUST insert the canonical token. |

### 13.3 Explicit non-goals

These are NOT bugs to be fixed; they are deliberate design choices:

- **The popup is not a search engine.** It does not score arbitrary text against arbitrary queries; it ranks a curated catalog against a typed prefix.
- **The popup does not surface permission-denied entries.** Even with a "show denied" toggle. Surfacing denied entries leaks existence information.
- **The popup does not show usage statistics.** "This skill was used 42 times this week" is not displayed; it would create social-pressure ranking and degrade determinism.
- **The popup does not warn about deprecated skills.** Manifest authors are responsible for removing deprecated skills; the popup is not a deprecation surface.
- **The popup does not auto-select on Tab if there is exactly one match.** Even if there is only one match, Tab still requires the popup to be in `Open`/`Filtering`. This avoids surprising mid-typing insertions.

---

## §14. Open Questions

| ID | Question | Status | Notes |
|----|----------|--------|-------|
| Q1 | Should the conversation-context boost (§5.1 signal 5) be enabled by default in v1, or behind a feature flag? | Open | Determinism risk vs UX benefit; needs prototype evaluation. Default to off for v1. |
| Q2 | What is the canonical encoding when JSON is not used? | Open | CBOR vs MessagePack; punted to `spec-cross-cutting-wiring.md`. |
| Q3 | Should hosts persist MRU across restarts? | Open | If yes, in what storage (host config, engine session state, separate)? Default no for v1. |
| Q4 | Should the engine de-duplicate manifests with identical content but different paths (e.g. a symlinked-in skill that already exists at canonical location)? | Open | Edge case; current behavior is deduplicate-by-name-within-scope, which incidentally handles this for symlinks pointing to scope-root paths but not for cross-scope cases. |
| Q5 | What is the exact debounce window for filesystem watching? | Open | §4.3 says 250ms recommended; should this be normative? Variations across OSes (inotify vs kqueue) make a single number tricky. |
| Q6 | Should `autocomplete.subscribe` (§9.5) be required for v1 or optional? | Open | If optional, hosts may have stale popups when the user edits a SKILL.md in another window. |
| Q7 | How should the engine handle a SKILL.md that imports / extends another SKILL.md? | Out of scope for autocomplete | Skill composition is a feature deferred to skill activation. |
| Q8 | Should there be a "do not show this skill in autocomplete" hint in the manifest? | Closed | Resolved: use `enabled: false` in the manifest. The popup respects it (§3.2.1). |
| Q9 | Should the popup support multi-line skill descriptions? | Closed | Resolved: descriptions are single-line in the popup (truncate with ellipsis); the full description appears in tooltip / hover. |
| Q10 | What is the expected behavior when the workspace root changes mid-session (e.g. user opens a different folder)? | Open | Engine MUST re-scan workspace scope; in-flight queries MAY observe either pre- or post-change index. Document explicitly. |
| Q11 | Should there be a CLI command (e.g. `caduceus skills list`) that shows the same catalog as the popup, for debugging? | Open | Useful operationally but technically a separate CLI feature. |
| Q12 | Should the engine track and report the count of permission-filtered entries (so hosts can show "X skills hidden by permissions")? | Open | Possible UX, but conflicts with Z-6 spirit. Lean toward NO. |
| Q13 | Is the FQN form `/<scope>:<name>` allowed inside the manifest's `aliases[]` field? | Closed | No. Aliases are bare names only. Scope is a discovery property, not part of the name. |
| Q14 | What happens if two manifests within the same scope share an alias? | Open | Currently §6.7 says alias collisions across scopes are tolerated; same-scope alias collision should be a load-time error analogous to same-scope name collision. To be normative-ized in next revision. |
| Q15 | Should there be an `autocomplete.diagnostics` RPC that returns the list of skipped-malformed manifests for the host to surface? | Open | Useful for "Why is my skill not showing up?" debuggability. Lean toward yes for v2. |

---

*End of Skill / Agent Autocomplete specification.*

---

## Appendix A. Reference Pseudocode

The pseudocode below is **non-normative** and exists to disambiguate edge cases in the prose specification. Implementers MAY use it as a starting point but are free to implement the same observable behavior using different internal structures.

### A.1 Composer trigger state machine (host side)

```rust
enum TriggerState {
    Closed,
    Open { trigger_pos: Position, trigger_char: char },
    Filtering { trigger_pos: Position, trigger_char: char, prefix: String },
    Selected,  // transient; resets to Closed next tick
}

fn on_keystroke(state: &mut TriggerState, key: Key, editor: &mut Editor) {
    match (state, key) {
        // Closed -> Open
        (TriggerState::Closed, Key::Char(c)) if c == '/' || c == '@' => {
            if is_at_word_boundary(editor) && !is_in_code_region(editor) {
                editor.insert_char(c);
                let pos = editor.caret().offset_by(-1);
                *state = TriggerState::Open { trigger_pos: pos, trigger_char: c };
                rpc_query(c, "");
            } else {
                editor.insert_char(c);
            }
        }

        // Open -> Filtering / stay Open
        (TriggerState::Open { trigger_pos, trigger_char }, Key::Char(c))
            if is_identifier_char(c) =>
        {
            editor.insert_char(c);
            let prefix = String::from(c).to_lowercase();
            *state = TriggerState::Filtering {
                trigger_pos: *trigger_pos,
                trigger_char: *trigger_char,
                prefix,
            };
            rpc_query(*trigger_char, &prefix);
        }

        // Filtering -> Filtering (extend prefix)
        (TriggerState::Filtering { trigger_pos, trigger_char, prefix }, Key::Char(c))
            if is_identifier_char(c) =>
        {
            editor.insert_char(c);
            prefix.push(c.to_ascii_lowercase());
            rpc_query(*trigger_char, prefix);
        }

        // Filtering with backspace
        (TriggerState::Filtering { trigger_pos, trigger_char, prefix }, Key::Backspace) => {
            if prefix.is_empty() {
                // backspaced through trigger char itself
                editor.delete_backward();
                *state = TriggerState::Closed;
            } else {
                editor.delete_backward();
                prefix.pop();
                if prefix.is_empty() {
                    *state = TriggerState::Open {
                        trigger_pos: *trigger_pos,
                        trigger_char: *trigger_char,
                    };
                }
                rpc_query(*trigger_char, prefix);
            }
        }

        // Open with backspace
        (TriggerState::Open { .. }, Key::Backspace) => {
            editor.delete_backward();
            *state = TriggerState::Closed;
        }

        // Dismissal
        (TriggerState::Open { .. } | TriggerState::Filtering { .. }, Key::Escape | Key::Space) => {
            if matches!(key, Key::Space) {
                editor.insert_char(' ');
            }
            *state = TriggerState::Closed;
        }

        // Selection (Enter / Tab)
        (TriggerState::Open { trigger_pos, .. } | TriggerState::Filtering { trigger_pos, .. },
         Key::Enter | Key::Tab) =>
        {
            if let Some(highlighted) = popup.highlighted_item() {
                let token = highlighted.token.clone();  // engine-canonical
                let from = *trigger_pos;
                let to = editor.caret();
                editor.replace_range(from..to, &token);
                *state = TriggerState::Selected;
                rpc_select(highlighted);
            }
        }

        // Other
        _ => default_handle(state, key, editor),
    }
}

fn on_tick(state: &mut TriggerState) {
    if matches!(state, TriggerState::Selected) {
        *state = TriggerState::Closed;
    }
}
```

### A.2 Manifest discovery (engine side)

```rust
fn scan_scope(root: &Path, scope: Scope, kind: ManifestKind) -> Vec<Entry> {
    let mut entries = Vec::new();
    let dir = match read_dir(root) {
        Ok(d) => d,
        Err(_) => return entries,  // missing root is OK
    };

    for child in dir {
        let child = match child { Ok(c) => c, Err(_) => continue };
        let path = child.path();
        let manifest_path = match (kind, child.file_type()) {
            (ManifestKind::Skill, Ok(ft)) if ft.is_dir() => path.join("SKILL.md"),
            (ManifestKind::Agent, Ok(ft)) if ft.is_dir() => path.join("AGENT.md"),
            (ManifestKind::Agent, Ok(ft)) if ft.is_file()
                && path.to_string_lossy().ends_with(".agent.md") => path.clone(),
            _ => continue,
        };

        if !manifest_path.exists() { continue; }
        if file_size(&manifest_path).unwrap_or(0) > 1_048_576 {
            log_warn!("manifest > 1MiB skipped: {:?}", manifest_path);
            continue;
        }

        match parse_manifest(&manifest_path, kind) {
            Ok(entry) => entries.push(entry.with_scope(scope)),
            Err(e) => log_warn!("manifest skipped: {:?} ({})", manifest_path, e),
        }
    }

    dedup_by_name_in_scope(&mut entries);
    entries
}

fn build_index(workspace_root: Option<&Path>, user_root: &Path) -> AutocompleteIndex {
    let mut idx = AutocompleteIndex::new();
    if let Some(ws) = workspace_root {
        for e in scan_scope(&ws.join(".github/skills"), Scope::Workspace, ManifestKind::Skill) {
            idx.insert_skill(e);
        }
        for e in scan_scope(&ws.join(".github/agents"), Scope::Workspace, ManifestKind::Agent) {
            idx.insert_agent(e);
        }
    }
    for e in scan_scope(&user_root.join("skills"), Scope::User, ManifestKind::Skill) {
        idx.insert_skill(e);
    }
    for e in scan_scope(&user_root.join("agents"), Scope::User, ManifestKind::Agent) {
        idx.insert_agent(e);
    }
    idx.bump_generation();
    idx
}
```

### A.3 Ranking (engine side)

```rust
fn rank(items: &[Entry], prefix: &str, profile: RankingProfile,
        mru: &Mru, ctx: &ContextHints) -> Vec<RankedEntry> {
    let prefix_lc = prefix.to_ascii_lowercase();
    let mut scored: Vec<RankedEntry> = items.iter()
        .filter_map(|e| {
            let mc = match_class(e, &prefix_lc)?;  // None = no match
            let score = (
                match_class_rank(mc),                 // categorical, dominates
                scope_rank(e.scope),                  // workspace > user > ...
                e.priority,                           // manifest priority
                if profile.recency_enabled() && mru.contains(e.id()) { 1 } else { 0 },
                if profile.context_enabled() { context_boost(e, ctx) } else { 0 },
            );
            Some(RankedEntry { entry: e.clone(), score, match_class: mc })
        })
        .collect();

    // stable sort by descending score, then ascending name
    scored.sort_by(|a, b| {
        b.score.cmp(&a.score)
            .then_with(|| a.entry.name.to_ascii_lowercase().cmp(&b.entry.name.to_ascii_lowercase()))
    });
    scored
}
```

---

## Appendix B. Example Walkthroughs

### B.1 Happy path: workspace skill selection

1. User has `<workspace>/.github/skills/test-runner/SKILL.md` (description: "Run project tests with smart defaults").
2. User opens caduceus-zed, opens chat, focuses composer.
3. User types: `please /` — popup opens, state `Open`, full enabled catalog displayed, ranked. `test-runner` is one of many entries.
4. User types: `t` — state transitions to `Filtering { prefix: "t" }`. RPC sent, engine returns entries matching prefix `t`. Popup shows `test-runner` plus other `t*` entries.
5. User types: `est-r` — state stays `Filtering { prefix: "test-r" }`. Popup narrows to `test-runner` only.
6. User presses `Enter`. State transitions to `Selected`, then `Closed` next tick. Editor text is now `please /test-runner ` with caret after the trailing space.
7. User types: `on the auth module` — composer text: `please /test-runner on the auth module`.
8. User presses Cmd+Enter to submit. Engine parses tokens, resolves `/test-runner` against the catalog (workspace scope wins; only one entry for that name), activates the skill (out of scope), constructs the turn.

### B.2 Shadowed skill: explicit FQN selection

1. User has `~/.copilot/skills/python-pro/SKILL.md` (description: "Personal Python style preferences").
2. Workspace has `<workspace>/.github/skills/python-pro/SKILL.md` (description: "Project-specific Python conventions").
3. User types `/py` in composer. Popup shows:
   ```
   python-pro          Project-specific Python conventions (workspace)
   python-pro          Personal Python style preferences (user) [shadowed]
   ```
4. User uses ↓ arrow to highlight the second entry, presses Enter.
5. Inserted token: `/user:python-pro ` (FQN form because the entry is shadowed).
6. The user's submitted message contains `/user:python-pro`, which the engine resolves unambiguously to the user-scope entry.

### B.3 Empty catalog (newly installed system)

1. User has just installed caduceus; no skills are present.
2. User types `/` in composer.
3. Popup opens; engine returns `items: [], empty_reason: "no_catalog"`.
4. Popup renders: a short message "No skills installed. Install skills from the marketplace or place a `SKILL.md` under `~/.copilot/skills/`."
5. User presses Esc; popup closes.

### B.4 Permission denial (skill exists but is not visible)

1. Catalog contains `dangerous-net-skill` (declares `tools: [network]`).
2. Permission policy denies `network` for the calling principal in the current session.
3. User types `/da`. Popup MAY be empty (if no other matching entries) with `empty_reason: "no_match"`.
4. The user has no way, via autocomplete, to learn that `dangerous-net-skill` exists. (They may learn through other channels, e.g. explicit catalog browsing UI, but not through the popup.)

### B.5 Refresh after install

1. User has caduceus running.
2. User types `git clone <skill-repo> ~/.copilot/skills/new-skill` in a terminal.
3. Engine's filesystem watcher fires; debounced re-scan picks up `new-skill/SKILL.md`. Index generation bumps.
4. If the host has subscribed via `autocomplete.subscribe`, it receives a generation-changed push; if its popup is currently open, it re-issues the query.
5. User types `/new` in the composer. Popup shows `new-skill` immediately (no host-side action required).

### B.6 Malformed manifest (developer authoring a new skill)

1. Developer creates `~/.copilot/skills/my-skill/SKILL.md` with malformed YAML (forgot to quote a colon-containing description).
2. Engine watcher fires; re-scan attempts to parse; YAML parser fails; engine logs `WARN`-level diagnostic citing the file and parse-error line.
3. Catalog continues to serve all previously valid entries. `my-skill` is absent.
4. Developer types `/my` in composer; `my-skill` does not appear.
5. Developer checks `caduceus skills list --diagnostics` (Q11; if implemented) or the engine log; finds the parse error; fixes the manifest; saves.
6. Watcher re-scans; `my-skill` is now valid; appears in the popup.

---

## Appendix C. Telemetry hooks (informative)

The autocomplete subsystem MAY emit telemetry events for product analytics. Telemetry is **strictly opt-in** per the project's privacy posture and MUST be disabled by default in non-production builds. The events below are informative; concrete event schemas are owned by the telemetry spec (out of scope here).

| Event | When emitted | Notable fields |
|-------|--------------|----------------|
| `autocomplete.popup_opened` | State transition Closed → Open | `trigger`, `prefix_len_at_close` (filled when popup eventually closes) |
| `autocomplete.popup_dismissed` | State transition * → Closed (no selection) | `trigger`, `prefix`, `dismissal_reason` |
| `autocomplete.item_selected` | State * → Selected | `trigger`, `name_hash`, `scope`, `match_class`, `popup_open_duration_ms` |
| `autocomplete.empty_reason_shown` | Engine returns non-null `empty_reason` | `trigger`, `empty_reason` |
| `autocomplete.refresh_completed` | Index re-scan finishes | `trigger_source` (watch/explicit/startup), `entries_count`, `skipped_count`, `duration_ms` |

Field naming: `name_hash` is a hash of the canonical name to avoid leaking skill names in telemetry; `prefix` may be transmitted as a length-only field if privacy policy requires.

---

## Appendix D. Known limitations and deferred work

This section consolidates pointers to deferred work that may inform future revisions:

- **Q1, Q3, Q5, Q6, Q10, Q11, Q12, Q14, Q15** from §14 are tracked as backlog items.
- The conversation-context boost (§5.1) is described but not specified in detail; future revisions need to define the exact intent classification interface.
- The `autocomplete.subscribe` push API is sketched but its delivery semantics (at-most-once vs at-least-once, ordering with respect to `autocomplete.query` responses) need elaboration.
- Cross-host MRU sharing (a user using both caduceus-zed and the CLI) is not specified. Currently each host has its own MRU.
- The host-conformance test suite (§8.6) is described but not yet implemented; it is on the roadmap for the v1 release of the autocomplete subsystem.

---

*End of Skill / Agent Autocomplete specification.*
