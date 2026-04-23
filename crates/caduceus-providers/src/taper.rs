//! G33 / P11.1 — `ProviderTaper`: production record/replay middleware.
//!
//! Wraps any [`LlmAdapter`] in a thin "tape" that either:
//! - **Off** — passthrough; identical to the inner provider.
//! - **Record(path)** — call inner provider, append `(request, response)`
//!   to an NDJSON file, return the live response.
//! - **Replay(path)** — read the next `(request, response)` from the
//!   tape file and return the recorded `response` without ever calling
//!   the inner provider.
//!
//! This is the production-side analogue of `caduceus-eval`'s
//! `RecordingLlmAdapter` / `ReplayingLlmAdapter`. The eval crate's
//! versions live alongside benchmark trajectories and require the full
//! eval crate's dependency surface; `ProviderTaper` is intentionally
//! minimal (single file, no `anyhow`, no async-trait macro from outside)
//! so the orchestrator can swap it in via a single builder call without
//! pulling caduceus-eval into the production binary.
//!
//! Wire format: NDJSON, one JSON object per line, schema:
//! ```json
//! {"v":1,"req":<ChatRequest>,"res":<ChatResponse>}
//! ```
//!
//! Replay matches on call order (not request equality), mirroring
//! caduceus-eval's strategy. A future variant could add request-shape
//! hashing for stricter validation.

use crate::{ChatRequest, ChatResponse, LlmAdapter, ProviderId, StreamResult};
use async_trait::async_trait;
use caduceus_core::{CaduceusError, ModelId, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// On-disk record schema. Versioned so a future format change can be
/// detected and refused rather than silently misparsed.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TapeEntry {
    v: u32,
    req: ChatRequest,
    res: ChatResponse,
}

const TAPE_SCHEMA_VERSION: u32 = 1;

/// Operating mode for [`ProviderTaper`]. A single struct per session;
/// switching modes mid-run requires constructing a new taper.
#[derive(Debug, Clone)]
pub enum TaperMode {
    /// Passthrough — identical to the inner provider.
    Off,
    /// Append every `chat` round-trip to `path` (NDJSON).
    Record { path: PathBuf },
    /// Read the tape from `path` and serve recorded responses in order.
    Replay { path: PathBuf },
}

/// Production middleware adapter. Holds an inner provider and a mode.
/// Cheap to clone (Arc-shared inner state).
pub struct ProviderTaper {
    inner: Arc<dyn LlmAdapter>,
    mode: TaperMode,
    /// Replay cursor (only used in `Replay` mode). Lazy-loaded on first
    /// `chat` so construction never does I/O.
    replay_state: Mutex<Option<ReplayState>>,
    /// File handle for `Record` mode. Open-on-construction so a bad path
    /// fails loudly at wire-up time, not on the first chat.
    record_writer: Mutex<Option<std::fs::File>>,
}

struct ReplayState {
    entries: Vec<TapeEntry>,
    cursor: usize,
}

impl std::fmt::Debug for ProviderTaper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderTaper")
            .field("mode", &self.mode)
            .finish()
    }
}

impl ProviderTaper {
    /// Construct a taper. For `Record` mode, the file is created
    /// (or truncated) immediately; for `Replay`, the file is read
    /// lazily on first `chat()`.
    pub fn new(inner: Arc<dyn LlmAdapter>, mode: TaperMode) -> Result<Self> {
        let record_writer = match &mode {
            TaperMode::Record { path } => {
                let f = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(path)
                    .map_err(|e| {
                        CaduceusError::Provider(format!(
                            "ProviderTaper: cannot open tape '{}' for write: {e}",
                            path.display()
                        ))
                    })?;
                Mutex::new(Some(f))
            }
            _ => Mutex::new(None),
        };
        Ok(Self {
            inner,
            mode,
            replay_state: Mutex::new(None),
            record_writer,
        })
    }

