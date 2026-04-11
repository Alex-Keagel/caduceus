# Research: Context Eviction, Skill Evolution & Graceful Degradation

## Sources

### Papers
1. **CORAL** — "Don't Lose the Thread: Empowering Long-Horizon LLM Agents with Cognitive Resource Self-Allocation" (OpenReview, Nov 2025)
2. **Confucius Code Agent** — "Scalable Agent Scaffolding for Real-World Codebases" (arXiv, Dec 2025)
3. **AI Scientist-v2** — "Workshop-Level Automated Scientific Discovery via Agentic Tree Search" (Sakana AI, April 2025)
4. **Memory Survey** — "Memory for Autonomous LLM Agents: Mechanisms, Evaluation, and Emerging Frontiers" (arXiv, March 2026)
5. **SkillClaw** — "Let Skills Evolve Collectively with Agentic Evolver" (arXiv:2604.08377, AMAP-ML)

### Industry Sources
6. **Anthropic** — "Effective Context Engineering for AI Agents" (anthropic.com/engineering)
7. **Microsoft Agent Framework** — Compaction strategies (learn.microsoft.com)
8. **Forge** — "How We Extended LLM Conversations by 10x" (dev.to/amitksingh1490)
9. **LangChain** — "Context Engineering for Agents" (blog.langchain.com)
10. **CSA** — "Cognitive Degradation Resilience (CDR) Framework" (cloudsecurityalliance.org)

---

## Key Concepts Extracted

### 1. CORAL: Agent Self-Managed Memory (Paper #1)

**Core Insight:** Give the LLM explicit tools to manage its own context window.

**Mechanism:**
- Agent can **checkpoint** verified facts to external storage
- Agent can **purge** its own context window (context optimization)
- Agent **resumes** reasoning from clean checkpoint state
- Mathematically proven: self-managed deletion of bloated history improves success rate

**Caduceus Features Derived:**
- `ContextCheckpoint` — Agent creates verified-fact snapshots mid-conversation
- `SelfEviction` — Agent actively decides what to remove from its own context
- `CheckpointResume` — Resume from clean state with only verified facts loaded

### 2. Confucius: Scaffolding > Model Size (Paper #2)

**Core Insight:** A weaker LLM with optimized scaffolding outperforms a stronger model on a basic scaffold.

**Mechanism:**
- Orchestration layer (tool routing, context management, task decomposition) is the primary bottleneck
- Context management quality matters more than raw model intelligence
- Systematic tool routing prevents wasted tokens on wrong tools

**Caduceus Features Derived:**
- `ContextQualityScorer` — Score context density/relevance before each LLM call
- `ToolRoutingOptimizer` — Reduce tool call waste through pre-routing heuristics
- `ScaffoldBenchmark` — Measure scaffolding quality independent of model

### 3. AI Scientist-v2: Agentic Tree Search (Paper #3)

**Core Insight:** Instead of linear retry loops, branch into a tree of hypotheses with autonomous pruning.

**Mechanism:**
- Generate tree of possible approaches for abstract goals
- Test each branch, capture errors
- On failure: prune branch, reflect on error log, traverse different path
- Never asks human for help — fully autonomous error recovery

**Caduceus Features Derived:**
- `AgenticTreeSearch` — Branch/prune hypothesis tree for complex tasks
- `BranchReflection` — On error, analyze log and choose alternative path
- `AutonomousErrorRecovery` — Self-heal without human escalation

### 4. Memory Survey: Write-Manage-Read Loop (Paper #4)

**Core Insight:** The fundamental loop of agentic memory is write→manage→read, not just static retrieval.

**Mechanism:**
- **Write**: Save memories externally (scratchpads, files, vector stores)
- **Manage**: Active compression, garbage collection, relevance decay
- **Read**: Context-aware retrieval with embedding search + recency bias
- Shift from static DB retrieval to **context-resident compression** (AI compresses its own memories in real-time)
- Hard engineering reality: preventing infinite loops is unsolved

