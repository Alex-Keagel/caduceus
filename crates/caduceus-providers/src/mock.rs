use crate::{ChatRequest, ChatResponse, LlmAdapter, StreamChunk, StreamResult};
use async_trait::async_trait;
use caduceus_core::{CaduceusError, ModelId, ProviderId, Result};
use futures::stream;
use std::collections::VecDeque;
use std::sync::Mutex;

pub struct MockLlmAdapter {
    provider_id: ProviderId,
    scripted_responses: Mutex<VecDeque<ChatResponse>>,
    scripted_streams: Mutex<VecDeque<Vec<StreamChunk>>>,
    /// Audit C5 test support: scripted streams that can inject mid-stream errors.
    /// Takes precedence over `scripted_streams` when non-empty.
    scripted_fallible_streams: Mutex<VecDeque<Vec<Result<StreamChunk>>>>,
    /// Audit C3/T1 test support: injected chat() pre-response delay.
    /// When set, chat() awaits `tokio::time::sleep(delay)` before popping
    /// a scripted response, letting tests exercise the timeout wrapper.
    chat_delay: Mutex<Option<std::time::Duration>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl MockLlmAdapter {
    pub fn new(scripted_responses: Vec<ChatResponse>) -> Self {
        Self {
            provider_id: ProviderId::new("mock"),
            scripted_responses: Mutex::new(VecDeque::from(scripted_responses)),
            scripted_streams: Mutex::new(VecDeque::new()),
            scripted_fallible_streams: Mutex::new(VecDeque::new()),
            chat_delay: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn with_stream_chunks(mut self, scripted_streams: Vec<Vec<StreamChunk>>) -> Self {
        self.scripted_streams = Mutex::new(VecDeque::from(scripted_streams));
        self
    }

    /// Audit C5 test support: script a stream that emits N Ok chunks then an Err.
    /// The harness must surface this as an `Err`, not as a truncated `Ok(EndTurn)`.
    pub fn with_fallible_stream_chunks(
        mut self,
        scripted_streams: Vec<Vec<Result<StreamChunk>>>,
    ) -> Self {
        self.scripted_fallible_streams = Mutex::new(VecDeque::from(scripted_streams));
        self
    }

    /// Audit C3/T1 test support: delay each `chat()` by the given
    /// duration before popping a scripted response. Lets tests
    /// assert that the harness-level timeout wrapper fires.
    pub fn with_chat_delay(mut self, delay: std::time::Duration) -> Self {
        self.chat_delay = Mutex::new(Some(delay));
        self
    }

    pub fn recorded_requests(&self) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .expect("mock requests mutex poisoned")
            .clone()
    }
}

#[async_trait]
impl LlmAdapter for MockLlmAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.requests
            .lock()
            .expect("mock requests mutex poisoned")
            .push(request);

        // T1 test support: honor injected delay before responding.
        let delay = *self
            .chat_delay
            .lock()
            .expect("mock chat_delay mutex poisoned");
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }

        self.scripted_responses
            .lock()
            .expect("mock responses mutex poisoned")
            .pop_front()
            .ok_or_else(|| {
                CaduceusError::Provider("mock adapter has no scripted chat response".into())
            })
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        self.requests
            .lock()
            .expect("mock requests mutex poisoned")
            .push(request);

        // Audit C5: if a fallible stream is queued, prefer it.
        if let Some(fallible) = self
            .scripted_fallible_streams
            .lock()
            .expect("mock fallible stream mutex poisoned")
            .pop_front()
        {
            return Ok(Box::pin(stream::iter(fallible)));
        }

        let chunks = self
            .scripted_streams
            .lock()
            .expect("mock stream mutex poisoned")
            .pop_front()
            .ok_or_else(|| {
                CaduceusError::Provider("mock adapter has no scripted stream chunks".into())
            })?;

        Ok(Box::pin(stream::iter(chunks.into_iter().map(Ok))))
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        Ok(vec![ModelId::new("mock-model")])
    }
}
