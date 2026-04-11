# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |
| < 0.1   | :x:                |

## Reporting a Vulnerability

We take security seriously. If you discover a security vulnerability in Caduceus, please report it responsibly.

### How to Report

1. **Do NOT open a public GitHub issue** for security vulnerabilities.
2. **Email**: Send a detailed report to the repository owner via [GitHub Security Advisories](https://github.com/Alex-Keagel/caduceus/security/advisories/new).
3. **Include**:
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact
   - Suggested fix (if any)

### What to Expect

- **Acknowledgement**: Within 48 hours of your report.
- **Assessment**: We will evaluate the severity and impact within 7 days.
- **Fix timeline**: Critical vulnerabilities will be patched within 14 days. Lower severity issues will be addressed in the next release cycle.
- **Credit**: We will credit reporters in the release notes (unless you prefer anonymity).

### Scope

The following are in scope for security reports:

| Area | Examples |
|------|----------|
| **Sandbox escape** | Breaking out of BashSandbox, ContainerSandbox, or E2B isolation |
| **Path traversal** | Accessing files outside workspace boundaries via tools |
| **Secret leakage** | API keys, tokens, or credentials exposed in logs, outputs, or telemetry |
| **Command injection** | Crafted inputs that execute arbitrary commands |
| **Prompt injection** | Inputs that override system prompts or bypass permissions |
| **SSRF** | Server-side request forgery via provider adapters or WebSearch |
| **Privilege escalation** | Bypassing PrivilegeRings or PolicyEngine controls |
| **MCP tool poisoning** | Malicious MCP server definitions that bypass security scanner |

### Out of Scope

- Vulnerabilities in upstream dependencies (report to the upstream project)
- Issues requiring physical access to the machine
- Social engineering attacks
- Denial of service via expected resource consumption

### Built-in Security Controls

Caduceus includes multiple layers of security:

- **BashValidator** — Multi-stage command validation (dangerous patterns, sudo detection)
- **SecretScanner** — Regex-based detection of leaked credentials in outputs
- **KillSwitch** — Emergency stop for all running agents
- **PolicyEngine** — YAML-based rules evaluated before every tool call
- **PrivilegeRings** — 4-tier execution privilege model
- **McpSecurityScanner** — Typosquatting and hidden instruction detection
- **CircuitBreaker** — Auto-disable failing tools/providers
- **TrustScorer** — Agent trust scoring based on behavior history
- **OwaspChecker** — Coverage for OWASP Agentic Security Top 10
