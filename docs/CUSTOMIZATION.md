# Caduceus Customization Guide

> How to extend Caduceus with skills, custom agents, project instructions, and context-aware README files.

---

## Table of Contents

1. [Skills (SKILL.md)](#1-skills-skillmd)
2. [Agents (.agent.md)](#2-agents-agentmd)
3. [Project Instructions (CADUCEUS.md)](#3-project-instructions-caduceusmd)
4. [README Best Practices for AI Context](#4-readme-best-practices-for-ai-context)
5. [.caduceusignore](#5-caduceusignore)

---

## 1. Skills (SKILL.md)

Skills are reusable instruction sets that extend the agent's capabilities for a specific domain or task. They are invoked by the agent when a user's request matches the skill's description.

### File Format

```markdown
---
name: my-skill
description: "What this skill does and when to invoke it.\n\nTrigger phrases:\n- 'phrase 1'\n- 'phrase 2'\n\nExamples:\n- User says 'do X' → invoke this skill to Y"
---

# My Skill

You are an expert in [domain]. When invoked:

1. First, understand [context]
2. Then, [action]
3. Finally, [output format]

## Quality Checklist
- [ ] Requirement 1
- [ ] Requirement 2
```

### Where to Put Skills

| Location | Scope |
|----------|-------|
| `.caduceus/skills/SKILL.md` | Project-scoped — only active in this project |
| `~/.caduceus/skills/SKILL.md` | User-scoped — active in all projects |
| `.caduceus/skills/my-skill.skill.md` | Named skill file (multiple skills per directory) |

Skills are loaded in this priority order: project > user. A project-level skill overrides a user-level skill with the same name.

### How Triggers Work

The agent reads the `description` field to determine when to invoke a skill. Be explicit:
- List exact trigger phrases
- Give concrete examples with `→` arrow notation
- Include what the skill should **not** be used for

The description is matched using semantic similarity — you don't need to list every synonym, but the more specific you are, the better the trigger accuracy.

### Good vs. Bad Skill Definitions

#### ❌ Bad — too vague

```markdown
---
name: code-helper
description: "Helps with code"
---

Help the user with their code.
```

Problems: No trigger phrases, no examples, instructions are too generic to be useful.

#### ❌ Bad — too broad a trigger

```markdown
---
name: python-helper
description: "Use for any Python question"
---
```

Problems: Will trigger on every Python question even when the agent could handle it directly without a skill.

#### ✅ Good — specific trigger and clear instructions

```markdown
---
name: django-migrations
description: "Expert guidance on Django database migrations.\n\nTrigger phrases:\n- 'create a migration'\n- 'migrate database'\n- 'squash migrations'\n- 'migration conflict'\n\nExamples:\n- User says 'I need to add a field to my model' → invoke this skill to generate the migration and update the model\n- User asks 'how do I squash 50 migrations?' → invoke this skill\n\nDo NOT invoke for: general Django questions, views, templates, or authentication."
---

# Django Migrations Expert

You are a senior Django developer specializing in database migrations.

## When Creating Migrations

1. Check for existing migrations in the app's `migrations/` directory
2. Use `makemigrations --name descriptive_name` (never use auto-generated names)
3. Always add `RunSQL` with `reverse_sql` for data migrations
4. Check for circular dependencies before applying

## Migration Naming Convention
- Schema changes: `0042_add_user_email_verified`
- Data migrations: `0043_data_backfill_email_verified`
- Squashes: `0001_squashed_0043`

## Checklist
- [ ] Migration is reversible (has `reverse_sql` or `backwards` method)
- [ ] No raw SQL that bypasses the ORM without a comment explaining why
- [ ] Dependencies list is minimal (no unnecessary cross-app deps)
- [ ] Tested with both `migrate` and `migrate --fake`
```

### Real-World Skill Examples

#### Commit Message Skill

```markdown
---
name: commit-messages
description: "Write conventional commit messages following the Conventional Commits spec.\n\nTrigger phrases:\n- 'write a commit message'\n- 'commit message for'\n- 'summarize these changes as a commit'\n\nExamples:\n- User says 'write a commit message for these changes' → invoke this skill"
---

# Conventional Commit Message Writer

Format: `<type>(<scope>): <description>`

Types: feat, fix, docs, style, refactor, perf, test, build, ci, chore

Rules:
- Subject line ≤ 72 characters
- Use imperative mood ("add" not "added")
- Include body if the change needs explanation
- Reference issues with `Closes #123`
- Add Co-authored-by trailer for pair/AI work
```

#### Test Generation Skill

```markdown
---
name: rust-tests
description: "Generate idiomatic Rust unit and integration tests.\n\nTrigger phrases:\n- 'write tests for'\n- 'add test coverage'\n- 'generate unit tests'\n\nExamples:\n- User says 'write tests for this function' → invoke this skill\n- User asks 'add test coverage to this module' → invoke this skill"
---

# Rust Test Generator

## Test Module Structure
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_happy_path() { ... }

    #[test]
    fn test_edge_case_empty_input() { ... }

    #[test]
    #[should_panic(expected = "invalid")]
    fn test_invalid_input_panics() { ... }
}
```

## Rules
- One `assert!` per test (prefer focused tests)
- Use `tempfile::TempDir` for filesystem tests (auto-cleans up)
- Use `MockLlmAdapter` for any test touching the LLM layer
- Property tests with `proptest` for parsing/serialization code
- Name tests `test_<what>_<condition>_<expected_outcome>`
```

---

## 2. Agents (.agent.md)

Agents are autonomous sub-agents with specialized roles, their own tool access, and optionally a different model than the default. The orchestrator can delegate tasks to agents, and agents can be invoked directly via the command palette.

### File Format

```markdown
---
name: my-agent
description: "When to invoke this agent...\n\nTrigger phrases:\n- 'phrase 1'\n- 'phrase 2'\n\nExamples:\n- User says 'X' → invoke this agent to Y"
tools: ['shell', 'read', 'edit', 'search']
model: claude-opus-4-5
---

# Agent Name

You are a [role] with expertise in [domain].

## When Invoked

1. Gather context by [action]
2. [Next step]

## Quality Criteria
- [ ] Criterion 1
```

### Where to Put Agents

| Location | Scope | Notes |
|----------|-------|-------|
| `.github/agents/*.agent.md` | Project-scoped | Committed to the repo; shared with the team |
| `.caduceus/agents/*.agent.md` | Project-scoped | Local to your machine; not committed |
| `~/.caduceus/agents/*.agent.md` | User-scoped | Active in all projects |

**Recommendation:** Put team-shared agents in `.github/agents/` (version-controlled). Put personal experimental agents in `~/.caduceus/agents/`.

### Available Tools

| Tool Name | What it grants |
|-----------|----------------|
| `shell` | Run shell commands (subject to permission gating) |
| `read` | Read files and directories |
| `edit` | Write, edit, and create files |
| `search` | Code search, grep, semantic search |
| `git` | Git operations (status, diff, commit, branch) |
| `browser` | Fetch URLs |
| `mcp` | Call external MCP tool servers |

Agents should request only the tools they need. A read-only research agent should not have `edit` or `shell`.

### Setting Model Per Agent

```markdown
---
name: architect-agent
model: claude-opus-4-5        # Use the most capable model for planning
tools: ['read', 'search']
---
```

```markdown
---
name: quick-fixer
model: claude-haiku-4-5      # Use a fast model for simple fixes
tools: ['read', 'edit']
---
```

### Trigger Phrases and Examples

The `description` field is critical — it determines when the orchestrator delegates to this agent vs. handling the request directly. Follow this pattern:

```yaml
description: "One sentence summary.\n\nTrigger phrases:\n- 'exact phrase'\n- 'another phrase'\n\nExamples:\n- User says 'review this PR' → invoke this agent to analyze the diff and produce structured feedback\n- User asks 'is this architecture sound?' → invoke this agent\n\nDo NOT invoke for: [negative examples]"
```

### Complete Agent Example — Security Reviewer

```markdown
---
name: security-reviewer
description: "Expert security code review for Rust projects.\n\nTrigger phrases:\n- 'security review'\n- 'check for vulnerabilities'\n- 'audit this code'\n- 'is this code safe?'\n\nExamples:\n- User says 'do a security review of the auth module' → invoke this agent\n- User asks 'are there any injection risks here?' → invoke this agent\n\nDo NOT invoke for: general code review, style review, or performance analysis."
tools: ['read', 'search', 'shell']
model: claude-opus-4-5
---

# Security Reviewer

You are a senior security engineer specializing in Rust application security.

## When Invoked

1. Read the target files identified by the user
2. Search for related files (error handling, input validation, authentication paths)
3. Run `cargo audit` if a `Cargo.lock` is present

## What to Look For

- **Injection:** shell command construction from user input, SQL string concatenation
- **Path traversal:** file operations that accept user-controlled paths without canonicalization
- **Unsafe blocks:** `unsafe` code that is broader than necessary
- **Secrets in code:** hardcoded credentials, API keys, or tokens
- **Cryptography:** use of deprecated algorithms, weak random number generation
- **Dependency vulnerabilities:** known CVEs in `Cargo.lock`

## Output Format

Produce a structured report:

```markdown
## Security Review: [target]

### Critical
- [Issue] — [file:line] — [remediation]

### High
- ...

### Informational
- ...

### Passed Checks
- [ ] No hardcoded secrets found
- [ ] Path operations use canonicalized paths
```
```

---

## 3. Project Instructions (CADUCEUS.md)

`CADUCEUS.md` (or `.caduceus/instructions.md`) is loaded at the start of every session. It tells the agent about your project's conventions, architecture, and constraints. **This is the most important customization you can make.**

### What to Include

- **Project overview:** What does this project do? Who uses it?
- **Architecture:** Key modules/crates/packages and how they relate
- **Coding standards:** Naming conventions, error handling patterns, formatting rules
- **Testing requirements:** What tests are required? What mocking strategy?
- **Build and test commands:** Exact commands to build, test, lint
- **Off-limits files:** Auto-generated code, vendor dirs, build artifacts
- **Key files:** The most important files for understanding the codebase

### What to Leave Out

- Implementation details that are already in the code
- Content that changes frequently (it'll be stale within days)
- Generic advice that applies to any project (the agent already knows this)

---

### Example: Rust Project CADUCEUS.md

```markdown
# ProjectName — Caduceus Instructions

## What This Project Does
A high-performance HTTP API server for processing financial transactions.
Built with Axum, SQLx (PostgreSQL), and Tokio.

## Architecture

```
src/
├── main.rs          # Entry point; sets up router and DB pool
├── routes/          # One file per route group (auth, transactions, reports)
├── models/          # SQLx model structs with Serialize/Deserialize
├── services/        # Business logic layer (no HTTP types here)
├── db/              # Database queries and migrations
└── errors.rs        # AppError enum — all errors flow through here
```

## Coding Conventions
- Error handling: use `AppError` (in `src/errors.rs`) for all HTTP errors; no `.unwrap()` in production code
- Async: all service functions are `async`; use `tokio::spawn` only for fire-and-forget background work
- Naming: `snake_case` everywhere; HTTP handler functions named `handle_<verb>_<resource>`
- No `unwrap()` or `expect()` outside of tests and `main()`

## Testing Requirements
- Every public function in `services/` needs a unit test
- Integration tests go in `tests/` and use a real test database (see `tests/helpers.rs`)
- Use `wiremock` for mocking external HTTP calls
- Run: `cargo test --workspace`

## Build Commands
```bash
cargo build --release          # production build
cargo test --workspace         # all tests
cargo clippy --workspace       # linting
cargo sqlx migrate run         # apply DB migrations
```

## Do Not Edit
- `migrations/` — managed by `cargo sqlx migrate add`
- `target/` — build artifacts
- Any `*_generated.rs` file

## Key Files
- `src/errors.rs` — understand this before touching any error handling
- `src/routes/mod.rs` — the router; all routes registered here
- `.env.example` — copy to `.env` and fill in before running locally
```

---

### Example: Python Project CADUCEUS.md

```markdown
# ProjectName — Caduceus Instructions

## What This Project Does
A Django REST API for a multi-tenant SaaS platform.
Python 3.12, Django 5.x, Django REST Framework, PostgreSQL, Celery + Redis.

## Architecture

```
myapp/
├── api/             # DRF viewsets and serializers
├── models/          # Django models (one file per domain)
├── services/        # Business logic (no Django ORM imports in views)
├── tasks/           # Celery tasks
├── migrations/      # Django migrations (never edit manually)
└── tests/           # pytest tests
```

## Coding Conventions
- Views call services; services call the ORM — never put ORM queries in views
- Use `select_related` / `prefetch_related` to avoid N+1 queries
- All API responses use the shared serializer base class in `api/base.py`
- Type annotations required on all function signatures
- Black formatting (line length 88)

## Testing
- pytest with `pytest-django`; fixtures in `conftest.py`
- Use `factory_boy` for model factories; never create raw objects with `Model.objects.create()` in tests
- Mock external services with `responses` library
- Run: `pytest -x --reuse-db`

## Build Commands
```bash
pip install -e ".[dev]"    # install with dev deps
pytest -x --reuse-db       # run tests (fast, reuses DB)
black .                    # format
ruff check .               # lint
python manage.py migrate   # apply migrations
```

## Do Not Edit
- `migrations/` — use `makemigrations`
- `staticfiles/` — generated by `collectstatic`

## Key Files
- `myapp/models/base.py` — abstract base model all models inherit from
- `myapp/api/base.py` — base serializer and viewset with shared behavior
- `settings/base.py` — base Django settings
```

---

### Example: TypeScript Project CADUCEUS.md

```markdown
# ProjectName — Caduceus Instructions

## What This Project Does
A Next.js 14 app router application with a tRPC API, Prisma ORM, and PostgreSQL.

## Architecture

```
src/
├── app/             # Next.js app router pages and layouts
├── components/      # React components (ui/ for primitives, features/ for domain)
├── server/          # tRPC routers and server-side logic
│   ├── routers/     # One file per domain (user, post, comment)
│   └── trpc.ts      # tRPC context and middleware
├── lib/             # Shared utilities and clients
└── types/           # Shared TypeScript types
prisma/
└── schema.prisma    # Database schema
```

## Coding Conventions
- No `any` types — ever
- Functional components with hooks; no class components
- Server components by default; add `'use client'` only when necessary
- tRPC for all API calls; no raw `fetch` from client components
- Zod for all input validation (use the same schema for tRPC and forms)
- Component naming: PascalCase; utility functions: camelCase; constants: UPPER_SNAKE_CASE

## Testing
- Vitest for unit tests; Playwright for E2E
- Mock Prisma with `vitest-mock-extended`
- Run: `npm run test` (unit) / `npm run test:e2e` (Playwright)

## Build Commands
```bash
npm install                  # install deps
npm run dev                  # dev server
npm run build                # production build
npm run test                 # unit tests
npx prisma db push           # push schema changes (dev only)
npx prisma migrate dev       # create migration
```

## Do Not Edit
- `prisma/migrations/` — managed by Prisma CLI
- `.next/` — build output
- `node_modules/`

## Key Files
- `src/server/trpc.ts` — tRPC context, middleware, auth
- `src/server/routers/_app.ts` — root router (all sub-routers merged here)
- `prisma/schema.prisma` — source of truth for data shape
```

---

## 4. README Best Practices for AI Context

Your `README.md` is the first file the agent reads when opening a project. A well-structured README dramatically improves the quality of AI assistance from the first interaction.

### Essential Sections for AI Context

```markdown
# Project Name

One sentence: what this project does and who it's for.

## Architecture

Brief description of the top-level structure. Include a directory tree
or a layer diagram. This is the single most valuable section for AI context.

## Key Files

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point |
| `src/config.rs` | Configuration schema |
| `ARCHITECTURE.md` | Detailed architecture guide |

## How to Build

```bash
# Required tools
cargo --version  # 1.78+

# Build
cargo build --release

# Test
cargo test --workspace
```

## How to Test

What the test strategy is. What mocking approach is used.
What environment variables are needed.

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `DATABASE_URL` | Yes | PostgreSQL connection string |
| `API_KEY` | No | Optional external service key |
```

### What Makes a README AI-Friendly

**✅ Include:**
- Architecture section with directory tree
- Exact build and test commands (copy-paste ready)
- Table of key files with one-line descriptions
- Environment variable table
- Links to deeper docs (`ARCHITECTURE.md`, `CONTRIBUTING.md`)

**❌ Avoid:**
- Marketing language that doesn't describe structure
- Outdated screenshots or badges
- Vague phrases like "it's built with modern tools"

---

## 5. .caduceusignore

`.caduceusignore` tells the ProjectScanner what to exclude from indexing. It uses the same syntax as `.gitignore`.

### Sensible Default .caduceusignore

```gitignore
# Build artifacts
target/
dist/
build/
.next/
out/

# Dependencies
node_modules/
vendor/
.venv/
__pycache__/
*.pyc

# Generated files
*_generated.rs
*.pb.go
*.pb.swift
prisma/migrations/

# Large binary assets
*.png
*.jpg
*.jpeg
*.gif
*.svg
*.ico
*.woff
*.woff2
*.ttf
*.eot
*.mp4
*.mov

# Data files (often large and not useful for code context)
*.csv
*.parquet
*.json.gz
data/
fixtures/large/

# IDE and OS noise
.DS_Store
.idea/
.vscode/extensions.json
*.swp

# Secrets (should never be in context)
.env
.env.local
.env.*.local
secrets/
```

### Tips

- Keep `.caduceusignore` lean — over-ignoring reduces the quality of code search
- Always ignore generated files; the agent doesn't need to read what it would regenerate
- Ignore large data files that don't contribute to code understanding
- Ignore secrets files unconditionally — the agent should never read them into context
