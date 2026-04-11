use async_trait::async_trait;
use caduceus_core::{AuthStore, CaduceusError, ModelId, ProviderId, Result};
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tracing::warn;

pub mod mock;

// ── Message types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_blocks: Option<Vec<MessageContentBlock>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: String,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self {
            kind: "ephemeral".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContentBlock {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            role: "user".into(),
            content: content.clone(),
            content_blocks: Some(vec![MessageContentBlock::text(content)]),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            role: "assistant".into(),
            content: content.clone(),
            content_blocks: Some(vec![MessageContentBlock::text(content)]),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            role: "system".into(),
            content: content.clone(),
            content_blocks: Some(vec![MessageContentBlock::text(content)]),
        }
    }

    pub fn with_content_blocks(mut self, blocks: Vec<MessageContentBlock>) -> Self {
        self.content = blocks
            .iter()
            .map(MessageContentBlock::text_value)
            .collect::<Vec<_>>()
            .join("");
        self.content_blocks = Some(blocks);
        self
    }

    pub fn content_blocks(&self) -> Vec<MessageContentBlock> {
        self.content_blocks
            .clone()
            .unwrap_or_else(|| vec![MessageContentBlock::text(self.content.clone())])
    }

    pub fn content_text(&self) -> String {
        self.content_blocks()
            .iter()
            .map(MessageContentBlock::text_value)
            .collect::<Vec<_>>()
            .join("")
    }
}

impl MessageContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            cache_control: None,
        }
    }

    pub fn text_with_cache(text: impl Into<String>, cache_control: CacheControl) -> Self {
        Self::Text {
            text: text.into(),
            cache_control: Some(cache_control),
        }
    }

    fn text_value(&self) -> String {
        match self {
            Self::Text { text, .. } => text.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: ModelId,
    pub messages: Vec<Message>,
    pub system: Option<String>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    /// When true, prepend "Think step by step" to system prompt and use higher max_tokens.
    #[serde(default)]
    pub thinking_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub content: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub delta: String,
    pub is_final: bool,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_read_tokens: Option<u32>,
    pub cache_creation_tokens: Option<u32>,
}

pub type StreamResult = Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>;

// ── LlmAdapter trait ───────────────────────────────────────────────────────────

#[async_trait]
pub trait LlmAdapter: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;
    async fn stream(&self, request: ChatRequest) -> Result<StreamResult>;
    async fn list_models(&self) -> Result<Vec<ModelId>>;
}

// ── Constants ──────────────────────────────────────────────────────────────────

const ANTHROPIC_VERSION: &str = "2023-06-01";

// ── Retry configuration ───────────────────────────────────────────────────────

/// Configuration for retry-with-jitter behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_retries: usize,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
        }
    }
}

impl RetryConfig {
    /// Compute delay with exponential backoff and jitter:
    /// `delay = min(base_delay * 2^attempt + random(0..base_delay), max_delay)`
    pub fn delay_for_attempt(&self, attempt: usize) -> std::time::Duration {
        use rand::Rng;
        let shift = (attempt as u32).min(63);
        let exp_delay = self.base_delay_ms.saturating_mul(1u64 << shift);
        let jitter = rand::thread_rng().gen_range(0..=self.base_delay_ms);
        let total = exp_delay.saturating_add(jitter).min(self.max_delay_ms);
        std::time::Duration::from_millis(total)
    }
}

// ── Retry helper ──────────────────────────────────────────────────────────────

fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504 | 529)
}

async fn send_with_retry(
    _client: &reqwest::Client,
    build_request: impl Fn() -> reqwest::RequestBuilder,
    retry_config: &RetryConfig,
) -> Result<reqwest::Response> {
    let mut last_error = None;

    for attempt in 0..retry_config.max_retries {
        let resp = match build_request().send().await {
            Ok(r) => r,
            Err(e) => {
                last_error = Some(CaduceusError::Provider(format!("Network error: {}", e)));
                if attempt + 1 < retry_config.max_retries {
                    let delay = retry_config.delay_for_attempt(attempt);
                    tokio::time::sleep(delay).await;
                    continue;
                }
                break;
            }
        };

        let status = resp.status().as_u16();

        if resp.status().is_success() {
            return Ok(resp);
        }

        if is_retryable_status(status) && attempt + 1 < retry_config.max_retries {
            let delay = retry_config.delay_for_attempt(attempt);
            warn!(
                "Retryable status ({}), retrying in {:?} (attempt {}/{})",
                status,
                delay,
                attempt + 1,
                retry_config.max_retries
            );
            // Respect Retry-After header if present
            if let Some(retry_after) = resp.headers().get("retry-after") {
                if let Ok(secs) = retry_after.to_str().unwrap_or("").parse::<u64>() {
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    continue;
                }
            }
            tokio::time::sleep(delay).await;
            continue;
        }

        let body = resp.text().await.unwrap_or_default();

        if status == 401 || status == 403 {
            return Err(CaduceusError::Provider(format!(
                "Authentication failed ({}): {}",
                status, body
            )));
        }

        if is_retryable_status(status) {
            return Err(CaduceusError::RateLimited {
                retry_after_secs: 60,
            });
        }

        return Err(CaduceusError::Provider(format!(
            "API error ({}): {}",
            status, body
        )));
    }

    Err(last_error.unwrap_or_else(|| CaduceusError::Provider("Max retries exhausted".into())))
}

// ── Anthropic wire types ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[allow(dead_code)]
        id: String,
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        input: serde_json::Value,
    },
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

fn map_anthropic_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    }
}