**Caduceus Features Derived:**
- `MemoryGarbageCollector` — Periodic pruning of stale/unused memories
- `RelevanceDecay` — Memories lose relevance score over time unless re-accessed
- `ContextResidentCompression` — LLM compresses its own context in-place
- `LoopCircuitBreaker` — Enhanced infinite loop detection with tree-search escape

### 5. SkillClaw: Collective Skill Evolution (Repo)

**Core Insight:** Skills shouldn't be static — they should evolve autonomously from real session data across users.

**Architecture:**
```
Client Proxy → intercepts API calls, records session artifacts
         ↓
Shared Storage (S3/OSS/local) → session trajectories pooled
         ↓
Evolve Server → 3-stage pipeline:
  1. Summarize: Extract behavioral patterns from sessions
  2. Aggregate: Cross-session pattern consolidation
  3. Execute: Create/update SKILL.md files
         ↓
Skills synced back to all agents → collective improvement
```

**Key Components:**
- `skill_manager.py` (30KB) — Local skill lifecycle (load, inject, match)
- `skill_hub.py` (20KB) — Cloud skill sync (push/pull/list-remote)
- `prm_scorer.py` — Process Reward Model for evaluating skill quality
- `session_judge.py` — LLM-based session outcome evaluation
- `claw_adapter.py` — Framework adapter for injecting skills into prompts
- Agent Evolve Server — Uses an agent with full tool access to write skill files

**Validity Assessment:** ✅ **VALID AND NOVEL**
- Backed by arXiv paper (2604.08377) with WildClawBench results
- Proven: Qwen3-Max + SkillClaw outperforms larger models without skill evolution
- Non-intrusive: users don't change behavior; learning is background
- Limitation: skill quality depends on session data quality; needs governance
- Compatible with our marketplace — extends it with autonomous evolution

**Caduceus Features Derived:**
- `SkillEvolver` — Background service that analyzes completed sessions
- `SessionTrajectoryRecorder` — Record full session artifacts for evolution
- `PatternAggregator` — Cross-session pattern detection and consolidation
- `SkillAutoGenerator` — Auto-create SKILL.md from recurring patterns
- `SkillQualityScorer` — PRM-based scoring of skill effectiveness
- `CollectiveSkillSync` — Push/pull evolved skills across instances
- `SkillVersioning` — Track skill evolution history with rollback

### 6. Anthropic: Context Engineering Principles

**Key Concepts:**
- **Context rot**: As tokens increase, recall accuracy decreases (n² attention problem)
- **Attention budget**: Finite resource with diminishing returns
- **Just-in-time context**: Maintain lightweight references, load data on demand
- **Progressive disclosure**: Agents discover context layer by layer
- **Hybrid strategy**: Pre-load some data + autonomous exploration

**Caduceus Features Derived:**
- `AttentionBudgetTracker` — Track remaining attention budget, warn on degradation
- `JustInTimeContextLoader` — Replace large content with references, load on demand
- `ContextRotDetector` — Monitor for signs of context rot (repeated questions, hallucination)

### 7. Microsoft Agent Framework: Compaction Pipeline

**Key Architecture:**
```
MessageIndex → MessageGroups → CompactionTriggers → Strategies

Strategies (gentle → aggressive):
1. ToolResultCompaction — Collapse tool call groups into summaries
2. SummarizationCompaction — LLM-summarize older conversation spans
3. SlidingWindowCompaction — Keep only last N user turns
4. TruncationCompaction — Drop oldest groups (emergency backstop)

Pipeline: Chain strategies with token budget ceiling
```

**Key Concepts:**
- **Atomic MessageGroups**: Tool call + results = one unit (never split)
- **Trigger vs Target**: When to start vs when to stop compacting
- **MinimumPreserved floor**: Always keep N most recent groups
- **Token budget ceiling**: Pipeline stops when under budget

