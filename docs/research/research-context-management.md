# Research: Context Management in AI Coding Assistants

> **Date:** 2025-07-15  
> **Scope:** Claude Code, GitHub Copilot CLI, Cursor, Anthropic "Context Engineering" framework  
> **Goal:** Inform Caduceus context management — compaction, zones, pinning, retrieval

---

## 1. Claude Code Context Management

### 1.1 `/compact` Command & Auto-Compaction

Claude Code provides an explicit `/compact` slash command that summarizes the conversation so far into a single system-level summary message, discarding the raw turn history. This is the user-visible mechanism for reclaiming context window space.

**Auto-compaction** fires when context usage reaches **75–92%** of the model's context limit (configurable per-model). The system uses a two-pass approach:

1. **Pre-compact hook** (`preCompactTool`) — Users can register a hook that runs immediately before compaction. This hook can inject "must-preserve" content (e.g., current task state, key decisions) into the summary.
2. **Summarization pass** — The LLM is asked to produce a compressed summary of the conversation, preserving key decisions, code references, and task progress. Recent messages (typically the last 3–5 turns) are kept verbatim.

### 1.2 Performance Zones

Claude Code internally tracks context usage as a percentage and maps it to performance zones:

| Zone | Fill % | Behavior |
|------|--------|----------|
| **Green** | 0–50% | Optimal — model has full attention budget |
| **Yellow** | 50–70% | Slight degradation — long conversations start losing early context |
| **Orange** | 70–85% | Noticeable loss — the model struggles to recall early details |
| **Red** | 85–95% | Critical — should compact immediately |
| **Critical** | 95%+ | Auto-compact triggered; user warned |

### 1.3 Context Component Breakdown

Claude Code decomposes context usage into:

- **System prompt** — Base instructions, tool descriptions, project context
- **Tool schemas** — JSON schemas for all registered tools
- **Project context** — CLAUDE.md, .cursorrules, workspace metadata
- **Conversation history** — User/assistant turns
- **Tool results** — Outputs from tool calls (often the largest component)
- **Pinned context** — User-pinned content that survives compaction

### 1.4 Compaction Strategies

- **Summarize** — LLM generates a summary; old messages replaced by summary
- **Truncate** — Drop oldest N messages beyond a threshold
- **Hybrid** — Summarize old turns, keep recent N turns verbatim (default)
- **Smart** — LLM identifies high-salience content (decisions, errors, TODOs) and preserves it while dropping routine back-and-forth

---

## 2. GitHub Copilot CLI Context Management

### 2.1 `/context` Visualization

Copilot CLI provides `/context` to show an ASCII breakdown of context window usage. The visualization shows a progress bar with per-component token counts and a zone indicator.

### 2.2 Auto-Compaction at 95%

Copilot CLI triggers auto-compaction at **95%** context fill. This is more aggressive than Claude Code, leaving less headroom but maximizing the usable conversation length.

### 2.3 Checkpoint-Based Recovery

Copilot CLI ties context management to its checkpoint system. When compaction occurs, a checkpoint is created first, allowing the user to restore the full conversation if the summary lost important details.

### 2.4 Parallel Agent Context Management

When running multiple agents (via the `task` tool), each agent gets its own context window. The parent conversation only receives a summary of the agent's work, keeping the main context clean. This is a form of "context isolation" — sub-agents don't pollute the parent's context budget.

---

## 3. Cursor Context Management

### 3.1 Semantic Indexing

Cursor maintains a semantic index of the entire codebase using embeddings. When the user asks a question, Cursor retrieves the most relevant code chunks using vector similarity, rather than including entire files.

### 3.2 `.cursorignore` / `.cursorindexignore`

Cursor supports glob-based exclusion files that prevent specified paths from being indexed or injected into context. This is analogous to `.gitignore` but for the AI context window.

### 3.3 Relevance Scoring & Chunk-Based Retrieval

Cursor scores each code chunk by:
- **Semantic similarity** to the current query
- **Recency** — recently edited files get a boost
- **Structural proximity** — files in the same directory or import chain score higher
- **Frequency** — files referenced multiple times in conversation score higher

### 3.4 File Prioritization

Cursor maintains a priority queue of files:
1. **Currently open files** — always included
2. **Recently edited files** — strong boost
3. **Import-chain files** — files that import or are imported by the current file
4. **Semantically similar files** — from the vector index

---

## 4. Anthropic "Context Engineering" Framework

Anthropic's framework organizes context management into four operations:

### 4.1 Write

Adding information to the context window:
- System prompts
- Tool schemas and results
- Retrieved documents (RAG)
- User instructions
- Injected metadata (git status, project structure)

### 4.2 Select

Choosing what to include given limited budget:
- Relevance scoring (semantic similarity)
- Recency weighting
- Priority tiers (system > pinned > recent conversation > old conversation)
- Budget allocation per component

### 4.3 Compress

Reducing token count while preserving information:
- Summarization (LLM-generated summaries of old turns)
- Truncation (drop oldest messages)
- Deduplication (remove repeated tool results)
- Token-aware trimming (shorten large code blocks to signatures)

### 4.4 Isolate