fn parse_anthropic_chat_response(body: &str) -> Result<ChatResponse> {
    let resp: AnthropicResponse = serde_json::from_str(body).map_err(|e| {
        CaduceusError::Provider(format!(
            "Failed to parse Anthropic response: {} (body: {})",
            e,
            &body[..body.len().min(200)]
        ))
    })?;

    let content = resp
        .content
        .iter()
        .filter_map(|block| match block {
            AnthropicContentBlock::Text { text } => Some(text.as_str()),
            AnthropicContentBlock::ToolUse { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let stop_reason = resp
        .stop_reason
        .as_deref()
        .map(map_anthropic_stop_reason)
        .unwrap_or(StopReason::EndTurn);

    Ok(ChatResponse {
        content,
        input_tokens: resp.usage.input_tokens,
        output_tokens: resp.usage.output_tokens,
        cache_read_tokens: resp.usage.cache_read_input_tokens,
        cache_creation_tokens: resp.usage.cache_creation_input_tokens,
        stop_reason,
    })
}

fn parse_anthropic_sse_event(event_type: &str, data: &str) -> Option<Result<StreamChunk>> {
    match event_type {
        "message_start" => {
            let val: serde_json::Value = serde_json::from_str(data).ok()?;
            let input_tokens = val["message"]["usage"]["input_tokens"]
                .as_u64()
                .map(|n| n as u32);
            Some(Ok(StreamChunk {
                delta: String::new(),
                is_final: false,
                input_tokens,
                output_tokens: None,
                cache_read_tokens: val["message"]["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .map(|n| n as u32),
                cache_creation_tokens: val["message"]["usage"]["cache_creation_input_tokens"]
                    .as_u64()
                    .map(|n| n as u32),
            }))
        }
        "content_block_delta" => {
            let val: serde_json::Value = serde_json::from_str(data).ok()?;
            let delta_type = val["delta"]["type"].as_str().unwrap_or("");
            match delta_type {
                "text_delta" => {
                    let text = val["delta"]["text"].as_str().unwrap_or("").to_string();
                    if text.is_empty() {
                        return None;
                    }
                    Some(Ok(StreamChunk {
                        delta: text,
                        is_final: false,
                        input_tokens: None,
                        output_tokens: None,
                        cache_read_tokens: None,
                        cache_creation_tokens: None,
                    }))
                }
                _ => None,
            }
        }
        "message_delta" => {
            let val: serde_json::Value = serde_json::from_str(data).ok()?;
            let output_tokens = val["usage"]["output_tokens"].as_u64().map(|n| n as u32);
            Some(Ok(StreamChunk {
                delta: String::new(),
                is_final: false,
                input_tokens: None,
                output_tokens,
                cache_read_tokens: val["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .map(|n| n as u32),
                cache_creation_tokens: val["usage"]["cache_creation_input_tokens"]
                    .as_u64()
                    .map(|n| n as u32),
            }))
        }
        "message_stop" => Some(Ok(StreamChunk {
            delta: String::new(),
            is_final: true,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        })),
        _ => None,
    }
}

// ── Anthropic adapter ──────────────────────────────────────────────────────────

pub struct AnthropicAdapter {
    provider_id: ProviderId,
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl AnthropicAdapter {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("anthropic"),
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com/v1".into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    fn build_request_body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| {
                let content_blocks = anthropic_content_blocks(&m.content_blocks());
                serde_json::json!({
                    "role": m.role,
                    "content": content_blocks,
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": request.model.0,
            "max_tokens": request.max_tokens,
            "messages": messages,
            "stream": stream,
        });

        let mut system_blocks = Vec::new();
        if let Some(ref system) = request.system {
            system_blocks.push(MessageContentBlock::text_with_cache(
                system.clone(),
                CacheControl::ephemeral(),
            ));
        }
        for message in request.messages.iter().filter(|m| m.role == "system") {
            for block in message.content_blocks() {
                system_blocks.push(match block {
                    MessageContentBlock::Text { text, .. } => {
                        MessageContentBlock::text_with_cache(text, CacheControl::ephemeral())
                    }
                });
            }
        }
        if !system_blocks.is_empty() {
            body["system"] = serde_json::Value::Array(anthropic_content_blocks(&system_blocks));
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        body
    }
}

fn anthropic_content_blocks(blocks: &[MessageContentBlock]) -> Vec<serde_json::Value> {
    blocks
        .iter()
        .map(|block| match block {
            MessageContentBlock::Text {
                text,
                cache_control,
            } => {
                let mut value = serde_json::json!({
                    "type": "text",
                    "text": text,
                });
                if let Some(cache_control) = cache_control {
                    value["cache_control"] = serde_json::json!({
                        "type": cache_control.kind,
                    });
                }
                value
            }
        })
        .collect()
}

#[async_trait]
impl LlmAdapter for AnthropicAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.build_request_body(&request, false);
        let url = format!("{}/messages", self.base_url);
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json")
                    .json(&body)
            },
            &retry,
        )
        .await?;

        let resp_body = resp
            .text()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to read response: {}", e)))?;

        parse_anthropic_chat_response(&resp_body)
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        let body = self.build_request_body(&request, true);
        let url = format!("{}/messages", self.base_url);
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json")
                    .json(&body)
            },
            &retry,
        )
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_anthropic_sse_event(&event.event, &event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!("SSE error: {:?}", e)))),
                }
            });

        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        Ok(vec![
            ModelId::new("claude-opus-4-5"),
            ModelId::new("claude-sonnet-4-5"),
            ModelId::new("claude-haiku-4-5"),
        ])
    }
}

// ── OpenAI wire types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: Option<OpenAiMessage>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct OpenAiPromptTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChunkWire {
    choices: Vec<OpenAiStreamChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiDelta {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModelInfo>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelInfo {
    id: String,
}

fn map_openai_finish_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    }
}

fn parse_openai_chat_response(body: &str) -> Result<ChatResponse> {
    let resp: OpenAiResponse = serde_json::from_str(body).map_err(|e| {
        CaduceusError::Provider(format!(
            "Failed to parse OpenAI response: {} (body: {})",
            e,
            &body[..body.len().min(200)]
        ))
    })?;

    let choice = resp
        .choices
        .first()
        .ok_or_else(|| CaduceusError::Provider("No choices in response".into()))?;

    let content = choice
        .message
        .as_ref()
        .and_then(|m| m.content.as_ref())
        .cloned()
        .unwrap_or_default();

    let stop_reason = choice
        .finish_reason
        .as_deref()
        .map(map_openai_finish_reason)
        .unwrap_or(StopReason::EndTurn);

    let (input_tokens, output_tokens, cache_read_tokens) = resp
        .usage
        .map(|u| {
            (
                u.prompt_tokens,
                u.completion_tokens,
                u.prompt_tokens_details
                    .map(|details| details.cached_tokens)
                    .unwrap_or_default(),
            )
        })
        .unwrap_or((0, 0, 0));

    Ok(ChatResponse {
        content,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens: 0,
        stop_reason,
    })
}