**Caduceus Features Derived:**
- `CompactionPipeline` — Ordered chain of strategies (gentle → aggressive)
- `AtomicMessageGroups` — Never split tool call/result pairs
- `CompactionTriggers` — Configurable triggers (token/message/turn thresholds)
- `EmergencyTruncation` — Last-resort oldest-first drop with minimum preserved

### 8. Forge: Pattern-Based Sequence Detection

**Key Insight:** Don't summarize everything — only compact specific patterns:
```
[Assistant] → [Tool Call] → [Tool Result] → [Assistant]
```

**Key Concepts:**
- **Logarithmic sampling** for token estimation (avoid counting every token)
- **Entropy analysis** of summaries to prevent lossy compaction
- **Retention window**: Always preserve N most recent messages
- **Separate model for compaction** (cheaper/faster than primary model)

**Caduceus Features Derived:**
- `PatternBasedCompaction` — Only compact matching sequence patterns
- `CompactionEntropyCheck` — Verify summaries retain sufficient information density
- `DualModelCompaction` — Use cheaper model for summarization

### 9. CSA: Cognitive Degradation Resilience (CDR)

**6 Stages of Degradation:**
1. Trigger Injection — Excessive token load, irrelevant tool calls
2. Resource Starvation — Memory/planning pushed to latency/disconnection
3. Behavioral Drift — Skips reasoning steps, hallucinates actions
4. Memory Entrenchment — Poisoned content persists in long-term memory
5. Functional Override — Compromised memory overrides role/intent
6. Systemic Collapse — Output suppression, execution loops, null responses

**Caduceus Features Derived:**
- `CognitiveDegradationDetector` — Monitor for all 6 CDR stages
- `BehavioralDriftDetector` — Detect when agent deviates from expected logic
- `MemoryEntrenchmentGuard` — Prevent poisoned data from persisting
- `DegradationCircuitBreaker` — Auto-reset agent state on stage 3+ detection

---

## Proposed New Features for Caduceus

### Context Eviction System (from CORAL + Microsoft + Forge)

| # | Feature | Description | Priority | Crate |
|---|---------|-------------|----------|-------|
| 190 | Compaction Pipeline | Ordered chain: tool-collapse → summarize → sliding-window → truncate | P1 | `caduceus-orchestrator` |
| 191 | Atomic Message Groups | Tool call + results as atomic units; never split during compaction | P1 | `caduceus-orchestrator` |
| 192 | Compaction Triggers | Token/message/turn threshold triggers with AND/OR composition | P1 | `caduceus-orchestrator` |
| 193 | Self-Eviction Tools | Agent-callable tools: `/checkpoint`, `/purge`, `/resume` | P1 | `caduceus-orchestrator` |
| 194 | Dual-Model Compaction | Use cheaper model (e.g., Haiku) for summarization pass | P2 | `caduceus-orchestrator` |
| 195 | Compaction Entropy Check | Verify summary information density before replacing original | P2 | `caduceus-orchestrator` |
| 196 | Pattern-Based Compaction | Only compact [Assistant→ToolCall→ToolResult→Assistant] sequences | P2 | `caduceus-orchestrator` |
| 197 | Emergency Truncation | Last-resort oldest-first drop with configurable minimum preserved | P1 | `caduceus-orchestrator` |

### Cognitive Health Monitoring (from CDR + CORAL)

| # | Feature | Description | Priority | Crate |
|---|---------|-------------|----------|-------|
| 198 | Context Rot Detector | Monitor recall accuracy, detect attention degradation | P1 | `caduceus-orchestrator` |
| 199 | Behavioral Drift Detector | Detect deviation from expected reasoning patterns | P2 | `caduceus-orchestrator` |
| 200 | Memory Entrenchment Guard | Prevent poisoned/hallucinated data from persisting to memory | P2 | `caduceus-storage` |
| 201 | Cognitive Degradation Circuit Breaker | Auto-reset on stage 3+ degradation detection | P1 | `caduceus-orchestrator` |
| 202 | Attention Budget Tracker | Track/display remaining effective attention capacity | P2 | `caduceus-telemetry` |

