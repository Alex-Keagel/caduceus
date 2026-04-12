# Caduceus Indexing Deep Dive

> How Caduceus builds and maintains its understanding of your codebase: ProjectScanner, FederatedIndex, CodePropertyGraph, and WikiEngine.

---

## Table of Contents

1. [Overview](#1-overview)
2. [ProjectScanner](#2-projectscanner)
3. [FederatedIndex](#3-federatedindex)
4. [CodePropertyGraph](#4-codepropertygraph)
5. [WikiEngine and WikiWatcher](#5-wikiengine-and-wikiwatcher)
6. [Manual Controls](#6-manual-controls)
7. [Performance Considerations](#7-performance-considerations)

---

## 1. Overview

The Omniscience layer (`caduceus-omniscience` + `caduceus-scanner`) is responsible for giving the agent accurate, up-to-date knowledge of your codebase without requiring you to copy-paste code into every message.

It is built on three open-source foundations:

| Foundation | Provides |
|------------|---------|
| **tree-sitter** | Incremental, language-aware AST parsing for 40+ languages |
| **qdrant-edge** | Embedded vector search engine (no external server required) |
| **SQLite WAL** | Persistent index metadata and graph adjacency storage |

These run entirely on your local machine. No code is sent to any cloud indexing service.

---

## 2. ProjectScanner

`caduceus-scanner` is the entry point for all indexing. It runs on project open and incrementally on file changes.

### What It Does

1. **Directory walk** — Recursively walks the project root, respecting `.caduceusignore` and `.gitignore`
2. **Language detection** — Identifies the language of each file using extension + content heuristics. Supported languages include Rust, TypeScript, JavaScript, Python, Go, Java, C, C++, Ruby, Swift, Kotlin, and more.
3. **Framework fingerprinting** — Detects project type from marker files:

   | File | Detected Framework |
   |------|-------------------|
   | `Cargo.toml` | Rust (workspace or crate) |
   | `package.json` | Node.js / npm |
   | `pyproject.toml` / `setup.py` | Python |
   | `go.mod` | Go module |
   | `pom.xml` | Java Maven |
   | `build.gradle` | Java Gradle |
   | `Makefile` | C / C++ / generic |

4. **Context map construction** — Builds a map of `file_path → {language, size_bytes, line_count, token_estimate, importance_score}`. The importance score is derived from:
   - How many other files import this file (in-degree in the dependency graph)
   - Presence in a root directory vs. a deeply nested path
   - Whether the file is referenced in `CADUCEUS.md` or `README.md`

5. **Chunking and embedding** — Each file is split into chunks (functions, classes, or fixed-size windows with overlap for non-AST-parseable formats). Each chunk is embedded using a local embedding model and upserted into the qdrant-edge vector shard.

### .caduceusignore Processing

ProjectScanner reads `.caduceusignore` from the project root (and any subdirectory that has one). The rules are processed identically to `.gitignore`:

- `*.rs` — ignore all Rust files (unusual — don't do this)
- `target/` — ignore the build artifact directory
- `!src/main.rs` — un-ignore a previously ignored file

Files ignored by `.gitignore` are also ignored unless you explicitly un-ignore them in `.caduceusignore`.

### Output

The scanner writes its output to `.caduceus/index/`:

```
.caduceus/
└── index/
    ├── file_tree.json       # Full file tree with metadata
    ├── context_map.json     # Per-file token estimates and importance scores
    └── qdrant/              # Embedded vector shard (binary)
```

---

## 3. FederatedIndex

The FederatedIndex enables cross-project symbol search. It is most valuable in monorepos and when you have related projects checked out locally.

### Architecture

Each project maintains its own isolated qdrant-edge shard. The FederatedIndex acts as a query federation layer:

```
User query: "find all usages of AuthToken"
         │
         ▼
┌─────────────────────────────┐
│  FederatedIndex             │
│  Query planner              │  Rewrites query for each shard
└──────┬──────────────────────┘
       │ Fan-out
       ├────────────────────┬───────────────────┐
       ▼                    ▼                   ▼
┌──────────────┐   ┌──────────────┐   ┌──────────────┐
│ Project A    │   │ Project B    │   │ Project C    │
│ qdrant shard │   │ qdrant shard │   │ qdrant shard │
└──────┬───────┘   └──────┬───────┘   └──────┬───────┘
       │                  │                  │
       └──────────────────┼──────────────────┘
                          │ Merge + rank
                          ▼
                  ┌───────────────┐
                  │ Merged results│  With source-project provenance
                  └───────────────┘
```

### Configuring Cross-Project Indexing

To include a sibling project in the federated index, add it to `.caduceus/config.toml`:

```toml
[federation]
projects = [
  "../shared-types",
  "../auth-service",
  "/Users/you/dev/company/platform",
]
```

Or use the `/config` command:

```
/config federation.projects.add ../shared-types
```

### Query Syntax

FederatedIndex queries work through the normal search interface — no special syntax needed. The agent uses it automatically when your query implies cross-project context:

```
# These will trigger cross-project search:
"where is UserPermission defined?"
"find all imports of the auth library"
"are there any other usages of this pattern?"

# Explicit cross-project @mention:
@federated UserPermission
```

---

## 4. CodePropertyGraph

The CodePropertyGraph (CPG) maps the semantic relationships between code entities using AST analysis.

### Graph Schema

**Nodes:**

| Type | Example |
|------|---------|
| `Function` | `fn parse_config(path: &Path) -> Result<Config>` |
| `Class` / `Struct` | `struct AgentConfig { ... }` |
| `Method` | `impl Config { fn validate(&self) }` |
| `Module` | `mod providers;` |
| `Interface` / `Trait` | `trait LlmProvider { ... }` |
| `Enum` | `enum PermissionLevel { ... }` |
| `Constant` | `const MAX_TOKENS: usize = 200_000;` |

**Edges:**

| Type | Meaning |
|------|---------|
| `CALLS` | Function A calls Function B |
| `IMPORTS` | Module A imports symbol from Module B |
| `INHERITS` | Class A extends Class B |
| `IMPLEMENTS` | Struct A implements Trait B |
| `INSTANTIATES` | Function A creates an instance of Struct B |
| `DEFINES_IN` | Symbol is defined in this module |
| `USES_TYPE` | Function uses this type in its signature |

### How the Graph Is Built

1. **Parse:** tree-sitter parses the file into a concrete syntax tree
2. **Extract:** Structured queries extract function signatures, class definitions, import statements, and call sites
3. **Resolve:** Import paths are resolved to their definition nodes (handling re-exports and aliasing)
4. **Upsert:** New and changed nodes are upserted into the SQLite graph store; deleted nodes are tombstoned

**Incremental updates:** Only files modified since the last scan are re-parsed. The graph diffing algorithm updates only the affected nodes and edges without a full rebuild.

### What the Agent Uses the CPG For

The agent queries the CPG to answer questions like:

- **Impact analysis:** "What calls `parse_config`? If I change its signature, what breaks?"
- **Dependency direction:** "Does `caduceus-orchestrator` import from `caduceus-permissions` in a way that could create a cycle?"
- **Dead code detection:** "Are there any functions with no inbound `CALLS` edges?"
- **Refactoring scope:** "Where is `OldTypeName` used? Show me all sites that need updating."
- **Architecture validation:** "List all modules that import from `caduceus-storage` — does that match the intended layer diagram?"

### Limitations

- **Dynamic dispatch:** Calls through trait objects or function pointers are tracked where statically resolvable; dynamic dispatch paths are annotated but not fully traced
- **Macros:** Code generated inside macros may not be fully represented in the graph
- **Cross-language:** The CPG is per-language; a TypeScript frontend calling a Rust Tauri command is connected via the IPC schema, not the CPG

---

## 5. WikiEngine and WikiWatcher

The WikiEngine auto-generates and maintains a Markdown knowledge base in `.caduceus/wiki/`.

### Wiki Structure

```
.caduceus/wiki/
├── index.md              # Master index — entry point for AI context injection
├── architecture.md       # Auto-generated system architecture overview
├── api/
│   ├── index.md          # API surface index
│   └── <module>.md       # One page per public module/crate/package
├── patterns.md           # Detected architectural patterns
├── dependencies.md       # Key external dependencies and their roles
└── glossary.md           # Project-specific terms and abbreviations
```

### WikiWatcher — Trigger Events

The WikiWatcher monitors for events that should invalidate wiki pages:

| Event | Pages invalidated | Action |
|-------|-------------------|--------|
| File saved | Pages for that file's module/crate | Re-analyze public API surface |
| New file created | `index.md`, parent module page | Add to index, generate new page |
| File deleted | `index.md`, orphaned pages | Remove from index, tombstone page |
| Agent turn completes | Any page whose source changed during the turn | Batch regenerate at turn end |
| `/wiki refresh` | All pages | Full rebuild |
| `/init` | All pages | Full generation from scratch |

### What Each Page Contains

**`architecture.md`** — Generated from the project structure and any existing `ARCHITECTURE.md`:
- Layer/module diagram
- Key design decisions extracted from comments and docs
- Dependency direction summary

**`api/<module>.md`** — Generated from public API analysis:
- List of public types with doc comments
- List of public functions with signatures and doc comments
- Usage examples extracted from tests and doc-tests

**`patterns.md`** — Detected from code structure:
- Builder pattern instances
- Repository pattern usages
- Common error handling idioms
- Middleware chains

**`glossary.md`** — Extracted from:
- Doc comments that define domain terms
- Type names that appear to be domain nouns
- `CADUCEUS.md` and `README.md`

### Wiki Content Injection

On each agent turn, the WikiEngine selects relevant wiki pages to inject into the system prompt based on semantic similarity to the current query. This is bounded by the context token budget.

Injection priority:
1. `index.md` (always included, it's small)
2. Wiki pages for files explicitly `@mention`ed
3. Wiki pages semantically similar to the user's query
4. `architecture.md` (for architectural queries)

### Opting Out of Wiki Generation

To disable automatic wiki generation:

```toml
# .caduceus/config.toml
[wiki]
enabled = false
```

To disable wiki for specific paths:

```toml
[wiki]
ignore = ["src/generated/", "vendor/"]
```

---

## 6. Manual Controls

### Trigger a Full Re-scan

```bash
/scan
```

Forces ProjectScanner to re-walk the entire directory, rebuild the context map, and re-embed any changed files. Useful after large git operations (merge, rebase) or if you suspect the index is stale.

### Rebuild the Wiki

```bash
/wiki refresh          # Rebuild all wiki pages from current source
/wiki show             # Print the wiki index to the terminal
/wiki show architecture # Print a specific wiki page
```

### Inspect the Index

```bash
/ctx_viz               # Visual breakdown of context window by source
/config scanner        # Show current scanner configuration
```

### Force Re-embedding

If you change the embedding model (via `/config omniscience.embedding_model`), the existing embeddings are stale. Trigger a full re-embedding:

```bash
/scan --reembed
```

This re-reads all files and re-embeds them with the new model. It does not re-parse ASTs (those are model-independent).

---

## 7. Performance Considerations

### Large Repositories (> 100k files)

For very large repos, the initial scan can take several minutes. Strategies:

1. **Use `.caduceusignore` aggressively** — exclude build outputs, vendor dirs, test fixtures, and data files that don't contribute to code understanding
2. **Enable partial indexing** — index only the subdirectory you're actively working in:
   ```toml
   # .caduceus/config.toml
   [scanner]
   root = "src/"          # Only index src/, not the whole repo
   ```
3. **Adjust embedding concurrency:**
   ```toml
   [scanner]
   embedding_concurrency = 4   # Default: number of CPU cores / 2
   ```

### Monorepos

Monorepos with many packages/crates benefit from federation. Configure each package as a separate federated project and use `@federated` queries for cross-package search.

```toml
# .caduceus/config.toml
[federation]
projects = [
  "./packages/auth",
  "./packages/api",
  "./packages/ui",
  "./packages/shared",
]

[scanner]
root = "."             # Still scan the root for workspace config
include_patterns = [
  "packages/*/src/**",  # Only embed source, not dist
  "*.toml",
  "*.json",
]
```

### Incremental Indexing Performance

The scanner uses file modification times to skip unchanged files. After the initial scan, incremental updates are fast (typically < 1 second for a single file save).

If incremental indexing seems slow, check:
1. File watcher is running: `/config scanner.watch_enabled` → should be `true`
2. The qdrant shard is not fragmented: `/scan --optimize` compacts the vector index

### Embedding Model Tradeoffs

| Model | Speed | Quality | Size |
|-------|-------|---------|------|
| `nomic-embed-text` (default) | Fast | Good | 137M |
| `mxbai-embed-large` | Medium | Better | 335M |
| `text-embedding-3-small` (OpenAI API) | Network-limited | Good | API |
| `text-embedding-3-large` (OpenAI API) | Network-limited | Best | API |

Configure with:
```toml
[omniscience]
embedding_model = "nomic-embed-text"   # Local via Ollama
# embedding_model = "text-embedding-3-small"  # OpenAI API (requires OPENAI_API_KEY)
```

Local models (via Ollama) are recommended for privacy and offline use. Remote models may produce higher-quality embeddings at the cost of an API call per chunk.
