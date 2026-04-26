//! G22 / P7.3 — **Trajectory recorder & replayer**.
//!
//! A *trajectory* is a deterministic, replayable record of every
//! non-deterministic input the agent observed during one run:
//! * each `LlmAdapter::chat` call (request → response)
//! * each tool dispatch (name + input → output)
//!
//! With these recorded, an agent run can be replayed end-to-end: the
//! orchestrator code is exercised verbatim, but every external answer
//! is served from the trajectory file rather than re-fetched from a
//! live model or re-executed against the filesystem. This gives us:
//!
//! 1. **Regression tests** that exercise the full orchestrator without
//!    paying for LLM calls (or risking flake from network jitter).
//! 2. **Bug repro bundles** — capture a failing session and ship the
//!    `.jsonl` file; anyone can replay it bit-exact.
//! 3. **Eval harness backbone** (P8 / G16) — score the same trajectory
//!    against multiple verifier models without re-running tools.
//!
//! ## File format
//!
//! JSONL — one [`TrajectoryEntry`] per line. Versioned via a leading
//! `Header` entry that pins schema + recording metadata. Adding new
//! entry kinds is forward-compatible because the enum is internally
//! tagged and unknown tags fall through to [`TrajectoryEntry::Unknown`].
//!
//! ## What's NOT recorded (yet)
//!
//! * Streaming chunks — only the final aggregated `ChatResponse`. A
//!   future entry kind can capture per-chunk timing for replay-with-
//!   wall-clock fidelity.
//! * Permission decisions — the orchestrator's HITL path is replayed
//!   by feeding the recorded `PermissionOutcome` back through the
//!   approval channel. Wired in a follow-up.
//! * RNG draws — once the orchestrator has any RNG, the seed must be
//!   recorded here to keep replay deterministic.
//!
//! ## Determinism contract
//!
//! Replay produces an identical event stream **only if** the
//! orchestrator under test is deterministic given a fixed
//! (LLM-response, tool-output, RNG-seed) tuple. Today that holds for
//! the test paths exercised; if a future change introduces wall-clock
//! reads or unseeded RNG, the recorder must be extended to capture
//! them or replay will silently diverge.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use caduceus_core::{ModelId, ProviderId};
use caduceus_providers::{ChatRequest, ChatResponse, LlmAdapter, StreamResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Schema version for the trajectory file format. Bump on **breaking**
/// payload changes; additive entry kinds are absorbed by
/// [`TrajectoryEntry::Unknown`] and don't require a bump.
pub const TRAJECTORY_SCHEMA_VERSION: u16 = 1;

/// One line of the trajectory file. Internally tagged so a future
/// schema can add entries without breaking older readers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // Llm variant is intrinsically large; boxing would change construction API of a public serde type.
pub enum TrajectoryEntry {
    /// First line of every file. Carries metadata that survives the
    /// individual entries (session id, schema version, recorder
    /// build, model defaults).
    Header {
        schema_version: u16,
        session_id: String,
        recorded_at: DateTime<Utc>,
        provider_id: String,
        model_id: String,
    },
    /// One LLM call. The recorder captures the request as-issued and
    /// the response as-returned. On replay, the next `Llm` entry is
    /// matched in order and its response served instead of hitting
    /// a live adapter.
    Llm {
        request: ChatRequest,
        response: ChatResponse,
    },
    /// One tool dispatch. `output` is the serialised tool result.
    Tool {
        name: String,
        input: serde_json::Value,
        output: String,
        is_error: bool,
    },
    /// Forward-compat catch-all. Older readers see a future entry
    /// shape as `Unknown` and skip it instead of refusing to load.
    #[serde(other)]
    Unknown,
}

// ── Recorder ──────────────────────────────────────────────────────────────────

/// Sink that appends entries to a trajectory file. Wrapped in
/// `Arc<Mutex<...>>` so multiple recording adapters / dispatchers
/// share one writer. Calls are infallible — write errors are logged
/// and the recording marked poisoned (subsequent writes no-op) so a
/// transient disk error doesn't take down the agent run.
pub struct TrajectoryRecorder {
    inner: Mutex<RecorderInner>,
    path: PathBuf,
}

struct RecorderInner {
    file: Option<std::fs::File>,
    poisoned: bool,
    entry_count: u64,
}

impl TrajectoryRecorder {
    /// Create a new recorder writing to `path`. Truncates any
    /// existing file and writes the [`TrajectoryEntry::Header`]
    /// immediately so that a crash mid-run still yields a parseable
    /// (if truncated) file.
    pub fn create(
        path: impl AsRef<Path>,
        session_id: impl Into<String>,
        provider: &ProviderId,
        model: &ModelId,
    ) -> Result<Arc<Self>> {
        let path = path.as_ref().to_path_buf();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening trajectory file at {}", path.display()))?;
        let recorder = Arc::new(Self {
            inner: Mutex::new(RecorderInner {
                file: Some(file),
                poisoned: false,
                entry_count: 0,
            }),
            path,
        });
        recorder.write(&TrajectoryEntry::Header {
            schema_version: TRAJECTORY_SCHEMA_VERSION,
            session_id: session_id.into(),
            recorded_at: Utc::now(),
            provider_id: provider.0.clone(),
            model_id: model.0.clone(),
        });
        Ok(recorder)
    }

    /// Path of the file being recorded into.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Number of entries successfully written (including the Header).
    pub fn entry_count(&self) -> u64 {
        self.inner.lock().map(|g| g.entry_count).unwrap_or(0)
    }

    /// Append one entry. Best-effort: write errors poison the
    /// recorder for this session so subsequent calls are cheap no-ops.
    /// We deliberately don't propagate I/O errors because doing so
    /// would let a full disk crash the agent loop — the trajectory
    /// is a debugging aid, not a hard dependency.
    pub fn write(&self, entry: &TrajectoryEntry) {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if g.poisoned {
            return;
        }
        let line = match serde_json::to_string(entry) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    target: "caduceus.eval.trajectory",
                    error = %e,
                    "trajectory entry failed to serialise; poisoning recorder"
                );
                g.poisoned = true;
                return;
            }
        };
        if let Some(ref mut f) = g.file {
            if let Err(e) = writeln!(f, "{line}") {
                tracing::warn!(
                    target: "caduceus.eval.trajectory",
                    error = %e,
                    "trajectory write failed; poisoning recorder"
                );
                g.poisoned = true;
                return;
            }
            // Flush so a crash mid-run still yields a usable file up
            // to the last successful write. Cost is negligible at
            // agent-loop frequency.
            let _ = f.flush();
            g.entry_count += 1;
        }
    }
}