### Autonomous Error Recovery (from AI Scientist-v2)

| # | Feature | Description | Priority | Crate |
|---|---------|-------------|----------|-------|
| 203 | Agentic Tree Search | Branch/prune hypothesis tree for complex tasks | P2 | `caduceus-orchestrator` |
| 204 | Branch Reflection | On error, analyze log and choose alternative approach path | P2 | `caduceus-orchestrator` |
| 205 | Autonomous Error Recovery | Self-heal from errors without human escalation | P2 | `caduceus-orchestrator` |

### Skill Evolution (from SkillClaw)

| # | Feature | Description | Priority | Crate |
|---|---------|-------------|----------|-------|
| 206 | Session Trajectory Recorder | Record full session artifacts (tool calls, outcomes) for evolution | P2 | `caduceus-storage` |
| 207 | Skill Evolver Service | Background service: summarize → aggregate → execute skill updates | P2 | `caduceus-marketplace` |
| 208 | Pattern Aggregator | Cross-session behavioral pattern detection and consolidation | P2 | `caduceus-marketplace` |
| 209 | Skill Auto-Generator | Auto-create SKILL.md files from recurring successful patterns | P2 | `caduceus-marketplace` |
| 210 | Skill Quality Scorer (PRM) | Process Reward Model scoring for skill effectiveness | P3 | `caduceus-marketplace` |
| 211 | Collective Skill Sync | Push/pull evolved skills across Caduceus instances | P3 | `caduceus-marketplace` |
| 212 | Skill Versioning & Rollback | Track skill evolution history with safe rollback | P2 | `caduceus-marketplace` |

### Context Engineering (from Anthropic + LangChain)

| # | Feature | Description | Priority | Crate |
|---|---------|-------------|----------|-------|
| 213 | Just-In-Time Context Loader | Replace large content with lightweight references; load on demand | P1 | `caduceus-orchestrator` |
| 214 | Relevance Decay | Memory relevance scores decay over time unless re-accessed | P2 | `caduceus-storage` |
| 215 | Memory Garbage Collector | Periodic pruning of stale/unreferenced memories | P2 | `caduceus-storage` |
| 216 | Context Quality Scorer | Score context density/relevance before each LLM call | P2 | `caduceus-orchestrator` |
| 217 | Scaffold Quality Benchmark | Measure orchestration quality independent of model choice | P3 | `caduceus-telemetry` |

---

## Validity Assessment: SkillClaw

**Verdict: ✅ VALID — Novel and empirically backed**

**Strengths:**
- Peer-reviewed arXiv paper (2604.08377) with reproducible benchmarks
- WildClawBench results show measurable improvement without scaling model size
- Non-intrusive design (zero user overhead — evolution is fully background)
- Framework-agnostic (supports multiple "Claw" frameworks via adapters)
- 3-stage pipeline (Summarize→Aggregate→Execute) is clean and extensible
- PRM scorer adds objective quality measurement to evolved skills

**Weaknesses/Risks:**
- Still early (April 2026 paper, limited community adoption)
- Heavily tied to Alibaba/AMAP ecosystem (OSS storage, Chinese LLMs)
- Skill quality depends on session data quality — garbage in, garbage out
- No built-in governance for evolved skills (could propagate bad patterns)
- Requires careful privacy handling for multi-user session data

**How It Fits Caduceus:**
SkillClaw's architecture maps directly onto our existing marketplace:
- Our `caduceus-marketplace` already has skill/agent catalog and registry
- Add a `SkillEvolver` that reads from `caduceus-storage` session data
- 3-stage pipeline runs as a background service
- Evolved skills go through our existing governance (trust scoring, policy engine) before activation
- This makes Caduceus skills self-improving over time — a major differentiator
