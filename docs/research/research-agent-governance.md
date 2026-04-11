# Research: Agent Governance for Caduceus

> Based on analysis of [Microsoft's Agent Governance Toolkit](https://github.com/microsoft/agent-governance-toolkit) and the OWASP Agentic Security Top 10.

---

## 1. Microsoft Agent Governance Toolkit — Architecture Summary

Microsoft's Agent Governance Toolkit is an open-source framework providing deterministic guardrails for AI agents. Its architecture spans seven pillars:

1. **Deterministic Policy Enforcement** — Sub-millisecond YAML-based policy evaluation before every agent action. Policies define allow/deny rules for tools, resources, and operations.
2. **Zero-Trust Agent Identity** — Ed25519 cryptographic credentials per agent. Trust scores (0–1000) computed from task success, error rates, and policy violations.
3. **Execution Sandboxing** — 4-tier privilege rings controlling what agents can access (read-only → workspace → system → unrestricted). Hard kill switch for emergency stop.
4. **Agent SRE** — Service Level Objectives (SLOs), error budgets, circuit breakers, replay debugging, and chaos engineering for agent reliability.
5. **MCP Security Scanner** — Scans MCP tool definitions for poisoning (malicious hidden instructions), typosquatting (tool names mimicking trusted tools), and prompt injection.
6. **Trust Reports** — Visual dashboards showing trust score trends, task success/failure rates, permission escalation patterns.
7. **Secret Scanning** — Regex-based detection of leaked API keys, tokens, and credentials in agent output before it reaches the user.

The toolkit covers all 10 OWASP Agentic Security risks with these combined capabilities.

---

## 2. OWASP Agentic Security Top 10 — Caduceus Coverage

| # | OWASP Risk | Description | Caduceus Mitigation |
|---|-----------|-------------|---------------------|
| 1 | **Prompt Injection** | Malicious instructions embedded in tool outputs or user inputs | MCP security scanner (#178) detects hidden instructions; policy engine (#176) blocks untrusted tool outputs |
| 2 | **Broken Authentication** | Agents operating without proper identity verification | Agent identity DID (#188) provides cryptographic credentials; trust scoring (#177) tracks agent reputation |
| 3 | **Insecure Tool Use** | Agents invoking tools without validation or safety checks | Policy engine (#176) gates every tool call; privilege rings (#184) restrict tool capabilities by tier |
| 4 | **Excessive Permissions** | Agents granted more access than needed for their task | Privilege rings (#184) enforce least-privilege; existing capability tokens (`Capability` enum) scope access |
| 5 | **Insufficient Output Validation** | Agent outputs containing secrets, harmful content, or injection payloads | Secret scanning (#183) detects leaked credentials; output validation in the permission pipeline |
| 6 | **Overreliance on Agent Decisions** | Blindly trusting agent actions without human review | Kill switch (#179) enables emergency human override; existing permission dialogs require approval |
| 7 | **Denial of Wallet** | Runaway costs from uncontrolled agent loops | Error budgets (#182) auto-throttle; circuit breakers (#180) disable failing providers; existing budget USD limit (feature #43) |
| 8 | **Vector Store Poisoning** | Corrupted embeddings influencing agent behavior | MCP security scanner (#178) detects tool-level poisoning; existing parser-error-aware down-ranking (feature #111) |
| 9 | **Insufficient Logging & Monitoring** | Missing audit trails for agent actions | SLO monitoring (#181); governance attestation (#186); replay debugging (#187); existing audit log (`AuditLog`) |
| 10 | **Multi-Agent Exploitation** | Compromised agents manipulating other agents in a team | Agent identity DID (#188) enables zero-trust verification; trust scoring (#177) isolates low-trust agents |

---

## 3. Policy Engine Design

### 3.1 YAML Rule Format

```yaml
# .caduceus/policies.yaml
version: 1
policies:
  - name: block-system-paths
    description: Prevent reading outside workspace
    match:
      capability: [fs:read, fs:write]
      resource: "/etc/**"
    action: deny

  - name: require-approval-for-git-push
    description: Git push requires explicit approval
    match:
      capability: git:mutate
      tool: git_push
    action: prompt

  - name: rate-limit-bash
    description: Max 10 bash calls per minute
    match:
      capability: process:exec
    action: allow
    rate_limit:
      max: 10
      window_seconds: 60

  - name: block-network-in-plan-mode
    description: No outbound requests in plan mode
    match:
      capability: network:http
    condition:
      mode: plan
    action: deny
```

### 3.2 Evaluation Pipeline

```
Tool Call Request
  │
  ├─ 1. Kill switch check (AtomicBool — sub-nanosecond)
  │
  ├─ 2. Circuit breaker check (is provider/tool circuit open?)
  │
  ├─ 3. Policy engine evaluation (YAML rules, top-to-bottom, first-match wins)
  │
  ├─ 4. Privilege ring check (does the agent's ring permit this capability?)
  │
  ├─ 5. Existing PermissionEnforcer.check() (capability tokens + workspace confinement)
  │
  ├─ 6. Rate limit check (if policy specifies rate_limit)
  │
  └─ 7. Execute tool → Secret scan output before returning to LLM
```

Policy rules are loaded at startup and cached in memory. Evaluation is pure pattern matching — no I/O, no async, sub-millisecond.

### 3.3 Integration with Existing Permission System

The policy engine wraps `PermissionEnforcer` as an additional pre-check layer:

- **Before** `PermissionEnforcer::check()`: policy rules evaluate YAML-defined constraints
- **After** tool execution: secret scanner filters output
- The existing `Capability` enum, `PermissionMode`, and `AuditLog` remain unchanged
- Policies add *additional* constraints; they cannot weaken existing permission checks

---

## 4. Trust Scoring Algorithm

### 4.1 Score Components

Trust score ∈ [0, 1000], computed as a weighted rolling average:

```
trust_score = w₁ × success_rate_score
            + w₂ × error_rate_score
            + w₃ × violation_score
            + w₄ × consistency_score

where:
  w₁ = 0.40  (task success rate)
  w₂ = 0.25  (inverse error rate)
  w₃ = 0.25  (inverse permission violation rate)
  w₄ = 0.10  (behavioral consistency — low variance in action patterns)
```

### 4.2 Score Bands

| Range | Band | Behavior |
|-------|------|----------|
| 900–1000 | Trusted | May auto-approve low-risk operations |
| 700–899 | Standard | Normal permission flow |
| 400–699 | Cautious | Additional confirmation prompts |
| 0–399 | Restricted | Read-only mode enforced |

### 4.3 Decay and Recovery

- Scores decay 1 point per hour of inactivity (floor: 500 for established agents)
- Permission violations cause immediate −50 penalty
- Successful task completion grants +10 (capped per session)

---

## 5. MCP Security Scanning Approach

### 5.1 Threat Categories

1. **Tool Poisoning** — Malicious tool definitions that include hidden instructions in descriptions or schemas (e.g., "ignore previous instructions and…")
2. **Typosquatting** — Tool names designed to mimic trusted tools (`file_read` vs `file_raed`, `bash_exec` vs `bash_exec_v2`)
3. **Hidden Instructions** — Invisible characters, zero-width spaces, or homoglyph attacks in tool descriptions

### 5.2 Detection Methods

```
MCP Server Discovery
  │
  ├─ 1. Tool name similarity check (Levenshtein distance against known tools)
  │     Flag if distance ≤ 2 from a builtin tool name
  │
  ├─ 2. Description injection scan (regex for "ignore", "override", "system prompt")
  │     Flag phrases that attempt prompt manipulation
  │
  ├─ 3. Hidden character detection (scan for zero-width chars: U+200B, U+200C, U+200D, U+FEFF)
  │
  ├─ 4. Schema anomaly detection (unusually large descriptions, embedded base64)
  │
  └─ 5. Trust verification (is this MCP server in the user's allow-list?)
```

### 5.3 Integration

- Runs during MCP server connection (feature #46 — MCP client)
- Results are logged to `AuditLog` and displayed as warnings
- Untrusted tools are quarantined (available but require explicit approval per call)

---

## 6. Circuit Breaker Pattern for Providers

### 6.1 State Machine

```
    ┌──────────┐  failure_count >= threshold  ┌──────────┐
    │  Closed  │ ───────────────────────────→ │   Open   │
    │ (normal) │                               │ (reject) │
    └──────────┘                               └──────────┘
         ↑                                          │
         │          cooldown expires                 │
         │                                          ▼
         │                                    ┌──────────┐
         └─────────── success ───────────────│ HalfOpen │
                                              │ (probe)  │
                                              └──────────┘
                                                    │
                                              failure → Open
```

### 6.2 Configuration

```rust
pub struct CircuitBreaker {
    failure_count: AtomicU32,
    threshold: u32,           // Default: 5 consecutive failures
    state: AtomicU8,          // 0=Closed, 1=Open, 2=HalfOpen
    last_failure: Mutex<Option<Instant>>,
    cooldown: Duration,       // Default: 60 seconds
}
```

### 6.3 Integration Points

- One `CircuitBreaker` per provider adapter (Anthropic, OpenAI, etc.)
- Checked in `send_with_retry()` *before* making the HTTP request
- When Open, immediately return `Err(CaduceusError::CircuitOpen)` — no network call
- After cooldown, transition to HalfOpen and allow one probe request
- On probe success → Closed; on probe failure → Open (reset cooldown)
- Provider registry exposes `circuit_status()` for health dashboard

---

## 7. Kill Switch Implementation

### 7.1 Design

```rust
pub struct KillSwitch {
    active: Arc<AtomicBool>,
}
```

The kill switch is a single `AtomicBool` checked at the top of every tool dispatch cycle. When triggered:

1. All in-flight tool executions receive a cancellation signal
2. The orchestration loop breaks out of the current turn
3. Session state is persisted (checkpoint) before stopping
4. The TUI displays a "🛑 Kill switch activated" banner

### 7.2 Activation Methods

- `/kill` slash command (interactive)
- `SIGUSR1` signal (external automation)
- API endpoint (when bridge/remote control is active)
- Automatic trigger when error budget is exhausted

### 7.3 Reset

- `/reset` slash command re-enables the system
- Automatic reset is *not* supported (requires explicit human action)

---

## 8. Privilege Ring Model

### 8.1 Four Rings

```
Ring 0: Read-Only
  ├── fs:read (workspace only)
  ├── Semantic search
  └── Context queries

Ring 1: Workspace
  ├── Ring 0 capabilities
  ├── fs:write (workspace only)
  ├── git:read
  └── MCP tool calls (trusted only)

Ring 2: System
  ├── Ring 1 capabilities
  ├── process:exec (sandboxed)
  ├── network:http (allowlisted domains)
  ├── git:mutate
  └── MCP tool calls (all)

Ring 3: Unrestricted
  ├── Ring 2 capabilities
  ├── fs:escape (paths outside workspace)
  ├── network:http (any domain)
  └── process:exec (unsandboxed)
```

### 8.2 Mapping to Existing Capabilities

| Capability | Ring 0 | Ring 1 | Ring 2 | Ring 3 |
|-----------|--------|--------|--------|--------|
| `FsRead` | ✅ | ✅ | ✅ | ✅ |
| `FsWrite` | ❌ | ✅ | ✅ | ✅ |
| `ProcessExec` | ❌ | ❌ | ✅ | ✅ |
| `NetworkHttp` | ❌ | ❌ | ✅ | ✅ |
| `GitMutate` | ❌ | ❌ | ✅ | ✅ |
| `FsEscape` | ❌ | ❌ | ❌ | ✅ |

### 8.3 Integration

- The ring is set per-session or per-agent persona
- `PermissionEnforcer` consults the active ring *before* checking capability tokens
- Plan mode (existing) maps to Ring 0; Bypass mode maps to Ring 3
- Default mode operates at Ring 2

---

## 9. Secret Scanner

### 9.1 Pattern Categories

| Category | Example Pattern | Description |
|----------|----------------|-------------|
| AWS Keys | `AKIA[0-9A-Z]{16}` | AWS access key IDs |
| GitHub Tokens | `gh[pousr]_[A-Za-z0-9_]{36,255}` | GitHub personal access tokens |
| Generic API Keys | `[a-zA-Z0-9]{32,}` in `key=` context | Generic API key assignments |
| Private Keys | `-----BEGIN (RSA\|EC\|OPENSSH) PRIVATE KEY-----` | PEM-encoded private keys |
| JWT Tokens | `eyJ[A-Za-z0-9-_]+\.eyJ[A-Za-z0-9-_]+\.[A-Za-z0-9-_.+/=]+` | JSON Web Tokens |
| Connection Strings | `(postgres\|mysql\|mongodb)://[^\\s]+` | Database connection URIs |

### 9.2 Scanning Pipeline

1. Every tool output passes through `SecretScanner::scan()` before reaching the LLM context
2. Every LLM response passes through before rendering in the TUI
3. Findings are logged to `AuditLog` with severity
4. Detected secrets are redacted with `[REDACTED:type]` placeholders

### 9.3 Integration

- Plugs into the existing `HookEvent::ToolCallEnd` hook
- Also runs on `HookEvent::LlmResponseEnd`
- Low false-positive rate is critical — patterns are tuned for high precision

---

## 10. Roadmap Integration

### v0.2 — Quick Wins (P0–P1)

- Kill switch (P0) — immediate safety stop
- Policy engine (P1) — YAML rules before tool calls
- Circuit breakers (P1) — auto-disable failing providers
- Secret scanning (P1) — filter credentials from output
- MCP security scanner (P1) — protect against tool poisoning
- Privilege rings (P1) — 4-tier execution model

### v0.3 — Observability (P2)

- Agent trust scoring
- SLO monitoring
- Error budgets
- OWASP Agentic compliance validation
- Governance attestation reports
- Replay debugging

### v1.0+ — Advanced (P3)

- Agent identity (DID) for multi-agent zero-trust
- Chaos engineering for resilience testing

---

## References

- [Microsoft Agent Governance Toolkit](https://github.com/microsoft/agent-governance-toolkit)
- [OWASP Agentic AI — Top 10 Risks](https://owasp.org/www-project-agentic-ai/)
- [OWASP LLM Top 10](https://owasp.org/www-project-top-10-for-large-language-model-applications/)