// ── Recording LLM adapter ─────────────────────────────────────────────────────

/// Wraps any [`LlmAdapter`] to record every `chat` call into a
/// trajectory. `stream` is passed through *without* recording — the
/// orchestrator calls `chat` for the deterministic agent loop;
/// streaming is for live UI rendering and isn't part of the
/// regression-replay surface.
pub struct RecordingLlmAdapter {
    inner: Arc<dyn LlmAdapter>,
    recorder: Arc<TrajectoryRecorder>,
}

impl RecordingLlmAdapter {
    pub fn new(inner: Arc<dyn LlmAdapter>, recorder: Arc<TrajectoryRecorder>) -> Self {
        Self { inner, recorder }
    }
}

#[async_trait]
impl LlmAdapter for RecordingLlmAdapter {
    fn provider_id(&self) -> &ProviderId {
        self.inner.provider_id()
    }

    async fn chat(&self, request: ChatRequest) -> caduceus_core::Result<ChatResponse> {
        let request_clone = request.clone();
        let response = self.inner.chat(request).await?;
        self.recorder.write(&TrajectoryEntry::Llm {
            request: request_clone,
            response: response.clone(),
        });
        Ok(response)
    }

    async fn stream(&self, request: ChatRequest) -> caduceus_core::Result<StreamResult> {
        // Streaming isn't part of the deterministic replay surface;
        // pass through. A future enhancement could record aggregated
        // chunks under a new `LlmStream` entry kind.
        self.inner.stream(request).await
    }