    fn ensure_replay_loaded(&self, path: &PathBuf) -> Result<()> {
        let mut guard = self.replay_state.lock().unwrap();
        if guard.is_some() {
            return Ok(());
        }
        let f = std::fs::File::open(path).map_err(|e| {
            CaduceusError::Provider(format!(
                "ProviderTaper: cannot open tape '{}' for read: {e}",
                path.display()
            ))
        })?;
        let reader = BufReader::new(f);
        let mut entries = Vec::new();
        for (i, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| {
                CaduceusError::Provider(format!(
                    "ProviderTaper: tape '{}' read error at line {}: {e}",
                    path.display(),
                    i + 1
                ))
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: TapeEntry = serde_json::from_str(&line).map_err(|e| {
                CaduceusError::Provider(format!(
                    "ProviderTaper: malformed tape line {} in '{}': {e}",
                    i + 1,
                    path.display()
                ))
            })?;
            if entry.v != TAPE_SCHEMA_VERSION {
                return Err(CaduceusError::Provider(format!(
                    "ProviderTaper: tape '{}' has unsupported schema v{} (this build expects v{})",
                    path.display(),
                    entry.v,
                    TAPE_SCHEMA_VERSION
                )));
            }
            entries.push(entry);
        }
        *guard = Some(ReplayState { entries, cursor: 0 });
        Ok(())
    }

    /// Diagnostic: how many entries have been written/replayed so far.
    /// Useful for tests that want to assert the full tape was consumed.
    pub fn cursor(&self) -> usize {
        self.replay_state
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.cursor)
            .unwrap_or(0)
    }
}