fn parse_openai_sse_event(data: &str) -> Option<Result<StreamChunk>> {
    let trimmed = data.trim();
    if trimmed == "[DONE]" {
        return Some(Ok(StreamChunk {
            delta: String::new(),
            is_final: true,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        }));
    }

    let chunk: OpenAiStreamChunkWire = serde_json::from_str(trimmed).ok()?;
    let choice = chunk.choices.first()?;

    let is_final = choice.finish_reason.is_some();
    let delta = choice.delta.content.clone().unwrap_or_default();

    let (input_tokens, output_tokens, cache_read_tokens) = chunk
        .usage
        .map(|u| {
            (
                Some(u.prompt_tokens),
                Some(u.completion_tokens),
                Some(
                    u.prompt_tokens_details
                        .map(|details| details.cached_tokens)
                        .unwrap_or_default(),
                ),
            )
        })
        .unwrap_or((None, None, None));

    // Skip empty non-final chunks with no usage info
    if delta.is_empty() && !is_final && input_tokens.is_none() {
        return None;
    }

    Some(Ok(StreamChunk {
        delta,
        is_final,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens: Some(0),
    }))
}

// ── OpenAI-compatible adapter ──────────────────────────────────────────────────

pub struct OpenAiCompatibleAdapter {
    provider_id: ProviderId,
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl OpenAiCompatibleAdapter {
    pub fn new(
        provider_id: impl Into<String>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: ProviderId::new(provider_id),
            api_key: api_key.into(),
            base_url: base_url.into(),
            client: reqwest::Client::new(),
        }
    }

    fn build_request_body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        build_openai_request_body(request, stream, true)
    }
}

fn build_openai_request_body(
    request: &ChatRequest,
    stream: bool,
    include_model: bool,
) -> serde_json::Value {
    let mut messages: Vec<serde_json::Value> = Vec::new();

    if let Some(ref system) = request.system {
        messages.push(serde_json::json!({
            "role": "system",
            "content": system,
        }));
    }

    for msg in &request.messages {
        messages.push(serde_json::json!({
            "role": msg.role,
            "content": msg.content_text(),
        }));
    }

    let mut body = serde_json::json!({
        "messages": messages,
        "max_tokens": request.max_tokens,
        "stream": stream,
    });

    if include_model {
        body["model"] = serde_json::json!(request.model.0);
    }

    if let Some(temp) = request.temperature {
        body["temperature"] = serde_json::json!(temp);
    }

    if stream {
        body["stream_options"] = serde_json::json!({"include_usage": true});
    }

    body
}

#[async_trait]
impl LlmAdapter for OpenAiCompatibleAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.build_request_body(&request, false);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                let mut req = client
                    .post(&url)
                    .header("content-type", "application/json")
                    .json(&body);
                if !api_key.is_empty() {
                    req = req.header("authorization", format!("Bearer {}", &api_key));
                }
                req
            },
            &retry,
        )
        .await?;

        let resp_body = resp
            .text()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to read response: {}", e)))?;

        parse_openai_chat_response(&resp_body)
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        let body = self.build_request_body(&request, true);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                let mut req = client
                    .post(&url)
                    .header("content-type", "application/json")
                    .json(&body);
                if !api_key.is_empty() {
                    req = req.header("authorization", format!("Bearer {}", &api_key));
                }
                req
            },
            &retry,
        )
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_openai_sse_event(&event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!("SSE error: {:?}", e)))),
                }
            });

        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let mut req = self
            .client
            .get(&url)
            .header("content-type", "application/json");
        if !self.api_key.is_empty() {
            req = req.header("authorization", format!("Bearer {}", &self.api_key));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to list models: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CaduceusError::Provider(format!(
                "Failed to list models ({}): {}",
                status, body
            )));
        }

        let body = resp.text().await.map_err(|e| {
            CaduceusError::Provider(format!("Failed to read models response: {}", e))
        })?;

        let models: OpenAiModelsResponse = serde_json::from_str(&body).map_err(|e| {
            CaduceusError::Provider(format!("Failed to parse models response: {}", e))
        })?;

        Ok(models
            .data
            .into_iter()
            .map(|m| ModelId::new(m.id))
            .collect())
    }
}

