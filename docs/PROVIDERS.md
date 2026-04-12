# Caduceus LLM Provider Guide

> How to configure each supported LLM provider, set up per-operation model routing, and choose the right model for each task.

---

## Table of Contents

1. [Provider Overview](#1-provider-overview)
2. [GitHub Copilot](#2-github-copilot)
3. [Anthropic Claude](#3-anthropic-claude)
4. [OpenAI](#4-openai)
5. [Google Gemini](#5-google-gemini)
6. [Azure OpenAI](#6-azure-openai)
7. [Ollama (Local Models)](#7-ollama-local-models)
8. [Per-Operation Model Routing](#8-per-operation-model-routing)
9. [Model Comparison Table](#9-model-comparison-table)

---

## 1. Provider Overview

Caduceus supports two adapter types:

- **AnthropicAdapter** — Native Anthropic Messages API with streaming SSE
- **OpenAICompatibleAdapter** — Covers OpenAI, Azure OpenAI, Google Gemini (via compatibility layer), Ollama, vLLM, and LM Studio

You can configure multiple providers simultaneously and route different operations to different providers (see [Per-Operation Model Routing](#8-per-operation-model-routing)).

### Provider Priority

If multiple providers are configured, Caduceus selects the active provider in this order:

1. Explicit `/model <provider>/<model>` override in the current session
2. Per-operation routing config in `.caduceus/config.toml`
3. The first provider found in this detection order: GitHub Copilot → Anthropic → OpenAI → Gemini → Azure OpenAI → Ollama

---

## 2. GitHub Copilot

**Best for:** Teams with existing Copilot Business/Enterprise subscriptions. Zero additional cost.

### Setup

```bash
# Step 1: Authenticate with GitHub CLI (if not already done)
gh auth login

# Step 2: Verify authentication
gh auth status
```

That's it. Caduceus auto-detects an active `gh` session and uses the Copilot API without any additional configuration. No API key is needed.

### Verification

Open Caduceus and check the status bar — it should show `Copilot` as the active provider.

```bash
# Or check via slash command
/config provider
```

### Notes

- Models available: `gpt-4o`, `gpt-4o-mini`, `claude-3.5-sonnet`, `claude-3-opus` (availability depends on your Copilot plan)
- Requests are subject to your GitHub Copilot rate limits and usage policies
- Copilot does not support extended thinking mode

---

## 3. Anthropic Claude

**Best for:** Complex reasoning, multi-step planning, large codebase analysis. Claude models have 200K context windows and excel at following nuanced instructions.

### Setup

#### Option A: Environment Variable (recommended)

```bash
export ANTHROPIC_API_KEY=sk-ant-api03-...
```

Add this to your shell profile (`~/.zshrc`, `~/.bashrc`, etc.) to persist across sessions.

#### Option B: Settings UI

Open Caduceus → Settings → Providers → Anthropic → paste your API key.

#### Option C: Interactive setup

```bash
/connect anthropic
```

Follow the prompts. The key is stored in your OS keychain (macOS Keychain, GNOME Keyring, etc.) — not in any config file.

### Models

```toml
# .caduceus/config.toml — example Anthropic model routing
[models]
chat = "claude-opus-4-5"           # Most capable, for complex planning
code_edit = "claude-sonnet-4-5"    # Balanced speed/quality for edits
planning = "claude-opus-4-5"       # Deep reasoning for architecture
quick = "claude-haiku-4-5"         # Fast, for simple lookups
```

### Extended Thinking

Claude Opus and Sonnet models support extended thinking (chain-of-thought reasoning):

```bash
/config thinking.enabled true
/config thinking.budget_tokens 10000
```

Use extended thinking for architecture decisions and complex debugging tasks. Disable it for simple code edits to save tokens and time.

### Getting an API Key

1. Go to [console.anthropic.com](https://console.anthropic.com)
2. Create a new API key under API Keys → Create Key
3. Copy the key (it's shown only once)

---

## 4. OpenAI

**Best for:** GPT-4o for general coding tasks; o1/o3 for mathematical reasoning and algorithm design.

### Setup

#### Option A: Environment Variable

```bash
export OPENAI_API_KEY=sk-proj-...
```

#### Option B: Interactive setup

```bash
/connect openai
```

#### Option C: Config file

```toml
# .caduceus/config.toml
[providers.openai]
api_key_env = "OPENAI_API_KEY"    # read from this env var
base_url = "https://api.openai.com/v1"  # default, can omit
```

### Models

```toml
[models]
chat = "gpt-4o"
code_edit = "gpt-4o"
planning = "o3"                    # o3 for deep reasoning tasks
quick = "gpt-4o-mini"
```

### Using a Custom Base URL

OpenAI-compatible APIs (vLLM, LM Studio, local OpenAI proxy) use the same adapter:

```toml
[providers.custom]
name = "my-vllm-server"
base_url = "http://localhost:8000/v1"
api_key = "not-required"           # Some servers require a placeholder
models = ["meta-llama/Llama-3.3-70B-Instruct"]
```

### Getting an API Key

1. Go to [platform.openai.com](https://platform.openai.com)
2. API keys → Create new secret key
3. Set a spending limit under Billing → Usage limits

---

## 5. Google Gemini

**Best for:** Long-context tasks (Gemini 1.5 Pro has a 1M token context window); multimodal inputs.

### Setup

#### Option A: Environment Variable

```bash
export GEMINI_API_KEY=AIza...
```

#### Option B: Interactive setup

```bash
/connect gemini
```

### Models

```toml
[models]
chat = "gemini-1.5-pro"
code_edit = "gemini-1.5-flash"     # Faster and cheaper for edits
planning = "gemini-1.5-pro"
quick = "gemini-1.5-flash"
```

### Notes

- Gemini is accessed through the OpenAI-compatible endpoint (`generativelanguage.googleapis.com/v1beta/openai`)
- Gemini 1.5 Pro's 1M token context window is useful for loading entire large codebases
- Function calling (tool use) is supported on Gemini 1.5 Pro and Flash

### Getting an API Key

1. Go to [aistudio.google.com](https://aistudio.google.com)
2. Get API key → Create API key
3. Select or create a Google Cloud project

---

## 6. Azure OpenAI

**Best for:** Enterprise environments with data residency requirements or existing Azure spend.

### Setup

Azure OpenAI requires both an API key and an endpoint URL (your Azure resource endpoint).

#### Option A: Environment Variables

```bash
export AZURE_OPENAI_API_KEY=abc123...
export AZURE_OPENAI_ENDPOINT=https://your-resource.openai.azure.com
export AZURE_OPENAI_API_VERSION=2024-02-01   # optional, defaults to latest stable
```

#### Option B: Config file

```toml
# .caduceus/config.toml
[providers.azure]
type = "azure_openai"
api_key_env = "AZURE_OPENAI_API_KEY"
endpoint = "https://your-resource.openai.azure.com"
api_version = "2024-02-01"

# Azure uses deployment names, not model names
[providers.azure.deployments]
chat = "my-gpt4o-deployment"
code_edit = "my-gpt4o-deployment"
quick = "my-gpt4o-mini-deployment"
```

#### Option C: Interactive setup

```bash
/connect azure
```

The interactive flow prompts for endpoint, API key, and deployment names.

### Deployment Names vs. Model Names

In Azure OpenAI, you deploy models under custom deployment names. The Caduceus config uses deployment names, not model names:

```toml
# Azure: use your deployment name
[providers.azure.deployments]
chat = "gpt4o-prod"               # Your deployment name

# OpenAI: use the model name directly
[models]
chat = "gpt-4o"                   # OpenAI model name
```

### Notes

- Supported model families: GPT-4o, GPT-4o mini, GPT-4, GPT-35-Turbo, o1, o3
- Streaming is supported
- Tool calling is supported on GPT-4o and later models
- Data stays within your Azure region

---

## 7. Ollama (Local Models)

**Best for:** Offline use, air-gapped environments, privacy-sensitive codebases, cost-free experimentation.

### Setup

#### Step 1: Install Ollama

```bash
# macOS
brew install ollama

# Linux
curl -fsSL https://ollama.com/install.sh | sh

# Windows
# Download from https://ollama.com/download
```

#### Step 2: Pull a Model

```bash
# General purpose — good balance of speed and quality
ollama pull llama3.3

# Code-optimized models
ollama pull deepseek-coder-v2:16b
ollama pull qwen2.5-coder:14b

# Fast small models for quick tasks
ollama pull llama3.2:3b

# Embedding model for local indexing
ollama pull nomic-embed-text
```

#### Step 3: Start Ollama (if not auto-started)

```bash
ollama serve
# Ollama runs at http://localhost:11434 by default
```

#### Step 4: Configure Caduceus

Caduceus auto-detects a running Ollama instance. Verify:

```bash
/connect ollama         # Interactive setup if not auto-detected
```

Or configure manually:

```toml
# .caduceus/config.toml
[providers.ollama]
base_url = "http://localhost:11434"   # default, can omit
# No API key needed

[models]
chat = "ollama/llama3.3"
code_edit = "ollama/qwen2.5-coder:14b"
quick = "ollama/llama3.2:3b"

[omniscience]
embedding_model = "ollama/nomic-embed-text"
```

### Recommended Models

| Use Case | Model | Size | Notes |
|----------|-------|------|-------|
| General chat | `llama3.3` | 43GB | Best quality for general tasks |
| Code editing | `deepseek-coder-v2:16b` | 9GB | Excellent for code |
| Code editing (faster) | `qwen2.5-coder:14b` | 9GB | Fast, code-optimized |
| Fast tasks | `llama3.2:3b` | 2GB | Quick responses, limited quality |
| Embeddings | `nomic-embed-text` | 274MB | Required for local indexing |

### Hardware Requirements

| Model Size | Minimum VRAM | Notes |
|------------|-------------|-------|
| 3B params | 4GB | Runs on most modern laptops |
| 7B params | 8GB | M1/M2 MacBook Pro, RTX 3060 |
| 13–14B params | 12GB | M1/M2 MacBook Pro Max, RTX 3080 |
| 70B params | 48GB | Mac Studio M2 Ultra, A100 |

Ollama falls back to CPU if VRAM is insufficient — this is much slower but functional.

---

## 8. Per-Operation Model Routing

Different operations benefit from different models. You can route each operation type to the optimal model independently.

### Configuration

```toml
# .caduceus/config.toml

[models]
# Used for conversational chat and question answering
chat = "claude-sonnet-4-5"

# Used when the agent writes or edits code
code_edit = "claude-sonnet-4-5"

# Used for long-horizon planning and architecture tasks
planning = "claude-opus-4-5"

# Used for quick lookups, single-line answers
quick = "claude-haiku-4-5"

# Used for semantic search and indexing (local recommended)
embedding = "ollama/nomic-embed-text"

# Used for generating wiki pages and summaries
summarization = "claude-haiku-4-5"

# Used for structured output (JSON) generation
structured = "claude-sonnet-4-5"
```

### Cross-Provider Routing

You can mix providers in per-operation routing:

```toml
[models]
planning = "claude-opus-4-5"          # Anthropic for complex reasoning
code_edit = "gpt-4o"                  # OpenAI for code edits
quick = "ollama/llama3.2:3b"          # Local for fast responses
embedding = "ollama/nomic-embed-text" # Local for privacy
```

### Per-Agent Model Override

Individual agents can specify their own model (see [CUSTOMIZATION.md](CUSTOMIZATION.md)):

```markdown
---
name: security-reviewer
model: claude-opus-4-5    # This agent always uses Opus regardless of global config
---
```

### Runtime Model Switch

Switch the active model for the current session without editing config:

```bash
/model claude-opus-4-5
/model gpt-4o
/model ollama/llama3.3
```

---

## 9. Model Comparison Table

| Model | Provider | Context | Strengths | Best For | Cost |
|-------|----------|---------|-----------|----------|------|
| `claude-opus-4-5` | Anthropic | 200K | Deep reasoning, instruction following, nuance | Architecture planning, complex debugging, code review | $$$ |
| `claude-sonnet-4-5` | Anthropic | 200K | Balanced speed and quality | Most coding tasks, code edits, chat | $$ |
| `claude-haiku-4-5` | Anthropic | 200K | Fast, cheap | Quick lookups, simple edits, summarization | $ |
| `gpt-4o` | OpenAI | 128K | Strong coding, multimodal | Code generation, function calling | $$ |
| `gpt-4o-mini` | OpenAI | 128K | Fast, cheap | Quick tasks, simple questions | $ |
| `o3` | OpenAI | 200K | Mathematical reasoning, logic | Algorithm design, proof-like reasoning | $$$$ |
| `gemini-1.5-pro` | Google | 1M | Huge context, multimodal | Loading entire repos, long docs | $$ |
| `gemini-1.5-flash` | Google | 1M | Fast, huge context | Fast analysis of large files | $ |
| `llama3.3` | Ollama | 128K | Good general capability, free | Offline use, privacy-sensitive work | Free |
| `deepseek-coder-v2:16b` | Ollama | 128K | Code-optimized | Offline code editing | Free |
| `qwen2.5-coder:14b` | Ollama | 128K | Fast, code-optimized | Offline code editing | Free |
| `llama3.2:3b` | Ollama | 128K | Very fast, small | Quick tasks on limited hardware | Free |

### Recommended Configurations by Use Case

#### Maximum Quality (cost-no-object)

```toml
[models]
chat = "claude-opus-4-5"
code_edit = "claude-opus-4-5"
planning = "claude-opus-4-5"
quick = "claude-sonnet-4-5"
```

#### Balanced (recommended default)

```toml
[models]
chat = "claude-sonnet-4-5"
code_edit = "claude-sonnet-4-5"
planning = "claude-opus-4-5"
quick = "claude-haiku-4-5"
embedding = "ollama/nomic-embed-text"
```

#### Cost-Optimized

```toml
[models]
chat = "claude-haiku-4-5"
code_edit = "gpt-4o-mini"
planning = "claude-sonnet-4-5"
quick = "claude-haiku-4-5"
embedding = "ollama/nomic-embed-text"
```

#### Fully Local (privacy / offline)

```toml
[models]
chat = "ollama/llama3.3"
code_edit = "ollama/deepseek-coder-v2:16b"
planning = "ollama/llama3.3"
quick = "ollama/llama3.2:3b"
embedding = "ollama/nomic-embed-text"
```

### Token Cost Reference (approximate, subject to change)

| Provider | Input (per 1M tokens) | Output (per 1M tokens) |
|----------|----------------------|------------------------|
| Claude Opus 4.5 | $15 | $75 |
| Claude Sonnet 4.5 | $3 | $15 |
| Claude Haiku 4.5 | $0.25 | $1.25 |
| GPT-4o | $5 | $15 |
| GPT-4o mini | $0.15 | $0.60 |
| Gemini 1.5 Pro | $3.50 | $10.50 |
| Gemini 1.5 Flash | $0.075 | $0.30 |
| Ollama (local) | Free | Free |

> Prices as of 2025; check provider pricing pages for current rates.
