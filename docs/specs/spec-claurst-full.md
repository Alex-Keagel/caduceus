# Caduceus — Complete Behavioral Specification

> Clean-room reimplementation guide derived from the **claurst** codebase.
> Every struct field, constant, algorithm step, and enum variant is documented.

---

## Table of Contents

1. [Overview & Architecture](#1-overview--architecture)
2. [Core Query Loop](#2-core-query-loop)
3. [Multi-Provider LLM API](#3-multi-provider-llm-api)
4. [Tool System](#4-tool-system)
5. [Command System](#5-command-system)
6. [Permission Model](#6-permission-model)
7. [Configuration System](#7-configuration-system)
8. [System Prompt Assembly](#8-system-prompt-assembly)
9. [Token Budget & Compaction](#9-token-budget--compaction)
10. [Memory System](#10-memory-system)
11. [Session Storage & History](#11-session-storage--history)
12. [Buddy / Companion System](#12-buddy--companion-system)
13. [Bridge / Remote Control](#13-bridge--remote-control)
14. [MCP Client](#14-mcp-client)
15. [Plugin System](#15-plugin-system)
16. [TUI Framework](#16-tui-framework)
17. [Hook System](#17-hook-system)
18. [Voice Input](#18-voice-input)
19. [OAuth & Authentication](#19-oauth--authentication)
20. [Feature Flags & Analytics](#20-feature-flags--analytics)
21. [Appendices](#21-appendices)

---

## 1. Overview & Architecture

### 1.1 Project Identity

| Field | Value |
|-------|-------|
| **Workspace name** | `claurst` |
| **Binary name** | `claurst` |
| **Display name** | Caduceus |
| **Version** | `0.0.8` |
| **Rust edition** | 2021 |
| **License** | GPL-3.0 |
| **MSRV** | Stable (no nightly features required) |

### 1.2 Workspace Crates

The workspace consists of **12 crates**, each with a distinct responsibility:

| Crate | Path | Role |
|-------|------|------|
| `claurst-acp` | `crates/acp` | Agent Communication Protocol — inter-agent messaging |
| `claurst-api` | `crates/api` | Multi-provider LLM API abstraction layer |
| `claurst-bridge` | `crates/bridge` | Remote control via JWT-authenticated WebSocket/polling |
| `claurst-buddy` | `crates/buddy` | Companion/pet system with deterministic PRNG evolution |
| `claurst-cli` | `crates/cli` | Binary entry point, argument parsing, TUI bootstrap |
| `claurst-commands` | `crates/commands` | Slash-command registry and execution |
| `claurst-core` | `crates/core` | Shared types, config, permissions, error handling |
| `claurst-mcp` | `crates/mcp` | Model Context Protocol client implementation |
| `claurst-plugins` | `crates/plugins` | Plugin manifest, lifecycle, hooks, marketplace |
| `claurst-query` | `crates/query` | Main query loop orchestration |
| `claurst-tools` | `crates/tools` | All 36+ tool implementations |
| `claurst-tui` | `crates/tui` | Terminal UI (ratatui-based) with ~45 source files |

### 1.3 Key Dependencies

| Dependency | Purpose |
|------------|---------|
| `tokio` | Async runtime (multi-threaded) |
| `ratatui` + `crossterm` | Terminal UI rendering and input |
| `reqwest` | HTTP client for LLM API calls |
| `serde` / `serde_json` | Serialization for configs, messages, sessions |
| `tiktoken-rs` | Token counting (OpenAI-compatible BPE) |
| `jsonschema` | JSON Schema validation for tool inputs |
| `clap` | CLI argument parsing |
| `ring` / `jsonwebtoken` | Cryptographic operations, JWT for bridge |
| `eventsource-stream` | SSE parsing for streaming LLM responses |
| `uuid` | Unique identifiers for sessions, messages |
| `chrono` | Timestamps throughout the application |
| `directories` | Platform-specific config/data/cache paths |
| `syntect` | Syntax highlighting in TUI |
| `pulldown-cmark` | Markdown parsing for TUI rendering |

### 1.4 Entry Point Flow

```
main()
  -> parse CLI args (clap)
  -> load configuration (layered: defaults -> global -> project -> env -> CLI)
  -> initialize auth store
  -> check feature flags
  -> if --bridge: start bridge listener
  -> if --version: print version, exit
  -> if --resume <session_id>: load session
  -> initialize TUI (ratatui + crossterm)
  -> enter main event loop
      -> on user input: dispatch to query loop or command handler
      -> on LLM response: render streaming tokens
      -> on tool call: execute tool, return result
  -> on exit: save session, persist config, cleanup
```

### 1.5 Directory Layout Conventions

```
~/.config/claurst/           # Global configuration
    config.toml              # User settings
    auth.json                # Authentication credentials
    memory/                  # Memory/instruction files
        AGENTS.md            # Default agent instructions
        *.md                 # Additional memory files

~/.local/share/claurst/      # Persistent data
    sessions/                # Session transcripts (JSONL)
    buddy/                   # Companion state
    plugins/                 # Installed plugins
    migrations/              # Schema migration tracking

~/.cache/claurst/            # Ephemeral cache
    models.json              # Cached model list
    features.json            # Cached feature flags

.claurst/                    # Project-local (per-repo)
    config.toml              # Project-specific settings
    memory/                  # Project-specific instructions
    AGENTS.md                # Project-specific agent file
    hooks.toml               # Project-specific hooks
    plugins.toml             # Project-specific plugin config
```

---

## 2. Core Query Loop

### 2.1 Overview

The query loop is the central orchestration mechanism. It sends user messages to the LLM,
processes streaming responses, handles tool calls, and manages the conversation lifecycle.
The loop is implemented in `claurst-query` and operates as a state machine.

### 2.2 QueryConfig

```rust
struct QueryConfig {
    /// Maximum number of tool-call rounds before forcing termination
    max_tool_rounds: usize,          // default: 50
    /// Whether to auto-compact when token budget is exceeded
    auto_compact: bool,              // default: true
    /// Effort level controlling response quality vs speed
    effort: EffortLevel,             // default: EffortLevel::Normal
    /// Whether to stream tokens or wait for complete response
    streaming: bool,                 // default: true
    /// Model to use for this query (can override session default)
    model_override: Option<String>,
    /// System prompt prefix for this query
    system_prompt_prefix: Option<SystemPromptPrefix>,
    /// Output style hint
    output_style: OutputStyle,       // default: OutputStyle::Normal
    /// Whether to include memory files
    include_memory: bool,            // default: true
    /// Temperature override
    temperature: Option<f32>,
    /// Max output tokens override
    max_tokens: Option<u32>,
}
```

### 2.3 EffortLevel

```rust
enum EffortLevel {
    Min,     // Minimal processing — fewer tokens, skip optional context
    Low,     // Reduced processing — shorter system prompt, fewer memory inclusions
    Normal,  // Standard processing
    High,    // Enhanced processing — richer context, more thorough
    Max,     // Maximum processing — full context, all memory, detailed instructions
}
```

**Effort level effects:**

| Level | System prompt | Memory files | Max tokens | Token budget | Auto-compact |
|-------|--------------|--------------|------------|--------------|--------------|
| Min | Minimal (core only) | None | 1024 | 50% of model max | Aggressive |
| Low | Reduced | Top-priority only | 2048 | 70% of model max | Normal |
| Normal | Full | All matching | Model default | 85% of model max | Normal |
| High | Full + extended | All + related | Model default x 1.5 | 90% of model max | Delayed |
| Max | Full + extended + examples | All files | Model max | 95% of model max | Disabled |

### 2.4 QueryOutcome

```rust
enum QueryOutcome {
    Success {
        response: String,
        tool_calls_made: usize,
        tokens_used: TokenUsage,
        cost: CostEstimate,
    },
    Cancelled,
    TokenBudgetExhausted,
    MaxToolRoundsReached {
        rounds_completed: usize,
        last_response: String,
    },
    Error(QueryError),
    Compacted,
}
```

### 2.5 QueryEvent

Events emitted during query execution for TUI rendering:

```rust
enum QueryEvent {
    Started { query_id: String },
    Token(String),
    ToolCallStarted {
        tool_name: String,
        tool_id: String,
        arguments: serde_json::Value,
    },
    ToolCallCompleted {
        tool_id: String,
        result: ToolResult,
        duration: Duration,
    },
    TokenWarning {
        level: TokenWarningLevel,
        used: usize,
        budget: usize,
    },
    Completed(QueryOutcome),
    CostUpdate(CostEstimate),
    EffortAdjusted {
        from: EffortLevel,
        to: EffortLevel,
        reason: String,
    },
}
```

### 2.6 The run_query_loop Algorithm

This is the most critical algorithm in the system:

```
function run_query_loop(messages, config, context) -> QueryOutcome:
    round = 0
    accumulated_cost = CostEstimate::zero()

    loop:
        // Step 1: Check token budget
        total_tokens = count_tokens(messages)
        budget = calculate_budget(config.effort, context.model)

        if total_tokens > budget:
            if config.auto_compact:
                messages = compact(messages, budget)
                if count_tokens(messages) > budget:
                    return QueryOutcome::TokenBudgetExhausted
            else:
                return QueryOutcome::TokenBudgetExhausted

        // Step 2: Assemble system prompt
        system_prompt = assemble_system_prompt(
            config.effort, config.output_style, config.system_prompt_prefix,
            config.include_memory, context,
        )

        // Step 3: Build API request
        request = LlmRequest {
            model: config.model_override.unwrap_or(context.default_model),
            messages: [SystemMessage(system_prompt)] + messages,
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            tools: get_available_tools(context),
            stream: config.streaming,
        }

        // Step 4: Send request and collect response
        emit(QueryEvent::Started { query_id })
        response = match context.provider.send(request).await {
            Ok(stream) => collect_streaming_response(stream, |token| {
                emit(QueryEvent::Token(token))
            }).await,
            Err(e) => return QueryOutcome::Error(e.into()),
        }

        // Step 5: Update cost tracking
        accumulated_cost += response.cost
        emit(QueryEvent::CostUpdate(accumulated_cost))

        // Step 6: Check for tool calls
        if response.tool_calls.is_empty():
            messages.push(AssistantMessage(response.text, response.tool_calls))
            return QueryOutcome::Success {
                response: response.text,
                tool_calls_made: round,
                tokens_used: response.usage,
                cost: accumulated_cost,
            }

        // Step 7: Process tool calls
        messages.push(AssistantMessage(response.text, response.tool_calls))
        for tool_call in response.tool_calls:
            emit(QueryEvent::ToolCallStarted { ... })

            // 7a: Permission check
            permission = check_permission(tool_call, context)
            if permission == Denied:
                result = ToolResult::error("Permission denied by user")
                messages.push(ToolMessage(tool_call.id, result))
                continue

            // 7b: Hook check (pre-tool)
            hook_outcome = run_hooks(HookEvent::PreTool, tool_call, context)
            if hook_outcome == Block:
                result = ToolResult::error("Blocked by hook")
                messages.push(ToolMessage(tool_call.id, result))
                continue

            // 7c: Execute tool
            start = Instant::now()
            result = execute_tool(tool_call.name, tool_call.arguments, context).await
            duration = start.elapsed()

            // 7d: Hook check (post-tool)
            run_hooks(HookEvent::PostTool, tool_call, result, context)

            // 7e: Record result
            messages.push(ToolMessage(tool_call.id, result))
            emit(QueryEvent::ToolCallCompleted { tool_id: tool_call.id, result, duration })

        // Step 8: Increment round counter
        round += 1
        if round >= config.max_tool_rounds:
            return QueryOutcome::MaxToolRoundsReached {
                rounds_completed: round,
                last_response: response.text,
            }

        // Step 9: Check for cancellation
        if context.cancellation_token.is_cancelled():
            return QueryOutcome::Cancelled

        // Step 10: Loop back to Step 1 with updated messages
```

### 2.7 Token Counting

Token counting uses `tiktoken-rs` with the `cl100k_base` encoding (GPT-4 compatible):

```rust
fn count_tokens(text: &str) -> usize {
    let bpe = tiktoken_rs::cl100k_base().unwrap();
    bpe.encode_with_special_tokens(text).len()
}

fn count_message_tokens(messages: &[Message]) -> usize {
    let mut total = 0;
    for msg in messages {
        total += 3; // every message has <|start|>role<|end|> overhead
        total += count_tokens(&msg.role);
        total += count_tokens(&msg.content);
        if let Some(name) = &msg.name {
            total += count_tokens(name) + 1;
        }
        if let Some(tool_calls) = &msg.tool_calls {
            for tc in tool_calls {
                total += count_tokens(&tc.name);
                total += count_tokens(&tc.arguments_json);
                total += 3; // tool call overhead
            }
        }
    }
    total += 3; // reply priming
    total
}
```

### 2.8 Streaming Response Collection

```rust
async fn collect_streaming_response(
    stream: impl Stream<Item = Result<StreamChunk, ApiError>>,
    on_token: impl Fn(String),
) -> LlmResponse {
    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut usage = TokenUsage::default();

    pin_mut!(stream);
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(StreamChunk::Text(t)) => {
                text.push_str(&t);
                on_token(t);
            }
            Ok(StreamChunk::ToolCallStart { id, name }) => {
                tool_calls.push(ToolCall { id, name, arguments_json: String::new() });
            }
            Ok(StreamChunk::ToolCallDelta { index, arguments }) => {
                if let Some(tc) = tool_calls.get_mut(index) {
                    tc.arguments_json.push_str(&arguments);
                }
            }
            Ok(StreamChunk::Usage(u)) => { usage = u; }
            Ok(StreamChunk::Done) => break,
            Err(e) => {
                log::warn!("Stream error: {}", e);
                break;
            }
        }
    }

    LlmResponse { text, tool_calls, usage, cost: calculate_cost(usage) }
}
```

---

## 3. Multi-Provider LLM API

### 3.1 Provider Trait

```rust
#[async_trait]
trait LlmProvider: Send + Sync {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ApiError>;
    async fn send(
        &self,
        request: &LlmRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, ApiError>> + Send>>, ApiError>;
    fn supports_tools(&self) -> bool;
    fn supports_streaming(&self) -> bool;
    fn supports_vision(&self) -> bool;
    fn model_token_limit(&self, model: &str) -> Option<usize>;
    fn token_cost(&self, model: &str) -> Option<TokenCost>;
}
```

### 3.2 ModelInfo

```rust
struct ModelInfo {
    id: String,
    display_name: String,
    context_window: usize,
    max_output_tokens: Option<usize>,
    supports_tools: bool,
    supports_vision: bool,
    supports_streaming: bool,
    input_cost_per_million: Option<f64>,
    output_cost_per_million: Option<f64>,
    provider_id: String,
}
```

### 3.3 LlmRequest

```rust
struct LlmRequest {
    model: String,
    messages: Vec<Message>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    top_p: Option<f32>,
    tools: Option<Vec<ToolDefinition>>,
    tool_choice: Option<ToolChoice>,
    stream: bool,
    stop: Option<Vec<String>>,
    response_format: Option<ResponseFormat>,
}
```

### 3.4 Message Types

```rust
enum Message {
    System { content: String, cache_control: Option<CacheControl> },
    User { content: MessageContent },
    Assistant { content: Option<String>, tool_calls: Vec<ToolCall> },
    Tool { tool_call_id: String, content: String },
}

enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

enum ContentPart {
    Text { text: String },
    Image { source: ImageSource, detail: Option<ImageDetail> },
}

enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

enum ImageDetail { Low, High, Auto }

enum CacheControl { Ephemeral }
```

### 3.5 StreamChunk

```rust
enum StreamChunk {
    Text(String),
    ToolCallStart { id: String, name: String },
    ToolCallDelta { index: usize, arguments: String },
    Usage(TokenUsage),
    Done,
}
```

### 3.6 TokenUsage and Cost

```rust
struct TokenUsage {
    input_tokens: usize,
    output_tokens: usize,
    cache_read_tokens: Option<usize>,
    cache_write_tokens: Option<usize>,
}

struct TokenCost {
    input_cost_per_million: f64,
    output_cost_per_million: f64,
    cache_read_cost_per_million: Option<f64>,
    cache_write_cost_per_million: Option<f64>,
}

struct CostEstimate {
    input_cost: f64,
    output_cost: f64,
    cache_read_cost: f64,
    cache_write_cost: f64,
    total: f64,
    currency: String, // always "USD"
}

impl CostEstimate {
    fn zero() -> Self { /* all fields 0.0, currency "USD" */ }

    fn calculate(usage: &TokenUsage, cost: &TokenCost) -> Self {
        let input_cost = usage.input_tokens as f64 * cost.input_cost_per_million / 1_000_000.0;
        let output_cost = usage.output_tokens as f64 * cost.output_cost_per_million / 1_000_000.0;
        let cache_read_cost = usage.cache_read_tokens.unwrap_or(0) as f64
            * cost.cache_read_cost_per_million.unwrap_or(0.0) / 1_000_000.0;
        let cache_write_cost = usage.cache_write_tokens.unwrap_or(0) as f64
            * cost.cache_write_cost_per_million.unwrap_or(0.0) / 1_000_000.0;
        Self {
            input_cost, output_cost, cache_read_cost, cache_write_cost,
            total: input_cost + output_cost + cache_read_cost + cache_write_cost,
            currency: "USD".to_string(),
        }
    }
}
```

### 3.7 Provider Implementations

The system supports **30+ providers** through a unified interface.

#### 3.7.1 Provider Registry

```rust
struct ProviderRegistry {
    providers: HashMap<String, Box<dyn LlmProvider>>,
}

impl ProviderRegistry {
    fn register(&mut self, provider: Box<dyn LlmProvider>) {
        self.providers.insert(provider.id().to_string(), provider);
    }

    fn get(&self, id: &str) -> Option<&dyn LlmProvider> {
        self.providers.get(id).map(|p| p.as_ref())
    }

    fn resolve_model(&self, model_string: &str) -> Option<(String, String)> {
        // Format: "provider:model" or just "model"
        if let Some((provider, model)) = model_string.split_once(':') {
            Some((provider.to_string(), model.to_string()))
        } else {
            for (pid, provider) in &self.providers {
                if provider.model_token_limit(model_string).is_some() {
                    return Some((pid.clone(), model_string.to_string()));
                }
            }
            None
        }
    }
}
```

#### 3.7.2 Provider List

| Provider ID | Display Name | API Base | Auth | Notes |
|-------------|-------------|----------|------|-------|
| `anthropic` | Anthropic | `https://api.anthropic.com/v1` | x-api-key header | Native tool use, caching |
| `openai` | OpenAI | `https://api.openai.com/v1` | Bearer token | Function calling, vision |
| `azure-openai` | Azure OpenAI | Custom per-deployment | API key or Entra ID | Deployment-based routing |
| `google` | Google AI | `https://generativelanguage.googleapis.com/v1beta` | API key param | Gemini |
| `vertex` | Vertex AI | `https://{region}-aiplatform.googleapis.com/v1` | OAuth2 SA | Google Cloud |
| `aws-bedrock` | AWS Bedrock | Regional endpoints | AWS Sig v4 | Model IDs differ |
| `mistral` | Mistral AI | `https://api.mistral.ai/v1` | Bearer | |
| `cohere` | Cohere | `https://api.cohere.com/v2` | Bearer | |
| `groq` | Groq | `https://api.groq.com/openai/v1` | Bearer | OpenAI-compat |
| `together` | Together AI | `https://api.together.xyz/v1` | Bearer | OpenAI-compat |
| `fireworks` | Fireworks AI | `https://api.fireworks.ai/inference/v1` | Bearer | OpenAI-compat |
| `perplexity` | Perplexity | `https://api.perplexity.ai` | Bearer | Search-augmented |
| `deepseek` | DeepSeek | `https://api.deepseek.com/v1` | Bearer | OpenAI-compat |
| `ollama` | Ollama | `http://localhost:11434/api` | None | Local |
| `lmstudio` | LM Studio | `http://localhost:1234/v1` | None | OpenAI-compat |
| `openrouter` | OpenRouter | `https://openrouter.ai/api/v1` | Bearer | Multi-router |
| `xai` | xAI | `https://api.x.ai/v1` | Bearer | Grok |
| `sambanova` | SambaNova | `https://api.sambanova.ai/v1` | Bearer | |
| `cerebras` | Cerebras | `https://api.cerebras.ai/v1` | Bearer | |
| `ai21` | AI21 | `https://api.ai21.com/studio/v1` | Bearer | Jamba |
| `replicate` | Replicate | `https://api.replicate.com/v1` | Bearer | Async predictions |
| `cloudflare` | Cloudflare AI | `https://api.cloudflare.com/client/v4/accounts/{id}/ai` | Bearer | Workers AI |
| `huggingface` | Hugging Face | `https://api-inference.huggingface.co/models` | Bearer | Inference API |
| `anyscale` | Anyscale | `https://api.endpoints.anyscale.com/v1` | Bearer | OpenAI-compat |
| `databricks` | Databricks | Custom per-workspace | Bearer | |
| `nvidia` | NVIDIA NIM | `https://integrate.api.nvidia.com/v1` | Bearer | OpenAI-compat |
| `lepton` | Lepton AI | `https://api.lepton.ai/v1` | Bearer | |
| `moonshot` | Moonshot AI | `https://api.moonshot.cn/v1` | Bearer | |
| `zhipu` | Zhipu AI | `https://open.bigmodel.cn/api/paas/v4` | Bearer | GLM |
| `minimax` | MiniMax | `https://api.minimax.chat/v1` | Bearer | |
| `custom` | Custom | User-configured | User-configured | Any OpenAI-compat |

#### 3.7.3 Provider-Specific Normalizations

**Anthropic:**
- Uses `messages` API (not `completions`)
- Tool calls: `content_block_start` / `content_block_delta` events
- System message is top-level field, not in messages array
- Supports `cache_control` on messages for prompt caching
- Tool use via `tool_use` / `tool_result` content blocks
- Streaming events: `message_start`, `content_block_start`, `content_block_delta`,
  `content_block_stop`, `message_delta`, `message_stop`
- `max_tokens` is a required field

**OpenAI:**
- Uses `chat/completions` API
- Tool calls in `choices[0].delta.tool_calls` during streaming
- `tools` parameter with `type: "function"`
- Supports `parallel_tool_calls` (default: true)
- Vision via `image_url` content parts
- Streaming sentinel: `data: [DONE]`
- System messages in messages array with `role: "system"`

**Google (Gemini):**
- `generateContent` / `streamGenerateContent` endpoints
- System instruction as separate `system_instruction` field
- Tool declarations via `functionDeclarations`
- Streaming SSE: `candidates[0].content.parts`
- Role mapping: `assistant` -> `model`
- Tool results as `functionResponse` parts

**AWS Bedrock:**
- `InvokeModelWithResponseStream` API
- Model IDs: `anthropic.claude-3-5-sonnet-20241022-v2:0`
- AWS Signature Version 4 auth
- Content format varies by model family
- Requires `region` and `profile`

**Ollama:**
- Local server, no auth
- `/api/chat` endpoint
- Newline-delimited JSON streaming
- Check availability via `/api/tags`
- Supports `keep_alive` parameter

### 3.8 Error Handling

```rust
enum ApiError {
    Network(reqwest::Error),
    RateLimited { retry_after: Option<Duration>, message: String },
    Unauthorized { message: String },
    ModelNotFound { model: String },
    ContextLengthExceeded { requested: usize, maximum: usize },
    ContentFiltered { message: String },
    ProviderError { provider: String, code: Option<String>, message: String },
    ParseError { message: String },
    Timeout { duration: Duration },
    ServerError { status: u16, message: String },
}
```

### 3.9 Retry Logic

```rust
struct RetryConfig {
    max_retries: usize,          // default: 3
    base_delay: Duration,        // default: 1s
    max_delay: Duration,         // default: 30s
    backoff_factor: f64,         // default: 2.0
    jitter: bool,                // default: true
}

fn should_retry(error: &ApiError, attempt: usize, config: &RetryConfig) -> Option<Duration> {
    if attempt >= config.max_retries { return None; }
    match error {
        ApiError::RateLimited { retry_after, .. } =>
            retry_after.or(Some(calculate_backoff(attempt, config))),
        ApiError::ServerError { status, .. } if *status >= 500 =>
            Some(calculate_backoff(attempt, config)),
        ApiError::Network(_) => Some(calculate_backoff(attempt, config)),
        ApiError::Timeout { .. } => Some(calculate_backoff(attempt, config)),
        _ => None,
    }
}

fn calculate_backoff(attempt: usize, config: &RetryConfig) -> Duration {
    let delay = config.base_delay.as_secs_f64() * config.backoff_factor.powi(attempt as i32);
    let delay = delay.min(config.max_delay.as_secs_f64());
    if config.jitter {
        Duration::from_secs_f64(delay + rand::random::<f64>() * delay * 0.5)
    } else {
        Duration::from_secs_f64(delay)
    }
}
```

