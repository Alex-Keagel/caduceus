# Caduceus Instruction Management — Research Findings

## Best Practices for Agent Instruction Files (2026)

### File Types & Naming Conventions

| File | Location | Format | Purpose | Standard |
|------|----------|--------|---------|----------|
| `AGENTS.md` | repo root | Markdown | Universal agent instructions (tool-agnostic) | OpenAI/Linux Foundation standard |
| `CADUCEUS.md` | repo root | Markdown | Caduceus-specific project instructions (like CLAUDE.md) | Our convention |
| `.caduceus/agents/*.md` | project dir | YAML frontmatter + Markdown | Custom agent personas | Copilot pattern |
| `.caduceus/skills/*.md` | project dir | YAML frontmatter + Markdown | Reusable task modules | Copilot pattern |
| `.caduceus/instructions/*.md` | project dir | YAML frontmatter + Markdown | Path-specific rules (glob patterns) | Cursor pattern |
| `.caduceus/mcp.json` | project dir | JSON | MCP server configurations | VS Code/Cursor standard |
| `.caduceus/memory.md` | project dir | Markdown | Persistent learned context | Claude pattern |
| `~/.caduceus/instructions.md` | user home | Markdown | User-global defaults | Claude pattern |

### Format Decision: YAML Frontmatter + Markdown

**Winner: YAML frontmatter + Markdown body**

Research shows:
- **YAML is ~30% more token-efficient than JSON** for the same structured data
- YAML frontmatter + Markdown is the dominant pattern across Claude Code, Copilot, Cursor, Windsurf, Codex
- Better readability for human editing
- Supported by all major agent frameworks

```markdown
---
name: code-reviewer
description: Reviews code for bugs and security
tools: [read_file, grep_search]
applyTo: "**/*.rs"
triggers:
  - "review this"
  - "check for bugs"
---
You are a senior code reviewer...
```

### Priority Hierarchy (highest to lowest)

1. **User global** (`~/.caduceus/instructions.md`)
2. **Project root** (`CADUCEUS.md` or `AGENTS.md`)
3. **Path-specific** (`.caduceus/instructions/*.md` with `applyTo` globs)
4. **Active agent** (`.caduceus/agents/*.md` when selected)
5. **Active skill** (`.caduceus/skills/*.md` when triggered)
6. **MCP-discovered** (dynamic from MCP servers)
7. **Memory** (`.caduceus/memory.md` — lowest priority, auto-updated)

### MCP Server Config Format

Standard `mcpServers` format (compatible with VS Code, Cursor, Claude):

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."],
      "type": "stdio"
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" },
      "type": "stdio"
    }
  }
}
```

Naming: kebab-case, descriptive, no spaces. Keys: `command`, `args`, `type` (stdio|http|sse), `env`.

### Agent Definition Format

```markdown
---
name: test-writer
description: Writes comprehensive tests for code
tools: [read_file, write_file, bash, grep_search]
model: claude-sonnet-4-6
triggers:
  - "write tests"
  - "add coverage"
---
You are a test engineer. For each function:
1. Write happy path test
2. Write edge case tests
3. Use project's test patterns
```

### Skill Definition Format

```markdown
---
name: release
description: Prepare and ship a release
triggers:
  - "create a release"
  - "ship it"
steps:
  - Run tests
  - Update version
  - Create tag
  - Push
---
## Release Procedure
1. Verify all tests pass: `cargo test --workspace`
2. Update version in Cargo.toml
3. Create git tag: `git tag v{version}`
4. Push: `git push origin main --tags`
```

### Key Design Decisions for Caduceus

1. **Support AGENTS.md as universal standard** — read it alongside CADUCEUS.md
2. **Use YAML frontmatter** for all structured config (30% fewer tokens than JSON)
3. **JSON only for MCP config** (industry standard, tools expect it)
4. **Glob-based path scoping** from Cursor's pattern (most granular)
5. **Trigger phrases** for agent/skill discovery (from Copilot pattern)
6. **Memory as append-only markdown** (from Claude's MEMORY.md pattern)
7. **Hierarchical merge** — more specific always wins
