# caduceus: Multi-Repo Workspace Model

> **Spec ID:** caduceus / multi-repo-workspace-model
> **Status:** Draft (P0 — foundation; consumed by spec #4 snapshot, spec #7 run identity)
> **Audience:** caduceus engine implementers, `caduceusd` daemon implementers, runner-side cwd handling.
> **Normative key words:** "MUST", "MUST NOT", "SHOULD", "SHOULD NOT", "MAY", "REQUIRED", "RECOMMENDED", "OPTIONAL" are to be interpreted as in [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119) and [RFC 8174](https://www.rfc-editor.org/rfc/rfc8174).

> **⚠️ Known residual issues — iter-28 backlog (2026-04-29).**
> The following items were surfaced by `gpt-5.4` standalone review at iter-27
> with verbatim replacement text saved in
> `private/reviews/iter27-spec3-gpt.md` (avg 5.0 — the strictest reviewer
> regressed on the iter-27 DRY consolidation). They were not blocking for
> the iter-27 ship — the spec converged on `claude-opus-4.6` +
> `gpt-5.3-codex` at min 7 / 7 respectively. Resolve in iter-28+.
>
> 1. **§3.3 `sanitize_run_id` regex check** — assertion contradicts §3.2
>    acceptance: §3.2 permits uppercase ULIDs but the §3.3 regex narrower
>    than that. Align both to `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$` with
>    explicit `.`/`..`/no-`..`-substring rules.
> 2. **§3.5 step 1.4 placeholder row** — currently inserts a registry row
>    keyed by `workspace_id` and `target` *before* step 2 computes
>    `safe_run_id`, `target`, and step 9's `workspace_id`. Reorder so step
>    1.4 first computes `safe_run_id`, `target`, `workspace_id` (per I-6),
>    then inserts the `Status::Creating` row; step 2 MUST NOT re-derive.
> 3. ~~**§3.5 step 9 vs §5 I-6 `workspace_id` derivation**~~ — **RESOLVED 2026-04-30**: §3.5 step 9 and §5 I-6 both now use `BLAKE3_128_keyed(slug || 0x1F || safe_run_id)`.
> 4. **§3.6 cleanup short-circuit** — "short-circuit to cleanup-cleared"
>    is undefined: which steps skip, do hooks run, when is the registry
>    row deleted? Specify: on `ENOENT` at slug → classify
>    `OrphanedNoSlug`, skip steps 4–8 (no probe, no hooks, no `unlinkat`),
>    proceed to step 9; on `ENOENT`/`ELOOP` at leaf → classify
>    `OrphanedNoLeaf`, hooks MUST NOT run.
> 5. **§3.5 env exports — `CADUCEUS_RUN_ID_SAFE`** — currently labeled
>    "shell-safe" with unquoted-interpolation recommendation; that
>    guarantee is false. Strike "shell-safe" wording, add explicit
>    shell-injection notice instructing hook authors to quote
>    `"$CADUCEUS_RUN_ID_SAFE"`.
> 6. **§3.5 lines 371-382 lock order** — buried in tag-heavy prose;
>    surface as a short normative sentence: acquire order is
>    `registry-wide → per-slug → per-workspace`. Try-lock semantics for
>    v1 strategy (a) on synchronous create.
> 7. **§3.6 lines 875-883** — "registry write lock for this workspace"
>    conflates short registry-row mutation with per-workspace lock. Split
>    into Phase-1 row claim (registry-wide mutex, brief) and per-workspace
>    acquire in canonical order.
> 8. **§3.5 step 8.5 leaf-ownership handoff** — redefines §5A.5 contract
>    inline; replace with single-source pointer to §5A.5 (Z6-G1).
> 9. **§5B.2 canonical bypass scope** — followed by a five-source
>    rationale wall, drift-prone. Reduce to single-source-of-truth
>    statement: `OrphanReclaim` re-entry skips ONLY step 4 (layered
>    liveness probe) regardless of enqueue source.
> 10. ~~**§5 I-6 BLAKE3 key size**~~ — **RESOLVED 2026-04-30**: I-6 now correctly specifies a 32-byte key per BLAKE3 keyed-mode requirements.

## 0. Header & Attribution

This specification is authored as a normative re-implementation target for caduceus. It draws on the design and source of the **Symphony** project (`openai/symphony`, Apache-2.0 licensed) for prior art on per-issue workspace lifecycle, hook scheduling, and filesystem-safety invariants. Symphony source is cited inline as `(file:line)` and Symphony's normative spec is cited as `(SPEC §n.n)` referring to `symphony/SPEC.md` at the source revision recorded in the upstream analysis bundle (`symphony-multirepo-ux.md` Parts A.1, B.1–B.3, D.3).

```
SPDX-License-Identifier: Apache-2.0
Portions of the algorithms in §3.2, §3.4, §3.5, §3.6 are derivative of
Symphony (https://github.com/openai/symphony), Apache License 2.0.
Symphony attribution and the Apache-2.0 NOTICE MUST be preserved in any
implementation that ports Symphony's `validate_workspace_path/2`,
`workspace_path_for_issue/2`, or `Workspace.remove/2` algorithms.
```

Where this spec deliberately diverges from Symphony — most notably by introducing a `(repo_slug, run_id)` keying primitive and a `caduceusd` daemon owning the workspace registry across N repos — the divergence is called out explicitly as **caduceus-new** so reviewers can audit the boundary.

---

## 1. Scope

### 1.1 In scope

This spec is normative for:

1. The **shape and derivation** of `WorkspaceId` and the per-row `RepoCoordinate` structure (§4).
2. The **on-disk layout** rooted at `<workspace_root>/<repo_slug>/<run_id>/` (§3.3, §4).
3. The **algorithms** for path construction, sanitization, validation, creation, and cleanup (§3).
4. **Filesystem safety invariants** ported verbatim from Symphony SPEC §9.5 lines 886–905, plus caduceus-new invariants for multi-repo, daemon ownership, and shared-repo lock semantics (§5).
5. **Lifecycle hooks** — `before_create`, `after_create`, `before_cleanup`, `after_cleanup` — their ordering, failure modes, and abort/rollback semantics (§3.5, §3.6, §5).
6. **Sanitization rules** for the `repo_slug` segment (derived from a remote URL) and the `run_id` segment (derived from caduceus run identity).
7. **Symlink-escape protection** — both at create time and at cleanup time, fail-closed.
8. **Shared-workspace exclusion** — semantics for two runs targeting the same `(repo_slug, branch)`; lock acquisition; whether v1 supports concurrent runs on the same repo at all (open question, §8).
9. **Daemon ownership boundary** — `caduceusd` owns the registry; engine reads via the daemon API. The C-hybrid topology is locked in (per `symphony-multirepo-ux.md` Q3 disposition).

### 1.2 Out of scope

- **Orchestrator dispatch** (which run gets which workspace, retry semantics, queue ordering): defer to spec #1 (orchestrator).
- **Agent runner cwd handling** (how the runner binds the agent process to `workspace.path`, environment-variable plumbing, sandbox enforcement): defer to spec #2 (runner).
- **Runs panel UI surface** (how zed renders the rows, sort affordances, filters): defer to spec #8 (runs panel).
- **Git operations themselves** (which branch to check out, how to push, how to handle merge conflicts): owned by the workflow YAML's `hooks.after_create`, not by the workspace model. See spec #6 (workflow).
- **Cross-host workspaces** (SSH worker hosts; Symphony Appendix A.1–A.3 introduces these via `worker.ssh_hosts`). caduceus v1 is single-host; revisited in §7.
- **Multi-repo work unit** (one Run touches N repos): explicitly deferred to v2 per `symphony-multirepo-ux.md` Q1 (lines 816–832). v1 enforces "one Run = one repo".

---

## 2. Terms

The following table is normative. Where a term overlaps with a Symphony concept, the analogue is noted in Appendix B.

| Term | Definition |
|---|---|
| **Workspace** | A directory on the daemon's host filesystem that is the cwd for exactly one Run. Holds the working tree (typically a `git clone` or `git worktree`). Owned by `caduceusd`; engine and runner are read-only consumers. |
| **WorkspaceId** | A stable, content-derived identifier of shape `wsp_<hex>` where `<hex>` is the lower-case hex of a 128-bit BLAKE3 keyed hash of `(repo_coordinate.slug \|\| 0x1F \|\| run_id)`. Derivable from inputs; not random. See §4 and Invariant I-6. |
| **RunId** | The caduceus Run identifier as defined in spec #7. Treated as opaque here; sanitized before being used as a path segment (§3.2). |
| **RepoSlug** | A filesystem-safe string derived from a repo's remote URL by `sanitize_repo_slug` (§3.1). Lowercase, ASCII alphanumerics plus `_`, capped at 64 bytes. The unit of repo identity at the workspace layer. |
| **RepoCoordinate** | The daemon's row-level repo identity: `{ slug: RepoSlug, remote_url: Option<Url>, default_branch: Option<String> }`. The slug is the canonical key; `remote_url` and `default_branch` are advisory metadata used by hooks. |
| **WorkspaceRoot** | An absolute, canonicalized path on the daemon's host that contains all workspaces for that daemon instance. Configured at daemon startup; immutable for the lifetime of the daemon process. |
| **RunWorkspace** | The per-Run leaf directory, located at `<workspace_root>/<repo_slug>/<run_id>/`. Created by `create_workspace` and torn down by `cleanup_workspace`. |
| **BareCheckout** | (caduceus-new) A `.git` directory shared by N RunWorkspaces against the same repo, attached via `git worktree add` so each Run has an isolated working tree. Located at `<workspace_root>/<repo_slug>/.bare/`. Optional implementation strategy for shared-repo lock semantics; see §3.7 and §8. |
| **Hook** | A user-supplied shell command from the workflow YAML, invoked at one of four lifecycle points (`before_create`, `after_create`, `before_cleanup`, `after_cleanup`). Inherits the RunWorkspace directory as cwd and a defined environment (§3.5). |
| **Sanitization** | A deterministic, idempotent transformation of a string into a filesystem-safe segment. Specified in §3.1 (slug) and §3.2 (run_id). |
| **SymlinkEscape** | A condition where a path component, after canonicalization, resolves outside `WorkspaceRoot`. MUST be detected and rejected (§3.4 and Invariant I-3). |

---

## 3. Normative algorithms

All algorithms are specified in pseudocode. Implementations MUST preserve the observable behaviour (inputs, outputs, error modes, ordering of side effects) but MAY use any internal data structure.

### 3.0 Call-flow overview (informative)

The following sequence is the canonical happy-path interaction between the orchestrator (spec #1), the daemon's workspace subsystem (this spec), and the runner (spec #2). It is informative; the normative behaviour lives in §3.1–§3.7.

```
orchestrator                 caduceusd / workspace                 runner
     │                                │                              │
     │ dispatch(run_id, repo_coord)   │                              │
     ├───────────────────────────────►│                              │
     │                                │ acquire_shared_repo_lock     │
     │                                │ (§3.7)                       │
     │                                │ create_workspace (§3.5)      │
     │                                │   ├── before_create hook     │
     │                                │   └── after_create hook      │
     │                                │      (typically git clone)   │
     │      Workspace { path, ... }   │                              │
     │◄───────────────────────────────┤                              │
     │ start_runner(workspace)        │                              │
     ├──────────────────────────────────────────────────────────────►│
     │                                │   validate_workspace_path    │
     │                                │   (§3.4, runner-side recheck)│
     │                                │   bind cwd; spawn agent      │
     │                                │                              │
     │  ... agent runs ...            │                              │
     │                                │                              │
     │ run_completed(run_id, status)  │                              │
     ├───────────────────────────────►│                              │
     │                                │ cleanup_workspace (§3.6)     │
     │                                │   ├── liveness check         │
     │                                │   ├── before_cleanup hook    │
     │                                │   ├── recursive remove       │
     │                                │   └── after_cleanup hook     │
     │                                │ release lock guard           │
     │◄───────────────────────────────┤                              │
```

The same flow applies to failure paths, with the difference that on `create_workspace` failure (Invariant I-7) the orchestrator never sees a `Workspace` and the runner is never started; on `cleanup_workspace` failure (`Status::CleanupFailed`) the row is left for reconcile to retry.

### 3.1 `sanitize_repo_slug(remote_url) → RepoSlug`

**Purpose:** Convert a repo's remote URL into a filesystem-safe segment that is stable across cosmetic URL changes (HTTPS vs SSH, trailing `.git`, case).

**Input:** A URL-like string. Acceptable shapes:
- `https://github.com/owner/repo`
- `https://github.com/owner/repo.git`
- `git@github.com:owner/repo.git`
- `ssh://git@github.com/owner/repo.git`

**Output:** A `RepoSlug` of shape `^[a-z0-9][a-z0-9_]{0,63}$`.

**Algorithm:**

1. Parse `remote_url` into `(host, path)`. If parse fails, MUST return error `InvalidRemoteUrl`.
2. Strip a single trailing `.git` from `path` if present.
3. Strip any leading `/` from `path`.
4. Lowercase `host` and `path`.
5. Replace each maximal run of characters not in `[a-z0-9]` with a single `_`.
6. Trim leading and trailing `_`.
7. Construct `slug_body = host + "_" + path` (e.g. `github_com_owner_repo`).
   The host segment is included **unconditionally**; same-host omission is
   FORBIDDEN. See worked examples below and Appendix C for the canonical
   host-prefixed form. (N-5 fix: prior drafts allowed host-omission in
   single-host deployments and produced examples inconsistent with the
   algorithm body — that ambiguity is closed here.)
8. If `len(slug_body) > 64`, truncate to 56 bytes and append `_` plus the first 7 hex chars of BLAKE3-128(`host || "/" || path`). Result MUST still be `≤ 64`.
9. Return `slug_body`.

**Collision policy:** Two distinct remote URLs MAY collide on `slug_body`. Same-host
collisions are possible (e.g. `acme/foo-bar` and `acme/foo_bar` both normalize to
`acme_foo_bar`); cross-host collisions are also possible (e.g. `github.com/acme/app`
vs `gitlab.com/acme/app`). A host-only suffix is therefore insufficient and is
**FORBIDDEN**. On collision detected at registry-write time, the daemon MUST
disambiguate as follows:

1. Compute `repo_hash7` = the first 7 hex chars of
   `BLAKE3-128(lowercase(normalized_host) || "/" || lowercase(normalized_path))`,
   i.e. the hash is taken over the full normalized remote (host *and* path), not
   host alone. This distinguishes both same-host and cross-host collisions.
2. Compute `keep = min(len(slug_body), 56)`. The trailing region (up to 8 bytes:
   one underscore plus 7 hex) of `slug_body` is replaced — never appended — so
   the result CANNOT overflow the 64-byte cap of the canonical regex
   `^[a-z0-9][a-z0-9_]{0,63}$`.
3. `rewritten = slug_body[..keep] + "_" + repo_hash7`. `len(rewritten) ≤ 64`.
4. If `rewritten` is itself already taken in the registry, the daemon MUST extend
   the hex suffix width (8, 9, ... up to 32 hex chars) until the result is unique,
   trimming `keep` further as needed to stay within the 64-byte cap. A host-only
   suffix MUST NOT be used at any width.

This is a one-shot rewrite for a given `(slug, RepoCoordinate)` pair; the registry
MUST persist the rewritten slug as the canonical RepoSlug for that RepoCoordinate
from then on (Invariant I-4 stickiness applies). See test T-5.

**Stability:** Once a `(remote_url → slug)` binding is recorded in the registry, a subsequent `remote_url` change for the same logical repo (e.g. owner rename, host migration) MUST NOT change the slug. The slug is sticky on first observation. See Invariant I-4.

**Worked examples:**

| Input | Output |
|---|---|
| `https://github.com/openai/symphony` | `github_com_openai_symphony` (host-prefixed; ≤ 64 bytes; no collision rewrite needed) |
| `https://github.com/openai/symphony.git` | same as above |
| `git@github.com:openai/symphony.git` | same as above |
| `https://github.com/Openai/Symphony` | same as above (case-folded) |
| `https://example.com/a/very/deeply/nested/path/to/some/repo.git` | first 56 bytes + `_` + `<hash7>` |

> Note: the host segment is included **unconditionally** (not only on
> collision). This is a normative requirement as of this revision —
> earlier drafts allowed implementations to omit the host prefix in
> single-host deployments and add it only on collision; that policy is
> FORBIDDEN because it produces ambiguous slugs across cross-host
> migrations and is inconsistent with the Appendix C worked example.
> Implementations MUST always emit `<host>_<owner>_<repo>` (or, for
> deeper paths, `<host>_<seg1>_<seg2>_…` after the §3.1 sanitization
> rules).

### 3.1A `resolve_repo_coordinate(remote_url, default_branch) → RepoCoordinate` *(caduceus-new)*

**Purpose:** Provide the **only** authoritative path from a remote URL to a
`RepoCoordinate`. Trusting a caller-supplied `slug` directly is FORBIDDEN —
Invariant I-4 (slug stickiness) cannot otherwise be enforced across rename or
host migration.

**Daemon-owned state.** The daemon MUST maintain a persistent `repo_bindings`
table (in the same store as the workspace registry; recovery semantics owned by
spec #1) keyed by `repo_key`, where `repo_key` is a stable identity for the
*logical* repo (e.g. host-provider repo id when known; otherwise the
canonicalized remote URL at first sight). Each row stores at minimum:

| Field | Type | Notes |
|---|---|---|
| `repo_key` | opaque string | Primary key. Stable across cosmetic remote_url changes. |
| `slug` | `RepoSlug` | Sticky: assigned once, never mutated. |
| `canonical_remote_url` | `Url` | Most recently observed canonical remote. MAY update. |
| `remote_aliases` | `Vec<Url>` | Every remote_url ever seen for this `repo_key`. Append-only. |
| `default_branch` | `Option<String>` | Advisory metadata; MAY update. |

**Algorithm:**

1. Normalize `remote_url` (lowercase host, strip trailing `.git`, strip leading
   `/` from path) to a canonical form `canon_url`.
2. Compute `repo_key` from `canon_url` (or from a host-provider repo id if
   available — an implementation-defined enrichment, e.g. via a previously
   recorded GitHub repo node id; the caduceus posture is "best available stable
   identity").
3. Look up `repo_key` in `repo_bindings`:
   - **Hit:** return the existing row's `slug` as `RepoCoordinate.slug`.
     `canonical_remote_url`, `remote_aliases`, and `default_branch` MAY be
     updated under the same critical section (slug MUST NOT). If `canon_url`
     is not already in `remote_aliases`, append it.
   - **Miss:** derive a candidate slug per §3.1; apply §3.1's collision policy
     against existing `repo_bindings` rows; persist a new row with the resolved
     slug, `canonical_remote_url = canon_url`, `remote_aliases = [canon_url]`,
     `default_branch` as supplied. Return `RepoCoordinate{ slug,
     remote_url = Some(canon_url), default_branch }`.

**Caller contract:** `create_workspace` (§3.5) MUST consume a `RepoCoordinate`
that originated from this function (or, for tests / fixtures, from a fixture
that has been registered via the same code path). `create_workspace` MUST NOT
trust a caller-supplied `slug` that has not been resolved through
`repo_bindings`. Implementations that bypass this binding step do not satisfy
Invariant I-4.

**Concurrency:** This function MUST be safe under concurrent callers; same
`repo_key` arriving from N callers MUST result in exactly one row written and
all N callers observing the same slug.

### 3.2 `sanitize_run_id(run_id) → safe_segment`

**Purpose:** Convert an opaque `RunId` (spec #7) into a filesystem-safe segment.

**Reference:** Symphony's per-issue sanitization at `workspace.ex:206–208` (replace non-alphanumerics, collapse, trim). The behaviour is ported; the input domain is different (RunId, not issue identifier).

**Algorithm:**

1. If `run_id` is empty or longer than 128 bytes after UTF-8 encoding, MUST return error `InvalidRunId`.
2. Replace each maximal run of characters not in `[A-Za-z0-9._-]` with a single `_`.
3. Trim leading and trailing `_` and `.`.
4. Reject the result if it is `.`, `..`, empty, or matches the reserved pattern `^\.+$`.
5. Reject the result if it contains `..` as a substring (defence-in-depth; pre-canonicalization filter).
6. Return the result.

**Idempotence:** `sanitize_run_id(sanitize_run_id(x)) == sanitize_run_id(x)` MUST hold.

**Worked examples:**

| Input | Output | Note |
|---|---|---|
| `01H8XYZABC` (ULID) | `01H8XYZABC` | unchanged |
| `run/2024-04-10/abc` | `run_2024-04-10_abc` | slashes collapsed |
| `../etc/passwd` | rejected (rule 5) | path traversal |
| `..` | rejected (rule 4) | dot dirs |
| `   ` | rejected (rule 1 after trim) | empty |

### 3.3 `build_workspace_path(workspace_root, repo_slug, run_id) → AbsolutePath`

**Purpose:** Compute the absolute path of a RunWorkspace.

**Algorithm:**

1. Assert `workspace_root` is absolute and previously canonicalized (§3.4 prerequisite).
2. Assert `repo_slug` matches `^[a-z0-9][a-z0-9_]{0,63}$`. Otherwise MUST return `InvalidRepoSlug`.
3. Assert `run_id` is the output of `sanitize_run_id` (i.e. passes the same regex). Otherwise MUST return `InvalidRunId`.
4. Return `<workspace_root>/<repo_slug>/<run_id>/`.

**Output requirements:**

- MUST be absolute.
- MUST end with the platform path separator (`/` on POSIX). Trailing-slash form is normative because downstream callers concatenate against it.
- MUST NOT contain any `.` or `..` segments after construction.
- MUST NOT, by itself, traverse a symlink. (Symlink presence is detected by §3.4, not here.)

This function is a pure string-construction step; it MUST NOT touch the filesystem. Filesystem-touching validation lives in §3.4.

### 3.4 `validate_workspace_path(path, workspace_root) → Result<AbsolutePath, Error>`

**Purpose:** Verify a candidate workspace path is safe to act on (create, write, or delete). Ported verbatim in observable behaviour from Symphony's `workspace.ex:358–384 validate_workspace_path/2` (Apache-2.0; attribution preserved).

**Inputs:**
- `path`: the candidate absolute path (typically the output of §3.3, but MAY be supplied by hostile input — e.g. a registry row read after a tampered DB).
- `workspace_root`: the canonicalized daemon-wide root.

**Algorithm:**

1. **Pre-canonicalization rejects.** If `path` contains `..` as a literal path component, MUST return `Error::PathTraversal`. (Defence-in-depth before realpath.)
2. **Canonicalize via longest-existing-prefix.** A direct `realpath(path)` is
   FORBIDDEN here: on the normal create path (§3.5) the leaf does not yet exist
   and `realpath` would fail (or worse, on some platforms, succeed against a
   tampered ancestor). Instead:
   1. Walk `path` from left to right, splitting at platform separators, and
      identify the longest existing prefix `P_exists` (i.e. the longest
      ancestor that exists on the filesystem at lstat time). The empty walk
      MUST yield `P_exists = "/"` on POSIX.
   2. Canonicalize `P_exists` via the host's `realpath(3)` equivalent — this
      resolves symlinks in every existing ancestor.
   3. Define `P_suffix = path[len(P_exists)..]`. `P_suffix` MUST contain no
      `..` component (re-checked here as defence-in-depth) and, by
      construction, contains no symlinks (its components do not yet exist).
   4. The canonicalized result is `realpath(P_exists) + P_suffix`.
   5. The `realpath(3)` implementation used here MUST be the same one used in
      §3.5 step 2 (consistency).
   6. The `P_exists` boundary identified in this step is the **same boundary**
      that §3.5 step 5 MUST use when creating the suffix components.
      Implementations MUST NOT recompute `P_exists` independently in step 5
      under different inode state — pass the boundary through, or re-derive
      it under the same per-slug guard while holding the per-workspace lock so
      no concurrent writer can have advanced the filesystem in between. Any
      observable disagreement on the boundary between validate and create is a
      correctness bug.
3. **Prefix check.** The canonicalized result MUST begin with `workspace_root` followed by the platform separator. String comparison; no fuzzy matching. Otherwise MUST return `Error::EscapedRoot`.
4. **Symlink-escape sweep.** For every prefix path of the original (pre-canonical) path between `workspace_root` and the leaf, if that prefix is itself a symlink, the link target MUST canonicalize to a path that is also under `workspace_root`. Otherwise MUST return `Error::SymlinkEscape`. This catches the case where `<workspace_root>/<repo_slug>` is a symlink to `/`.
5. **Post-canonicalization re-check.** After canonicalization, MUST NOT contain any `..` component. Otherwise MUST return `Error::PathTraversal`. (Belt-and-braces; realpath should already have removed these.)
6. **Filesystem boundary check.** (OPTIONAL, governed by §8 open question.) If the implementation chooses to enforce single-filesystem semantics, MUST compare the device id (`st_dev`) of `path` and `workspace_root`. Mismatch ⇒ `Error::FilesystemBoundary`. The default for v1 is **not** to enforce; Symphony does not. Implementations that enforce MUST document it.
7. Return `Ok(canonicalized_path)`.

**Note on TOCTOU:** Steps 2–5 are vulnerable to a race where a symlink is swapped in between validation and use. Mitigations:
- `create_workspace` (§3.5) MUST validate-then-create-leaf-with-`O_NOFOLLOW`-equivalent, not validate-then-cd.
- `cleanup_workspace` (§3.6) MUST re-validate immediately before each filesystem operation that descends into the tree.

This is the highest-risk algorithm in the spec. Implementations MUST cover it with the test cases in §6 (T-1, T-2, T-3) at minimum.

### 3.5 `create_workspace(repo_coordinate, run_id, workflow) → Workspace`

**Purpose:** Create a RunWorkspace for a single Run, including running user-supplied bootstrap hooks.

**Inputs:**
- `repo_coordinate: RepoCoordinate` — supplied by the orchestrator (spec #1).
- `run_id: RunId` — supplied by the orchestrator.
- `workflow: Workflow` — the parsed workflow YAML (spec #6) supplying `hooks.before_create` and `hooks.after_create`.

**Output:** A `Workspace` struct (§4) on success; an `Error` on any failure step.

**Algorithm:**

1. **Two-phase reservation (registry-wide critical section).** This step is
   short by construction; it MUST NOT span any of the lifecycle hooks.
   1. Acquire the registry-wide write mutex.
   2. **Pre-existing row check (status-sensitive).** If a `Workspace`
      row exists for `(slug, sanitize_run_id(run_id))` with
      `Status ∈ {Creating, Active, CleaningUp}`, the spawn MUST fail
      with `Error::WorkspaceBusy` (the row is owned by another live
      invocation or active spawn) — release the registry-wide mutex
      first. Rows with `Status ∈ {CleanupFailed, OrphanPending}` are
      NOT live-owner rows; the spawn skips step 1.4's placeholder
      insertion entirely (per the step 1.4 precondition below) and
      transfers control directly to step 5b's reclaim path (per
      §3.5 step 5b decision table) which reuses the slot under
      controlled conditions — the existing `CleanupFailed` /
      `OrphanPending` row MUST be carried forward intact for
      reclaim routing, NOT overwritten. The current invocation's
      own `Creating` placeholder (inserted at step 1.4, only when
      no row was found in step 1.2) MUST NOT count as a "matching
      row" for any subsequent lookup within this spawn — step 5b's
      lookup uses the pre-step-1.4 registry snapshot.
      Implementations SHOULD capture the lookup result during this
      step (which already iterates the registry) and reuse it at
      step 5b, rather than re-querying after step 1.4.
   3. **Try-lock the per-slug shared-repo lock guard** (§3.7).
      **Acquire order is normative (Z6-I1):
      `registry-wide → per-slug → per-workspace`.** The
      registry-wide mutex was taken in step 1.1; this step takes
      per-slug; step 1.3a takes per-workspace. This ordering is
      uniform across `create_workspace`, `cleanup_workspace`
      (which inherits the per-slug guard from create per §3.7
      Lock-guard contract), and the `OrphanReclaim` reattach path
      (which acquires `registry-wide → per-slug → per-workspace`
      in the same order before re-entering §3.6 from step 2).
      The canonical drain/bypass semantics are defined only in
      §5B.2 step 7. See §4.5 Lock hierarchy. v1 strategy (a) uses **try-lock** semantics
      on the synchronous create path: if acquisition would block
      (another Run holds the slug), release the registry-wide
      mutex and return `Error::SharedRepoLocked` (refuse-fast
      posture per §8 OQ-5). The blocking-`Wait` discipline of §3.7
      strategy (a) applies ONLY to the `OrphanReclaim` background
      sweeper (see §3.7 caller table); it MUST NOT be used here.
   3a. **Try-lock the per-workspace lock** (keyed by `workspace_id`,
      derivable per Invariant I-6 from `(slug, sanitize_run_id(run_id))`).
      Try-lock semantics; on failure release the registry-wide mutex AND
      the per-slug guard acquired in step 1.3, then return
      `Error::WorkspaceBusy`. The lock is held for the remainder of
      `create_workspace` (steps 2–9) and released on scope exit (RAII) at
      step 9 success or step 10 rollback. Pattern:

      ```
      workspace_lock = acquire_per_workspace_lock(workspace_id)?;
      // try_lock; on contention → Error::WorkspaceBusy
      // ... validation, mkdir, hooks, snapshot ...
      // workspace_lock released on scope exit (RAII)
      ```
   4. Insert a placeholder registry row keyed by `workspace_id` (computed per
      step 9 — `workspace_id` is derivable from `(slug, run_id)`), with
      `Status::Creating` and `path = target` (the pre-canonical form is
      tolerable here because the row is overwritten on success in step 9).

      **Precondition (B19 invariant continuation; normative).** If
      step 1.2 found a row with `Status ∈ {CleanupFailed,
      OrphanPending}`, step 1.4 MUST NOT insert or overwrite. The
      orchestrator carries that row forward to step 5b for reclaim
      routing without re-creating the placeholder. Equivalently:
      step 1.4 inserts a `Creating` placeholder ONLY when step 1.2
      found no matching row; for `{CleanupFailed, OrphanPending}`
      rows, control transfers from step 1.2 directly to step 5b
      after the inode-creation step's `EEXIST` (preserving the
      pre-existing row's status as the reclaim discriminator).
   5. Release the registry-wide mutex.

   Steps 2–9 below proceed under the **per-workspace lock** (keyed by
   `workspace_id`) **and** the **per-slug shared-repo guard** ONLY. The
   registry-wide mutex MUST NOT be held across hook execution. Holding the
   registry-wide mutex across the up-to-600s `after_create` hook is FORBIDDEN
   — one repo's clone would block every other repo's create or cleanup.

1b. **Rollback obligation.** Steps 3–8 (and step 1.4, the placeholder-row insert) on failure enter the rollback path at step 10. Rollback may abort at step 10a-pre (`CleanupOwnershipFailed`), step 10a (`ParentRevalidationFailed`), or step 10e mid-walk unlinkat failure (`CleanupIncomplete (rollback-side)`); in those cases the row and leaf are retained as `Status::CleanupFailed` for reconcile. Step 10 MUST always release the per-slug shared-repo guard. Placeholder-row handling is conditional: if rollback reaches step 10c, remove the `Status::Creating` row before releasing the per-slug guard; on any of the three abort conditions above, the row is retained as `Status::CleanupFailed` for reconcile (§4.3), i.e. step 10c is skipped.
2. **Compute canonical path.**
   - `slug = repo_coordinate.slug` (already a `RepoSlug` by §3.1).
   - `safe_run_id = sanitize_run_id(run_id)`.
   - `target = build_workspace_path(workspace_root, slug, safe_run_id)`.
3. **Pre-validate.** Call `validate_workspace_path(target, workspace_root)`. The expected outcome at this stage is that the leaf does **not** yet exist; the validation function MUST tolerate non-existence (it validates the ancestor chain). Reject on any `Error` from §3.4.
4. **Check for shared-repo lock conflict.** If another RunWorkspace exists under `<workspace_root>/<slug>/`, consult §3.7. If acquisition refuses, MUST abort here with `Error::SharedRepoLocked`.
5. **Create the directory tree (fd-based, TOCTOU-safe).** `mkdir -p` is
   FORBIDDEN here: on POSIX it follows symlinks for existing components, so an
   attacker who replaces `<workspace_root>/<slug>/` with a symlink between
   step 3 (validate) and step 5 (create) would escape `workspace_root`.
   Implementations MUST instead use `*at`-family syscalls anchored on a
   `workspace_root` file descriptor that was opened once at daemon startup
   with `O_DIRECTORY | O_NOFOLLOW`-equivalent semantics:
   - 5a. **Slug parent.** `mkdirat(workspace_root_fd, slug, 0o755)`.
     **Z-16: ALWAYS call `fstatat(workspace_root_fd, slug,
     AT_SYMLINK_NOFOLLOW)` immediately after `mkdirat`** — both on the
     fresh-create path (mkdirat returned 0) AND on the EEXIST path.
     The result MUST report a directory (`S_ISDIR`); if it reports a
     symlink (`S_ISLNK`) or any non-directory, MUST return
     `Error::SymlinkEscape` (the operator must remove the offending
     entry; the daemon MUST NOT auto-remove). In both branches the
     daemon MUST capture `(st_dev, st_ino)` from
     `fstatat(workspace_root_fd, slug, AT_SYMLINK_NOFOLLOW)` and
     persist it into the registry row's
     `parent_dev_ino: Option<(u64, u64)>` field (§4.2). The
     capture MUST happen after `mkdirat` succeeds and before
     `spawn_worker` returns control. On the fresh-create branch,
     persistence is the only write path that ever populates this
     field; the EEXIST branch MUST overwrite the registry value
     if it was populated by an earlier daemon process (legacy
     null tolerance per Appendix D — ITER3-FOLLOWUP-4). The
     captured `(st_dev, st_ino)` tuple is the defence-in-depth
     check used by §3.6 step 5 (cleanup-side parent-fd
     revalidation).
   - 5b. **Run leaf.** Open the slug parent as `slug_fd` via
     `openat(workspace_root_fd, slug, O_DIRECTORY | O_NOFOLLOW)`.

     **Slug-fd `(st_dev, st_ino)` reassertion (normative;
     closes `fstatat`→`openat` window, symmetric with step 5a's
     `mkdirat`→`fstatat` close).** After
     `slug_fd ← openat(workspace_root_fd, slug, O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`,
     the daemon MUST `fstat(slug_fd)` and assert
     `(stat.st_dev, stat.st_ino) == row.parent_dev_ino`. On
     mismatch, fail with
     `Error::ParentDevInoMismatch { expected, actual }` and roll
     back per step 10.

     Then `mkdirat(slug_fd, sanitize_run_id(run_id), 0o755)`. On `EEXIST` the
     daemon MUST NOT immediately return `Error::WorkspaceAlreadyExists` —
     it MUST consult the registry to disambiguate orphan-vs-duplicate per
     the decision table below (gpt-N-2 fix; prior text misclassified
     orphan leaves as duplicates):

     **Step 5b lookup invariant (normative).** The "matching row"
     lookup referenced by this step (and the inline-reclaim
     deadlock-avoidance paragraph below) uses the registry snapshot
     taken BEFORE step 1.4's `Creating` placeholder was inserted.
     The current invocation's own placeholder MUST NOT be treated
     as the matching row. Implementations SHOULD capture the lookup
     result during step 1.2 (which already iterates the registry)
     and reuse it here, rather than re-querying after step 1.4.

     | Dir at `<slug>/<run_id>/` exists | Registry row exists | `runner_uuid` alive? | Classification | Action |
     |---|---|---|---|---|
     | Yes | No | n/a | ORPHAN (crash mid-create) | Enqueue `(slug, run_id)` for asynchronous `OrphanReclaim` on the reconcile queue (§4.5); return `Error::WorkspaceBusyOrReclaiming`. |
     | Yes | Yes | Yes (`heartbeat_fresh` **OR** `pid_live_same_pgrp`) | DUPLICATE | Return `Error::WorkspaceAlreadyExists`. |
     | Yes | Yes | No (`!heartbeat_fresh` **AND** `!pid_live_same_pgrp`, i.e. heartbeat stale **AND** pid dead/in different pgrp) | ORPHAN (stale row) | Enqueue `(slug, run_id)` for asynchronous `OrphanReclaim` on the reconcile queue (§4.5); return `Error::WorkspaceBusyOrReclaiming`. |

     **Inline-reclaim deadlock avoidance (normative).** If
     `mkdirat` returns `EEXIST` and the discovered leaf has no
     matching registry row (Cleaned-or-absent — `Cleaned` is not a
     persisted row; row removal IS the `Cleaned` terminal per §4.3)
     OR the matching row's `Status` is `CleanupFailed` OR
     `OrphanPending` (both are "reconcile owns this row" states):

     **Pre-enqueue inline re-probe (B22 — fail-closed corner-case
     defense for step 5b; normative).** Before enqueuing
     `OrphanReclaim { slug, run_id }` for an `OrphanPending` source
     row, the orchestrator MUST perform §3.6 step 4's layered
     liveness probe IN FULL. To do so, it MUST first acquire
     `leaf_fd ← openat(slug_fd, sanitize_run_id(run_id),
     O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)` (same fd-anchored
     openat-through-slug_fd discipline as §3.6 step 3; `slug_fd` is the fd the create
     path opened earlier in this §3.5 step 5b) and then run the exact
     fd-anchored procedure of §3.6 step 4 (heartbeat-file `mtime`
     check via `fstatat(leaf_fd, ".caduceus-heartbeat",
     AT_SYMLINK_NOFOLLOW)` against `2 × heartbeat_interval`, AND
     `(pid AND pgrp)` existence check against the row's persisted
     `last_known_pid` / `last_known_process_group_id`; the
     conjunction is the Z-10 fail-closed `alive()` predicate stated
     below). On `ENOENT` / `ELOOP` from the `openat` (the leaf
     vanished or was replaced by a symlink between `mkdirat`'s
     `EEXIST` and this re-probe), the row is treated as
     `OrphanedNoLeaf` and the enqueue proceeds without re-probe (the
     no-leaf condition makes liveness moot). The `leaf_fd` MUST be
     closed before this step continues. Verdict handling:

     - `Live` ⇒ the runner is still alive; the orchestrator MUST
       NOT enqueue and MUST return `Error::WorkspaceBusy` to the
       caller (the row's `OrphanPending` classification was stale
       and the runner has resumed reporting).
     - `Dead` (both signals positively miss — heartbeat positively
       stale AND pid positively dead/in different pgrp) ⇒ proceed
       with the enqueue below; the positive-disproof verdict is
       attached to the queue entry so §5B.2 step 7's drain need
       not re-probe.
     - `Inconclusive` (signals disagree, or any signal is
       inconclusive — e.g. EPERM /proc on a sandboxed runner) ⇒
       Z-10 fail-closed: the orchestrator MUST NOT enqueue and
       MUST return `Error::WorkspaceBusy` to the caller. The next
       periodic reconcile pass will re-probe the row from
       `OrphanPending` per §5B.2 step 7's `OrphanPending`
       re-evaluation.

     This re-probe is logically equivalent to invoking §3.6 step 4
     inline (NOT a separate procedure); §3.6 step 4 is the
     canonical definition and any change to it propagates here
     automatically. The pseudocode comment at this site MUST
     cross-reference §3.6 step 4. This re-probe applies ONLY to
     `OrphanPending` source rows: `CleanupFailed` is a
     deterministic per-row failure with no live runner to probe,
     and the no-row case has no runner record. The §5B.2 step 7
     `OrphanReclaim`-queue drain still skips step 4 because the
     inline re-probe at this site already performed it.

     - Enqueue `(slug, run_id)` for asynchronous `OrphanReclaim`
       on the reconcile queue (§4.5) and return
       `Error::WorkspaceBusyOrReclaiming` to the caller.
     - The synchronous `create_workspace` path MUST NOT call
       `cleanup_workspace` inline while holding any of the
       per-slug, per-workspace, or registry-wide locks
       established in §3.5 step 1.3 / 1.3a.
     - The reconciler owns reclaim execution under §4.5
       lock-acquisition order (registry-wide → per-slug →
       per-workspace).

     This avoids deadlock between the synchronous create path
     and any concurrent reattach / cleanup path competing for
     the per-workspace lock.

     **Sub-case (B21 — carried-forward `CleanupFailed`/`OrphanPending`
     row, leaf absent at step 5b; normative).** If step 1.4 carried
     forward a row with `Status ∈ {CleanupFailed, OrphanPending}`
     (i.e. step 1.4 did NOT insert a fresh `Creating` placeholder
     per its precondition), AND the leaf inode is absent at step
     5b's `mkdirat`/lookup (the `mkdirat` returns 0, NOT `EEXIST`,
     OR the lookup does not find the leaf — a rare race where
     reconcile cleaned the leaf but did not yet drop the row, or
     where step 1.2's snapshot was taken before reconcile's row
     deletion), the orchestrator MUST treat this as a "row-only
     leftover" — there is no cleanup work to do on disk:

     1. The orchestrator MUST NOT proceed with the create against
        the freshly-`mkdirat`-created leaf using the carried-forward
        row. It MUST `unlinkat(slug_fd, sanitize_run_id(run_id),
        AT_REMOVEDIR)` any leaf this step's `mkdirat` just created
        (best-effort; failure logged) and abort the create.
     2. Enqueue `OrphanReclaim { slug, run_id }` on the reconcile
        queue (§4.5) so the reconciler drops the row (idempotent:
        if reconcile races and drops the row first, the queued
        reclaim becomes a no-op when §3.6 step 3's leaf-fd
        acquisition short-circuits to `OrphanedNoLeaf`).
        The `OrphanPending` sub-case of this carry-forward path
        is also subject to the pre-enqueue inline re-probe above.
     3. Return `Error::WorkspaceBusyOrReclaiming` to the caller.
        The orchestrator MAY retry after backoff once the
        reconcile queue has drained.

     This sub-case is intentionally fail-safe: the orchestrator
     does NOT delete the registry row directly in step 5b; row
     deletion is exclusively the reconciler's responsibility per
     §5B.2.

     **Z-10 — explicit liveness predicate (normative).** The
     `runner_uuid alive?` column above is decided by the layered probe
     in §3.6 step 4, but the v1 *fail-closed* binding for the table is
     stated here so two implementations cannot disagree on the
     boundary:

     ```text
     // alive == true  ⇒ the daemon MUST NOT delete (DUPLICATE row).
     // alive == false ⇒ the daemon MAY proceed to OrphanReclaim.
     // The decision is fail-closed: any inconclusive probe MUST
     // collapse to alive == true, NOT to alive == false. (The
     // earlier "(heartbeat stale OR pid dead)" disjunction was
     // dangerous: a transient missing /proc entry on a sandboxed
     // runner — pid_dead == true under a uid-mismatched probe —
     // would, with OR, fire orphan reclamation against a healthy
     // workspace. The conjunction below cannot.)
     fn alive(workspace) -> bool {
         let hb = heartbeat_file_mtime(workspace);
         let heartbeat_positively_stale =
             hb.is_ok()
             && now() - hb.unwrap() > 2 * heartbeat_interval;
         // Inconclusive (Err) ⇒ NOT positively stale ⇒ treated as alive-side.

         let pid_positively_dead = match probe_pid(workspace.last_known_pid) {
             ProbeResult::Dead => true,                      // ESRCH
             ProbeResult::AliveDifferentPgrp => true,        // pgrp drift
             ProbeResult::Alive => false,
             ProbeResult::Inconclusive => false,             // EPERM / sandbox / no /proc
         };

         // Disproved iff BOTH signals positively indicate non-life.
         // Any inconclusive probe collapses that signal to "alive-side",
         // so the conjunction below cannot fire on inconclusive input.
         let disproved = heartbeat_positively_stale && pid_positively_dead;
         !disproved
     }
     // Equivalently, the table's "No" cell is:
     // !alive == heartbeat_positively_stale && pid_positively_dead
     // i.e. heartbeat positively stale AND pid positively dead/in different pgrp.
     ```

     Z-15 implication: `last_known_pid` and pgid are read from the
     registry's persisted columns, and `last_heartbeat_at` is now a
     `SystemTime` (Z-15) so cross-restart probes have a stable
     reference frame.

     Liveness for the table is decided per §3.6 step 4 (heartbeat-file
     primary, cwd-probe confirmatory).
   - 5c. **Leaf fd for hooks.** Open the run-leaf as `leaf_fd` via
     `openat(slug_fd, sanitize_run_id(run_id), O_DIRECTORY | O_NOFOLLOW)`.
     This `leaf_fd` is the cwd handle that step 6 hooks inherit.
   - The `P_exists` boundary identified by §3.4 step 2 MUST be consistent with
     the directories materialized here: implementations MUST NOT walk past a
     boundary the validator did not authorize. On platforms without `*at`
     primitives, an equivalent fd-anchored construction (e.g. Windows
     `CreateFileW` + relative ops via the directory handle) MUST be used; the
     property "no path component is re-resolved by the kernel from a string
     after validation" is normative.
6. **Establish hook execution context.** Compose the environment for hook subprocesses:
   - cwd: hook subprocesses MUST inherit cwd via `fchdir(leaf_fd)` (or the
     platform-equivalent fd-based chdir) before `execve`. Passing the string
     `target` for re-resolution by the kernel at hook start is FORBIDDEN —
     that would re-introduce the TOCTOU window step 5 closes.
   - `CADUCEUS_WORKSPACE_PATH = canonical_target` (without trailing slash for
     hook ergonomics; canonical form per §3.4).
   - `CADUCEUS_RUN_ID = run_id` (raw, NOT sanitized — hooks may need the
     original). **Shell-injection notice:** the raw form MAY contain
     characters that are special to a shell. Hook authors writing
     shell-fragment hooks (e.g. `bash -lc "git clone $URL $CADUCEUS_RUN_ID"`)
     MUST quote `"$CADUCEUS_RUN_ID"` or use the safe form below. Hooks that
     interpolate `CADUCEUS_RUN_ID` into a shell command line without quoting
     MUST use `CADUCEUS_RUN_ID_SAFE` instead.
   - `CADUCEUS_RUN_ID_SAFE = sanitize_run_id(run_id)` — the
     filesystem-safe / shell-safe form of `run_id`, equal to the leaf segment
     used in `CADUCEUS_WORKSPACE_PATH`. Always present; never empty (callers
     that reach this step have already passed §3.2). This is the form
     recommended for unquoted shell interpolation.
   - `CADUCEUS_REPO_SLUG = slug`.
   - `CADUCEUS_REPO_REMOTE_URL = repo_coordinate.remote_url` (empty string if `None`).
   - `CADUCEUS_REPO_DEFAULT_BRANCH = repo_coordinate.default_branch` (empty string if `None`).
   - `PATH` inherited from daemon process; nothing else from the daemon environment is forwarded by default. Implementations MAY allow workflow-declared environment forwarding, but MUST NOT forward by default.
7. **Run `before_create` hook.** If defined, execute as a subprocess with the context above. Non-zero exit ⇒ MUST jump to step 10 with `Error::HookFailed("before_create", exit_code)`.
8. **Run `after_create` hook.** Typically a `git clone` or `git worktree add`. Same context, same failure handling. Non-zero exit ⇒ jump to step 10 with `Error::HookFailed("after_create", exit_code)`.
8.5. **Leaf-ownership handoff (§5A.5).** After step 8 returns
   successfully, the daemon MUST transfer leaf ownership to
   `runner_uid`/`runner_gid` via
   `fchownat(slug_fd, sanitize_run_id(run_id), runner_uid, runner_gid, AT_SYMLINK_NOFOLLOW)`.
   On success, the daemon MUST set a local
   `chowned_to_runner = true` flag (consumed by step 10a-pre on
   the rollback path).
   On failure ⇒ jump to step 10 with
   `Error::WorkspaceOwnershipFailed { reason }` (with
   `chowned_to_runner == false`, since the chown did not
   succeed). Sequenced after
   `after_create` because `after_create` typically runs as daemon
   uid (§5A.4 default) and writes into the leaf (`git clone`,
   `git worktree add`); chowning earlier would deny the daemon
   write access (EACCES) under the leaf's mode. See §5A.5 for
   the full normative form and rationale.
9. **Snapshot metadata.** Construct the `Workspace` record with:
   - `workspace_id = wsp_<hex(BLAKE3_128_keyed(slug || 0x1F || safe_run_id))>`
   - `path = canonical_target` (the canonicalized form returned by
     `validate_workspace_path` in step 3; see Invariant I-1). The
     pre-canonical `target` from step 2 MUST NOT be persisted.
   - `root = workspace_root` (the canonicalized daemon-wide root passed
     to `build_workspace_path` in step 2; see §3.4 for canonicalization
     semantics). Y-9: this is the same value spec #1 §3.3 hands to
     `spawn_worker` and that spec #2 §3.1 passes to
     `validate_workspace_path(workspace.path, workspace.root)` on the
     runner side. `path` MUST be a strict descendant of `root`; the
     prefix relationship was already enforced in step 3 and is
     re-asserted here as a debug-only invariant.
   - `created_at = now()`
   - `branch_at_create = read_current_branch_at(leaf_fd)` — fd-anchored
     using the `leaf_fd` opened in step 5c. Re-resolution from the
     `target` string (`read_current_branch(target)`) is FORBIDDEN: it
     would re-introduce the post-validation TOCTOU window that step 5
     closes (gpt-N-3 fix). On platforms without `*at` git plumbing,
     equivalent fd-anchored construction (e.g. `git --git-dir=<fd-relative>`
     or libgit2 with a directory handle) MUST be used. Best-effort: if
     no git repo present, `None`.
   - `repo_coordinate` and `run_id` as supplied
   Briefly re-acquire the registry-wide mutex; transition the placeholder row
   inserted in step 1.4 from `Status::Creating` to `Status::Active` and
   overwrite `path` with `canonical_target`; release the registry-wide mutex.
   Release the per-workspace lock. Return the `Workspace` (the per-slug
   shared-repo guard remains held — it is released only at cleanup, §3.6
   step 8).
10. **Cleanup-on-failure (Z-12: inline rollback, no recursive call).**
    Calling `cleanup_workspace(partial_workspace, reason=CreateFailed, Some(&placeholder_row))`
    here would deadlock: §3.6 step 1 acquires the per-workspace lock,
    but step 1.3a of *this* algorithm already holds it. Instead, the
    rollback MUST be performed **inline** (no recursive entry into
    §3.6) using the fds already open in this scope:

    a-pre. **Ownership reclaim if step 8.5 succeeded
       (normative).** The daemon tracks a local
       `chowned_to_runner` flag, set to `true` immediately after
       the §3.5 step 8.5 `fchownat` returns 0. If rollback
       enters step 10 with `chowned_to_runner == true`, the
       daemon MUST issue
       `fchownat(slug_fd, sanitize_run_id(run_id), daemon_uid, daemon_gid, AT_SYMLINK_NOFOLLOW)`
       BEFORE the fd-anchored unlink walk in sub-step (a). On
       reclaim failure: log, transition the placeholder row to
       `Status::CleanupFailed` with
       `Error::CleanupOwnershipFailed`. Skip sub-steps (a), (b),
       and (c); proceed directly to sub-step (d) (lock-release
       order is normative on every rollback exit, per step 1b).
       The daemon MUST NOT attempt the unlink walk (sub-step (a))
       in this case, because a daemon-uid rollback would EACCES
       mid-walk on a runner-owned leaf and leak the orphan;
       reconcile (§4.3) resumes from `CleanupFailed`. After (d),
       return the original triggering error per Invariant I-7;
       `CleanupOwnershipFailed` is a side-channel state, not the
       returned value. If `chowned_to_runner == false` (step
       8.5 never succeeded, including the
       `Error::WorkspaceOwnershipFailed` failure branch), this
       sub-step is skipped — the leaf is already daemon-owned.
    a. **Best-effort fd-anchored leaf removal.** If `leaf_fd` was
       successfully opened in step 5c, walk it with the same
       `openat`/`unlinkat`/`fdopendir` machinery §3.6 step 7 uses
       (CVE-2022-21658 class avoidance) and remove the leaf via
       `unlinkat(slug_fd, sanitize_run_id(run_id),
       AT_REMOVEDIR)`. If `leaf_fd` was never opened (failure was
       upstream of step 5c), skip this sub-step.
       **Parent `(st_dev, st_ino)` re-validation (normative).**
       Before issuing the `unlinkat(slug_fd, …, AT_REMOVEDIR)`, the
       daemon MUST re-`fstatat(workspace_root_fd, slug,
       AT_SYMLINK_NOFOLLOW)` and compare the result's
       `(st_dev, st_ino)` tuple to `row.parent_dev_ino` captured in
       step 5a. On mismatch (the slug parent has been swapped out
       from under us — e.g. an operator manually `rm -rf`'d the
       parent and a malicious sibling re-created it, or a bind-mount
       swap raced the create), the daemon MUST abort the inline
       removal with `Error::ParentRevalidationFailed`, transition
       the placeholder row to `Status::CleanupFailed`, and leave the
       leaf for reconcile to handle. Skip the remainder of
       sub-step (a), (b), and (c); proceed directly to sub-step
       (d) (lock-release order is normative on every rollback
       exit, per step 1b). After (d), return the original
       triggering error per Invariant I-7;
       `ParentRevalidationFailed` is a side-channel state, not
       the returned value. The daemon MUST NOT issue any
       `unlinkat` against a slug parent whose `(st_dev, st_ino)`
       tuple no longer matches the create-time capture. (`slug_fd`
       itself is fd-anchored and immune to swap, but the tuple
       mismatch is diagnostic of operator-or-attacker activity and
       MUST NOT be silently papered over.)
    b. **Hook for `before_cleanup`/`after_cleanup` is NOT run on
       this path.** Rollback is a daemon-internal operation; the
       workflow's teardown hooks are reserved for the
       run-completed/run-cancelled paths in §3.6 (rationale: a
       half-created workspace did not run `after_create` to
       completion, so workflow-authored teardown has nothing to
       reverse). Workflows that need symmetric setup/teardown MUST
       use `before_create`/`after_create` carefully so that a
       partial `after_create` is itself idempotent.
    c. **Registry placeholder removal.** Briefly re-acquire the
       registry-wide mutex; remove the `Status::Creating`
       placeholder row inserted in step 1.4. Release the
       registry-wide mutex.
    d. **Lock release order (Z-14 echo).** Release the
       per-workspace lock FIRST, then release the per-slug
       shared-repo guard (the same order §3.6 step 9 mandates for
       the success path). The per-slug guard remains held across
       (a)–(c) so a concurrent `create_workspace` call on the same
       slug observes the rollback in progress and waits.
    e. **Error masking.** Rollback failures (e.g. unlinkat error
       mid-walk) MUST be logged but MUST NOT mask the original
       `Error::HookFailed` returned to the caller. On mid-walk
       unlinkat failure, surface `Error::CleanupIncomplete
       (rollback-side)` (per §4.4 taxonomy) and transition the
       row to `Status::CleanupFailed`. If the leaf walk fails
       partway, the registry row MUST be transitioned to
       `Status::CleanupFailed` so reconcile retries (§4.3) can
       resume the cleanup later.

    Return the original error. See Invariant I-7.

**Determinism:** Two concurrent `create_workspace` calls with identical `(repo_coordinate, run_id)` MUST be serialized by the registry write lock; the second MUST observe the first's leaf and fail with `Error::WorkspaceAlreadyExists` (a sub-case of step 5). Idempotency at a higher layer (the orchestrator) is the orchestrator's concern.

**Hook execution contract:**

- **Timeouts.** Each hook subprocess MUST be subject to a wall-clock timeout. Default `before_create` / `before_cleanup`: 30 seconds. Default `after_create`: 600 seconds (typical `git clone` worst case on a large repo). Default `after_cleanup`: 30 seconds. All four are workflow-overridable. Timeout MUST kill the subprocess (SIGTERM, then SIGKILL after a 5-second grace) and MUST be reported as `Error::HookTimeout("<phase>")`, treated identically to `Error::HookFailed` for rollback purposes (Invariant I-7).
- **stdout / stderr capture.** Both streams MUST be captured line-buffered and forwarded to the daemon's per-run log channel (spec #1). Hooks MAY emit progress; consumers (the runs panel, spec #8) MAY surface the most recent line. Capture buffers MUST be capped (default 1 MiB per stream) — beyond the cap, oldest output is dropped and a marker line is inserted. Hooks MUST NOT be terminated for over-talkative output.
- **stdin.** Hooks receive `/dev/null` on stdin. Interactive hooks are unsupported.
- **Signals.** SIGCHLD is the only OS signal the daemon explicitly handles to detect hook exit. SIGINT to the daemon MUST propagate to a SIGTERM of any in-flight hook subprocess after the daemon's own shutdown grace period.
- **PATH.** Hook PATH MUST be the daemon's PATH at startup. Implementations MUST NOT extend it with workspace-local bin directories by default (a workflow MAY do so by setting `PATH` in its own environment forwarding).

**TOCTOU defense (normative summary).** The create path closes the
validate→create race window by combining four mechanisms; an
implementation that omits any of them is non-conformant:

1. **`workspace_root_fd` is opened once at daemon startup** with
   `O_DIRECTORY | O_NOFOLLOW`-equivalent semantics (step 5
   prefix). Every subsequent path resolution is anchored on this
   fd, so an attacker who swaps `<workspace_root>` post-startup
   cannot redirect daemon writes.
2. **Slug parent is materialized via `mkdirat(workspace_root_fd,
   slug, …)` followed unconditionally by
   `fstatat(workspace_root_fd, slug, AT_SYMLINK_NOFOLLOW)`** on
   both the fresh-create and `EEXIST` branches (step 5a). The
   `fstatat` MUST report `S_ISDIR`; symlink ⇒ `Error::SymlinkEscape`.
3. **`(st_dev, st_ino)` is captured BEFORE any `fchdir` or hook
   spawn** (step 5a, before step 6) and persisted into the
   registry's `parent_dev_ino` field. Step 6 (`fchdir(leaf_fd)`)
   and steps 7–8 (hook spawn) are the first operations that
   change cwd; step 5a runs before any of them, so the captured
   tuple is taken under the original, validated parent.
4. **Rollback (step 10a) re-`fstatat`s the slug parent** and
   compares against `row.parent_dev_ino` before issuing any
   `unlinkat`. Mismatch ⇒ refuse to delete and surface
   `Error::ParentRevalidationFailed`. The same re-validation
   protects the cleanup path (§3.6 step 5).

The window between `mkdirat` (step 5a) and the first
`fstatat(AT_SYMLINK_NOFOLLOW)` is closed because both syscalls
are anchored on `workspace_root_fd`: any attacker substitution
that races between them either fails the `mkdirat` (`EEXIST` and
`fstatat` reports a non-directory) or is detected by the
`fstatat` `S_ISDIR` check. There is no string-path resolution
between validate and create; therefore there is no TOCTOU
window the kernel can re-resolve through.

### 3.6 `cleanup_workspace(workspace, reason, registry_row) → Result<(), Error>`

**Purpose:** Tear down a RunWorkspace, including running user-supplied teardown hooks.

**Inputs:**
- `workspace: Workspace` — typically read from the registry; in the create-failure path of §3.5, a partial record.
- `reason: CleanupReason` — one of `RunCompleted`, `RunCancelled`, `CreateFailed`, `OrphanReclaim`, `OperatorRequested`. Forwarded to the hook environment as `CADUCEUS_CLEANUP_REASON`.
- `registry_row: Option<&Row>` — the registry row backing this cleanup (§5B.2 step 5 discriminator). `Some(row)` ⇒ row-backed cleanup; `None` ⇒ synthetic no-row leaf reclaim. The caller MUST pass the correct value; §3.6 MUST NOT infer it from `workspace.parent_dev_ino`'s nullity (legacy rows may carry `None` while still being row-backed — see Appendix D ITER3-FOLLOWUP-4).

**Algorithm:**

1. **Acquire registry write lock for this workspace.** A workspace-level lock; concurrent `cleanup` calls on different workspaces MAY proceed in parallel.

   **Lock acquire order (FORBIDDEN to invert):**
   `registry-wide → per-slug → per-workspace`.
   **Release order is the REVERSE of acquire:**
   `per-workspace → per-slug → registry-wide`.
   Any code path that takes these locks in any other order is a defect and
   MUST be caught by the lock-order linter (see §6 T-N where applicable).
   See §4.5 Lock hierarchy for the canonical statement.
2. **Re-validate path.** Call `validate_workspace_path(workspace.path, workspace_root)`. On any error MUST refuse to proceed and return `Error::ValidationFailed`. **MUST NOT** delete anything if validation fails. This is Invariant I-2; it is the load-bearing safety property of this algorithm.
3. **Fd-acquisition prelude (MUST run before liveness probe).**
   Before any liveness probe, the cleaner MUST acquire the leaf fd
   through the anchored chain:
   1. `workspace_root_fd` (already held by the daemon process; opened
      once at startup with `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`).
   2. `slug_fd ← openat(workspace_root_fd, slug, O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`.
      On `ENOENT`, classify the row as `OrphanedNoSlug` and
      short-circuit to cleanup-cleared. (Earlier drafts called
      this `parent_fd`; the alias has been removed for
      unambiguity.)
   3. `leaf_fd ← openat(slug_fd, sanitize_run_id(run_id), O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`.
      On `ENOENT` or `ELOOP` (symlink), classify the row as
      `OrphanedNoLeaf` and short-circuit to cleanup-cleared.

   These fds are reused by step 4 (liveness probe) and step 5
   (parent revalidation + `before_cleanup` hook); no string-path
   resolution may occur after this point. The fd-acquisition prelude
   MUST run BEFORE any liveness probe — performing the probe before
   the prelude would leave the cleaner stringly-resolving paths or
   probing a path component that has since been swapped.
4. **Liveness probe (fd-anchored).** The liveness probe MUST be
   performed through `leaf_fd` (e.g.,
   `fstatat(leaf_fd, ".caduceus-heartbeat", AT_SYMLINK_NOFOLLOW)`)
   using the fd acquired in step 3. The cleaner MUST NOT re-resolve
   any path string at this step.

   **OrphanReclaim-queue bypass (pointer to canonical statement).**
   Rows reaching this step via the `OrphanReclaim` queue MUST skip ONLY the
   layered liveness probe of step 4 and MUST execute steps 5-9 unchanged.
   The canonical enqueue-source list, skip scope, and rationale live at
   §5B.2 step 7; any divergence is a defect and §5B.2 step 7 wins.

   Look up the daemon's running-runs map (spec #1) by `workspace.run_id`.
   - If a live agent process is recorded in the in-memory map with cwd inside
     `workspace.path`, behaviour depends on `reason`:
     - `RunCancelled`, `OperatorRequested`: signal the agent (spec #2 cancellation), wait up to `cleanup_grace_ms` (default 5000ms, configurable), then proceed.
     - `RunCompleted`, `CreateFailed`: a live process here is a contract
       violation; MUST log a warning and refuse cleanup
       (`Error::LiveAgentPresent`).
   - **Daemon-restart liveness gap.** The in-memory running-runs map is empty
     after a daemon restart. Trusting it alone would let `OrphanReclaim`
     delete the cwd of a still-running agent. Therefore, when
     `reason == OrphanReclaim`, the daemon MUST verify liveness against
     persisted state (§4.2 fields `last_known_pid`,
     `last_known_process_group_id`, `last_heartbeat_at`,
     `heartbeat_interval`) using the following layered probes:

     - **Heartbeat-file probe (PRIMARY signal; N-4 fix).** Each spawned
       runner MUST write a heartbeat file at
       `<workspace.path>/.caduceus-heartbeat` containing
       `{pid, runner_uuid, mtime}` every `heartbeat_interval` (default
       5s). The file is fd-anchored under the workspace leaf
       (`openat(leaf_fd, ".caduceus-heartbeat", O_WRONLY|O_CREAT|O_NOFOLLOW)`);
       only the spawned runner uid may write to it (`0600`, owned by
       runner uid). The OrphanReclaim sweep classifies the workspace
       as ORPHAN iff **both**:
         1. The heartbeat file's `mtime` (read via
            `fstatat(leaf_fd, ".caduceus-heartbeat", AT_SYMLINK_NOFOLLOW)`)
            is older than `2 × heartbeat_interval` (default 10s), AND
         2. The recorded `pid` is dead (no live OS process matches),
            or the process is in a different process group than
            `last_known_process_group_id`.
       If either condition fails (heartbeat fresh, OR pid live in same
       pgrp), the daemon MUST NOT delete: transition to
       `Status::OrphanPending` and return without error; reconcile
       retries on a later pass.

     - **Cwd-probe (CONFIRMATORY only).** A `/proc/<pid>/cwd` probe
       (or platform-equivalent process-scan) MAY be used as a
       confirmatory signal when available, but SHOULD NOT be the sole
       input. Under sandboxed runners (separate uid namespace, e.g.
       bubblewrap/firejail) the daemon may be unable to read other-uid
       `/proc` entries — the probe then returns inconclusive and
       SHOULD silently degrade to "heartbeat is sole signal", NOT to
       false-orphan classification.

     - **Privilege model.** The daemon SHOULD run with
       `CAP_DAC_READ_SEARCH` on Linux to read other-uid `/proc`
       entries for confirmatory cwd-probes. If this capability is
       unavailable, the heartbeat file is the SOLE liveness signal and
       implementations MUST document this in the deployment runbook.
       See §3.8 for the heartbeat-file forge threat model.

     - Liveness MUST be considered "cannot be disproved" if the
       heartbeat probe is inconclusive (e.g. file missing on a
       pre-heartbeat workspace, or fs error reading mtime) AND the
       cwd-probe is unavailable or inconclusive — in that case the
       daemon MUST refuse to delete and transition to `OrphanPending`.
       Fail-closed.
     - Only when liveness is positively disproved (heartbeat stale
       AND pid dead, optionally confirmed by cwd-probe outside
       `workspace.path`) MAY the daemon proceed with steps 5–9
       (i.e. step 5 parent revalidation + `before_cleanup`,
       step 5a ownership reclaim, step 6 path re-validation,
       step 7 recursive remove, step 8 `after_cleanup`, and
       step 9 lock release).
5. **Parent revalidation + `before_cleanup` hook (fd-anchored;
   N-1 fix).** With `slug_fd` and `leaf_fd` already
   open from step 3, perform the parent inode revalidation and run
   the `before_cleanup` hook:
   - Validate the parent against the slug-derived `(st_dev, st_ino)`
     tuple recorded at create time. The cleaner MUST `fstat(slug_fd)`
     and assert
     `(stat.st_dev, stat.st_ino) == row.parent_dev_ino` before any
     unlink. On mismatch, classify as `BindMountSwap` and refuse to
     clean (transition to `CleanupFailed`; reconcile retries) —
     surface `Error::ParentRevalidationFailed`. (Parent
     revalidation is a deterministic per-row failure, not a
     liveness-uncertainty signal; routing here MUST be
     `CleanupFailed`, NOT `OrphanPending` — the latter is
     reserved for liveness-uncertain rows awaiting reconcile
     re-evaluation.)
   - **Legacy null tolerance (TOFU rule, normative).** If
     `row.parent_dev_ino == None` (legacy row written by a pre-Z6
     daemon, per Appendix D ITER3-FOLLOWUP-4), apply the **TOFU
     rule**: the cleaner captures `(st_dev, st_ino)` from the
     current `slug_fd`, writes it back to the registry row under
     the per-workspace lock, and continues. The TOFU rule MUST be
     logged as `parent_dev_ino_tofu_populated`. Subsequent cleanups
     on the same row MUST then enforce the strict mismatch check.

     **Exception — `registry_row == None` (no-row leaf reclaim).**
     The TOFU rule MUST NOT apply when `registry_row.is_none()`
     (the §5B.2 step 5 path). That path has no registry row to
     write back to and no historical capture to defer to; per
     §5B.2 step 5 the cleaner instead captures the slug parent's
     `(st_dev, st_ino)` once at fd-acquisition and enforces
     byte-for-byte equality on every subsequent `fstatat` within
     the same cleanup (fail-closed on mismatch). The
     discriminator MUST be the explicit `registry_row` parameter,
     NOT inferred from `workspace.parent_dev_ino`'s nullity
     (legacy rows may carry `None` while still being row-backed —
     see Appendix D ITER3-FOLLOWUP-4).
   - On `EACCES` from `fstat(slug_fd)` ⇒ abort cleanup with
     `Error::ParentRevalidationFailed`. (`ENOENT`/`ELOOP` were
     already short-circuited in step 3.) Do NOT fall back to
     string-path resolution.
   - All subsequent reads/writes in steps 6–8 MUST use `slug_fd` /
     `leaf_fd`-relative ops; the kernel MUST NOT re-resolve any path
     component from a string after this point.
   - With `slug_fd` and `leaf_fd` open, run the `before_cleanup` hook
     if defined: same env context as §3.5 step 6, plus
     `CADUCEUS_CLEANUP_REASON`. cwd MUST be inherited via
     `fchdir(leaf_fd)` (NOT by re-resolving the string `workspace.path`).
   - Non-zero exit ⇒ log a warning. **MUST proceed with cleanup** — a
     teardown hook failure MUST NOT block reclamation. (Symphony's
     posture; preserved.)
5a. **Ownership reclaim (normative).** Before recursive removal
   of the leaf, the daemon MUST issue
   `fchownat(slug_fd, sanitize_run_id(run_id), daemon_uid, daemon_gid, AT_SYMLINK_NOFOLLOW)`
   to reclaim leaf ownership from `runner_uid` (handed off in
   §5A.5 / §3.5 step 8.5). Without this, a daemon with non-root
   uid cannot unlink runner-owned files inside a `0700` leaf.
   On failure ⇒ return `Error::CleanupOwnershipFailed`;
   transition the row to `CleanupFailed` so reconcile retries.
   (Ownership reclaim failure is a deterministic per-row
   failure, not a liveness-uncertainty signal; routing here MUST
   be `CleanupFailed`, NOT `OrphanPending`.)
   The slug parent itself is not chowned (it remains
   daemon-owned per §5A.5).
6. **Re-validate path.** Call `validate_workspace_path(workspace.path, workspace_root)` AGAIN immediately before the destructive step. This narrows the TOCTOU window to milliseconds. MUST refuse on any error.
7. **Recursive remove (fd-anchored; N-2 fix, CVE-2022-21658 class).** A
   direct `std::fs::remove_dir_all(string_path)` is FORBIDDEN — the
   string-path form is vulnerable to the symlink-race CVE class
   (CVE-2022-21658 and successors). The implementation MUST:
   - Walk `leaf_fd` (opened in step 3) using fd-relative `openat` /
     `unlinkat` / `fdopendir` primitives only. Implementation guides:
     `nix::dir::Dir` + `nix::unistd::unlinkat`,
     `std::os::unix::fs::OpenOptionsExt`, or the
     `openat`/`openat2` libc bindings. At each directory, open child
     entries with `O_NOFOLLOW`; symlinks MUST be unlinked via
     `unlinkat(parent_dir_fd, name, 0)` without ever following the
     link target.
   - On encountering a symlink, unlink only the symlink, never its
     target.
   - After the recursive walk drains the leaf, remove the leaf itself
     via `unlinkat(slug_fd, sanitize_run_id(workspace.run_id),
     AT_REMOVEDIR)`.
   - On any error mid-walk, abort the walk and return
     `Error::CleanupIncomplete` with the partial state recorded in the
     registry as `Status::CleanupFailed`. The next reconcile pass MAY
     retry.

   **Normative:** Implementations MUST use fd-relative removal
   (`openat`-walk + `unlinkat`). Implementations MUST NOT call any
   function that resolves symlinks during cleanup — including
   `realpath`/`canonicalize` on intermediate paths,
   `std::fs::remove_dir_all` on a string path, `chdir(string_path)`,
   or any helper that internally re-opens a path component by name.
8. **Run `after_cleanup` hook.** Step 7 has just removed `workspace.path`;
   using the deleted leaf as cwd is FORBIDDEN (it would either refuse to
   spawn or, worse, succeed against a stale fd).

   **Reserved-key collision handling (normative).** If the
   workflow's env-forwarding policy (per spec #6) attempts to
   forward any key matching the reserved `CADUCEUS_*` prefix or
   any of the explicitly-reserved keys listed in this section,
   hook spawn MUST fail with `Error::HookEnvConflict { key:
   <key> }` and emit a `CRITICAL` log with the offending key and
   the run_id. The hook MUST NOT be spawned with a tampered or
   shadowed reserved key. This applies to all four hooks
   (`before_create`, `after_create`, `before_cleanup`,
   `after_cleanup`).

   The daemon MUST instead set:
   - **cwd via `fchdir(slug_fd)` (Z-13, normative).** The hook
     subprocess inherits cwd by `fchdir`-ing the daemon's `slug_fd`
     (opened in step 3) BEFORE `execve`. This is the same fd-anchored
     discipline §3.5 step 6 applies to `before_create` /
     `after_create`; re-resolving the parent path from a string here
     would re-introduce a TOCTOU window between the leaf delete in
     step 7 and the hook spawn (an attacker who races
     `<workspace_root>/<slug>/` to a symlink in this gap would land
     the hook in an unexpected directory). The `slug_fd` is
     guaranteed live across step 7 because step 7 only operates
     fd-relative through `slug_fd` and `leaf_fd` and never closes
     the parent. The hook process inherits the daemon's PATH and the
     reserved env (§3.5 step 6) plus:
   - `CADUCEUS_WORKSPACE_PATH` = the *former* `workspace.path` (so the hook
     can still report on what was removed).
   - `CADUCEUS_CLEANUP_REASON` = `<reason>`.
   - `CADUCEUS_WORKSPACE_REMOVED = "1"` (Z-13, normative) — explicit
     signal that the path no longer exists. Hook authors checking
     "did the leaf get removed" MUST consult this flag rather than
     `stat`-ing `CADUCEUS_WORKSPACE_PATH` (which by construction
     would race with concurrent reconcile retries against the same
     parent inode, even though the leaf itself is gone). The flag is
     present ONLY on `after_cleanup` invocations that follow a
     successful step 7 leaf removal; on any path that surfaces
     `Error::CleanupIncomplete` and re-enters via reconcile retry,
     the daemon MUST re-derive the flag value at hook-spawn time
     based on whether the leaf is now absent.

   **`after_cleanup` hook environment (normative table).** The
   complete set of env vars set by the daemon for the
   `after_cleanup` hook is:

   | Variable | Value | Always present? |
   |---|---|---|
   | `CADUCEUS_WORKSPACE_PATH` | the former (now-removed) `workspace.path`, canonical form, no trailing slash | yes |
   | `CADUCEUS_PARENT_PATH` | `<workspace_root>/<repo_slug>` — the slug parent that survives leaf removal; what `slug_fd` from step 3 points at | yes |
   | `CADUCEUS_WORKSPACE_ROOT` | the daemon-wide canonical `workspace_root` | yes |
   | `CADUCEUS_REPO_SLUG` | `repo_coordinate.slug` | yes |
   | `CADUCEUS_REPO_REMOTE_URL` | `repo_coordinate.remote_url` (empty string if `None`) | yes |
   | `CADUCEUS_REPO_REMOTE_URL_SAFE_B64` | base64url (unpadded) encoding of `CADUCEUS_REPO_REMOTE_URL` for safe shell-fragment use; hook authors SHOULD prefer this over the raw URL in shell-quoted contexts | yes |
   | `CADUCEUS_REPO_DEFAULT_BRANCH` | `repo_coordinate.default_branch` (empty string if `None`) | yes |
   | `CADUCEUS_RUN_ID` | raw `run_id` (NOT sanitized; quote in shell) | yes |
   | `CADUCEUS_RUN_ID_SAFE` | `sanitize_run_id(run_id)` | yes |
   | `CADUCEUS_CLEANUP_REASON` | one of `RunCompleted` / `RunCancelled` / `CreateFailed` / `OrphanReclaim` / `OperatorRequested` | yes |
   | `CADUCEUS_WORKSPACE_REMOVED` | `"1"` iff step 7 succeeded; otherwise `"0"` | yes |
   | `PATH` | inherited from daemon startup PATH | yes |

   No other daemon env is forwarded by default (Invariant I-9).
   Workflow-declared opt-in forwarding MAY add entries; it MUST NOT
   shadow any of the reserved `CADUCEUS_*` keys.
   ITER4-FOLLOWUP: spec #6 (workflow) MUST cross-reference this
   table when documenting hook authoring; this spec does not edit
   spec #6.

   Failures MUST be logged and MUST NOT raise (parity with §3.6 step 6
   posture).
9. **Remove registry entry, release locks (Z-14: explicit release
   order).** Delete the row keyed by `workspace_id`. The lock
   release order is **normative** (the REVERSE of acquire order
   `registry-wide → per-slug → per-workspace`):

   1. Release the **per-workspace lock** (the lock acquired in step
      1) FIRST. After this point, a concurrent `cleanup_workspace`
      call on the same `workspace_id` observes a missing registry
      row and proceeds idempotently per §4.3 (registry-row removal IS the terminal state; `Cleaned` is not a persisted status).
   2. Release the **per-slug shared-repo guard** SECOND (the guard
      acquired by `acquire_shared_repo_lock`, §3.7). After this
      point, `create_workspace` on the same slug may proceed to
      step 4 of §3.5.

   Release order: `per-workspace` before `per-slug`, which is the
   REVERSE of acquire order (`per-slug → per-workspace`, see §3.5
   step 1.3 / 1.3a and §4.5). Inverting release-order risks a
   deadlock against a concurrent `create_workspace` that holds
   `per-slug` and is waiting for `per-workspace`. Additionally, a
   panic between row delete and slug-guard release would leave the
   slug guard orphaned, preventing all future creates on that slug
   — release-in-REVERSE-of-acquire is the standard idiom that
   defends against both failure modes.

**Safety check (verbatim port of Symphony's posture, SPEC §9.5 line 891):** If at any point in steps 6–7 the path being operated on does NOT begin with `workspace_root`, the entire operation MUST abort and the implementation MUST log a `CRITICAL`-severity message. This is the canary that catches a programming error in a future change to §3.4.

### 3.7 `acquire_shared_repo_lock(repo_coordinate, intent: Read | Write) → LockGuard` *(caduceus-new)*

**Purpose:** Mediate two or more Runs targeting the same `repo_coordinate`. This is novel relative to Symphony, which by virtue of "one process, one repo, one workspace per issue" never had two Runs collide on the same working tree.

**Caller → strategy → discipline (N-3 fix):**

| Caller | Discipline | On contention |
|---|---|---|
| `create_workspace` (§3.5, synchronous) | **Try-lock** (refuse-fast) | Return `Error::SharedRepoLocked` immediately; orchestrator decides whether to retry with backoff. |
| `OrphanReclaim` background sweeper (§3.6, async) | **Wait** (blocking) | Sweeper has no caller to refuse-fast back to; MAY block until the slug guard is free. |

The blocking-`Wait` discipline below applies ONLY to the `OrphanReclaim`
background sweeper. The synchronous `create_workspace` path MUST NOT use
`Wait`.

**Strategies:** An implementation MAY choose one of:

- **(a) Strict serialization (default v1).** Per `repo_coordinate.slug`, a
  single mutex covers all Runs. Synchronous callers (`create_workspace`)
  use **try-lock** semantics per the table above and MUST receive
  `Error::SharedRepoLocked` immediately on contention. The
  `OrphanReclaim` background sweeper is the ONLY caller permitted to
  `Wait` on the mutex; write fairness (FIFO) applies under `Wait`.
  **Concurrent Runs on the same repo are forbidden in v1**; this is the
  conservative posture per §8 open question.
- **(b) Read-write lock.** Per `repo_coordinate.slug`, a sloppy RwLock: N concurrent `Read` Runs may proceed; a single `Write` Run excludes all others. Reads cannot mutate the working tree (no commit, no checkout, no `git pull`). Enforcement of "read-only" is best-effort via hook conventions; the lock itself only mediates intent declaration.
- **(c) Worktree-isolated.** A single `BareCheckout` per `repo_coordinate.slug` lives at `<workspace_root>/<slug>/.bare/`. Each Run's working tree is `git worktree add`'d into `<workspace_root>/<slug>/<run_id>/`. The bare repo's `.git` dir is shared (single source of truth for refs, objects), but each worktree has an independent index and HEAD. Writes serialize per-repo at the `.git/index.lock` level (git's own mechanism); reads parallelize naturally. This is the recommended v2 strategy when concurrency on shared repos is enabled.

**Default:** v1 implementations MUST default to strategy (a) and MUST refuse to admit a second Run on a repo whose first Run has not finished cleanup. The error returned is `Error::SharedRepoLocked` from §3.5 step 4.

**Migration path:** When Q1 (multi-repo work unit) and the related concurrency question are resolved (§8), strategy (c) is the preferred upgrade. The on-disk layout `<workspace_root>/<slug>/{.bare,<run_id>}/` is forward-compatible; v1 simply omits `.bare/`.

**Lock guard contract:** The returned `LockGuard` MUST be released on workspace cleanup (§3.6 step 9) **and** on `create_workspace` failure-rollback (§3.5 step 10). Lock leaks here are a high-severity bug: the next Run on the same repo will hang.

### 3.8 Heartbeat file & threat model *(caduceus-new; N-4 follow-up)*

**Purpose:** Document the privilege model and forge-resistance properties
of the `.caduceus-heartbeat` liveness file introduced by §3.6 step 4.

**File location & permissions.** The heartbeat file lives at
`<workspace.path>/.caduceus-heartbeat`. It MUST be:

- Created via `openat(leaf_fd, ".caduceus-heartbeat",
  O_WRONLY|O_CREAT|O_NOFOLLOW|O_CLOEXEC, 0o600)` — fd-anchored under
  the workspace leaf; never opened by string path after the leaf is
  established.
- Owned by the spawned runner uid (set via `fchown` on the fd before
  the first write, or by virtue of the runner's effective uid at
  create time).
- Mode `0600` — readable and writable only by the runner uid. Other
  uids on the host MUST NOT have write access.

**Forge resistance.** The threat is an attacker writing a fresh
heartbeat to keep a dead workspace pinned (denying OrphanReclaim) or,
conversely, suppressing a fresh heartbeat to cause false-orphan
deletion. Mitigations:

- **Write side:** `0600` + runner-uid ownership + fd-anchored open
  means only the spawned runner uid can write. A different uid on the
  host (including other agents in different sandboxes) cannot forge.
- **Read side:** the daemon reads the heartbeat via
  `fstatat(leaf_fd, ".caduceus-heartbeat", AT_SYMLINK_NOFOLLOW)`. A
  symlink installed in place of the heartbeat file MUST NOT be
  followed; `fstatat` reports `S_ISLNK` and the daemon MUST treat the
  workspace as ORPHAN-suspect (heartbeat unreadable) and apply the
  fail-closed policy from §3.6 step 4 — i.e. transition to
  `OrphanPending` rather than delete.
- **Replay across runs:** a stale heartbeat from a prior run cannot
  resurrect a fresh leaf because §3.5 step 5b's discriminator table
  consults registry liveness, not the heartbeat alone. A stale
  heartbeat in an orphan leaf only delays reclamation by one sweep
  cycle.
- **Sandboxed runner uid mismatch:** if the runner's effective uid in
  the sandbox differs from its uid as seen by the daemon (e.g.
  user-namespace mapping), the operator MUST configure the sandbox
  uid map so the heartbeat file is daemon-readable. Failure to do so
  degrades the daemon to "heartbeat unreadable" and triggers the
  fail-closed posture above.

**Privilege model summary.** The daemon SHOULD run with
`CAP_DAC_READ_SEARCH` for confirmatory `/proc/<pid>/cwd` probes; if
unavailable, the heartbeat file is the SOLE liveness signal and
operators MUST document the runner-uid → daemon-uid trust boundary in
their deployment runbook.

---

## 4. Data shapes

```rust
struct Workspace {
    workspace_id: WorkspaceId,          // wsp_<32-hex>; derived (Invariant I-6).
    run_id: RunId,                       // opaque (spec #7); NOT sanitized in this struct.
    repo_coordinate: RepoCoordinate,
    /// Per-Run working tree root — the path the agent runs in
    /// (canonicalized, §3.4). This is the path that is `cd`'d into
    /// before launching the worker, that gets `.git/` and the
    /// repo's working files, and that gets cleaned up on Run exit.
    path: AbsolutePath,
    /// Operator-configured workspace root that holds *all* per-Run
    /// working trees for this daemon, i.e. the parent under which
    /// `path` was created (Y-9). Equal to `config.workspace_root`
    /// canonicalized at startup (§3.4 / spec #1 §6 config validation).
    /// Spec #1 §3.3 propagates `root` to `spawn_worker` so the worker
    /// can resolve sibling Run workspaces (e.g. for cross-Run
    /// reference) and so spec #2 §3.1 can `validate_workspace_path(
    /// workspace.path, workspace.root)` — the per-Run path MUST be a
    /// strict descendant of `root` after canonicalization, otherwise
    /// `create_workspace` is a programmer-error and panics. Stable
    /// for the lifetime of the daemon process.
    root: AbsolutePath,
    created_at: SystemTime,              // monotonic+wall hybrid; see spec #1.
    branch_at_create: Option<String>,    // best-effort; None if no git repo.
}

struct RepoCoordinate {
    slug: RepoSlug,                      // ^[a-z0-9][a-z0-9_]{0,63}$
    remote_url: Option<Url>,             // advisory; absent ⇒ slug-only repo (e.g. local fixtures).
    default_branch: Option<String>,      // advisory; populated by daemon registration, refreshed by hooks.
}

type WorkspaceId = String;               // "wsp_" + 32 hex chars.
type RepoSlug    = String;               // see regex above.
type RunId       = String;               // opaque per spec #7.
type AbsolutePath = PathBuf;             // host-platform absolute path.
```

### 4.1 Filesystem layout

```
<workspace_root>/
├── owner_repo/                          # RepoSlug directory; persistent across runs.
│   ├── .bare/                           # OPTIONAL bare git checkout (strategy (c) only).
│   ├── 01H8XYZ.../                      # RunWorkspace for run_id #1.
│   │   ├── .git/                        # full clone OR worktree pointer.
│   │   └── ...                          # working tree.
│   └── 01H8ABC.../                      # RunWorkspace for run_id #2 (worktree-isolated).
│       └── ...
└── other_owner_other_repo/
    └── 01H8QRS.../
        └── ...
```

**Notes:**

- The `<repo_slug>/` parent directory is created lazily on first Run for that repo and MAY be reused across Runs. It MUST NOT be deleted on individual Run cleanup; only on operator-driven repo deregistration (out of scope for this spec, see spec #1).
- The `.bare/` directory is OPTIONAL and present only under strategy (c) of §3.7. Under strategy (a) or (b), it is absent.
- No sibling files are placed under `<workspace_root>/<repo_slug>/` other than `.bare/` and the per-run leaves. A future `<repo_slug>/.cache/` is reserved (out of scope for v1).

### 4.2 Daemon registry shape

The `caduceusd` registry is the source of truth for every active or recently-cleaned-up Workspace. Engine and runner MUST read via the daemon API (spec #1); they MUST NOT scan the filesystem to discover workspaces.

The registry MUST persist (across daemon restart) at minimum:
- `workspace_id`, `run_id`, `repo_coordinate`, `path`, `created_at`, `branch_at_create`.
- A `status: Creating | Active | CleaningUp | CleanupFailed | OrphanPending` field.
  (`Creating` is the placeholder state from §3.5 step 1.4; `OrphanPending` is
  the state §3.6 step 4 transitions to when liveness cannot be disproved.
  `Cleaned` is NOT a persisted status — registry-row removal IS the terminal
  state; see §4.3.)

  ```rust
  // Status alias for cross-spec references:
  type WorkspaceStatus = Status;
  // Status = Creating | Active | CleaningUp | CleanupFailed | OrphanPending
  ```
- For any runner that was ever live for this row, the daemon MUST persist the
  following liveness-trace fields (used by §3.6 step 4's `OrphanReclaim`
  cross-restart liveness check; nullable until a runner has reported):
  - `last_known_pid: Option<u32>` — pid reported at runner start (and on
    each heartbeat).
  - `last_known_process_group_id: Option<u32>` — pgid (or platform-
    equivalent process-group identifier) at runner start.
  - `last_heartbeat_at: Option<SystemTime>` — wall-clock timestamp of the
    most recent runner heartbeat. **Z-15: this field MUST be a
    `SystemTime` (wall-clock, persistable), NOT an `Instant`
    (monotonic, process-local).** The earlier `Option<Instant>` shape
    was incorrect: `Instant` is opaque and meaningless across daemon
    restart, but the §3.6 step 4 / §3.5 step 5b liveness probe
    explicitly runs after restart (a fresh daemon process inherits
    the persisted registry state) and must compare against
    `now: SystemTime`. Implementations MUST persist this field via
    the same on-disk encoding the registry uses for `created_at`
    (a 64-bit ms-since-UNIX-epoch is RECOMMENDED). The §3.6 step 4
    "older than `2 × heartbeat_interval`" comparison is on wall
    time. Implementations MAY additionally persist a process
    start-time companion to defend against pid reuse; doing so is
    RECOMMENDED on platforms where pid reuse is observable within
    typical daemon-restart windows.
  - `heartbeat_interval: Duration` — default `5s`; configurable per
    workflow. OrphanReclaim treats a heartbeat file with mtime older
    than `2 × heartbeat_interval` as stale (§3.6 step 4, N-4).
  - `parent_dev_ino: Option<(u64, u64)>` — the `(st_dev, st_ino)`
    tuple of the slug directory at create time, captured by
    `fstatat(workspace_root_fd, slug, AT_SYMLINK_NOFOLLOW)` at §3.5
    step 5a. Used by §3.6 step 5 to defeat bind-mount and
    inode-recycle attacks (Z6-F1) and to defence-in-depth validate
    the parent-fd against parent-swap during cleanup (N-1 fix).
    `None` only on legacy rows; populated on next successful cleanup
    revalidation per §3.6 step 5 TOFU rule.

In addition, the runner (spec #2) MUST write a heartbeat file under
the workspace leaf:

- `<workspace.path>/.caduceus-heartbeat` — JSON `{pid, runner_uuid,
  mtime}`, mode `0600`, owned by the spawned runner uid. Written every
  `heartbeat_interval`. The daemon reads it via
  `fstatat(leaf_fd, ".caduceus-heartbeat", AT_SYMLINK_NOFOLLOW)` only
  (never follows symlinks). See §3.6 step 4 for OrphanReclaim
  semantics and §3.8 for the heartbeat-forge threat model.

**Two-source heartbeat persistence (normative).** Liveness is
derived from two persisted artifacts:

1. **Leaf-relative heartbeat file** (`<workspace.path>/.caduceus-heartbeat`).
   Written by the runner; the file's `mtime` (read via fd-anchored
   `fstatat`) is the PRIMARY signal. Lives on the workspace
   filesystem; survives daemon restart unless the leaf itself is
   gone.
2. **Registry-stored `last_heartbeat_at: SystemTime`** (Z-15). Written
   by the daemon on each heartbeat report from the runner. Lives in
   the daemon registry (parent-anchored — i.e. NOT on the workspace
   leaf), so it is observable on daemon restart even if the leaf
   filesystem is offline. Used as the secondary/cross-check signal.

The two sources MUST NOT contradict each other by more than
`2 × heartbeat_interval + clock_skew_tolerance`. If the registry
field reports a heartbeat newer than the leaf file's mtime by more
than this margin, the daemon MUST log a warning (possible
file-tampering or filesystem rollback) and fall back to the
fail-closed posture from §3.6 step 4 (treat as alive).

**Daemon restart recovery (normative).** On daemon startup, for
each registry row in `Active` or `OrphanPending`:

1. The daemon MUST NOT auto-cleanup based solely on
   `last_heartbeat_at` being older than `heartbeat_stale_threshold`.
   Auto-cleanup at startup is FORBIDDEN; the daemon MUST mark such
   rows as `OrphanPending` and let the reconcile pass (§7 item 4)
   re-evaluate liveness via §3.6 step 4 (which combines the leaf
   heartbeat file with optional `/proc` probe).
2. If the leaf heartbeat file is fresh (mtime within
   `2 × heartbeat_interval` of `now`), the row remains `Active`;
   the runner is presumed to have outlived the daemon.
3. If both signals are stale (`last_heartbeat_at` older than
   `heartbeat_stale_threshold` AND leaf file mtime older than
   `2 × heartbeat_interval` AND pid dead/in different pgrp), the
   row is reclaimable by `cleanup_workspace(_, OrphanReclaim, Some(&row))` on
   the first reconcile pass. The reconcile pass — not the startup
   path — issues the destructive cleanup.
4. If `last_heartbeat_at == None` (the row was created but the
   runner never heartbeat'd, e.g. crash mid-§3.5 step 9), the
   row is treated identically to "stale" for reclaim purposes,
   subject to the same reconcile flow.

`heartbeat_stale_threshold` defaults to `max(60s, 12 × heartbeat_interval)`
to absorb daemon restart latency and brief network/IO stalls.

**Clock-skew tolerance.** All `SystemTime` comparisons (registry
`last_heartbeat_at` vs. `now()`, leaf-file `mtime` vs. `now()`)
MUST tolerate a `clock_skew_tolerance` of `±5s` by default
(configurable). A heartbeat whose recorded time is up to `5s` in
the future is treated as fresh (clock drift between runner host
and daemon-observed wall clock — note that on a single-host v1
deployment these are the same clock, but the tolerance hardens
against monotonic-vs-wall transitions across suspend/resume and
NTP step adjustments). A heartbeat whose recorded time is more
than `5s` in the future MUST be logged as suspicious and treated
as if it were `now() - heartbeat_interval` (fail-closed: refuse to
trust the future timestamp, but do not delete on its account).

Recovery semantics on daemon restart are owned by spec #1 (orchestrator) and intersect with §7 (out of scope).

### 4.3 Workspace lifecycle states

The `status` field on a registry row transitions through the following states. Transitions are owned by the algorithms above; this table is informative.

| State | Entered when | Exits to | Notes |
|---|---|---|---|
| `Creating` | `create_workspace` (§3.5) step 1.4 inserts the placeholder row | `Active` (on step 9 success); `OrphanReclaim` queue (row remains `Creating` until reconcile drains queue, when liveness is positively disproved at startup/reconcile per §5B.2 step 4); `OrphanPending` (when liveness inconclusive at reconcile); on step 10 rollback the row is **removed iff rollback reaches step 10c**, otherwise retained as `Status::CleanupFailed` if rollback aborts at `Error::CleanupOwnershipFailed` (step 10a-pre), `Error::ParentRevalidationFailed` (step 10a), or mid-walk unlinkat failure (step 10e) per §3.5 step 1b | The runner MUST NOT bind cwd while status is `Creating`. |
| `Active` | `create_workspace` (§3.5) step 9 transitions placeholder | `CleaningUp` (operator/orchestrator-initiated cleanup), `OrphanPending` (reconcile detects liveness inconclusive at runtime, per §5B.1 / §5B.3), or via reconcile-driven `OrphanReclaim` (positively disproved, per §5B.2 step 4) | The runner MAY bind cwd only when status is `Active`. |
| `CleaningUp` | `cleanup_workspace` (§3.6) step 1 acquires the per-workspace lock | row removed (terminal: no `Cleaned` status persisted; row absence IS the terminal — §4.3), `CleanupFailed`, or `OrphanPending` | Liveness check (§3.6 step 4) and hooks run in this state. |
| `OrphanPending` | enters from: `Creating` (reconcile/startup), `Active` (reconcile), `CleaningUp` (cleanup-time inconclusive) | `Active` (if a subsequent §3.6 step 4 probe observes liveness as Alive / heartbeat fresh), or `CleaningUp` (only after liveness is positively disproved, reconcile enqueues `OrphanReclaim`, and §5B.2 step 7 drains that queue by re-entering §3.6 from step 2, skipping ONLY step 4); from `CleaningUp` then to row removed (terminal) or `CleanupFailed` | Reconcile re-probes; `OrphanPending` is not sticky if liveness becomes provably alive. |
| `CleanupFailed` | §3.6 step 7 aborts mid-walk; or §3.5 step 10a-pre (rollback `CleanupOwnershipFailed`), §3.5 step 10a (rollback `ParentRevalidationFailed`), or §3.5 step 10e (rollback mid-walk unlinkat failure) per §3.5 step 1b | `CleaningUp` (on retry) | Reconcile (spec #1) MUST retry; manual operator action is a fallback. |

A row in `Active` whose process tree has died (engine crash, runner crash) without orchestrator notification is detected by reconcile per the three-way switch: reconcile on `Active` row → liveness probe (§3.6 step 4): Alive ⇒ unchanged; Inconclusive ⇒ transition to `OrphanPending`; Positively disproved ⇒ enqueue `OrphanReclaim`.

### 4.4 Error taxonomy

The errors produced by this spec's algorithms are:

| Error | Source § | Recoverable? |
|---|---|---|
| `InvalidRemoteUrl` | §3.1 step 1 | No — caller bug; reject up the stack. |
| `InvalidRunId` | §3.2 rules 1, 4, 5 | No. |
| `InvalidRepoSlug` | §3.3 step 2 | No. |
| `PathTraversal` | §3.4 steps 1, 5 | No — likely tamper or bug. |
| `EscapedRoot` | §3.4 step 3 | No. |
| `SymlinkEscape` | §3.4 step 4 | No — operator must remove the offending symlink. |
| `FilesystemBoundary` | §3.4 step 6 (OPTIONAL) | Operator-resolvable. |
| `WorkspaceAlreadyExists` | §3.5 step 5 | At orchestrator layer (idempotency). |
| `WorkspaceBusy` | (1) §3.5 step 1.2 (registry row found with `Status ∈ {Creating, Active, CleaningUp}`; another orchestrator/runner is actively using or cleaning the workspace); (2) §3.5 step 1.3a (per-workspace lock try-lock failure on the same `workspace_id`); (3) §3.5 step 5b inline re-probe (B22) for an `OrphanPending` source row when the probe returns `Live` or `Inconclusive` (Z-10 fail-closed). | Yes — orchestrator MAY retry after backoff. |
| `WorkspaceBusyOrReclaiming` | §3.5 step 5b (EEXIST + (no matching row OR matching row's `Status ∈ {CleanupFailed, OrphanPending}`); reclaim deferred to reconciler. Per §3.5 step 1.2's status-sensitive routing, rows with `Status ∈ {Creating, Active, CleaningUp}` are routed to `WorkspaceBusy` at step 1.2 and never reach 5b's reclaim path. `Cleaned` is not a persisted status — row absence is the terminal per §4.3.) | Yes — orchestrator MAY retry after backoff once reconcile drains the `OrphanReclaim` queue. |
| `HookEnvConflict { key }` | §3.6 step 8 (workflow env-forwarding policy attempted to forward a reserved `CADUCEUS_*` key) | No — workflow-author bug; emits `CRITICAL` log with offending key and run_id. |
| `ParentRevalidationFailed` | §3.6 step 5 (parent-fd inode mismatch / EACCES / ENOENT / ELOOP); §3.5 step 10a (rollback-side parent `(st_dev, st_ino)` re-validation before `unlinkat`) | Yes — reconcile retries; persistent failure ⇒ operator. |
| `SharedRepoLocked` | §3.5 step 4, §3.7 | Yes — orchestrator MAY queue. |
| `HookFailed(phase, exit_code)` | §3.5 steps 7–8, §3.6 step 5 (warning only) | Workflow author's bug; no automatic retry. |
| `HookTimeout(phase)` | §3.5 hook contract | Same as `HookFailed`. |
| `LiveAgentPresent` | §3.6 step 4 | Yes — orchestrator should not have called cleanup yet. |
| `ValidationFailed` | §3.6 steps 2, 6 | No — `CRITICAL` log; operator intervention. |
| `CleanupIncomplete` | §3.6 step 7 | Yes — reconcile retries. |
| `CleanupIncomplete` (rollback-side) | §3.5 step 10e (rollback-side mid-walk unlinkat failure) | Yes — row transitions to `Status::CleanupFailed`; reconcile retries identically. |
| `CleanupOwnershipFailed` | §3.6 step 5a (cleanup-side `fchownat` failure to reclaim leaf ownership from runner uid) | Yes — row transitions to `CleanupFailed` (deterministic per-row failure, NOT `OrphanPending`); reconcile retries; persistent failure ⇒ operator (likely missing `CAP_CHOWN`). |
| `CleanupOwnershipFailed` (rollback-side) | §3.5 step 10a-pre (rollback-side reclaim failure) | Yes — row transitions to `Status::CleanupFailed`; reconcile retries identically; persistent failure ⇒ operator. |
| `WorkspaceRootLocked` | Invariant I-8 | No — exit; operator chooses which daemon survives. |
| `WorkspaceRootMode` | §5A.2 startup audit | No — operator must `chmod 0700 <workspace_root>` and restart. |
| `WorkspaceOwnershipFailed { reason }` | §5A.5 (leaf-ownership handoff `fchownat` failure) | No — operator must grant `CAP_CHOWN` or fix runner uid; rollback per §3.5 step 10. |
| `ParentDevInoMismatch { expected, actual }` | §3.5 step 5b (slug_fd `(st_dev, st_ino)` reassertion) | No — bind-mount swap or operator interference; rollback per §3.5 step 10. |

### 4.5 Lock hierarchy *(normative)*

The daemon holds three logical locks during workspace lifecycle.
Acquisition order is normative; release order is the reverse
(LIFO), per the Z-14 echo in §3.5 step 10 and §3.6 step 9.

| Order | Lock | Scope | Held across |
|---|---|---|---|
| 1 | **Registry-wide mutex** | All registry rows | Steps 1.1–1.5 of `create_workspace` (placeholder insert) and the brief re-acquisition windows in §3.5 step 9 (Active transition) and §3.5 step 10c (placeholder removal). MUST NOT be held across hooks or filesystem operations. |
| 2 | **Per-slug shared-repo guard** (§3.7) | One per `repo_coordinate.slug` | Acquired in §3.5 step 1.3; held across the entire workspace lifecycle (steps 2–9 of create, *and* across the Run, *and* across cleanup); released in §3.6 step 9.2 OR §3.5 step 10d. |
| 3 | **Per-workspace lock** (keyed by `workspace_id`) | One per workspace | Acquired in §3.5 step 1.3a; held across steps 2–9 of create. Re-acquired in §3.6 step 1 for cleanup. Released in §3.6 step 9.1 OR §3.5 step 10d. |

**Acquisition order (FORBIDDEN to invert; normative; Z6-I1).**
When a code path needs more than one of these, it MUST acquire in
the order shown above: `registry-wide → per-slug → per-workspace`.
No code path may acquire in any other order; doing so introduces
deadlock potential between concurrent create/cleanup pairs on the
same slug.

**Release order (normative; REVERSE of acquire; Z-14 echo;
Z6-I1).** The release order is the REVERSE of acquire order:
`per-workspace → per-slug → registry-wide` (the registry-wide
mutex is briefly re-taken for placeholder-row mutation in §3.5
step 9 / step 10c and for row removal in §3.6 step 9, but the
LIFO ordering of the three logical lock holds is normative).
§3.5 step 10d and §3.6 step 9 both mandate this. Any code path
that releases in any other order is a defect and MUST be caught
by the lock-order linter (see §6 T-N where applicable). The
pattern "per-workspace before per-slug" defends against a panic
between row-delete and slug-guard release leaving the slug guard
orphaned (which would block all future creates on that slug),
and against a deadlock against a concurrent `create_workspace`
that holds `per-slug` and is waiting for `per-workspace`.

**Reattach path (`OrphanReclaim` from §3.5 step 5b or from the
background sweeper).** The reclaiming caller MUST acquire the
per-slug guard (Wait discipline for the background sweeper;
try-lock for the foreground create path) BEFORE the per-workspace
lock, identical to fresh-create. A reattach that skips the
per-slug guard violates I-5 and may race a concurrent create.

**Caller obligations.** No caller of any §3.5/§3.6 entry point may
hold any of the three locks above on entry. The daemon is the
sole owner of the lock hierarchy; engine and runner consume the
daemon API and hold no internal locks of this hierarchy.

---

## 5. Invariants (MUST)

The following invariants are normative. Implementations MUST establish these by construction; they MUST be testable from outside the implementation (see §6).

### Ported from Symphony SPEC §9.5 (lines 886–905), Apache-2.0

- **I-1 (Path containment).** For every Workspace `w` in the registry, `validate_workspace_path(w.path, workspace_root) == Ok(w.path)`. Equivalently: `w.path` canonicalizes to a descendant of `workspace_root`. Symphony reference: `workspace.ex:358–384`.
- **I-2 (Cleanup containment).** `cleanup_workspace` MUST refuse to remove any path whose canonicalization is not strictly under `workspace_root`. The canary check in §3.6 step 7 makes this self-enforcing in the destructive step. Symphony reference: SPEC §9.5 line 891 ("cleanup MUST NOT remove paths outside workspace_root").
- **I-3 (No symlink escape).** No path component of any Workspace, at any time during its lifetime, MAY be a symlink whose target canonicalizes outside `workspace_root`. Read-only operations are NOT exempt: even reading via an escaping symlink is forbidden because it leaks the daemon's privilege. Enforced by §3.4 step 4 and re-checked in §3.6 step 6. Symphony reference: SPEC §9.5 lines 895–901.

### Caduceus-new

- **I-4 (Slug stickiness).** Once a `RepoCoordinate` is recorded with `slug = S`, S MUST NOT change for the lifetime of that RepoCoordinate's registry row. If `remote_url` changes (owner rename, host migration), the registry MUST keep S and update only `remote_url` and `default_branch`. Rationale: existing Workspace paths embed S; mutating S would require an on-disk move with full `cleanup_workspace` semantics, which is operator-territory, not automatic.
- **I-5 (Shared-repo serialization).** Two Workspaces with the same `repo_coordinate.slug` MAY coexist in the registry **only** under §3.7 strategy (b) or (c). Under strategy (a) (v1 default), at most one Workspace per `repo_coordinate.slug` exists at any time. Concurrent writes to the same working tree are forbidden by all strategies; the lock guard mediates.
- **I-6 (Derivable workspace_id).** `workspace_id = "wsp_" + lower_hex(BLAKE3_128_keyed(slug || 0x1F || safe_run_id))` where the key is a daemon-instance-stable but non-secret 32-byte value (BLAKE3 keyed mode requires a 32-byte key; e.g. derived from the workspace_root path and a hardcoded domain separator via `BLAKE3_256(workspace_root || domain_separator)`). `safe_run_id = sanitize_run_id(run_id)` per §3.2, matching the §3.5 step 9 derivation. Two daemons computing this for the same `(slug, safe_run_id)` and the same `workspace_root` MUST produce the same `workspace_id`. This makes the id derivable for diagnostics; it MUST NOT be relied upon as a security token. Random workspace ids are FORBIDDEN — they would break reconcile (spec #1).
- **I-7 (No orphan dirs on hook failure).** If `before_create` or `after_create` returns non-zero, the leaf directory created in §3.5 step 5 MUST be removed before `create_workspace` returns to the caller iff rollback reaches step 10c; on rollback abort at `CleanupOwnershipFailed`, `ParentRevalidationFailed`, or mid-walk unlinkat failure, the leaf is retained for reconcile alongside the `CleanupFailed` row. The error returned MUST be `Error::HookFailed`, NOT a cleanup error from the rollback. See §3.5 step 10. Placeholder-row handling is conditional: rows are removed (terminal per §4.3; `Cleaned` is not a persisted status) iff rollback reaches step 10c; rollback-side aborts at `CleanupOwnershipFailed`, `ParentRevalidationFailed`, or mid-walk unlinkat failure retain the row as `Status::CleanupFailed` for reconcile (§3.5 step 1b).
- **I-8 (Single-writer registry).** At most one `caduceusd` process per `workspace_root` MAY hold an exclusive advisory lock (e.g. `flock(2)` on `<workspace_root>/.caduceusd.lock`) at any time. A second daemon started against the same root MUST refuse to come up. See test T-7. Rationale: two daemons would race the registry and produce orphaned leaves and double-locks.
- **I-9 (Hook isolation).** Hooks MUST observe only the environment specified in §3.5 step 6. They MUST NOT inherit the daemon's full environment (which may carry tokens) by default. Workflow-declared opt-in forwarding is permitted; default is deny.

---

## 5A. Security model *(normative)*

This section consolidates the threat model the algorithms in §3
defend against and the concrete enforcement points. It is
normative: an implementation that omits any of these is
non-conformant.

### 5A.1 Threat model

The daemon runs as a privileged process (uid distinct from the
runner uid in the recommended deployment) on a host that may
also run untrusted workloads, including:

- Other agents in differently-uid'd sandboxes (bubblewrap,
  firejail, or container).
- Operator-issued shell sessions.
- Background services with file-write permissions inside or
  adjacent to `<workspace_root>`.

Threats explicitly addressed:

| Threat | Surface | Defense (§) |
|---|---|---|
| **T-A: Symlink escape from `workspace_root`** — attacker pre-creates `<workspace_root>/<slug>` as a symlink to `/` to redirect `mkdir`/`open`/cleanup. | §3.4 step 4, §3.5 step 5a | Validate-and-`fstatat(AT_SYMLINK_NOFOLLOW)` on every materialized component; `O_NOFOLLOW` on every `openat`. |
| **T-B: TOCTOU swap between validate and create** — attacker swaps `<workspace_root>/<slug>` to a symlink in the gap between §3.4 validation and §3.5 step 5a `mkdirat`. | §3.5 step 5 | Eliminate the gap: `mkdirat(workspace_root_fd, …)` followed unconditionally by `fstatat(AT_SYMLINK_NOFOLLOW)` on both fresh and `EEXIST` branches. See test T-2b. |
| **T-C: Sibling-workspace race on parent inode** — a malicious sibling workspace (under a different `slug` but on the same `workspace_root`) attempts to swap our slug parent during cleanup. | §3.5 step 5a, §3.6 step 5, §3.5 step 10a | `parent_dev_ino` (the `(st_dev, st_ino)` tuple) captured at create; re-validated by `fstatat(workspace_root_fd, slug, AT_SYMLINK_NOFOLLOW)` before every destructive op. Mismatch ⇒ refuse cleanup, surface `Error::ParentRevalidationFailed` (or `Error::ParentDevInoMismatch` on the create path). The slug parent is a directory; sibling workspaces under a *different* slug write only into their own slug subtrees and have no path through which to swap a peer's slug entry without uid permissions on `workspace_root` itself. |
| **T-D: Recursive-remove follows symlink** (CVE-2022-21658 class) | §3.6 step 7 | fd-anchored `openat`/`unlinkat` walk; symlinks unlinked, never followed; `realpath`/`canonicalize` on intermediate paths is FORBIDDEN during cleanup. |
| **T-E: Heartbeat forge to pin a dead workspace** | §3.6 step 4, §3.8 | Heartbeat file mode `0600`, owned by runner uid, opened via `openat(leaf_fd, …, O_NOFOLLOW)`; only runner uid can write. See §3.8 forge resistance. |
| **T-F: Heartbeat suppression to false-orphan a live workspace** | §3.6 step 4 | Layered probe (heartbeat + pid+pgrp); fail-closed: inconclusive ⇒ `OrphanPending`, NOT row-removed (terminal). |
| **T-G: Token leak via hook env** | §3.5 step 6, I-9 | Default-deny env forwarding; only the reserved `CADUCEUS_*` keys + `PATH` are set. |
| **T-H: Two daemons clobber the same `workspace_root`** | I-8 | Exclusive `flock(2)` on `<workspace_root>/.caduceusd.lock`; second daemon exits with `Error::WorkspaceRootLocked`. |
| **T-I: Registry tampering** (e.g. attacker rewrites `path` field to `/etc`) | §3.6 step 2 | Re-validate via §3.4 before every destructive op; `Error::ValidationFailed` + `CRITICAL` log; `/etc` untouched. See test T-3. |

### 5A.2 Filesystem permissions *(normative)*

The following modes are normative. Implementations MUST set them
exactly:

| Path | Mode | Owner | Notes |
|---|---|---|---|
| `<workspace_root>/` | `0700` | daemon uid | Operator MUST `chmod 0700` on creation. World-readable workspace_root is FORBIDDEN — it would expose other-tenant workspace metadata to host-local processes. The daemon SHOULD verify the mode at startup and refuse to start (logged as `Error::WorkspaceRootMode`) if it is broader than `0700`; deployments that intentionally widen access (e.g. `0750` with a daemon-readers group) MUST opt in via an explicit config flag. |
| `<workspace_root>/.caduceusd.lock` | `0600` | daemon uid | Created by the daemon at startup; never world-readable. |
| `<workspace_root>/<slug>/` | `0755` | daemon uid | Slug parent is traversable by other uids on the host so the runner uid can `chdir` into the leaf. The slug parent is non-sensitive (its name is a sanitized public remote URL); permissions are deliberately weaker than `workspace_root` so a sandboxed runner can enter. |
| `<workspace_root>/<slug>/<run_id>/` | `0755` (v1 normative) | runner uid (after §5A.5 handoff) | Created `0755` and remains `0755` in v1; daemon cleanup reopens `leaf_fd` via `slug_fd` without additional capability. Tightening to `0700`/`0750` is deferred to spec #6 with an explicit capability/group model — implementations MUST NOT tighten unilaterally in v1. |
| `<workspace_root>/<slug>/<run_id>/.caduceus-heartbeat` | `0600` | runner uid | See §3.8. |

The `0700` requirement on `workspace_root` is the load-bearing
permission: it ensures that an attacker who is not the daemon uid
cannot enumerate slug names (a side-channel revealing what repos
this daemon orchestrates) and cannot pre-create slug entries to
trigger T-A.

### 5A.3 Symlink-traversal policy *(normative)*

`O_NOFOLLOW`-equivalent semantics MUST be applied at every fd
opening operation on a path under `<workspace_root>`:

| Operation | Where | Enforcement |
|---|---|---|
| Open `workspace_root_fd` | daemon startup | `O_DIRECTORY \| O_NOFOLLOW` |
| Open `slug_fd` (create) | §3.5 step 5b | `openat(workspace_root_fd, slug, O_DIRECTORY \| O_NOFOLLOW)` |
| Open `leaf_fd` (create) | §3.5 step 5c | `openat(slug_fd, sanitize_run_id(run_id), O_DIRECTORY \| O_NOFOLLOW)` |
| Open `slug_fd` (cleanup) | §3.6 step 3 | `openat(workspace_root_fd, slug, O_DIRECTORY \| O_NOFOLLOW)` |
| Open `leaf_fd` (cleanup) | §3.6 step 3 | `openat(slug_fd, sanitize_run_id(run_id), O_DIRECTORY \| O_NOFOLLOW)` |
| Recursive walk during cleanup | §3.6 step 7 | every child `openat` uses `O_NOFOLLOW`; symlinks `unlinkat`'d, never followed |
| Open heartbeat file | §3.8 | `openat(leaf_fd, ".caduceus-heartbeat", O_WRONLY\|O_CREAT\|O_NOFOLLOW\|O_CLOEXEC, 0o600)` |
| `fstatat` of slug parent (create capture, cleanup re-validate, rollback re-validate) | §3.5 step 5a, §3.6 step 5, §3.5 step 10a | `AT_SYMLINK_NOFOLLOW` |
| `fstatat` of heartbeat file | §3.6 step 4 | `AT_SYMLINK_NOFOLLOW` |

A path component that is a symlink at any of these enforcement
points MUST cause the operation to fail (`ELOOP` from
`O_NOFOLLOW`, or `S_ISLNK` from `fstatat`). Symlinks in the
recursive-remove walk are *unlinked* (the symlink itself), never
followed (the link target is never opened or removed).

`realpath(3)` / `canonicalize_path` is permitted only in §3.4
step 2 (ancestor canonicalization) and only against an
already-existing prefix; it is FORBIDDEN during cleanup and
during any post-validation operation. See §3.6 step 7 normative
note.

### 5A.4 Privilege model

- The daemon SHOULD run as a dedicated uid distinct from any
  runner uid.
- The daemon SHOULD have `CAP_DAC_READ_SEARCH` (Linux) for
  optional `/proc/<pid>/cwd` cross-uid probing in §3.6 step 4.
  Without it, the heartbeat file is the sole liveness signal
  (§3.8 privilege model summary).
- Runners SHOULD run as a uid distinct from the daemon, ideally
  one per workspace (or one per workflow tier) so heartbeat-file
  permissions provide meaningful uid-scoped isolation.
- The daemon MUST NOT run as `root` in production. A
  `root`-running daemon defeats the heartbeat-forge resistance
  (§3.8) because no uid boundary exists to forge across.
- If daemon uid ≠ runner uid, the daemon MUST have `CAP_CHOWN`
  (Linux; or platform-equivalent) — required by §3.5 step 8.5
  (handoff to `runner_uid`) and §3.6 step 5a (reclaim to
  `daemon_uid` before recursive removal). Without this
  capability, both create and cleanup paths fail closed: create
  rolls back per §3.5 step 10 (including the §3.5 step 10a-pre
  ownership reclaim if step 8.5 had already succeeded); cleanup
  transitions the row to `CleanupFailed` with
  `Error::CleanupOwnershipFailed` (deterministic per-row
  failure; routing here MUST be `CleanupFailed`, NOT
  `OrphanPending`). The daemon MUST refuse to
  start (capability error) if configured with
  `daemon_uid ≠ runner_uid` but lacking `CAP_CHOWN`. `CAP_CHOWN`
  is the substitute for root in this deployment model.

**Hook execution uid (normative).** Hook subprocesses
(`before_create`, `after_create`, `before_cleanup`,
`after_cleanup`) MUST be spawned with the daemon's effective
uid by default. Implementations MAY support a workflow-declared
`hooks.run_as` uid (e.g. drop to runner uid before `execve` via
`setresuid`), and SHOULD recommend it for hosts where the
workflow YAML is not fully trusted by the daemon operator.
Running hooks as the daemon uid concentrates privilege; the
operator MUST treat workflow YAML as trusted code unless
`hooks.run_as` is enforced.

### 5A.5 Leaf-ownership handoff *(normative; Z6-G1)*

After §3.5 step 8 (`after_create`) returns successfully, and
immediately before §3.5 step 9 (`Status::Creating →
Status::Active` snapshot/return — the handoff to spec #1
`spawn_worker`), the daemon MUST transfer leaf ownership to
`runner_uid`/`runner_gid`. The single normative call is:

```text
fchownat(slug_fd, sanitize_run_id(run_id), runner_uid, runner_gid, AT_SYMLINK_NOFOLLOW)
```

The slug parent (`workspace_root_fd, slug`) MUST remain
daemon-owned for the lifetime of the daemon; any code path that
chowns the slug parent is a defect.

The leaf directory mode remains `0755`: daemon retains read
access (for liveness probes via `slug_fd`-anchored `openat`,
NEVER via string-path traversal), runner has write access for
its own heartbeat file. The heartbeat file is then created by
the runner under its own uid with mode `0600` per §5A.2.

**Failure modes.** If `fchownat` fails (e.g., daemon lacks
`CAP_CHOWN`, `runner_uid` is invalid, or filesystem rejects the
chown), `create_workspace` MUST fail with
`Error::WorkspaceOwnershipFailed { reason }` and execute the
rollback path per §3.5 step 10. Partial state (orphan leaf dir
owned by daemon uid) MUST be unlinked iff rollback reaches step
10c; on abort at `CleanupOwnershipFailed`,
`ParentRevalidationFailed`, or mid-walk unlinkat failure
(`CleanupIncomplete (rollback-side)`), the leaf is retained for
reconcile per §3.5 step 1b.

**Rationale.** Without this handoff, the leaf is daemon-owned
(default `mkdirat` → daemon uid) but the heartbeat file MUST be
runner-uid `0600` per §5A.2; the runner cannot
`openat(O_CREAT)` into a daemon-owned `0755` directory whose
DAC mode bits deny cross-uid create on a `0755` daemon-owned dir. The handoff makes the
heartbeat-create syscall path realizable in the default
config.

Sequenced after `after_create` because `after_create` typically
runs as daemon uid (§5A.4 default) and writes into the leaf
(`git clone`, `git worktree add`); chowning earlier would deny
the daemon write access (EACCES) under the leaf's mode.

Leaf-directory permissions are defined exclusively in §5A.2
(`0755` v1 normative). §5A.5 defines ownership transfer only
and MUST NOT redefine mode policy.

---

## 5B. Crash recovery *(normative)*

This section specifies orchestrator-restart and daemon-crash
recovery semantics. It is the normative companion to §7 item 4
(deferral of registry persistence implementation to spec #1).

**Reclaim-queue contract (normative).** All callers MUST enqueue
`OrphanReclaim { slug, run_id }` — never `OrphanReclaim
{ workspace_id }` — onto the reconcile queue. `workspace_id` is
derivable at drain time from `(slug, run_id)` per Invariant I-6.
This single shape applies uniformly to enqueues from §3.5 step 5b
(inline-reclaim deferral), §5B.2 step 4 (transient-state
re-classification of `Creating` / `Active` / `CleaningUp` rows,
including the `CleaningUp` startup recovery sub-case per B22),
§5B.2 step 5 (orphan-leaf scan), and §5B.2 step 7 (reconcile
re-evaluation of `OrphanPending`). The
`OrphanReclaim` queue is the canonical reclaim path: callers MUST
NOT bypass it by transitioning rows directly to `Status::CleaningUp`
outside §3.6 step 1.

### 5B.1 Crash points

The daemon may crash at any line. The recovery procedure depends
on which `Status` the registry row last persisted:

| Last persisted `Status` | Filesystem state likely | Classification at startup | Action on first reconcile pass |
|---|---|---|---|
| (no row) | leaf may exist if the leaf created at §3.5 step 5b `mkdirat` survived but the placeholder row from step 1.4 is absent at startup (registry-row loss before durable persistence, or a leaf left by a historical daemon not in the current registry); very narrow window — surfaced by the leaf scan at §5B.2 step 5 | ORPHAN-LEAF | Detected by leaf scan (§7 item 4); `cleanup_workspace(_, OrphanReclaim, None)` (synthetic no-row leaf reclaim per §5B.2 step 5). |
| `Creating` | leaf may or may not exist; `after_create` may have started | ORPHAN-CREATING | Reconcile pass on `Creating` row → run layered liveness probe (§3.6 step 4): Alive ⇒ unchanged; Inconclusive ⇒ transition to `OrphanPending`; Positively disproved (incl. no-pid/no-hb age rule per §5B.2 step 4) ⇒ enqueue `OrphanReclaim`. |
| `Active` | leaf exists; runner may be alive | LIVE-OR-ORPHAN | §3.6 step 4 layered liveness probe (heartbeat file + pid+pgrp). Alive ⇒ leave `Active`. Disproved ⇒ enqueue `OrphanReclaim { slug, run_id }`; row stays in `Active` until reconcile drains the queue (consistent with §5B.2 step 4). Inconclusive ⇒ `OrphanPending`. |
| `CleaningUp` | leaf may be partially removed | PARTIAL-CLEANUP | `cleanup_workspace(_, OrphanReclaim, Some(&row))` re-enters §3.6 from step 2, skipping ONLY step 4 (the layered liveness probe was already performed by the orchestrator at enqueue time, so the workspace is known unrecoverable); steps 2, 3, 5, 5a, 6, 7, 8, 9 run unchanged (see §5B.2 step 7 for the canonical bypass scope). The canonical statement of the OrphanReclaim-queue bypass scope (which steps are skipped, and from which enqueue sources) is §5B.2 step 7. |
| `OrphanPending` | leaf exists, runner dead-or-uncertain | ORPHAN-PENDING | Re-run §3.6 step 4 on each reconcile pass. Once liveness positively disproved, enqueue `OrphanReclaim { slug, run_id }`; the subsequent reconcile drain (§5B.2 step 7) transitions the row `→ CleaningUp` and re-enters §3.6 from step 2 (skipping ONLY step 4). The `OrphanPending` row MUST NOT transition directly to `CleaningUp` outside the queue drain — the queue is the canonical reclaim path. |
| `CleanupFailed` | leaf partially removed | OPERATOR-OR-RETRY | Reconcile retries up to N times (default 5, configurable); on persistent failure, emit a `CRITICAL` log and require operator action (alert via spec #1 ops surface). |
| `(Cleaned / row absent)` | leaf absent | TERMINAL | `Cleaned` is not a persisted row — row removal IS the terminal (§4.3). Listed here for completeness; reconcile observes only via row absence. |

### 5B.2 Startup recovery procedure *(normative)*

On daemon startup, after acquiring the `flock` per I-8, the
daemon MUST execute the following recovery sequence BEFORE
admitting any new `create_workspace` calls:

1. **Lock-file sweep.** Verify exclusive `flock` on
   `<workspace_root>/.caduceusd.lock`. Failure ⇒ exit per I-8.
2. **Permission audit.** Verify `<workspace_root>` is `0700`
   and owned by the daemon uid. Broader modes ⇒ refuse to
   start (per §5A.2) unless explicitly overridden by config.
3. **Registry scan.** Read every row. For each row, classify per
   the table in §5B.1.
4. **Mark transient / reclaimable states.** For every row in `Creating`, `Active`, or `CleaningUp`, run the layered liveness probe (§3.6 step 4):
   - Alive ⇒ leave the row unchanged (note: a `CleaningUp` row whose runner is still alive is anomalous but possible if cleanup raced a re-spawn; reconcile leaves it for operator review).
   - Inconclusive ⇒ transition `Creating` / `Active` to `OrphanPending`; for `CleaningUp` rows, ALSO transition to `OrphanPending` (the prior cleanup attempt could not prove the workspace dead, so the row must enter the reconcile re-evaluation loop rather than remain in a terminally-stuck `CleaningUp` state — the §5B.2 step 7 `OrphanPending` re-evaluation path then re-probes on each subsequent reconcile pass).
   - Positively disproved ⇒ MUST enqueue `OrphanReclaim { slug, run_id }` to the reconcile queue; row stays in its current state (`Creating` / `Active` / `CleaningUp`) until reconcile drains the queue at step 7. (Implementation note: a transient in-memory queue is sufficient; persistence is not required because startup re-enumerates from registry rows.)

   **`CleaningUp` startup recovery (B22; normative).** A daemon
   crash mid-cleanup leaves the row persisted as `CleaningUp`. The
   registry's `last_known_pid` / `last_known_process_group_id`
   describe the runner that last held the workspace, NOT the crashed
   daemon. Because cleanup begins only after that runner has
   completed, been cancelled, or been orphan-reclaimed, the layered
   probe will normally observe a stale heartbeat and a dead or
   different-pgrp pid, yielding a positively disproved verdict and
   enqueueing `OrphanReclaim`; step 7's drain then re-runs §3.6 from
   step 2 (skipping ONLY step 4) to complete the cleanup. If the probe is
   not positively disproved, the row follows the same `CleaningUp`
   verdict routing defined above (Inconclusive ⇒ transition to
   `OrphanPending` per the bullet above; Alive ⇒ leave for operator
   review per the Alive bullet above). Without inclusion of
   `CleaningUp` rows in this scan, they would be unreachable by any
   enqueue source and would persist indefinitely.

   This is the ONLY destructive-state transition the startup path makes; it
   is not destructive (no `unlinkat`) but it does mutate the
   registry. Auto-`cleanup_workspace` calls at startup are
   FORBIDDEN — that is reconcile's job.

   **No-pid / no-heartbeat undecidability rule (normative).** For a row in
   `Creating`, `Active`, or `OrphanPending` with `last_known_pid == None` AND
   `last_heartbeat_at == None` (the row was inserted at §3.5 step 1.4 but
   the runner never reported), age greater than `heartbeat_stale_threshold`
   counts as positive liveness disproof for reclaim purposes. (For `Active`
   rows specifically, age is measured since row creation if no heartbeat
   ever recorded.) Without this rule such rows would be permanently
   undecidable, since the layered liveness probe (§3.6 step 4) requires
   either a heartbeat or a pid to produce a "positively disproved" verdict.
5. **Leaf scan (orphan-leaf detection).** Walk **all
   first-level slug directories** under `<workspace_root>/`
   (excluding reserved daemon files such as `.caduceusd.lock`
   and any `.caduceus*` reserved-prefix entries). For each
   `<workspace_root>/<slug>/<run_id>/` leaf discovered:
   - If `(slug, run_id)` matches a registry row, do nothing —
     it will be classified by §5B.2 step 4.
   - If no registry row matches `(slug, run_id)`, enqueue an
     `OrphanReclaim { slug, run_id }` task on the reconcile
     queue (§4.5).
   - Reserved-prefix or non-directory entries are logged and
     skipped.

   **Synthetic-`Workspace` construction for orphan-leaf reclaim
   (normative).** When the reconcile loop drains an `OrphanReclaim
   { slug, run_id }` enqueued by this step 5 (no matching registry
   row), it MUST synthesize a `Workspace` for the `cleanup_workspace`
   call with:

   - `workspace_id` derived per Invariant I-6 from `(slug, run_id)`.
   - `path = build_workspace_path(workspace_root, slug, run_id)`.
   - `repo_coordinate.slug = slug`; `remote_url = None`;
     `default_branch = None`.
   - `parent_dev_ino = None`. The §3.6 step 5 TOFU rule MUST NOT
     apply on this path: a no-row leaf has no historical capture
     to defer to. Instead, the cleaner MUST capture the slug
     parent's `(st_dev, st_ino)` once at fd-acquisition, and any
     subsequent re-`fstatat` within the same cleanup must match
     byte-for-byte (fail-closed if not).
   - `created_at` and `branch_at_create`: synthesized as `now()`
     / `None` (advisory only; not consulted by §3.6).

   This synthetic record is consumed only by the OrphanReclaim
   path and MUST NOT be persisted to the registry.

   **Cleanup context (normative).** `cleanup_workspace` accepts an
   optional `registry_row: Option<&Row>` discriminator alongside
   the synthetic-or-real `Workspace`:

   - `registry_row = Some(row)` → row-backed cleanup; §3.6 step 5
     may use `row.parent_dev_ino` (legacy-null TOFU writeback
     applies if `None`, per the §3.6 step 5 TOFU rule).
   - `registry_row = None` → synthetic no-row leaf cleanup (this
     §5B.2 step 5 path); §3.6 step 5 MUST NOT TOFU-writeback;
     instead, the cleaner captures the slug parent's
     `(st_dev, st_ino)` once at fd-acquisition and fail-closes on
     subsequent re-`fstatat` mismatch.

   The discriminator MUST be passed by the caller; it MUST NOT be
   inferred from `Workspace.parent_dev_ino`'s nullity, because
   legacy rows MAY have `parent_dev_ino = None` while still being
   row-backed (legacy TOFU path per §3.6 step 5 / Appendix D
   ITER3-FOLLOWUP-4). This discriminator is what closes the type
   gap between the synthetic no-row leaf path and the row-backed
   path; both reach `cleanup_workspace`, but only the latter has a
   row to write back to.

   **Reserved-name exclusions (normative; Z6-H1).** The scanner
   MUST skip reserved children whose names match any of:

   - `.bare`
   - `.cache`
   - `.git`
   - any name beginning with `.caduceus` (caduceus-prefix
     reserved)
   - any name beginning with `.` (dot-prefixed reserved
     entries)

   Non-directory entries are logged
   (`leaf_scan_skipped_non_dir`) and skipped. Only directories
   whose names equal `sanitize_run_id(name)` per §3.2 are
   candidates for `OrphanReclaim`. All other entries are
   skipped silently (reserved) or with a log line
   (non-directory), and MUST NOT be unlinked.

   This scan MUST NOT be limited to the set of registered
   slugs; a crash mid-rollback (after `mkdirat` succeeded but
   before the registry row was committed) leaves an
   unregistered leaf that ONLY this scan can reclaim.
6. **Admit traffic.** Only after steps 1–5 complete may the
   daemon accept new `create_workspace` requests.
7. **Reconcile loop (canonical OrphanReclaim-queue bypass scope; normative).**
   A background reconcile task (cadence:
   `2 × heartbeat_interval`, default 10s) operates by **draining the
   `OrphanReclaim` in-memory queue end-to-end** — it does NOT scan the
   registry by `Status` column for reclamation work. Each queued
   `OrphanReclaim { slug, run_id }` is dispatched to
   `cleanup_workspace(_, OrphanReclaim, registry_row)` on the first reconcile pass
   (`registry_row = Some(&row)` for queue entries originating from §5B.2 step 4,
   from §3.5 step 5b's carried-forward `CleanupFailed` / `OrphanPending` path,
   or from this step's re-evaluation of `OrphanPending`; `registry_row = None`
   for queue entries originating from §5B.2 step 5's no-row leaf scan OR from
   §3.5 step 5b's no-matching-row `EEXIST` path).

   **Drain procedure (B22 — normative).** For each queued `OrphanReclaim { slug, run_id }`, the reconcile worker MUST reacquire locks in canonical order `registry-wide → per-slug → per-workspace` before re-entering §3.6. If `registry_row.is_some()` and the row's current `Status` is not already `CleaningUp`, the worker MUST, while holding the registry-wide mutex, transition that row to `Status::CleaningUp`; rows already in `CleaningUp` skip this mutation, and `registry_row.is_none()` entries perform no mutation. The worker then releases the registry-wide mutex and continues through §3.6 beginning at step 2 while holding the per-slug guard and per-workspace lock. Within the §3.6 cleanup body, the ONLY skipped step is step 4 (the layered liveness probe); step 2's path re-validation MUST run unchanged. The queue entry always carries `(slug, run_id)`; it carries `registry_row = Some(&row)` only for row-backed entries and `registry_row = None` for synthetic no-row leaf-scan entries.

   Net executed steps: **2, 3, 5, 5a, 6, 7, 8, 9**.

   **Canonical bypass scope (normative; single source of truth).**
   Uniformly, for every `OrphanReclaim { slug, run_id }` queue entry —
   regardless of which of the five enqueue sources produced it — the
   §3.6 re-entry skips ONLY step 4 (the layered liveness probe) and
   retains steps 2, 3, 5, 5a, 6, 7, 8, 9 unchanged. The five enqueue sources
   (and the rationale for skipping step 4 in each) are:

   1. **§3.5 step 5b — inline-reclaim deferral.** The synchronous create
      path observed `EEXIST` with a leaf whose registry row is absent or
      whose `Status ∈ {CleanupFailed, OrphanPending}`. The `OrphanPending`
      sub-case carries a fresh layered liveness verdict (positively
      disproved) captured by the §3.5 step 5b decision table at the
      moment of enqueue; `CleanupFailed` is a deterministic per-row
      failure with no live runner; the no-row case has no runner to
      probe. Re-running step 4 at queue drain would either be redundant
      (still positively disproved) or, on a uid-mismatched probe, risk a
      false `Inconclusive` re-classification that would route the row
      back to `OrphanPending` and stall the reclaim.
   2. **§5B.2 step 4 — transient-state re-classification.** `Creating` /
      `Active` rows whose layered probe at startup returned positively
      disproved (incl. the no-pid/no-heartbeat age rule). (`CleaningUp`
      rows are covered separately as source 5 below, with a distinct
      B22 rationale.)
   3. **§5B.2 step 5 — orphan-leaf scan.** Leaf with no matching
      registry row; no runner record exists to probe.
   4. **§5B.2 step 7 (this step) — `OrphanPending` re-evaluation.**
      A subsequent reconcile pass re-runs §3.6 step 4 layered liveness
      against the `OrphanPending` row; when liveness becomes positively
      disproved, the row is enqueued onto `OrphanReclaim` rather than
      calling `cleanup_workspace` directly, preserving the queue as the
      single canonical reclaim path. Re-running step 4 at drain would be
      redundant.
   5. **§5B.2 step 4 — `CleaningUp` startup recovery (B22).** A daemon
      crash mid-cleanup leaves a row in `Status::CleaningUp`. On daemon
      restart, the §5B.2 step 4 startup scan probes liveness against
      the row's persisted `last_known_pid` /
      `last_known_process_group_id` (which describe the *runner* that
      owned the workspace, not the crashed daemon). Cleanup begins
      only after that runner has completed, been cancelled, or been
      orphan-reclaimed, so the probe normally returns positively
      disproved and the row is enqueued onto `OrphanReclaim`.
      Re-running step 4 at drain would be redundant. Without this
      source, `CleaningUp` rows would be unrecoverable on restart.

   Equivalently: the layered liveness probe of §3.6 step 4 is invoked
   only for first-encounter rows; rows already on the `OrphanReclaim`
   queue have a positive-disproof verdict (or no runner) attached at
   enqueue time and skip the probe but retain every other safety check
   (parent revalidation, ownership reclaim, path re-validation,
   recursive remove, hooks, lock release).

   Independently, the same reconcile pass re-evaluates rows in
   `OrphanPending` and `CleanupFailed` per §5B.1 (re-running §3.6
   step 4 layered liveness for `OrphanPending`; bounded retry for
   `CleanupFailed`); when an `OrphanPending` row's liveness becomes
   positively disproved, that re-evaluation enqueues the row onto
   `OrphanReclaim` rather than directly calling `cleanup_workspace`,
   so the queue remains the single canonical reclaim path. Conversely,
   when the re-probe of an `OrphanPending` row returns `Alive` (heartbeat
   fresh / pid+pgrp match), the reconcile pass transitions the row back
   to `Active` — `OrphanPending` is not sticky, and lifecycle recovery
   is bidirectional with respect to liveness verdicts.

### 5B.3 State transition diagram

```
                ┌──────────────┐
                │   (no row)   │
                └──────┬───────┘
                       │ §3.5 step 1.4
                       ▼
                ┌──────────────┐    §3.5 step 10 rollback / hook fail
                │   Creating   ├──────────────────────┐
                └──────┬───────┘                      │
                       │ §3.5 step 9                  │
                       ▼                              │
                ┌──────────────┐                      │
   ┌────────────┤    Active    │                      │
   │            └──────┬───────┘                      │
   │ liveness          │ §3.6 step 1
   │ inconclusive      │ (operator / explicit
   │ (reconcile)       │ cleanup only)
   │                   ▼                              │
   │            ┌──────────────┐                      │
   │            │  CleaningUp  ├──────────┐           │
   │            └──┬─────────┬─┘          │           │
   │               │         │            │           │
   │   §3.6 step 9 │         │ reconcile  │ §3.6      │
   │               │         │ liveness   │ step 7    │
   │               ▼         │ inconcl.   │ partial   │
   │       ┌──────────────┐  │            │     ┌──────────────┐
   │       │ (row removed)│  │            │     │ (row removed)│
   │       │   terminal   │  │            │     │  §3.5 step   │
   │       │              │  │            ▼     │ 10c reached  │
   │       └──────────────┘  │                  └──────────────┘
   │                         │
   │                         │                 ┌──────────────┐
   │                         │                 │ §3.5 10a-pre │
   │                         │                 │ /10a/10e OR  │
   │                         │                 │ §3.6 step 7  │
   │                         │                 │ partial      │
   │                         │                 └──────┬───────┘
   │                         │                        ▼
   │                         │                 ┌──────────────┐
   │                         │                 │CleanupFailed │
   │                         │                 └──────┬───────┘
   │                         │                │ reconcile retry
   │                         │                ▼
   │                         │        (back to CleaningUp)
   │                         ▼                │ liveness disproved
   │                      ┌──────────────┐    │ → enqueue OrphanReclaim;
   └─────────────────────►│OrphanPending ├────┘   transitions to CleaningUp
                          └──────────────┘        only on §5B.2 step 7 drain

   Additional reconcile-driven edges (normative per §5B.1 / §5B.2 / §4.3):
   - `Creating ──crash-recovery / liveness inconclusive──► OrphanPending`.
     Fires when the registry holds a `Creating` placeholder but layered
     liveness (§3.6 step 4) is inconclusive at startup or on reconcile.
   - `OrphanPending ──§3.6 step 4 / liveness alive──► Active`.
     Fires when a re-probe of an `OrphanPending` row observes `Alive`
     (heartbeat fresh / pid+pgrp match); `OrphanPending` is not sticky.
   - `Creating`, `Active`, or `OrphanPending` ──liveness positively
     disproved──► enqueue `OrphanReclaim`; the row remains in its current
     state until §5B.2 step 7 drains the queue, transitions the row to
     `CleaningUp`, and re-enters §3.6 from step 2, skipping ONLY step 4.
     (`CleaningUp` rows follow the same enqueue path via B22 startup
     recovery but are already `CleaningUp`, so B23 skips the transition.)
```

The diagram is informative; the table in §5B.1 + §4.3 is
normative.

### 5B.4 Operator intervention

The following conditions REQUIRE operator action; reconcile MUST
NOT auto-resolve them:

- A row stuck in `CleanupFailed` after N reconcile retries
  (default N=5).
- A `SymlinkEscape` detection on any path: the offending symlink
  MUST be removed by the operator; the daemon MUST NOT
  auto-remove. (See §3.5 step 5a, §3.4 step 4.)
- A `ParentRevalidationFailed` on a parent inode that has
  legitimately changed (e.g. operator rebuilt the slug parent):
  the operator MUST drop and re-create the affected registry
  rows.
- `WorkspaceRootMode` at startup (mode broader than `0700` and
  no explicit override): operator MUST `chmod` and restart.

Operator-required conditions MUST be surfaced via:
1. A `CRITICAL`-severity log line.
2. A registry row in a clearly-stuck state (`CleanupFailed` past
   retry budget; `OrphanPending` past a configurable
   `orphan_pending_max_age` of default `24h`).
3. The orchestrator's ops API (spec #1) for runbook visibility.

---

## 6. Test contract

Implementations MUST pass tests covering at minimum the following behaviours. These tests SHOULD be black-box (driving the daemon API or the published library interface), not white-box, so future internal refactors do not break the suite.

### T-1 — `..` injection in run_id

**Setup:** Construct a candidate `run_id = "../etc/passwd"`.
**Action:** Call `create_workspace(coord, run_id, workflow)`.
**Expect:** `Error::InvalidRunId` from §3.2 rule 5; **no filesystem writes**; registry unchanged.

### T-2 — Symlink trap at workspace_root

**Setup:** `workspace_root = /tmp/wsroot`. Pre-create `/tmp/wsroot/owner_repo` as a symlink to `/`.
**Action:** Call `create_workspace` with `repo_coordinate.slug = "owner_repo"`, any valid run_id.
**Expect:** `Error::SymlinkEscape` from §3.4 step 4; the leaf is NOT created; the symlink is NOT followed; the symlink is NOT auto-removed (operator must intervene).

### T-2b — TOCTOU symlink swap between validate and mkdir

**Setup:** Daemon configured with `workspace_root = <root>`. No pre-existing
slug directory. A test harness is wired to interpose between §3.5 step 3
(`validate_workspace_path` returns Ok) and §3.5 step 5a
(`mkdirat(workspace_root_fd, slug, …)`); during that interposition window the
attacker creates `<root>/<slug>` as a symlink to `/`.
**Action:** Call `create_workspace(coord, run_id, workflow)`.
**Expect:** §3.5 step 5a observes `EEXIST` and the follow-up
`fstatat(..., AT_SYMLINK_NOFOLLOW)` reports `S_ISLNK`; `Error::SymlinkEscape`
is returned; **no `mkdir` succeeds against the symlink target**; the placeholder
row from step 1.4 is removed before the per-slug guard is released; the
attacker-installed symlink is NOT auto-removed (operator must intervene). This
test asserts the C4 fix: `mkdir -p` against a string path is FORBIDDEN; the
fd-anchored `mkdirat` + `fstatat(AT_SYMLINK_NOFOLLOW)` sequence is normative.

### T-3 — Cleanup with tampered registry path

**Setup:** A Workspace exists. After creation, an attacker (or buggy code) overwrites the registry's `path` field to `/etc`.
**Action:** Call `cleanup_workspace(workspace, RunCompleted, Some(&row))`.
**Expect:** `Error::ValidationFailed` from §3.6 step 2; **/etc untouched**; the registry row is left in `Active` (not transitioned to `CleaningUp`, and the row is not removed); a `CRITICAL` log is emitted (Invariant I-2 canary).

### T-4 — Two runs, same repo, isolated worktrees (strategy (c) only)

**Setup:** Daemon configured with strategy (c). Repo R has a `.bare/` checkout. Run A is mid-flight with a working tree at `<root>/R/A/`.
**Action:** Start Run B on the same R via `create_workspace(R_coord, B_run_id, workflow)` whose `after_create` performs `git worktree add` against `<root>/R/.bare/`.
**Expect:** Both succeed; B's working tree at `<root>/R/B/` is independent of A's; concurrent reads parallelize; concurrent writes serialize at git's `.git/index.lock` level.

(For strategy (a), the equivalent test T-4a expects Run B to receive `Error::SharedRepoLocked` until Run A completes cleanup.)

### T-5 — Slug collision

**Setup:** Register `https://github.com/acme/app` first; observe `slug = github_com_acme_app` (host-prefixed per §3.1, N-5). Then register `https://gitlab.com/acme/app`.
**Action:** Daemon detects collision at the second registry-write.
**Expect:** Per §3.1 collision policy, the second slug is rewritten by replacing the trailing 8 bytes of `slug_body` with `_<repo_hash7>` where `repo_hash7` derives from the full normalized remote (host *and* path) — e.g. `acme_app` → `_<repo_hash7>` substituted per the §3.1 algorithm, never simply appended. Result is ≤ 64 bytes (does not overflow the canonical regex). Registry persists the rewrite; subsequent `create_workspace` calls for that repo use the new slug. The first slug is unchanged (Invariant I-4 stickiness applies even across collision events). A same-host collision (e.g. `acme/foo-bar` vs `acme/foo_bar`) MUST also be disambiguated by this same algorithm; a host-only suffix is FORBIDDEN.

### T-6 — Hook failure mid-create cleans up

**Setup:** A workflow whose `after_create` is `false` (always exits 1).
**Action:** `create_workspace(coord, run_id, workflow)`.
**Expect:** Returns `Error::HookFailed("after_create", 1)`. The leaf `<workspace_root>/<slug>/<run_id>/` is **removed iff rollback reaches step 10c** (Invariant I-7); if rollback aborts at `CleanupOwnershipFailed` (reclaim failure), `ParentRevalidationFailed` (revalidation failure), or mid-walk unlinkat failure (`CleanupIncomplete (rollback-side)`), the leaf is **retained** for reconcile. The registry row is **removed iff rollback reached step 10c**; on any of the three abort conditions above, the row is **retained as `Status::CleanupFailed`** for reconcile per §3.5 step 1b. A subsequent `create_workspace` with the same `(slug, run_id)` succeeds (assuming the hook is fixed and any retained `CleanupFailed` row has been reconciled) — i.e. the prior failure does not poison the namespace.

### T-7 — Two daemons against same workspace_root

**Setup:** Daemon D1 is running on `workspace_root = /var/caduceus`.
**Action:** Start daemon D2 with the same `--workspace-root`.
**Expect:** D2 fails to acquire the `<workspace_root>/.caduceusd.lock` advisory exclusive lock; D2 exits with `Error::WorkspaceRootLocked` and a log line citing D1's PID. D1 is unaffected. (Invariant I-8.)

### Recommended additional tests (non-normative)

- T-8: `before_cleanup` exits non-zero ⇒ cleanup proceeds (warning logged); registry row is deleted on success (terminal; not `CleanupFailed`).
- T-9: `after_cleanup` failure on partial cleanup ⇒ `Status::CleanupFailed`; reconcile retries on next pass; second attempt succeeds and the registry row is deleted (terminal).
- T-10: `run_id` containing 1 KiB of newlines ⇒ rejected by §3.2 rule 1 (length cap before sanitization).
- T-11: Fuzz: random byte sequences as `remote_url` ⇒ either parse-rejected or yield a valid `RepoSlug`; never a slug containing `/`, `..`, or non-ASCII.
- T-12: Hook timeout on `after_create` ⇒ subprocess killed (SIGTERM then SIGKILL after grace); leaf removed iff rollback reaches step 10c (Invariant I-7), else retained for reconcile; error is `Error::HookTimeout("after_create")`. Registry row is removed iff rollback reaches step 10c; row retained as `CleanupFailed` if rollback aborts at `CleanupOwnershipFailed`, `ParentRevalidationFailed`, or mid-walk unlinkat failure (`CleanupIncomplete (rollback-side)`) (§3.5 step 1b).
- T-13: Hook produces 10 MiB of stdout ⇒ output truncated to capture cap (default 1 MiB) with a marker line; hook NOT terminated for verbosity; exit code respected.
- T-14: Daemon SIGINT mid-`after_create` ⇒ in-flight hook subprocess receives SIGTERM; if it exits within the daemon shutdown grace, the leaf is cleaned up; otherwise the orphan leaf is observable on next daemon start and reconcile reclaims it (§7 item 4).
- T-15: Slug stickiness — register a repo, then change its `remote_url` (owner rename); verify the registry's `slug` field is unchanged and the on-disk `<workspace_root>/<slug>/` directory is unaffected (Invariant I-4).
- T-16: Concurrent workspace creation conflict — expected error depends on conflicting row's `Status` AND, for `OrphanPending`, the inline re-probe verdict. Two concurrent `create_workspace` calls with the same `(slug, run_id)` ⇒ exactly one succeeds; the loser's error is determined by the conflicting row's `Status` per §3.5 step 1.2/1.3a/5b's status-sensitive routing. Test MUST cover all sub-cases as separate sub-tests (T-16a/T-16b/T-16c/T-16d); no partial leaves left behind in any sub-test.
  - **T-16a (no row, EEXIST due to inode race; reclaim path):** the loser observes `mkdirat` `EEXIST` at §3.5 step 5b with no matching registry row in the pre-step-1.4 snapshot (Cleaned-or-absent). Per §3.5 step 5b's "no-row + EEXIST" sub-case, `OrphanReclaim { slug, run_id }` is enqueued and the reclaim is deferred to the reconciler. Expected: `Error::WorkspaceBusyOrReclaiming`. (Note: `Error::WorkspaceAlreadyExists` is NOT returned for this case — it is reserved for the §3.5 step 5b decision-table DUPLICATE row, where the row exists AND the runner is alive; see §4.4.)
  - **T-16b (row with `Status ∈ {Creating, Active, CleaningUp}`):** the loser is routed at §3.5 step 1.2 (live-owner row) or fails the §3.5 step 1.3a per-workspace try-lock against the winner's hold. Expected: `Error::WorkspaceBusy`.
  - **T-16c (row with `Status == CleanupFailed`):** the loser is routed through §3.5 step 5b's reclaim path (the row is carried forward per the step 1.4 precondition; `OrphanReclaim { slug, run_id }` is enqueued for the reconciler). Expected: `Error::WorkspaceBusyOrReclaiming`.
  - **T-16d (row with `Status == OrphanPending`; verdict-sensitive):** the loser is routed through §3.5 step 5b's reclaim path, which performs the B22 pre-enqueue inline re-probe of §3.6 step 4. Expected error depends on the probe verdict:
    - probe returns `Live` ⇒ `Error::WorkspaceBusy` (no enqueue; runner has resumed reporting).
    - probe returns `Dead` (positively disproved) ⇒ `Error::WorkspaceBusyOrReclaiming` (enqueue proceeds).
    - probe returns `Inconclusive` ⇒ `Error::WorkspaceBusy` (Z-10 fail-closed; no enqueue; next periodic reconcile re-evaluates).
- T-17: Engine-side reconcile observes a leaf on disk with no registry row ⇒ `cleanup_workspace(_, OrphanReclaim, None)` is invoked and succeeds (§7 item 4 contract). Conversely, a registry row whose `path` is missing on disk is removed without invoking destructive cleanup.

---

## 7. Out of scope

The following are intentionally not specified here. Where there is a forward reference, the consumer is named.

1. **Cross-host workspaces.** Symphony's Appendix A.1–A.3 introduces `worker.ssh_hosts` for distributing workers across machines (each remote host needs the repo bootstrap independently). caduceus v1 is single-host: the `caduceusd` daemon runs on the same machine as every workspace it owns. v2 may revisit; until then, "the workspace_root is on the daemon's local filesystem" is a hard precondition.

2. **Multi-repo work unit.** A single Run touching N repos is **not supported in v1**. This is the disposition of `symphony-multirepo-ux.md` Q1 (lines 816–832). The recommended pattern for spanning a refactor across N repos is the Symphony pattern: N Runs, dependency-linked at the tracker layer. If revisited, the v2 unit becomes "Mission" (caduceus-side concept owning N runs); each Run still maps to one repo, so this spec stays applicable per-Run.

3. **Git operations themselves.** Branch selection, fetch strategies, push targets, conflict resolution, large-file handling, submodule semantics — all owned by the workflow YAML's `hooks.after_create` and the user's git tooling. This spec only guarantees that the hook runs in a sane cwd with a sane environment. See spec #6 (workflow).

4. **Workspace persistence across daemon restart.** The registry's persistence (sqlite? JSON file? something else) is owned by spec #1 (orchestrator), specifically its reconcile invariant I-6. **Edge case called out here:** a workspace MAY exist on disk but have no row in the registry (e.g. previous daemon process crashed mid-§3.5 between step 5 and step 9). The reconcile pass MUST treat such an orphan leaf as a candidate for `cleanup_workspace(_, OrphanReclaim, None)` (synthetic no-row leaf reclaim). Conversely, a registry row whose `path` does not exist on disk MUST be treated as a stale entry and removed. The orchestrator owns the policy; this spec only states the leaf scanning is its responsibility, not the engine's or the runner's.

5. **Disk-usage accounting.** Symphony does not surface workspace size (`workspace.ex` does no disk accounting per `symphony-multirepo-ux.md:327`). caduceus v1 inherits this posture; size-budgeting and quota are out of scope.

6. **Workspace renaming.** Renaming a `repo_slug` post-registration is operator territory and requires manual `cleanup_workspace + re-create`. Not automated.

---

## 8. Open questions

These are explicitly unresolved in this spec. Each MUST be settled before the corresponding behaviour is shipped; the conservative defaults stated here apply until then.

### OQ-1. Filesystem boundary crossing

Should `validate_workspace_path` reject if `path` and `workspace_root` reside on different filesystems (different `st_dev`)? Symphony does not check (`workspace.ex:358–384` is `st_dev`-agnostic). The argument for checking is defence-in-depth against a maliciously bind-mounted subdirectory; the argument against is operator convenience (e.g. a tmpfs `<workspace_root>/<repo_slug>/` for a hot repo). **Default: do not enforce.** §3.4 step 6 leaves it OPTIONAL.

### OQ-2. Worktree vs full-clone for shared-repo case

§3.7 strategy (c) (worktree) saves disk and clone latency at the cost of operational coupling between concurrent runs (one corrupting `.git` poisons all). Strategy (a) (serialize) is simpler but blocks concurrency. **Default: strategy (a) for v1.** Move to (c) when Q1 is revisited.

### OQ-3. Repo discovery

Does caduceus accept ad-hoc `remote_url` values from the workflow at Run dispatch time, or MUST repos be pre-registered with the daemon (`caduceusd repo add`)? Pre-registration supports operator policy (allow-list, default-branch override) at the cost of friction. Ad-hoc supports drive-by use at the cost of less control. **Default for this spec: ad-hoc allowed; daemon auto-registers on first sight, but MAY be configured to require pre-registration.** The choice does not affect the workspace algorithms themselves.

### OQ-4. Branch isolation per Run

Should each Run get its own branch by convention (e.g. `caduceus/run-<run_id_short>`), or is branch policy entirely the workflow's call? Auto-branching simplifies write isolation under strategy (c), but conflicts with workflows that already do their own branching. **Default: workflow's call.** This spec does not prescribe branch policy.

### OQ-5. Concurrent runs on the same repo at all in v1

Even with strategy (a), should v1 admit "second Run queued, waits for first Run to release the lock", or refuse-fast with `SharedRepoLocked`? Queueing is friendlier; refusing-fast is simpler and surfaces contention to the orchestrator (which can choose to re-dispatch). **Default: refuse-fast.** The orchestrator (spec #1) MAY layer a queue on top.

---

## 9. Cross-references

This spec is consumed by, and consumes, the following caduceus specs.

| Spec | Direction | Touchpoint |
|---|---|---|
| **#1 Orchestrator** | bidirectional | Calls `create_workspace` on dispatch and `cleanup_workspace` on Run exit. Owns the registry persistence and reconcile loop (§7 item 4). The C-hybrid daemon-owns-registry decision originates in spec #1 and is reflected here in Invariant I-8 and the §4.2 daemon-API note. |
| **#2 Runner** | reads | The agent process's cwd is `Workspace.path`. The runner MUST re-validate via §3.4 before binding cwd (defence-in-depth: TOCTOU window between create and dispatch). The runner is the second line of defence for Invariant I-3. |
| **#4 Snapshot** | reads | `Workspace.path`, `Workspace.repo_coordinate`, and `Workspace.created_at` appear in the per-row snapshot. The runs panel (spec #8) renders these. |
| **#6 Workflow** | feeds | Hook bodies (`before_create`, `after_create`, `before_cleanup`, `after_cleanup`) are defined in the workflow YAML; this spec defines the contract under which they execute (env vars, cwd, failure semantics). |
| **#7 Run identity** | consumes | `RunId` is the keying primitive supplied by spec #7. `WorkspaceId` is a derived field on the Run entity per Invariant I-6 — recoverable from `(repo_coordinate, run_id)` without a registry round-trip. |
| **#8 Runs panel** | reads | The panel renders one row per Run, grouped/filterable by `repo_coordinate.slug`. Columns include `Workspace.path` (as a clickable "open in editor" affordance) and `branch_at_create`. |

---

## Appendix A — Symphony port log (for auditors)

The following table maps each ported algorithm to its Symphony source and notes any behavioural changes.

| caduceus § | Symphony source | Change |
|---|---|---|
| §3.2 sanitize_run_id | `workspace.ex:206–208` | Input domain changed (RunId vs issue_identifier); behaviour identical. |
| §3.3 build_workspace_path | `workspace.ex:196–204 workspace_path_for_issue/2` | Path schema extended: `<root>/<slug>/<run_id>/` vs Symphony's `<root>/<issue_id>/`. Trailing slash made normative. |
| §3.4 validate_workspace_path | `workspace.ex:358–384` | Behaviour ported verbatim (canonicalize, prefix-check, symlink-escape sweep, post-canon `..` re-check). FS boundary check (step 6) added as OPTIONAL — Symphony does not check. TOCTOU note added. |
| §3.5 create_workspace | `workspace.ex create_for_issue/2` | Hooks order preserved (`before_create` then `after_create`). Failure rollback (Invariant I-7) made explicit. Hook env vars expanded for caduceus's repo coordinate (caduceus-new). |
| §3.6 cleanup_workspace | `workspace.ex Workspace.remove/2` (`:87–128`) | Liveness check (step 3) made explicit. Re-validation (step 5) made explicit. `before_cleanup` failures non-fatal (Symphony posture preserved). |
| §3.7 acquire_shared_repo_lock | n/a | Caduceus-new. Symphony has no analog because Symphony is single-repo. |
| §5 Invariants I-1 to I-3 | SPEC §9.5 lines 886–905 | Ported verbatim in observable behaviour. Citations preserved. |
| §5 Invariants I-4 to I-9 | n/a | Caduceus-new. |

Apache-2.0 attribution for the Symphony portions is preserved at the top of this document and MUST be preserved in any implementation that ports the cited algorithms.

---

## Appendix B — Glossary cross-walk to Symphony

| caduceus term | Symphony analog | Notes |
|---|---|---|
| Workspace | Workspace (SPEC §4.1.4) | Same role; different keying. |
| WorkspaceId | n/a | Symphony keys workspace by issue identifier; caduceus introduces an explicit id for cross-cutting reference (snapshot, panel, logs). |
| RunId | Run Attempt id (SPEC §4.1.5) | Symphony's run-attempt is not a first-class UI row (`symphony-multirepo-ux.md` A.1 lines 34–38); caduceus elevates it. |
| RepoSlug | n/a | No Symphony analog; repo identity is a hook concern in Symphony (`WORKFLOW.md:14–22`). |
| RepoCoordinate | n/a | Caduceus-new. |
| WorkspaceRoot | `workspace.root` (WORKFLOW.md, SPEC §4.2 lines 281–283) | Same role. |
| Hook | Hook (SPEC §9.2) | Four-point ordering (`before_create`/`after_create`/`before_cleanup`/`after_cleanup`) preserved. |
| BareCheckout | n/a | Caduceus-new (strategy (c) only). |

---

## Appendix C — Worked example: dispatch through cleanup

The following narrative exercises the spec end-to-end against a small concrete configuration. It is informative.

**Configuration:**

- `workspace_root = /var/lib/caduceus/wsroot` (canonicalized at daemon startup).
- Repo registered: `https://github.com/acme/widgets` ⇒ `slug = github_com_acme_widgets`.
- Workflow: `before_create` is unset; `after_create` is `git clone --depth 1 "$CADUCEUS_REPO_REMOTE_URL" .`; `before_cleanup` posts a comment to a tracker; `after_cleanup` is unset.

**Run dispatch:**

1. Orchestrator chooses `run_id = 01HZX0...J3K`.
2. Orchestrator calls `caduceusd.create_workspace({slug: "github_com_acme_widgets", remote_url: "...", default_branch: "main"}, "01HZX0...J3K", workflow)`.
3. Daemon: `sanitize_run_id("01HZX0...J3K") = "01HZX0...J3K"` (already safe).
4. Daemon: `target = /var/lib/caduceus/wsroot/github_com_acme_widgets/01HZX0...J3K/`.
5. `validate_workspace_path(target, root)` ⇒ Ok (leaf does not yet exist; ancestors validated).
6. Strategy (a): no other Run exists for this slug; lock acquired.
7. `mkdirat(workspace_root_fd, "github_com_acme_widgets", 0o755)` (parent missing); `mkdirat(slug_fd, "01HZX0...J3K", 0o755)` (single component, fd-anchored — see §3.5 step 5).
8. Hook env composed; `before_create` skipped; `after_create` runs `git clone` with cwd inherited via `fchdir(leaf_fd)` (NOT by re-resolving the string `target`). Exit 0 after 8 seconds.
9. `branch_at_create = read_current_branch_at(leaf_fd) = Some("main")`.
   Z-19: the appendix uses `read_current_branch_at(leaf_fd)` (the
   fd-anchored form) to match §3.5 step 9 (line 479). The earlier
   `read_current_branch(target)` was a bug: re-resolving the
   `target` string here would re-introduce the post-validation
   TOCTOU window §3.5 step 5 closes (gpt-N-3 fix). Worked-example
   parity with the algorithm is normative; appendices MUST track
   the algorithm's fd-anchored discipline.
10. `workspace_id = wsp_<hex(BLAKE3_128_keyed("github_com_acme_widgets" || 0x1F || "01HZX0...J3K"))>`.
11. Registry row written with status `Active`. Returned to orchestrator.
12. Orchestrator hands `Workspace` to runner. Runner calls `validate_workspace_path` (defence-in-depth per spec #2) and binds cwd. Agent process spawns.

**Run completes:**

13. Agent exits 0; runner notifies orchestrator; orchestrator calls `cleanup_workspace(ws, RunCompleted, Some(&row))`.
14. Per-workspace lock acquired; status `Active → CleaningUp`.
15. `validate_workspace_path` ⇒ Ok.
16. Liveness check: agent process is gone; proceed.
17. `before_cleanup` posts the tracker comment. Exit 0.
18. `validate_workspace_path` re-checked ⇒ Ok.
19. Recursive remove of the leaf via the fd-anchored walk specified
    in §3.6 step 7 (`openat`/`unlinkat`/`fdopendir` on `slug_fd`
    and `leaf_fd`; no string-path resolution). No symlinks
    encountered; no errors.
20. `after_cleanup` skipped.
21. Registry row deleted (terminal: row removal IS the terminal state per §4.3; no `Cleaned` status persisted); per-workspace lock released; shared-repo lock released.

**Failure variant — hook exits non-zero at step 8:**

- `Error::HookFailed("after_create", 1)` raised.
- §3.5 step 10 performs **inline rollback** (no recursive `cleanup_workspace` call): if `chowned_to_runner == true`, first reclaim ownership via `fchownat(slug_fd, sanitize_run_id(run_id), daemon_uid, daemon_gid, AT_SYMLINK_NOFOLLOW)`, then run the fd-anchored unlink walk and `unlinkat(..., AT_REMOVEDIR)`. On reclaim failure, parent revalidation failure, or mid-walk unlinkat failure, transition to `Status::CleanupFailed` (`Error::CleanupOwnershipFailed` / `Error::ParentRevalidationFailed` / `Error::CleanupIncomplete`) and defer completion to reconcile. On those abort paths the leaf is also retained for reconcile.
- Locks released; placeholder-row handling is conditional: removed if rollback reaches step 10c; if rollback aborts at `CleanupOwnershipFailed`, `ParentRevalidationFailed`, or mid-walk unlinkat failure, the row transitions to `Status::CleanupFailed` and is retained for reconcile (§4.3).
- Original `Error::HookFailed` returned to orchestrator; rollback errors logged but not surfaced.

**Failure variant — symlink trap:**

- An attacker pre-creates `<workspace_root>/github_com_acme_widgets` as a symlink to `/`.
- §3.4 step 4 detects this on the first `validate_workspace_path` call in §3.5 step 3.
- `Error::SymlinkEscape` returned. No `mkdir`. No hook execution. Operator MUST remove the symlink before any future Run on this slug.

---

## Appendix D — ITER3 follow-ups

The following items were identified during Iteration 3 Phase B2 review and
are deferred to a subsequent revision. They do NOT block the current draft.

- **ITER3-FOLLOWUP-1 (heartbeat content schema).** The heartbeat file body
  is currently described as `{pid, runner_uuid, mtime}` JSON. The exact
  schema (field names, encoding of `mtime`, version field, signature)
  belongs in spec #2 (runner) and should be cross-linked once defined.
- **ITER3-FOLLOWUP-2 (heartbeat-interval governance).** Whether
  `heartbeat_interval` is a per-workflow override or a daemon-global is
  left open here; spec #1 should pick one when it specifies the workflow
  → daemon configuration plumbing.
- **ITER3-FOLLOWUP-3 (cross-platform `*at` shim).** Step 5 (create) and
  step 4/6 (cleanup) both rely on `*at`-family syscalls. Windows requires
  an equivalent fd-anchored construction; a portability appendix or a
  `caduceus-fdops` crate boundary is implied but not specified. Tracked
  in §7 item 1 (cross-host) as a near neighbour.
- **ITER3-FOLLOWUP-4 (`parent_dev_ino` registry migration).** The
  `parent_dev_ino` registry field (§4.2) needs a migration story for
  pre-existing rows where it is absent; reconcile MUST tolerate
  `parent_dev_ino == None` and re-populate on next successful
  cleanup per the §3.6 step 5 TOFU rule
  (`parent_dev_ino_tofu_populated`).
- **ITER3-FOLLOWUP-5 (CVE-2022-21658 ratchet).** The fd-anchored remove
  in §3.6 step 7 closes the symlink-race CVE class for caduceus. A
  follow-up SHOULD add an explicit fuzz/property test (TOCTOU swap
  during walk) to T-2b's neighbourhood.

---

*End of spec-multi-repo-workspace-model.md*