Separating concerns into independent contexts:
- Sub-agent context windows (each agent has its own)
- Tool-specific context (tool results don't pollute conversation)
- Pinned vs. ephemeral context (pinned survives compaction)

---

## 5. Tiered Memory Architecture

### 5.1 Episodic Memory (Recent Turns)

Short-term memory of the current conversation. Stored as raw message turns. Subject to compaction when context fills up. Typically the last 5–20 turns are kept verbatim.

### 5.2 Semantic Memory (Knowledge Base)

Long-term knowledge extracted from past sessions:
- Project structure and conventions
- Key decisions and their rationale
- Error patterns and solutions
- User preferences and coding style

Stored as embeddings in a vector database. Retrieved via similarity search when relevant to the current query.

### 5.3 Procedural Memory (Steps & Workflows)

Learned sequences of actions:
- Build/test/deploy workflows
- Common refactoring patterns
- Project-specific tool configurations

Stored as structured recipes that can be replayed or adapted.

---

## 6. RAG Hybrid: Vector + Graph for Code Intelligence

### 6.1 Vector Search

Embedding-based retrieval for semantic similarity:
- Chunk code into functions/classes/blocks
- Embed each chunk using a code-aware model (e.g., OpenAI `text-embedding-3-small`)
- At query time, embed the query and find nearest neighbors
- Score by cosine similarity

### 6.2 Graph-Based Retrieval

Structural code relationships:
- Import/dependency graphs
- Call graphs (function A calls function B)
- Type hierarchies (class A extends class B)
- File co-change graphs (files that change together)

### 6.3 Hybrid Approach

Combine vector and graph results:
- Start with vector search for semantic relevance
- Expand results using the graph (add callers/callees of matched functions)
- Re-rank by combined score
- Trim to fit within the token budget

---

## 7. Best Practices

### 7.1 Position-Aware Injection

LLMs attend more strongly to the beginning and end of the context. Place high-priority content (system prompt, pinned context) at the beginning, and recent conversation at the end. Middle positions are "attention dead zones" — avoid placing critical information there.

### 7.2 TTL Decay

Assign time-to-live (TTL) values to context items:
- System prompt: infinite TTL
- Pinned context: infinite TTL (until unpinned)
- Recent turns: high TTL (kept verbatim)
- Old turns: decaying TTL (summarized, then dropped)
- Tool results: low TTL (summarized aggressively)

### 7.3 Sliding Window + Summarization

The most effective compaction strategy combines:
1. **Fixed window** — always keep the last N turns verbatim
2. **Rolling summary** — summarize everything before the window into a single message
3. **Pinned anchors** — user-pinned content is always preserved verbatim

This gives the LLM recent context for continuity while compressing historical context into a high-information-density summary.

### 7.4 Token Estimation

Fast token estimation without a tokenizer:
- English text: ~4 characters per token
- Code: ~3.5 characters per token (more symbols)
- JSON/structured data: ~3 characters per token

For precise counts, use `tiktoken` (Python) or `tiktoken-rs` (Rust). For budgeting, the character-based estimate is sufficient with a 10% safety margin.

### 7.5 Compaction Triggers

- **Threshold-based** — compact when usage exceeds X% (e.g., 85%)
- **Turn-based** — compact every N turns
- **Size-based** — compact when total tokens exceed a fixed limit
- **Hybrid** — combine threshold + minimum-age (don't compact conversations shorter than M turns)

---

## 8. Implications for Caduceus

### 8.1 Design Decisions

1. **Performance zones** — Adopt the 5-zone model (Green/Yellow/Orange/Red/Critical) with configurable thresholds
2. **Default strategy** — Use Hybrid compaction (summarize old + keep recent verbatim)
3. **Auto-compact threshold** — Default to 85% (Red zone) with user-configurable override
4. **Pinned context** — Support user-pinned items that survive compaction
5. **PreCompact hooks** — Fire `CompactionStart`/`CompactionEnd` events through the existing hook system
6. **RAG integration** — When in Yellow+ zones, use `caduceus-omniscience` vector search to inject only relevant chunks instead of whole files
7. **Visualization** — Provide both CLI (`/context`) and GUI (React component) views

### 8.2 Token Budget Allocation

Default budget allocation for a 200K context window:

| Component | Budget | Tokens |
|-----------|--------|--------|
| System prompt | 5% | 10,000 |
| Tool schemas | 5% | 10,000 |
| Project context | 10% | 20,000 |
| Pinned context | 5% | 10,000 |
| Conversation | 60% | 120,000 |
| Tool results | 15% | 30,000 |

### 8.3 Integration Points

- **caduceus-orchestrator** — Context manager, `/context` command, compaction logic
- **caduceus-permissions** — `CompactionStart`/`CompactionEnd` hook events (already exists)
- **caduceus-omniscience** — Context-aware retrieval for budget-constrained injection
- **caduceus-providers** — Token counting from `Message` types
- **React UI** — `ContextViewer.tsx` with zone indicator and compaction controls

---

*References: Claude Code source analysis, Copilot CLI documentation, Cursor technical blog, Anthropic "Building Effective Agents" (2024), LangChain context management patterns.*