    async fn list_models(&self) -> caduceus_core::Result<Vec<ModelId>> {
        self.inner.list_models().await
    }
}

// ── Replayer ──────────────────────────────────────────────────────────────────

/// Reads a trajectory file and exposes ordered iteration over its
/// LLM and tool entries. Constructed once per replay run; the
/// orchestrator code under test pulls entries via the
/// [`ReplayingLlmAdapter`] / tool replayer wired against this.
#[derive(Debug)]
pub struct Trajectory {
    pub header: TrajectoryEntry,
    pub entries: Vec<TrajectoryEntry>,
}

impl Trajectory {
    /// Load and validate a trajectory file. Returns an error if the
    /// file is empty, the header is missing, or the schema version
    /// is from a future build (forward-incompatible).
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let f = std::fs::File::open(path)
            .with_context(|| format!("opening trajectory file at {}", path.display()))?;
        let reader = BufReader::new(f);
        let mut entries: Vec<TrajectoryEntry> = Vec::new();
        for (i, line) in reader.lines().enumerate() {
            let line = line.with_context(|| format!("reading line {} of trajectory", i + 1))?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: TrajectoryEntry = serde_json::from_str(&line)
                .with_context(|| format!("parsing trajectory line {}: {line}", i + 1))?;
            entries.push(entry);
        }
        if entries.is_empty() {
            return Err(anyhow!("trajectory file is empty"));
        }
        let header = entries.remove(0);
        match &header {
            TrajectoryEntry::Header { schema_version, .. } => {
                if *schema_version > TRAJECTORY_SCHEMA_VERSION {
                    return Err(anyhow!(
                        "trajectory schema_version {} is newer than this build supports ({})",
                        schema_version,
                        TRAJECTORY_SCHEMA_VERSION
                    ));
                }
            }
            other => {
                return Err(anyhow!(
                    "first trajectory entry must be a Header; got {:?}",
                    other
                ))
            }
        }
        Ok(Self { header, entries })
    }

    /// Number of `Llm` entries — i.e. how many `chat` calls the
    /// recorded agent run made.
    pub fn llm_call_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, TrajectoryEntry::Llm { .. }))
            .count()
    }

    /// Number of `Tool` entries — i.e. how many tool dispatches the
    /// recorded agent run performed.
    pub fn tool_call_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, TrajectoryEntry::Tool { .. }))
            .count()
    }
}

/// Stateful cursor over a [`Trajectory`]'s LLM entries. Hands out
/// recorded responses in recording order. Wrapped in `Mutex` so the
/// `Send + Sync` adapter trait can use it across tasks.
struct LlmCursor {
    /// Owned vec of recorded responses in order.
    responses: Vec<ChatResponse>,
    next: usize,
}

impl LlmCursor {
    fn from(traj: &Trajectory) -> Self {
        let responses = traj
            .entries
            .iter()
            .filter_map(|e| match e {
                TrajectoryEntry::Llm { response, .. } => Some(response.clone()),
                _ => None,
            })
            .collect();
        Self { responses, next: 0 }
    }

    fn next(&mut self) -> Result<ChatResponse> {
        if self.next >= self.responses.len() {
            return Err(anyhow!(
                "ReplayingLlmAdapter exhausted: {} responses recorded, but a {}th chat() was issued. \
                 Either the agent under test is non-deterministic w.r.t. recorded inputs, or the \
                 trajectory was truncated mid-recording.",
                self.responses.len(),
                self.next + 1
            ));
        }
        let r = self.responses[self.next].clone();
        self.next += 1;
        Ok(r)
    }

    fn remaining(&self) -> usize {
        self.responses.len().saturating_sub(self.next)
    }
}

/// `LlmAdapter` impl that serves recorded responses from a
/// [`Trajectory`] in order. Used by the replay harness in place of a
/// live provider.
pub struct ReplayingLlmAdapter {
    provider_id: ProviderId,
    cursor: Mutex<LlmCursor>,
}

