
# Strategy Selection (Step 0.5 of nontrivial-pipeline)

**This file is a template you are encouraged to edit.** The thresholds below are defaults. Tune them for your project, team, or personal risk appetite — nothing in `nontrivial-pipeline` hardcodes these numbers; they are consumed from this skill.

Use this skill to answer: *"for this change, do I run the full pipeline once (A), fan out across domains with caps (B), or decompose into a DAG of sub-tasks (C)?"*

## Inputs measured

| Signal | Meaning |
|---|---|
| **N** | # of domain tags from Step 0 (`data-pipeline`, `algorithmic-ml`, `llm`, `cloud-infra`, `security`, `application-code`, `docs-specs`, …) |
| **L** | Estimated lines of code touched |
| **S** | # of subsystems / services / packages touched |
| **Surface** | Public API / schema / IaC change? (Y/N) |
| **Risk** | Critical / High / Medium / Low |

## Decision table

| Strategy | Trigger conditions | What happens |
|---|---|---|
| **A — Direct** | N ≤ 2 **AND** L < ~200 **AND** S ≤ 1 **AND** no surface change **AND** risk ≤ Medium | Run the pipeline once. Use Step 4 / Step 7 fan-out tables as written. |
| **B — Deduped fan-out** | N = 3–4 **AND** L < ~500 **AND** S ≤ 2 **AND** risk ≤ High | Union architects/validators across tags → dedupe (each agent runs once with merged prompt) → cap at 5 critics per fan-out step. Priority order: `security` > `data-pipeline`/`data-model` > `algorithmic-ml` > `ml-infra` > `llm` > `cloud-infra` > `application-code` > `docs-specs`. Domains above the cap are represented by `rubber-duck` with a *flag-if-deeper-needed* instruction. |
| **C — Decompose & orchestrate** | N ≥ 5 **OR** L ≥ ~500 **OR** S ≥ 3 **OR** Critical risk **OR** public-API / schema / IaC surface change | Do **not** run the DAG as a single unit. Hand to `task-orchestrator` → builds a work-DAG of sub-tasks, each with ≤ 2 domain tags, each running its own strategy-A pipeline. |

## Flow

```
                         ┌─────────────────────────────┐
                         │ Step 0 — Domain classify    │
                         │ produce tag set {T₁, T₂, …} │
                         └──────────────┬──────────────┘
                                        │
                         ┌──────────────▼──────────────┐
                         │ Step 0.5 — Measure N,L,S,   │
                         │ surface, risk               │
                         └──────────────┬──────────────┘
                                        │
            ┌───────────────────────────┼───────────────────────────┐
            │                           │                           │
    N≤2, L<200, S≤1            N=3-4, L<500, S≤2            N≥5 OR L≥500 OR
    no surface, risk≤Med       risk ≤ High                  S≥3 OR Crit risk OR
            │                           │                   surface change
            │                           │                           │
            ▼                           ▼                           ▼
    ┌───────────────┐           ┌───────────────┐          ┌──────────────────┐
    │  Strategy A   │           │  Strategy B   │          │   Strategy C     │
    │  Direct       │           │  Deduped      │          │  Decompose &     │
    │               │           │  fan-out      │          │  orchestrate     │
    └───────┬───────┘           └───────┬───────┘          └────────┬─────────┘
            │                           │                           │
            │                     Confirm with                Show DAG for
            │                     user before                 approval →
            │                     launching                   fine-tuning loop
            │                           │                           │
            │                           │                  ┌────────▼─────────┐
            │                           │                  │ task-orchestrator│
            │                           │                  │ builds sub-task  │
            │                           │                  │ DAG              │
            │                           │                  └────────┬─────────┘
            │                           │                           │
            │                           │                  ┌────────▼─────────┐
            │                           │                  │ Per sub-task:    │
            │                           │                  │   run strategy-A │
            │                           │                  │   pipeline       │
            │                           │                  │ (≤2 tags each)   │
            │                           │                  └────────┬─────────┘
            │                           │                           │
            │                           │                  ┌────────▼─────────┐
            │                           │                  │ Integration pass │
            │                           │                  │ iterate-hardcore │
            │                           │                  │ on composed code │
            │                           │                  └────────┬─────────┘
            │                           │                           │
            └───────────────┬───────────┴───────────────┬───────────┘
                            │                           │
                            ▼                           ▼
                       Steps 1–10 (discover, plan, test design, critique,
                       implement, test, validate, review, merge, cleanup)
```

## Key differences between B and C

| Dimension | Strategy B (deduped fan-out) | Strategy C (decompose) |
|---|---|---|
| **Unit of work** | Still one change | Multiple sub-tasks with a DAG |
| **Fan-out per step** | All domain critics in parallel, capped at 5 | Each sub-task has only ≤2 tags locally → narrow fan-out per sub-task |
| **Integration risk** | Low (it's one change) | High — seams between sub-tasks are where bugs hide |
| **Integration tests** | Nice to have | Automatically 🔴 CRITICAL per DAG edge (blocks merge) |
| **User approval gate** | Single "approve fan-out?" prompt | DAG approval + fine-tuning loop before anything runs |
| **Interface contracts** | N/A | Versioned, published upstream **before** either side implements |
| **Security** | Single pass | Two-pass (early threat model + full security sub-task near end) |
| **Telemetry contract** | Implicit | Locked during decomposition so all sub-tasks emit compatible signals |
| **Revertability** | PR revert | Per sub-task (feature flag / blue-green / compat window) |
| **Cost** | ~5–10 agent calls | Order-of-magnitude more; that's why the approval gate matters |

## Escape hatch (within strategy C)

If any sub-task after decomposition would **still** exceed strategy B's thresholds, split the PR itself into multiple merges rather than try to ship one mega-PR. `task-orchestrator` proposes a sequence of PRs, each independently reviewable and revertable.

---

## How to customize this file

Safe knobs to tune for your context:

1. **Line thresholds** (`L < 200` for A, `L < 500` for B) — lower them on critical-path codebases, raise them on greenfield side projects.
2. **Domain-count thresholds** (`N ≤ 2`, `N = 3–4`, `N ≥ 5`) — teams with more domain specialists can push B higher before decomposing.
3. **Risk mapping** — redefine what counts as Critical/High/Medium/Low for your system.
4. **Priority order** in Strategy B — if your org treats `cloud-infra` as higher than `security` (e.g., SRE-heavy shop), swap them.
5. **Surface change rule** — if you never ship schema changes, drop that as an auto-trigger to C.
6. **Cap per fan-out** (currently 5) — tighten to 3 for cost, loosen to 7 for depth.

After editing, the `nontrivial-pipeline` skill will pick up the new thresholds automatically the next time it evaluates Step 0.5 (it reads this file rather than hardcoding the logic).

If you fork this into team or project scope, put it in your repo at `.copilot/skills/strategy-selection/SKILL.md` and it will override this user-global copy when that repo is the cwd.