// ── Gemini adapter ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiCandidateContent>,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidateContent {
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
struct GeminiPart {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiUsage {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: u32,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached_content_token_count: u32,
}

fn map_gemini_finish_reason(reason: &str) -> StopReason {
    match reason {
        "MAX_TOKENS" => StopReason::MaxTokens,
        "STOP" => StopReason::EndTurn,
        _ => StopReason::EndTurn,
    }
}

fn parse_gemini_chat_response(body: &str) -> Result<ChatResponse> {
    let resp: GeminiResponse = serde_json::from_str(body).map_err(|e| {
        CaduceusError::Provider(format!(
            "Failed to parse Gemini response: {} (body: {})",
            e,
            &body[..body.len().min(200)]
        ))
    })?;

    let candidate = resp.candidates.first();
    let content = candidate
        .and_then(|candidate| candidate.content.as_ref())
        .map(|content| {
            content
                .parts
                .iter()
                .filter_map(|part| part.text.as_deref())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let usage = resp.usage_metadata.unwrap_or(GeminiUsage {
        prompt_token_count: 0,
        candidates_token_count: 0,
        cached_content_token_count: 0,
    });

    Ok(ChatResponse {
        content,
        input_tokens: usage.prompt_token_count,
        output_tokens: usage.candidates_token_count,
        cache_read_tokens: usage.cached_content_token_count,
        cache_creation_tokens: 0,
        stop_reason: candidate
            .and_then(|candidate| candidate.finish_reason.as_deref())
            .map(map_gemini_finish_reason)
            .unwrap_or(StopReason::EndTurn),
    })
}

fn parse_gemini_sse_event(data: &str) -> Option<Result<StreamChunk>> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return None;
    }

    let response: GeminiResponse = serde_json::from_str(trimmed).ok()?;
    let candidate = response.candidates.first();
    let delta = candidate
        .and_then(|candidate| candidate.content.as_ref())
        .map(|content| {
            content
                .parts
                .iter()
                .filter_map(|part| part.text.as_deref())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let usage = response.usage_metadata;
    let is_final = candidate
        .and_then(|candidate| candidate.finish_reason.as_ref())
        .is_some();

    if delta.is_empty() && usage.is_none() && !is_final {
        return None;
    }

    Some(Ok(StreamChunk {
        delta,
        is_final,
        input_tokens: usage.as_ref().map(|usage| usage.prompt_token_count),
        output_tokens: usage.as_ref().map(|usage| usage.candidates_token_count),
        cache_read_tokens: usage.as_ref().map(|usage| usage.cached_content_token_count),
        cache_creation_tokens: Some(0),
    }))
}

pub struct GeminiAdapter {
    provider_id: ProviderId,
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl GeminiAdapter {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("gemini"),
            api_key: api_key.into(),
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    fn endpoint(&self, model: &ModelId, stream: bool) -> String {
        let method = if stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        let mut url = format!(
            "{}/models/{}:{}",
            self.base_url.trim_end_matches('/'),
            model.0,
            method
        );
        if stream {
            url.push_str("?alt=sse");
        }
        url
    }

    fn build_request_body(&self, request: &ChatRequest) -> serde_json::Value {
        let mut contents = Vec::new();
        for message in request
            .messages
            .iter()
            .filter(|message| message.role != "system")
        {
            let gemini_role = if message.role == "assistant" {
                "model"
            } else {
                &message.role
            };
            contents.push(serde_json::json!({
                "role": gemini_role,
                "parts": message
                    .content_blocks()
                    .iter()
                    .map(|block| match block {
                        MessageContentBlock::Text { text, .. } => serde_json::json!({ "text": text }),
                    })
                    .collect::<Vec<_>>(),
            }));
        }

        let mut body = serde_json::json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": request.max_tokens,
            }
        });

        if let Some(ref system) = request.system {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": system}],
            });
        }

        if let Some(temp) = request.temperature {
            body["generationConfig"]["temperature"] = serde_json::json!(temp);
        }

        body
    }
}

#[async_trait]
impl LlmAdapter for GeminiAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.build_request_body(&request);
        let url = self.endpoint(&request.model, false);
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry_config = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("x-goog-api-key", &api_key)
                    .json(&body)
            },
            &retry_config,
        )
        .await?;

        let resp_body = resp
            .text()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to read response: {}", e)))?;
        parse_gemini_chat_response(&resp_body)
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        let body = self.build_request_body(&request);
        let url = self.endpoint(&request.model, true);
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry_config = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("x-goog-api-key", &api_key)
                    .json(&body)
            },
            &retry_config,
        )
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_gemini_sse_event(&event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!("SSE error: {:?}", e)))),
                }
            });
        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        Ok(vec![
            ModelId::new("gemini-1.5-flash"),
            ModelId::new("gemini-1.5-pro"),
            ModelId::new("gemini-2.0-flash"),
        ])
    }
}

// ── Azure OpenAI adapter ─────────────────────────────────────────────────────────

pub struct AzureOpenAiAdapter {
    provider_id: ProviderId,
    resource: String,
    deployment: String,
    api_key: String,
    api_version: String,
    base_url: Option<String>,
    client: reqwest::Client,
}

impl AzureOpenAiAdapter {
    pub fn new(
        resource: impl Into<String>,
        deployment: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: ProviderId::new("azure"),
            resource: resource.into(),
            deployment: deployment.into(),
            api_key: api_key.into(),
            api_version: "2024-02-01".into(),
            base_url: None,
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    fn endpoint(&self) -> String {
        let root = self
            .base_url
            .clone()
            .unwrap_or_else(|| format!("https://{}.openai.azure.com", self.resource));
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            root.trim_end_matches('/'),
            self.deployment,
            self.api_version
        )
    }

    fn build_request_body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        build_openai_request_body(request, stream, false)
    }
}

#[async_trait]
impl LlmAdapter for AzureOpenAiAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.build_request_body(&request, false);
        let url = self.endpoint();
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry_config = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("api-key", &api_key)
                    .json(&body)
            },
            &retry_config,
        )
        .await?;

        let resp_body = resp
            .text()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to read response: {}", e)))?;
        parse_openai_chat_response(&resp_body)
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        let body = self.build_request_body(&request, true);
        let url = self.endpoint();
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let retry_config = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("api-key", &api_key)
                    .json(&body)
            },
            &retry_config,
        )
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_openai_sse_event(&event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!("SSE error: {:?}", e)))),
                }
            });

        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        Ok(vec![ModelId::new(self.deployment.clone())])
    }
}

// ── Provider connector ───────────────────────────────────────────────────────────

#[async_trait]
pub trait ApiKeyPrompter: Send + Sync {
    async fn prompt_api_key(&self, provider_id: &ProviderId) -> Result<String>;
}

#[derive(Debug, Clone, Default)]
pub struct ProviderConnectionConfig {
    pub base_url: Option<String>,
    pub model: Option<ModelId>,
    pub azure_resource: Option<String>,
    pub azure_deployment: Option<String>,
}

pub struct ProviderConnector<S, P> {
    auth_store: Arc<S>,
    prompter: Arc<P>,
    configs: HashMap<String, ProviderConnectionConfig>,
}