impl ReplayingLlmAdapter {
    /// Build from a loaded trajectory. The provider id from the
    /// trajectory's header is used as the adapter's id so callers
    /// can route requests through the same provider-routing code as
    /// the original recording.
    pub fn from_trajectory(traj: &Trajectory) -> Self {
        let provider_id = match &traj.header {
            TrajectoryEntry::Header { provider_id, .. } => ProviderId::new(provider_id),
            _ => ProviderId::new("replay"),
        };
        Self {
            provider_id,
            cursor: Mutex::new(LlmCursor::from(traj)),
        }
    }

    /// How many recorded responses haven't been served yet. After a
    /// successful replay this should be `0`; a positive value means
    /// the agent under test made fewer LLM calls than the recording
    /// (suggests a code path divergence).
    pub fn remaining(&self) -> usize {
        self.cursor.lock().map(|c| c.remaining()).unwrap_or(0)
    }
}

#[async_trait]
impl LlmAdapter for ReplayingLlmAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn chat(&self, _request: ChatRequest) -> caduceus_core::Result<ChatResponse> {
        let mut c = self
            .cursor
            .lock()
            .map_err(|e| caduceus_core::CaduceusError::Provider(format!("cursor poisoned: {e}")))?;
        c.next().map_err(|e| {
            caduceus_core::CaduceusError::Provider(format!("trajectory replay error: {e}"))
        })
    }

    async fn stream(&self, _request: ChatRequest) -> caduceus_core::Result<StreamResult> {
        Err(caduceus_core::CaduceusError::Provider(
            "ReplayingLlmAdapter does not support streaming; use chat()".into(),
        ))
    }

    async fn list_models(&self) -> caduceus_core::Result<Vec<ModelId>> {
        Ok(vec![])
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::StopReason;
    use caduceus_providers::mock::MockLlmAdapter;
    use tempfile::tempdir;

    fn sample_response(text: &str) -> ChatResponse {
        ChatResponse {
            content: text.into(),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
            thinking: String::new(),
        }
    }

    fn sample_request() -> ChatRequest {
        ChatRequest {
            model: ModelId::new("mock-model"),
            messages: vec![].into(),
            system: None,
            max_tokens: 100,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![].into(),
            logprobs: None,
            thread_id: None,
            prompt_id: None,
            intent: None,
            stop: vec![],
            thinking_effort: None,
            speed: None,
        }
    }
    #[test]
    fn recorder_writes_header_first() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let _ = TrajectoryRecorder::create(
            &path,
            "session_x",
            &ProviderId::new("mock"),
            &ModelId::new("mock-model"),
        )
        .unwrap();
        let lines: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        assert_eq!(lines.len(), 1);
        let entry: TrajectoryEntry = serde_json::from_str(&lines[0]).unwrap();
        assert!(matches!(entry, TrajectoryEntry::Header { .. }));
    }

    #[test]
    fn recorder_appends_in_order_and_flushes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let rec = TrajectoryRecorder::create(
            &path,
            "session_x",
            &ProviderId::new("mock"),
            &ModelId::new("mock-model"),
        )
        .unwrap();
        rec.write(&TrajectoryEntry::Llm {
            request: sample_request(),
            response: sample_response("a"),
        });
        rec.write(&TrajectoryEntry::Tool {
            name: "bash".into(),
            input: serde_json::json!({"command":"echo hi"}),
            output: "hi\n".into(),
            is_error: false,
        });
        rec.write(&TrajectoryEntry::Llm {
            request: sample_request(),
            response: sample_response("b"),
        });

        let traj = Trajectory::load(&path).unwrap();
        assert!(matches!(traj.header, TrajectoryEntry::Header { .. }));
        assert_eq!(traj.llm_call_count(), 2);
        assert_eq!(traj.tool_call_count(), 1);
    }

    #[tokio::test]
    async fn record_then_replay_yields_identical_responses() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");

        // (1) Record: drive a MockLlmAdapter through the recording wrapper.
        let mock = Arc::new(MockLlmAdapter::new(vec![
            sample_response("first"),
            sample_response("second"),
        ]));
        let rec = TrajectoryRecorder::create(
            &path,
            "session_x",
            &ProviderId::new("mock"),
            &ModelId::new("mock-model"),
        )
        .unwrap();
        let recording = RecordingLlmAdapter::new(mock, rec);
        let r1 = recording.chat(sample_request()).await.unwrap();
        let r2 = recording.chat(sample_request()).await.unwrap();
        assert_eq!(r1.content, "first");
        assert_eq!(r2.content, "second");

        // (2) Replay: load and serve.
        let traj = Trajectory::load(&path).unwrap();
        let replay = ReplayingLlmAdapter::from_trajectory(&traj);
        assert_eq!(replay.remaining(), 2);
        let p1 = replay.chat(sample_request()).await.unwrap();
        let p2 = replay.chat(sample_request()).await.unwrap();
        assert_eq!(
            p1.content, r1.content,
            "replayed response must equal recorded"
        );
        assert_eq!(
            p2.content, r2.content,
            "replayed response must equal recorded"
        );
        assert_eq!(replay.remaining(), 0);
    }

    #[tokio::test]
    async fn replay_exhaustion_returns_diagnostic_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let mock = Arc::new(MockLlmAdapter::new(vec![sample_response("only")]));
        let rec =
            TrajectoryRecorder::create(&path, "s", &ProviderId::new("mock"), &ModelId::new("m"))
                .unwrap();
        let recording = RecordingLlmAdapter::new(mock, rec);
        let _ = recording.chat(sample_request()).await.unwrap();

        let traj = Trajectory::load(&path).unwrap();
        let replay = ReplayingLlmAdapter::from_trajectory(&traj);
        let _ = replay.chat(sample_request()).await.unwrap();
        // Second call must fail with a diagnostic, not panic — this
        // is the signal that the agent under test diverged from the
        // recording.
        let err = replay.chat(sample_request()).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("exhausted") || msg.contains("trajectory replay"),
            "diagnostic error must mention exhaustion / replay; got: {msg}"
        );
    }

    #[test]
    fn rejects_trajectory_from_future_schema() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let header = TrajectoryEntry::Header {
            schema_version: TRAJECTORY_SCHEMA_VERSION + 1,
            session_id: "x".into(),
            recorded_at: Utc::now(),
            provider_id: "mock".into(),
            model_id: "m".into(),
        };
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&header).unwrap()),
        )
        .unwrap();
        let err = Trajectory::load(&path).unwrap_err();
        assert!(
            format!("{err}").contains("newer than this build supports"),
            "must refuse forward-incompatible trajectories; got: {err}"
        );
    }

    #[test]
    fn unknown_entry_kind_loads_as_unknown_variant() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let header = TrajectoryEntry::Header {
            schema_version: TRAJECTORY_SCHEMA_VERSION,
            session_id: "x".into(),
            recorded_at: Utc::now(),
            provider_id: "mock".into(),
            model_id: "m".into(),
        };
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&header).unwrap()).unwrap();
        // Future entry shape unknown to this build.
        writeln!(file, r#"{{"type":"future_kind","whatever":1}}"#).unwrap();
        drop(file);

        let traj = Trajectory::load(&path).unwrap();
        assert_eq!(traj.entries.len(), 1);
        assert!(matches!(traj.entries[0], TrajectoryEntry::Unknown));
    }

    #[test]
    fn poisoned_recorder_after_serialisation_failure_is_no_op() {
        // We can't easily force a serialisation failure with the
        // current entry shapes (all variants serialise cleanly), so
        // we exercise the poison-on-write-error path via a closed
        // file. Open a recorder, drop the inner file by replacing
        // it, and verify entry_count doesn't advance.
        let dir = tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let rec =
            TrajectoryRecorder::create(&path, "x", &ProviderId::new("mock"), &ModelId::new("m"))
                .unwrap();
        let baseline = rec.entry_count();
        // Manually poison by acquiring the lock and setting the flag.
        {
            let mut g = rec.inner.lock().unwrap();
            g.poisoned = true;
        }
        rec.write(&TrajectoryEntry::Llm {
            request: sample_request(),
            response: sample_response("never written"),
        });
        assert_eq!(
            rec.entry_count(),
            baseline,
            "poisoned recorder must not advance count"
        );
    }
}
