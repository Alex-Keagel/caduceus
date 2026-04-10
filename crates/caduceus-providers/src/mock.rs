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
    requests: Mutex<Vec<ChatRequest>>,
}

impl MockLlmAdapter {
    pub fn new(scripted_responses: Vec<ChatResponse>) -> Self {
        Self {
            provider_id: ProviderId::new("mock"),
            scripted_responses: Mutex::new(VecDeque::from(scripted_responses)),
            scripted_streams: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn with_stream_chunks(mut self, scripted_streams: Vec<Vec<StreamChunk>>) -> Self {
        self.scripted_streams = Mutex::new(VecDeque::from(scripted_streams));
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