impl<S, P> ProviderConnector<S, P>
where
    S: AuthStore,
    P: ApiKeyPrompter,
{
    pub fn new(auth_store: Arc<S>, prompter: Arc<P>) -> Self {
        Self {
            auth_store,
            prompter,
            configs: HashMap::new(),
        }
    }

    pub fn with_provider_config(
        mut self,
        provider_id: impl Into<String>,
        config: ProviderConnectionConfig,
    ) -> Self {
        self.configs.insert(provider_id.into(), config);
        self
    }

    pub async fn connect(&self, provider_id: &ProviderId) -> Result<()> {
        let key = self.prompter.prompt_api_key(provider_id).await?;
        self.validate_key(provider_id, &key).await?;
        self.auth_store.set_api_key(provider_id, &key).await
    }

    pub async fn validate_key(&self, provider_id: &ProviderId, key: &str) -> Result<()> {
        let config = self
            .configs
            .get(&provider_id.0)
            .cloned()
            .unwrap_or_default();
        let request = ChatRequest {
            model: config
                .model
                .unwrap_or_else(|| default_validation_model(provider_id)),
            messages: vec![Message::user("ping")],
            system: Some("Reply with pong.".into()),
            max_tokens: 8,
            temperature: Some(0.0),
            thinking_mode: false,
        };

        match provider_id.0.as_str() {
            "anthropic" => {
                let mut adapter = AnthropicAdapter::new(key);
                if let Some(base_url) = config.base_url {
                    adapter = adapter.with_base_url(base_url);
                }
                adapter.chat(request).await.map(|_| ())
            }
            "openai" | "ollama" => {
                let base_url = config
                    .base_url
                    .unwrap_or_else(|| default_openai_base_url(provider_id));
                OpenAiCompatibleAdapter::new(provider_id.0.clone(), key, base_url)
                    .chat(request)
                    .await
                    .map(|_| ())
            }
            "gemini" => {
                let mut adapter = GeminiAdapter::new(key);
                if let Some(base_url) = config.base_url {
                    adapter = adapter.with_base_url(base_url);
                }
                adapter.chat(request).await.map(|_| ())
            }
            "azure" => {
                let resource = config.azure_resource.ok_or_else(|| {
                    CaduceusError::Provider("missing Azure resource for connector".into())
                })?;
                let deployment = config.azure_deployment.ok_or_else(|| {
                    CaduceusError::Provider("missing Azure deployment for connector".into())
                })?;
                let mut adapter = AzureOpenAiAdapter::new(resource, deployment, key);
                if let Some(base_url) = config.base_url {
                    adapter = adapter.with_base_url(base_url);
                }
                adapter.chat(request).await.map(|_| ())
            }
            "copilot" => {
                let mut adapter = CopilotLmAdapter::new(key);
                if let Some(base_url) = config.base_url {
                    adapter = adapter.with_base_url(base_url);
                }
                adapter.chat(request).await.map(|_| ())
            }
            other => Err(CaduceusError::Provider(format!(
                "unsupported provider for connection: {other}"
            ))),
        }
    }
}

fn default_validation_model(provider_id: &ProviderId) -> ModelId {
    match provider_id.0.as_str() {
        "anthropic" => ModelId::new("claude-haiku-4-5"),
        "openai" => ModelId::new("gpt-4o-mini"),
        "gemini" => ModelId::new("gemini-1.5-flash"),
        "azure" => ModelId::new("azure-deployment"),
        "ollama" => ModelId::new("llama3.2"),
        "copilot" => ModelId::new("gpt-4o-mini"),
        _ => ModelId::new("default"),
    }
}

fn default_openai_base_url(provider_id: &ProviderId) -> String {
    match provider_id.0.as_str() {
        "ollama" => "http://127.0.0.1:11434/v1".into(),
        _ => "https://api.openai.com/v1".into(),
    }
}

// ── Provider registry ──────────────────────────────────────────────────────────

pub struct ProviderRegistry {
    adapters: HashMap<String, Box<dyn LlmAdapter>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
        }
    }

    pub fn register(&mut self, adapter: Box<dyn LlmAdapter>) {
        let id = adapter.provider_id().0.clone();
        self.adapters.insert(id, adapter);
    }

    pub fn get(&self, provider_id: &ProviderId) -> Option<&dyn LlmAdapter> {
        self.adapters.get(&provider_id.0).map(|a| a.as_ref())
    }

    pub fn list_providers(&self) -> Vec<&ProviderId> {
        self.adapters.values().map(|a| a.provider_id()).collect()
    }

    /// Resolve "provider:model" strings into (ProviderId, ModelId) pairs.
    pub fn resolve_model(&self, model_string: &str) -> Option<(ProviderId, ModelId)> {
        if let Some((provider, model)) = model_string.split_once(':') {
            if self.adapters.contains_key(provider) {
                return Some((ProviderId::new(provider), ModelId::new(model)));
            }
        }
        None
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── GitHub Copilot LM API adapter ──────────────────────────────────────────────

/// Adapter for the GitHub Copilot Language Model API.
///
/// Uses the OpenAI-compatible chat/completions format with GitHub token auth.
/// Auth: `GITHUB_TOKEN` env var as Bearer token.
/// Base URL: configurable, defaults to GitHub Copilot's local proxy endpoint.
pub struct CopilotLmAdapter {
    provider_id: ProviderId,
    token: String,
    base_url: String,
    client: reqwest::Client,
}

impl CopilotLmAdapter {
    /// Create a new adapter using the `GITHUB_TOKEN` environment variable.
    pub fn from_env() -> std::result::Result<Self, String> {
        let token = std::env::var("GITHUB_TOKEN")
            .map_err(|_| "GITHUB_TOKEN env var not set".to_string())?;
        Ok(Self::new(token))
    }

    /// Create a new adapter with an explicit token.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("copilot"),
            token: token.into(),
            base_url: "http://localhost:1234".into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn build_request_body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        build_openai_request_body(request, stream, true)
    }
}

