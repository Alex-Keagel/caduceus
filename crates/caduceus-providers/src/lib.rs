use async_trait::async_trait;
use caduceus_core::{CaduceusError, ModelId, ProviderId, Result};
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;
use tracing::warn;

pub mod mock;

// ── Message types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: ModelId,
    pub messages: Vec<Message>,
    pub system: Option<String>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub content: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
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

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 1000;
const ANTHROPIC_VERSION: &str = "2023-06-01";

// ── Retry helper ───────────────────────────────────────────────────────────────

fn is_retryable_status(status: u16) -> bool {
    status == 429 || status == 529
}

async fn send_with_retry(
    _client: &reqwest::Client,
    build_request: impl Fn() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response> {
    let mut last_error = None;

    for attempt in 0..MAX_RETRIES {
        let resp = match build_request().send().await {
            Ok(r) => r,
            Err(e) => {
                last_error = Some(CaduceusError::Provider(format!("Network error: {}", e)));
                if attempt + 1 < MAX_RETRIES {
                    let delay_ms = INITIAL_BACKOFF_MS * 2u64.pow(attempt);
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }
                break;
            }
        };

        let status = resp.status().as_u16();

        if resp.status().is_success() {
            return Ok(resp);
        }

        if is_retryable_status(status) && attempt + 1 < MAX_RETRIES {
            let delay_ms = INITIAL_BACKOFF_MS * 2u64.pow(attempt);
            warn!(
                "Rate limited ({}), retrying in {}ms (attempt {}/{})",
                status,
                delay_ms,
                attempt + 1,
                MAX_RETRIES
            );
            // Respect Retry-After header if present
            if let Some(retry_after) = resp.headers().get("retry-after") {
                if let Ok(secs) = retry_after.to_str().unwrap_or("").parse::<u64>() {
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    continue;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
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
                    }))
                }
                _ => None,
            }
        }
        "message_delta" => {
            let val: serde_json::Value = serde_json::from_str(data).ok()?;
            let output_tokens = val["usage"]["output_tokens"]
                .as_u64()
                .map(|n| n as u32);
            Some(Ok(StreamChunk {
                delta: String::new(),
                is_final: false,
                input_tokens: None,
                output_tokens,
            }))
        }
        "message_stop" => Some(Ok(StreamChunk {
            delta: String::new(),
            is_final: true,
            input_tokens: None,
            output_tokens: None,
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
                serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": request.model.0,
            "max_tokens": request.max_tokens,
            "messages": messages,
            "stream": stream,
        });

        // System prompt is a top-level field for Anthropic
        if let Some(ref system) = request.system {
            body["system"] = serde_json::Value::String(system.clone());
        } else {
            let system_content: String = request
                .messages
                .iter()
                .filter(|m| m.role == "system")
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !system_content.is_empty() {
                body["system"] = serde_json::Value::String(system_content);
            }
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        body
    }
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

        let resp = send_with_retry(&client, || {
            client
                .post(&url)
                .header("x-api-key", &api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&body)
        })
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

        let resp = send_with_retry(&client, || {
            client
                .post(&url)
                .header("x-api-key", &api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&body)
        })
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_anthropic_sse_event(&event.event, &event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!(
                        "SSE error: {:?}",
                        e
                    )))),
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

    let (input_tokens, output_tokens) = resp
        .usage
        .map(|u| (u.prompt_tokens, u.completion_tokens))
        .unwrap_or((0, 0));

    Ok(ChatResponse {
        content,
        input_tokens,
        output_tokens,
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
        }));
    }

    let chunk: OpenAiStreamChunkWire = serde_json::from_str(trimmed).ok()?;
    let choice = chunk.choices.first()?;

    let is_final = choice.finish_reason.is_some();
    let delta = choice.delta.content.clone().unwrap_or_default();

    let (input_tokens, output_tokens) = chunk
        .usage
        .map(|u| (Some(u.prompt_tokens), Some(u.completion_tokens)))
        .unwrap_or((None, None));

    // Skip empty non-final chunks with no usage info
    if delta.is_empty() && !is_final && input_tokens.is_none() {
        return None;
    }

    Some(Ok(StreamChunk {
        delta,
        is_final,
        input_tokens,
        output_tokens,
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
        let mut messages: Vec<serde_json::Value> = Vec::new();

        // For OpenAI, system prompt is a message in the array
        if let Some(ref system) = request.system {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }

        for msg in &request.messages {
            messages.push(serde_json::json!({
                "role": msg.role,
                "content": msg.content,
            }));
        }

        let mut body = serde_json::json!({
            "model": request.model.0,
            "messages": messages,
            "max_tokens": request.max_tokens,
            "stream": stream,
        });

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if stream {
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }

        body
    }
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

        let resp = send_with_retry(&client, || {
            let mut req = client
                .post(&url)
                .header("content-type", "application/json")
                .json(&body);
            if !api_key.is_empty() {
                req = req.header("authorization", format!("Bearer {}", &api_key));
            }
            req
        })
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

        let resp = send_with_retry(&client, || {
            let mut req = client
                .post(&url)
                .header("content-type", "application/json")
                .json(&body);
            if !api_key.is_empty() {
                req = req.header("authorization", format!("Bearer {}", &api_key));
            }
            req
        })
        .await?;

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|result| async move {
                match result {
                    Ok(event) => parse_openai_sse_event(&event.data),
                    Err(e) => Some(Err(CaduceusError::Provider(format!(
                        "SSE error: {:?}",
                        e
                    )))),
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
            return Ok(Vec::new());
        }

        let body = resp.text().await.map_err(|e| {
            CaduceusError::Provider(format!("Failed to read models response: {}", e))
        })?;

        let models: OpenAiModelsResponse = serde_json::from_str(&body).map_err(|e| {
            CaduceusError::Provider(format!("Failed to parse models response: {}", e))
        })?;

        Ok(models.data.into_iter().map(|m| ModelId::new(m.id)).collect())
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

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(is_retryable_status(529));
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(500));
    }

    #[test]
    fn test_stop_reason_mapping() {
        assert_eq!(map_anthropic_stop_reason("end_turn"), StopReason::EndTurn);
        assert_eq!(map_anthropic_stop_reason("max_tokens"), StopReason::MaxTokens);
        assert_eq!(map_anthropic_stop_reason("stop_sequence"), StopReason::StopSequence);
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
        };

        let body = adapter.build_request_body(&request, false);
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["system"], "You are helpful.");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn test_openai_request_body_construction() {
        let adapter =
            OpenAiCompatibleAdapter::new("openai", "key", "https://api.openai.com/v1");
        let request = ChatRequest {
            model: ModelId::new("gpt-4"),
            messages: vec![Message::user("Hello")],
            system: Some("You are helpful.".into()),
            max_tokens: 1024,
            temperature: None,
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
}