#[async_trait]
impl LlmAdapter for ProviderTaper {
    fn provider_id(&self) -> &ProviderId {
        self.inner.provider_id()
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        match &self.mode {
            TaperMode::Off => self.inner.chat(request).await,
            TaperMode::Record { .. } => {
                let req_clone = request.clone();
                let response = self.inner.chat(request).await?;
                let entry = TapeEntry {
                    v: TAPE_SCHEMA_VERSION,
                    req: req_clone,
                    res: response.clone(),
                };
                let line = serde_json::to_string(&entry).map_err(|e| {
                    CaduceusError::Provider(format!(
                        "ProviderTaper: cannot serialize tape entry: {e}"
                    ))
                })?;
                let mut guard = self.record_writer.lock().unwrap();
                if let Some(ref mut f) = *guard {
                    writeln!(f, "{line}").map_err(|e| {
                        CaduceusError::Provider(format!("ProviderTaper: tape write failed: {e}"))
                    })?;
                    let _ = f.flush();
                }
                Ok(response)
            }
            TaperMode::Replay { path } => {
                self.ensure_replay_loaded(path)?;
                let mut guard = self.replay_state.lock().unwrap();
                let state = guard
                    .as_mut()
                    .expect("ensure_replay_loaded populates state");
                if state.cursor >= state.entries.len() {
                    return Err(CaduceusError::Provider(format!(
                        "ProviderTaper: tape exhausted at call #{} (tape has {} entries)",
                        state.cursor + 1,
                        state.entries.len()
                    )));
                }
                let entry = state.entries[state.cursor].clone();
                state.cursor += 1;
                // Sanity: model id should match (cheap drift detector).
                if entry.req.model.0 != request.model.0 {
                    tracing::warn!(
                        target: "caduceus.provider.taper",
                        recorded = %entry.req.model.0,
                        live = %request.model.0,
                        cursor = state.cursor,
                        "tape replay model drift — replaying recorded response anyway"
                    );
                }
                Ok(entry.res)
            }
        }
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        // Streaming is not part of the deterministic replay surface
        // (matches caduceus-eval's choice). Always passthrough.
        self.inner.stream(request).await
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        self.inner.list_models().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockLlmAdapter;
    use crate::{ChatRequest, ChatResponse};
    use caduceus_core::StopReason;
    use std::sync::Arc;

    fn mk_response(text: &str) -> ChatResponse {
        ChatResponse {
            content: text.to_string(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
            thinking: String::new(),
        }
    }

    fn mk_request(model: &str, user: &str) -> ChatRequest {
        ChatRequest {
            model: ModelId::new(model),
            messages: vec![crate::Message::user(user)],
            system: None,
            max_tokens: 1024,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            tools: vec![].into(),
            response_format: None,
            logprobs: None,
        }
    }

    #[tokio::test]
    async fn off_mode_passes_through() {
        let inner = Arc::new(MockLlmAdapter::new(vec![mk_response("hi")]));
        let taper = ProviderTaper::new(inner, TaperMode::Off).unwrap();
        let resp = taper.chat(mk_request("m", "x")).await.unwrap();
        assert_eq!(resp.content, "hi");
    }

    #[tokio::test]
    async fn record_then_replay_round_trips_response() {
        let dir = tempfile::tempdir().unwrap();
        let tape = dir.path().join("tape.ndjson");

        // Record phase
        let inner = Arc::new(MockLlmAdapter::new(vec![
            mk_response("first"),
            mk_response("second"),
        ]));
        let recorder = ProviderTaper::new(inner, TaperMode::Record { path: tape.clone() }).unwrap();
        let r1 = recorder.chat(mk_request("m", "a")).await.unwrap();
        let r2 = recorder.chat(mk_request("m", "b")).await.unwrap();
        assert_eq!(r1.content, "first");
        assert_eq!(r2.content, "second");

        // Replay phase — inner is a panicking adapter to prove the tape is
        // the only source of responses.
        let panicking = Arc::new(MockLlmAdapter::new(vec![]));
        let replayer =
            ProviderTaper::new(panicking, TaperMode::Replay { path: tape.clone() }).unwrap();
        let p1 = replayer.chat(mk_request("m", "a")).await.unwrap();
        let p2 = replayer.chat(mk_request("m", "b")).await.unwrap();
        assert_eq!(p1.content, "first");
        assert_eq!(p2.content, "second");
        assert_eq!(replayer.cursor(), 2);
    }

    #[tokio::test]
    async fn replay_exhaustion_returns_diagnostic_error() {
        let dir = tempfile::tempdir().unwrap();
        let tape = dir.path().join("tape.ndjson");
        let inner = Arc::new(MockLlmAdapter::new(vec![mk_response("only")]));
        let recorder = ProviderTaper::new(inner, TaperMode::Record { path: tape.clone() }).unwrap();
        let _ = recorder.chat(mk_request("m", "x")).await.unwrap();

        let panicking = Arc::new(MockLlmAdapter::new(vec![]));
        let replayer = ProviderTaper::new(panicking, TaperMode::Replay { path: tape }).unwrap();
        let _ = replayer.chat(mk_request("m", "x")).await.unwrap();
        let err = replayer
            .chat(mk_request("m", "x"))
            .await
            .expect_err("second chat must exhaust the tape");
        let msg = err.to_string();
        assert!(
            msg.contains("tape exhausted"),
            "expected exhaustion diagnostic, got: {msg}"
        );
    }

    #[tokio::test]
    async fn replay_refuses_unsupported_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let tape = dir.path().join("future-tape.ndjson");
        // Hand-craft a v=999 entry.
        let bad_entry = serde_json::json!({
            "v": 999,
            "req": mk_request("m", "x"),
            "res": mk_response("future"),
        });
        std::fs::write(&tape, format!("{bad_entry}\n")).unwrap();

        let panicking = Arc::new(MockLlmAdapter::new(vec![]));
        let replayer = ProviderTaper::new(panicking, TaperMode::Replay { path: tape }).unwrap();
        let err = replayer
            .chat(mk_request("m", "x"))
            .await
            .expect_err("future-schema tape must refuse");
        assert!(err.to_string().contains("unsupported schema"));
    }

    #[tokio::test]
    async fn record_mode_creates_file_eagerly() {
        let dir = tempfile::tempdir().unwrap();
        let tape = dir.path().join("eager.ndjson");
        let inner = Arc::new(MockLlmAdapter::new(vec![]));
        let _t = ProviderTaper::new(inner, TaperMode::Record { path: tape.clone() }).unwrap();
        assert!(
            tape.exists(),
            "Record mode must create the tape on construction"
        );
    }

    #[tokio::test]
    async fn record_mode_open_failure_surfaces_at_construction() {
        let inner = Arc::new(MockLlmAdapter::new(vec![]));
        let bad = PathBuf::from("/this/path/does/not/exist/tape.ndjson");
        let err = ProviderTaper::new(inner, TaperMode::Record { path: bad })
            .expect_err("opening into a non-existent dir must fail at construction");
        assert!(err.to_string().contains("cannot open tape"));
    }

    #[tokio::test]
    async fn replay_logs_warning_on_model_drift_but_serves_recorded_response() {
        let dir = tempfile::tempdir().unwrap();
        let tape = dir.path().join("drift.ndjson");
        let inner = Arc::new(MockLlmAdapter::new(vec![mk_response("rec")]));
        let recorder = ProviderTaper::new(inner, TaperMode::Record { path: tape.clone() }).unwrap();
        let _ = recorder.chat(mk_request("model-A", "x")).await.unwrap();

        let panicking = Arc::new(MockLlmAdapter::new(vec![]));
        let replayer = ProviderTaper::new(panicking, TaperMode::Replay { path: tape }).unwrap();
        // Different model — should log warn but still return recorded "rec".
        let resp = replayer.chat(mk_request("model-B", "x")).await.unwrap();
        assert_eq!(resp.content, "rec");
    }
}