#[async_trait]
impl LlmAdapter for CopilotLmAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.build_request_body(&request, false);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let token = self.token.clone();
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", &token))
                    .json(&body)
            },
            &retry,
        )
        .await?;

        let resp_body = resp
            .text()
            .await
            .map_err(|e| CaduceusError::Provider(format!("Failed to read response: {}", e)))?;

        parse_openai_chat_response(&resp_body)
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        let body = self.build_request_body(&request, true);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let token = self.token.clone();
        let client = self.client.clone();
        let retry = RetryConfig::default();

        let resp = send_with_retry(
            &client,
            || {
                client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", &token))
                    .json(&body)
            },
            &retry,
        )
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_openai_sse_event(&event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!("SSE error: {:?}", e)))),
                }
            });

        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        Ok(vec![
            ModelId::new("gpt-4o"),
            ModelId::new("gpt-4o-mini"),
            ModelId::new("claude-sonnet-4-5"),
        ])
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::thread;

    struct TestServer {
        base_url: String,
        _handle: thread::JoinHandle<()>,
    }

    impl TestServer {
        fn respond(status_line: &str, content_type: &str, body: &str, requests: usize) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let status = status_line.to_string();
            let content_type = content_type.to_string();
            let body = body.to_string();
            let handle = thread::spawn(move || {
                for _ in 0..requests {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut buffer = [0u8; 8192];
                    let _ = stream.read(&mut buffer);
                    let response = format!(
                        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status,
                        content_type,
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            });
            Self {
                base_url,
                _handle: handle,
            }
        }
    }

    #[derive(Default)]
    struct InMemoryAuthStore {
        keys: Mutex<HashMap<String, String>>,
    }

    #[async_trait]
    impl AuthStore for InMemoryAuthStore {
        async fn get_api_key(&self, provider_id: &ProviderId) -> Result<Option<String>> {
            Ok(self.keys.lock().unwrap().get(&provider_id.0).cloned())
        }

        async fn set_api_key(&self, provider_id: &ProviderId, key: &str) -> Result<()> {
            self.keys
                .lock()
                .unwrap()
                .insert(provider_id.0.clone(), key.to_string());
            Ok(())
        }

        async fn delete_api_key(&self, provider_id: &ProviderId) -> Result<()> {
            self.keys.lock().unwrap().remove(&provider_id.0);
            Ok(())
        }
    }

    struct StaticPrompter {
        key: String,
    }

    #[async_trait]
    impl ApiKeyPrompter for StaticPrompter {
        async fn prompt_api_key(&self, _provider_id: &ProviderId) -> Result<String> {
            Ok(self.key.clone())
        }
    }

    #[test]
    fn test_message_construction() {
        let user = Message::user("hello");
        assert_eq!(user.role, "user");
        assert_eq!(user.content, "hello");

        let asst = Message::assistant("world");
        assert_eq!(asst.role, "assistant");

        let sys = Message::system("you are helpful");
        assert_eq!(sys.role, "system");
    }

    #[test]
    fn test_provider_registry_register_and_lookup() {
        let mut registry = ProviderRegistry::new();
        assert!(registry.get(&ProviderId::new("anthropic")).is_none());
        assert!(registry.list_providers().is_empty());

        let adapter = AnthropicAdapter::new("test-key");
        registry.register(Box::new(adapter));

        assert!(registry.get(&ProviderId::new("anthropic")).is_some());
        assert_eq!(registry.list_providers().len(), 1);
        assert!(registry.get(&ProviderId::new("missing")).is_none());
    }

    #[test]
    fn test_resolve_model_with_provider_prefix() {
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(AnthropicAdapter::new("k")));

        let result = registry.resolve_model("anthropic:claude-sonnet-4-5");
        assert!(result.is_some());
        let (pid, mid) = result.unwrap();
        assert_eq!(pid.0, "anthropic");
        assert_eq!(mid.0, "claude-sonnet-4-5");

        assert!(registry.resolve_model("unknown:model").is_none());
        assert!(registry.resolve_model("claude-sonnet-4-5").is_none());
    }

    #[test]
    fn test_parse_anthropic_response_text() {
        let json = r#"{
            "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello, world!"}],
            "model": "claude-sonnet-4-5-20241022",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 25, "output_tokens": 13}
        }"#;

        let resp = parse_anthropic_chat_response(json).unwrap();
        assert_eq!(resp.content, "Hello, world!");
        assert_eq!(resp.input_tokens, 25);
        assert_eq!(resp.output_tokens, 13);
        assert_eq!(resp.cache_read_tokens, 0);
        assert_eq!(resp.cache_creation_tokens, 0);
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn test_parse_anthropic_response_tool_use() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "Running that."},
                {"type": "tool_use", "id": "toolu_01A", "name": "bash", "input": {"cmd": "ls"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 50, "output_tokens": 30}
        }"#;

        let resp = parse_anthropic_chat_response(json).unwrap();
        assert_eq!(resp.content, "Running that.");
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert_eq!(resp.input_tokens, 50);
    }

    #[test]
    fn test_parse_openai_response() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;

        let resp = parse_openai_chat_response(json).unwrap();
        assert_eq!(resp.content, "Hello!");
        assert_eq!(resp.input_tokens, 10);
        assert_eq!(resp.output_tokens, 5);
        assert_eq!(resp.cache_read_tokens, 0);
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn test_parse_anthropic_sse_events() {
        // message_start → input token count
        let chunk = parse_anthropic_sse_event(
            "message_start",
            r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chunk.input_tokens, Some(25));
        assert!(!chunk.is_final);

        // content_block_delta → text delta
        let chunk = parse_anthropic_sse_event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chunk.delta, "Hello");

        // message_delta → output token count
        let chunk = parse_anthropic_sse_event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chunk.output_tokens, Some(15));

        // message_stop → final
        let chunk = parse_anthropic_sse_event("message_stop", r#"{"type":"message_stop"}"#)
            .unwrap()
            .unwrap();
        assert!(chunk.is_final);

        // ping → ignored
        assert!(parse_anthropic_sse_event("ping", "").is_none());
    }

    #[test]
    fn test_parse_openai_sse_events() {
        // Text delta
        let chunk = parse_openai_sse_event(
            r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"Hi"},"finish_reason":null}]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chunk.delta, "Hi");
        assert!(!chunk.is_final);

        // Final chunk with usage
        let chunk = parse_openai_sse_event(
            r#"{"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .unwrap()
        .unwrap();
        assert!(chunk.is_final);
        assert_eq!(chunk.input_tokens, Some(10));
        assert_eq!(chunk.output_tokens, Some(5));

        // [DONE] sentinel
        let chunk = parse_openai_sse_event("[DONE]").unwrap().unwrap();
        assert!(chunk.is_final);
    }

    #[test]
    fn test_retryable_status_codes() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(504));
        assert!(is_retryable_status(529));
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(404));
    }

    #[test]
    fn test_stop_reason_mapping() {
        assert_eq!(map_anthropic_stop_reason("end_turn"), StopReason::EndTurn);
        assert_eq!(
            map_anthropic_stop_reason("max_tokens"),
            StopReason::MaxTokens
        );
        assert_eq!(
            map_anthropic_stop_reason("stop_sequence"),
            StopReason::StopSequence
        );
        assert_eq!(map_anthropic_stop_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(map_anthropic_stop_reason("unknown"), StopReason::EndTurn);

        assert_eq!(map_openai_finish_reason("stop"), StopReason::EndTurn);
        assert_eq!(map_openai_finish_reason("length"), StopReason::MaxTokens);
        assert_eq!(map_openai_finish_reason("tool_calls"), StopReason::ToolUse);
    }

    #[test]
    fn test_anthropic_request_body_construction() {
        let adapter = AnthropicAdapter::new("test-key");
        let request = ChatRequest {
            model: ModelId::new("claude-sonnet-4-5"),
            messages: vec![Message::user("Hello")],
            system: Some("You are helpful.".into()),
            max_tokens: 1024,
            temperature: Some(0.7),
            thinking_mode: false,
        };

        let body = adapter.build_request_body(&request, false);
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["system"][0]["text"], "You are helpful.");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn test_openai_request_body_construction() {
        let adapter = OpenAiCompatibleAdapter::new("openai", "key", "https://api.openai.com/v1");
        let request = ChatRequest {
            model: ModelId::new("gpt-4"),
            messages: vec![Message::user("Hello")],
            system: Some("You are helpful.".into()),
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
        };

        let body = adapter.build_request_body(&request, true);
        assert_eq!(body["model"], "gpt-4");
        assert_eq!(body["stream"], true);
        // System message is first in the messages array for OpenAI
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "You are helpful.");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn test_parse_malformed_response() {
        assert!(parse_anthropic_chat_response("not json").is_err());
        assert!(parse_openai_chat_response("not json").is_err());
        assert!(parse_openai_chat_response(r#"{"choices":[]}"#).is_err());
    }

    #[test]
    fn test_adapter_construction() {
        let a = AnthropicAdapter::new("key1");
        assert_eq!(a.provider_id.0, "anthropic");
        assert_eq!(a.base_url, "https://api.anthropic.com/v1");

        let a = a.with_base_url("http://localhost:8080");
        assert_eq!(a.base_url, "http://localhost:8080");

        let o = OpenAiCompatibleAdapter::new("openai", "key2", "https://api.openai.com/v1");
        assert_eq!(o.provider_id.0, "openai");
    }

    #[test]
    fn test_message_content_blocks_round_trip() {
        let message = Message::system("cache me").with_content_blocks(vec![
            MessageContentBlock::text_with_cache("cache me", CacheControl::ephemeral()),
        ]);
        assert_eq!(message.content_text(), "cache me");
        let blocks = message.content_blocks();
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            MessageContentBlock::Text {
                cache_control: Some(cache),
                ..
            } if cache.kind == "ephemeral"
        ));
    }

    #[test]
    fn test_parse_anthropic_cache_usage() {
        let json = r#"{
            "content": [{"type": "text", "text": "Cached!"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 11,
                "output_tokens": 7,
                "cache_read_input_tokens": 5,
                "cache_creation_input_tokens": 3
            }
        }"#;
        let resp = parse_anthropic_chat_response(json).unwrap();
        assert_eq!(resp.cache_read_tokens, 5);
        assert_eq!(resp.cache_creation_tokens, 3);
    }

    #[test]
    fn test_parse_gemini_response() {
        let json = r#"{
            "candidates": [{
                "content": {"parts": [{"text": "Hello from Gemini"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 12,
                "candidatesTokenCount": 4,
                "cachedContentTokenCount": 2
            }
        }"#;
        let resp = parse_gemini_chat_response(json).unwrap();
        assert_eq!(resp.content, "Hello from Gemini");
        assert_eq!(resp.input_tokens, 12);
        assert_eq!(resp.output_tokens, 4);
        assert_eq!(resp.cache_read_tokens, 2);
    }

    #[test]
    fn test_gemini_stream_sse_parsing() {
        let chunk = parse_gemini_sse_event(
            r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]}}],"usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":3,"cachedContentTokenCount":1}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chunk.delta, "Hi");
        assert_eq!(chunk.input_tokens, Some(8));
        assert_eq!(chunk.cache_read_tokens, Some(1));
    }

    #[test]
    fn test_azure_request_body_and_endpoint() {
        let adapter = AzureOpenAiAdapter::new("resource-name", "deployment-a", "key");
        let request = ChatRequest {
            model: ModelId::new("ignored"),
            messages: vec![Message::user("Hello Azure")],
            system: Some("Stay concise".into()),
            max_tokens: 128,
            temperature: Some(0.2),
            thinking_mode: false,
        };

        let body = adapter.build_request_body(&request, true);
        assert!(body.get("model").is_none());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            adapter.endpoint(),
            "https://resource-name.openai.azure.com/openai/deployments/deployment-a/chat/completions?api-version=2024-02-01"
        );
    }

    #[tokio::test]
    async fn test_provider_connector_connects_and_stores_key_for_openai() {
        let server = TestServer::respond(
            "200 OK",
            "application/json",
            r#"{"choices":[{"message":{"content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
            1,
        );
        let auth_store = Arc::new(InMemoryAuthStore::default());
        let prompter = Arc::new(StaticPrompter {
            key: "secret-key".into(),
        });
        let connector = ProviderConnector::new(auth_store.clone(), prompter).with_provider_config(
            "openai",
            ProviderConnectionConfig {
                base_url: Some(server.base_url),
                model: Some(ModelId::new("gpt-4o-mini")),
                ..Default::default()
            },
        );

        connector.connect(&ProviderId::new("openai")).await.unwrap();
        let stored = auth_store
            .get_api_key(&ProviderId::new("openai"))
            .await
            .unwrap();
        assert_eq!(stored.as_deref(), Some("secret-key"));
    }

    #[tokio::test]
    async fn test_gemini_adapter_streams_chunks() {
        let server = TestServer::respond(
            "200 OK",
            "text/event-stream",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":9,\"candidatesTokenCount\":2,\"cachedContentTokenCount\":1}}\n\n",
            1,
        );
        let adapter = GeminiAdapter::new("test-key").with_base_url(server.base_url);
        let request = ChatRequest {
            model: ModelId::new("gemini-1.5-flash"),
            messages: vec![Message::user("Hi")],
            system: None,
            max_tokens: 32,
            temperature: None,
            thinking_mode: false,
        };

        let mut stream = adapter.stream(request).await.unwrap();
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk.delta, "Hello");
        assert!(chunk.is_final);
    }

    // ── P0: RetryConfig tests ──────────────────────────────────────────────────

    #[test]
    fn test_retry_config_defaults() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_delay_ms, 1000);
        assert_eq!(config.max_delay_ms, 30_000);
    }

    #[test]
    fn test_retry_config_delay_increases_with_attempt() {
        let config = RetryConfig {
            max_retries: 5,
            base_delay_ms: 100,
            max_delay_ms: 60_000,
        };
        let d0 = config.delay_for_attempt(0);
        let d1 = config.delay_for_attempt(1);
        let d2 = config.delay_for_attempt(2);
        // Due to jitter, d1 should generally be >= d0 base, but we test the trend
        // by checking that the base delay doubles
        assert!(d0.as_millis() >= 100); // base + jitter(0..100)
        assert!(d1.as_millis() >= 200); // 2*base + jitter
        assert!(d2.as_millis() >= 400); // 4*base + jitter
    }

    #[test]
    fn test_retry_config_caps_at_max_delay() {
        let config = RetryConfig {
            max_retries: 10,
            base_delay_ms: 1000,
            max_delay_ms: 5_000,
        };
        let delay = config.delay_for_attempt(20); // Would be huge without cap
        assert!(delay.as_millis() <= 5_000);
    }

    // ── P1: Extended Thinking tests ────────────────────────────────────────────

    #[test]
    fn test_chat_request_thinking_mode_default() {
        let json = r#"{"model":"test","messages":[],"system":null,"max_tokens":100}"#;
        let req: ChatRequest = serde_json::from_str(json).unwrap();
        assert!(!req.thinking_mode);
    }

    #[test]
    fn test_chat_request_thinking_mode_enabled() {
        let req = ChatRequest {
            model: ModelId::new("test"),
            messages: vec![],
            system: Some("sys".into()),
            max_tokens: 100,
            temperature: None,
            thinking_mode: true,
        };
        assert!(req.thinking_mode);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"thinking_mode\":true"));
    }

    // ── Copilot LM adapter tests ───────────────────────────────────────────────

    #[test]
    fn test_copilot_adapter_construction() {
        let adapter = CopilotLmAdapter::new("gh-token-123");
        assert_eq!(adapter.provider_id().0, "copilot");
        assert_eq!(adapter.token(), "gh-token-123");
        assert_eq!(adapter.base_url(), "http://localhost:1234");
    }

    #[test]
    fn test_copilot_adapter_custom_base_url() {
        let adapter =
            CopilotLmAdapter::new("token").with_base_url("https://copilot.example.com/v1");
        assert_eq!(adapter.base_url(), "https://copilot.example.com/v1");
    }

    #[test]
    fn test_copilot_adapter_request_body() {
        let adapter = CopilotLmAdapter::new("token");
        let request = ChatRequest {
            model: ModelId::new("gpt-4o"),
            messages: vec![Message::user("Hello")],
            system: Some("You are helpful.".into()),
            max_tokens: 1024,
            temperature: Some(0.5),
            thinking_mode: false,
        };

        let body = adapter.build_request_body(&request, true);
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[tokio::test]
    async fn test_copilot_adapter_chat() {
        let server = TestServer::respond(
            "200 OK",
            "application/json",
            r#"{"choices":[{"message":{"content":"Hello from Copilot"},"finish_reason":"stop"}],"usage":{"prompt_tokens":15,"completion_tokens":4}}"#,
            1,
        );
        let adapter = CopilotLmAdapter::new("test-token").with_base_url(server.base_url);
        let request = ChatRequest {
            model: ModelId::new("gpt-4o"),
            messages: vec![Message::user("Hi")],
            system: None,
            max_tokens: 64,
            temperature: None,
            thinking_mode: false,
        };

        let resp = adapter.chat(request).await.unwrap();
        assert_eq!(resp.content, "Hello from Copilot");
        assert_eq!(resp.input_tokens, 15);
        assert_eq!(resp.output_tokens, 4);
    }

    #[test]
    fn test_copilot_adapter_in_registry() {
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(CopilotLmAdapter::new("token")));
        assert!(registry.get(&ProviderId::new("copilot")).is_some());
        let resolved = registry.resolve_model("copilot:gpt-4o");
        assert!(resolved.is_some());
        let (pid, mid) = resolved.unwrap();
        assert_eq!(pid.0, "copilot");
        assert_eq!(mid.0, "gpt-4o");
    }
}
