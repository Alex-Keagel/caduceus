use async_trait::async_trait;
use caduceus_core::{CaduceusError, ModelId, ProviderId, Result};
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[async_trait]
impl LlmAdapter for AnthropicAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
        todo!("Anthropic chat implementation")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<StreamResult> {
        todo!("Anthropic stream implementation")
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        Ok(vec![
            ModelId::new("claude-opus-4-5"),
            ModelId::new("claude-sonnet-4-5"),
            ModelId::new("claude-haiku-4-5"),
        ])
    }
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
}

#[async_trait]
impl LlmAdapter for OpenAiCompatibleAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
        todo!("OpenAI-compatible chat implementation")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<StreamResult> {
        todo!("OpenAI-compatible stream implementation")
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        todo!("OpenAI-compatible list_models implementation")
    }
}

// ── Provider registry ──────────────────────────────────────────────────────────

pub struct ProviderRegistry {
    adapters: std::collections::HashMap<String, Box<dyn LlmAdapter>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self { adapters: std::collections::HashMap::new() }
    }

    pub fn register(&mut self, adapter: Box<dyn LlmAdapter>) {
        let id = adapter.provider_id().0.clone();
        self.adapters.insert(id, adapter);
    }

    pub fn get(&self, provider_id: &ProviderId) -> Option<&dyn LlmAdapter> {
        self.adapters.get(&provider_id.0).map(|a| a.as_ref())
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let msg = Message::user("hello");
        assert_eq!(msg.role, "user");
    }
}
