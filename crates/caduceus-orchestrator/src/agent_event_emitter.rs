//! Agent event emitter — fans `AgentEvent` values out to consumers.
//!
//! Owns three things:
//!   1. An mpsc `tx` for the primary single-consumer driver (the harness).
//!   2. A broadcast `tx` so additional subscribers (UI bridges, telemetry
//!      sinks) can attach without taking the only mpsc receiver away (gap
//!      G17 — handled by ST-A2a).
//!   3. A bounded retention ring so a UI that disconnects mid-turn can
//!      replay the last N events on reattach (gap G14).
//!
//! Extracted from `lib.rs` (ST-B1 Wave 2).

use caduceus_core::{AgentEvent, SessionPhase, StopReason, TokenUsage, ToolCallId};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Sends `AgentEvent` values through a tokio mpsc channel for streaming to the frontend.
/// Default capacity for the emitter's retention ring (gap G14).
/// Picked to comfortably cover one long agent turn — typical turns
/// emit ~50–150 events, so 200 lets a UI that re-attaches mid-turn
/// reconstruct the full timeline without server-side replay logic.
pub const DEFAULT_EMITTER_RETENTION: usize = 200;

/// Default capacity for the broadcast fan-out (ST-A2a).
/// Per-subscriber buffer; slow subscribers get `RecvError::Lagged(n)`
/// and must resubscribe. The retention ring is the durable source of
/// truth, so lagged subscribers can always replay. Matches the
/// retention cap so a subscriber that keeps up sees every event.
pub const DEFAULT_BROADCAST_CAP: usize = 200;

/// Clonable so callers (e.g. the IDE bridge) can hold a handle for
/// [`AgentEventEmitter::replay`] on UI reattach without taking the only
/// `&AgentEventEmitter` away from the harness. The clone shares the same
/// retention ring (`Arc<Mutex<...>>`) and the same mpsc sender, so events
/// emitted by the harness are visible through every clone (gap G17).
#[derive(Clone)]
pub struct AgentEventEmitter {
    tx: mpsc::Sender<AgentEvent>,
    /// Broadcast fan-out (ST-A2a): callers can `subscribe()` at any
    /// time to get a fresh `broadcast::Receiver<AgentEvent>` without
    /// moving the sender. Cheap when no subscribers exist
    /// (`receiver_count()` is an atomic load). This is the API the
    /// Zed bridge uses to attach a fresh per-turn receiver to a
    /// long-lived harness — the mpsc `rx` from `channel(...)` remains
    /// for single-consumer callers that want backpressure / strict
    /// ordering semantics.
    broadcast_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    /// Retention ring (gap G14): every emitted event is also pushed here
    /// in order. UIs that disconnect (e.g. tab refresh, IPC reconnect)
    /// can call [`AgentEventEmitter::replay`] on reattach to rebuild the
    /// last `cap` events of timeline. Bounded so a long-running session
    /// doesn't grow without limit.
    retention: Arc<std::sync::Mutex<std::collections::VecDeque<AgentEvent>>>,
    retention_cap: usize,
    /// Counter for live-channel drops since the last successful emit
    /// (gap G27). When `try_send` returns `Full`, this is incremented;
    /// on the next successful emit, an `EventBufferOverflow` event is
    /// synthesised carrying the count, and the counter resets. Shared
    /// across clones so a multi-handle setup reports a single coherent
    /// drop count.
    dropped_since_last: Arc<std::sync::atomic::AtomicU64>,
}

impl AgentEventEmitter {
    pub fn new(tx: mpsc::Sender<AgentEvent>) -> Self {
        Self::with_retention(tx, DEFAULT_EMITTER_RETENTION)
    }

    /// Construct with a custom retention-ring cap. A `0` cap is normalised
    /// to 1: a fully disabled ring would mean reattaching UIs see nothing,
    /// which silently breaks the gap-G14 guarantee. If you want NO ring,
    /// use [`AgentEventEmitter::without_retention`] explicitly.
    pub fn with_retention(tx: mpsc::Sender<AgentEvent>, cap: usize) -> Self {
        let (broadcast_tx, _) = tokio::sync::broadcast::channel(DEFAULT_BROADCAST_CAP);
        Self {
            tx,
            broadcast_tx,
            retention: Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::with_capacity(cap.max(1)),
            )),
            retention_cap: cap.max(1),
            dropped_since_last: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Construct without retention. Reserved for tests and headless runs
    /// that explicitly do not want per-emitter memory cost.
    pub fn without_retention(tx: mpsc::Sender<AgentEvent>) -> Self {
        let (broadcast_tx, _) = tokio::sync::broadcast::channel(DEFAULT_BROADCAST_CAP);
        Self {
            tx,
            broadcast_tx,
            retention: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            retention_cap: 0,
            dropped_since_last: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Create a pair: (emitter, receiver). Includes the default retention
    /// ring; for a no-ring channel use [`AgentEventEmitter::channel_no_retention`].
    pub fn channel(buffer: usize) -> (Self, mpsc::Receiver<AgentEvent>) {
        let (tx, rx) = mpsc::channel(buffer);
        (Self::new(tx), rx)
    }

    pub fn channel_no_retention(buffer: usize) -> (Self, mpsc::Receiver<AgentEvent>) {
        let (tx, rx) = mpsc::channel(buffer);
        (Self::without_retention(tx), rx)
    }

    /// Snapshot of the retention ring, oldest-first. Cheap (O(n) clone of
    /// the buffered events), safe to call from any task. Returned vec is
    /// owned so the caller can hold it across awaits without keeping the
    /// emitter mutex.
    pub fn replay(&self) -> Vec<AgentEvent> {
        match self.retention.lock() {
            Ok(g) => g.iter().cloned().collect(),
            // Mutex poisoning means a previous emit panicked while
            // holding the lock — recover by returning an empty slice
            // instead of propagating the poison to every UI reattach.
            Err(poisoned) => poisoned.into_inner().iter().cloned().collect(),
        }
    }

    pub fn retention_cap(&self) -> usize {
        self.retention_cap
    }

    /// Subscribe to the broadcast fan-out (ST-A2a). Each call returns a
    /// fresh `broadcast::Receiver<AgentEvent>` that will observe every
    /// event emitted *after* this point (subscribers never see prior
    /// events through the live channel; use [`replay`] to seed them
    /// from the retention ring).
    ///
    /// Slow subscribers may observe `RecvError::Lagged(n)`, meaning
    /// `n` events were dropped from their per-subscriber buffer (cap
    /// = [`DEFAULT_BROADCAST_CAP`]). The retention ring still holds
    /// those events, so lagged subscribers can replay + resubscribe
    /// to resync without data loss.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<AgentEvent> {
        self.broadcast_tx.subscribe()
    }

    /// Current count of active broadcast subscribers. Primarily useful
    /// for tests asserting the wiring; callers should not branch on
    /// this in production paths (value can race with subscribe/drop).
    pub fn broadcast_receiver_count(&self) -> usize {
        self.broadcast_tx.receiver_count()
    }

    /// Number of events dropped from the live mpsc channel since the last
    /// successful emit (gap G27). Reset to 0 by every successful send.
    /// Surfaced for diagnostics and tests; UIs should observe overflow
    /// via the synthetic `EventBufferOverflow` event instead.
    pub fn dropped_since_last(&self) -> u64 {
        self.dropped_since_last
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub async fn emit(&self, event: AgentEvent) {
        // (0) ST-A2a broadcast fan-out. `receiver_count()` is a cheap
        //     atomic load; when no bridge / UI is subscribed this is a
        //     no-op and we avoid the clone. The `send` return value is
        //     intentionally ignored — a broadcast with zero live
        //     receivers returns `Err(SendError)`, but we've already
        //     guarded against that with the count check; other errors
        //     don't apply (broadcast has no "closed" state while the
        //     sender lives).
        if self.broadcast_tx.receiver_count() > 0 {
            let _ = self.broadcast_tx.send(event.clone());
        }
        // (1) Push into retention BEFORE try_send so the ring captures the
        //     event even if the live channel is full and we drop the
        //     real-time delivery. The ring is the durable source of truth
        //     for "what happened"; the channel is the live notifier.
        if self.retention_cap > 0 {
            if let Ok(mut ring) = self.retention.lock() {
                if ring.len() == self.retention_cap {
                    ring.pop_front();
                }
                ring.push_back(event.clone());
            }
        }
        // (2) Best-effort live delivery. Dropping is acceptable — UI can
        //     replay the ring on reconnect. Backpressure on the loop is
        //     NOT acceptable.
        //
        // Gap G27: when `try_send` returns `Full` we must not silently
        // swallow it. We bump a per-emitter counter and, on the *next*
        // successful emit, prepend a synthetic `EventBufferOverflow`
        // carrying the count so the UI knows it missed live events
        // (but that they are recoverable from the retention ring).
        match self.tx.try_send(event) {
            Ok(()) => {
                let prior = self
                    .dropped_since_last
                    .swap(0, std::sync::atomic::Ordering::Relaxed);
                if prior > 0 {
                    // Synthesise the overflow notice and try to push it
                    // through. Use try_send so a still-full channel
                    // simply re-arms the counter on the next emit
                    // rather than blocking the agent loop.
                    let notice = AgentEvent::EventBufferOverflow {
                        dropped_since_last: prior,
                    };
                    // Mirror into the retention ring so reattaching UIs
                    // also see a marker for the gap.
                    if self.retention_cap > 0 {
                        if let Ok(mut ring) = self.retention.lock() {
                            if ring.len() == self.retention_cap {
                                ring.pop_front();
                            }
                            ring.push_back(notice.clone());
                        }
                    }
                    // Mirror into the broadcast fan-out so live
                    // subscribers see the gap marker too (ST-A2a).
                    if self.broadcast_tx.receiver_count() > 0 {
                        let _ = self.broadcast_tx.send(notice.clone());
                    }
                    if self.tx.try_send(notice).is_err() {
                        // Couldn't deliver the notice live; restore the
                        // counter so the next attempt re-emits.
                        self.dropped_since_last
                            .fetch_add(prior, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_dropped)) => {
                let n = self
                    .dropped_since_last
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1;
                // Throttle the log: only on the first drop of a streak,
                // and on every power-of-two thereafter, so a long
                // overflow window doesn't spam tracing.
                if n == 1 || n.is_power_of_two() {
                    tracing::warn!(
                        target: "caduceus.emitter",
                        dropped_since_last = n,
                        retention_cap = self.retention_cap,
                        "AgentEventEmitter live channel full; event dropped from live stream (still in retention ring)"
                    );
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                // Receiver gone — log once at warn, but don't keep
                // counting against `dropped_since_last` (no point: no
                // future emit will succeed).
                tracing::warn!(
                    target: "caduceus.emitter",
                    "AgentEventEmitter receiver closed; event will be retained in ring only"
                );
            }
        }
    }

    pub async fn emit_text_delta(&self, text: impl Into<String>) {
        self.emit(AgentEvent::TextDelta { text: text.into() }).await;
    }

    pub async fn emit_tool_call_start(&self, id: ToolCallId, name: impl Into<String>) {
        self.emit(AgentEvent::ToolCallStart {
            id,
            name: name.into(),
        })
        .await;
    }

    pub async fn emit_tool_result_end(
        &self,
        id: ToolCallId,
        content: impl Into<String>,
        is_error: bool,
    ) {
        self.emit(AgentEvent::ToolResultEnd {
            id,
            content: content.into(),
            is_error,
        })
        .await;
    }

    pub async fn emit_turn_complete(&self, stop_reason: StopReason, usage: TokenUsage) {
        self.emit(AgentEvent::TurnComplete { stop_reason, usage })
            .await;
    }

    /// Emit a per-turn token-logprob summary (gap G10 / P3.2).
    /// Called once after `provider.chat()` returns when the response
    /// carried logprobs.
    pub async fn emit_token_logprob_summary(&self, summary: &caduceus_providers::LogprobsSummary) {
        self.emit(AgentEvent::TokenLogprobSummary {
            n_tokens: summary.n_tokens,
            min_token_p: summary.min_token_p,
            mean_token_p: summary.mean_token_p,
            confidence: format!("{:?}", summary.confidence).to_lowercase(),
        })
        .await;
    }

    pub async fn emit_error(&self, message: impl Into<String>) {
        self.emit(AgentEvent::Error {
            message: message.into(),
        })
        .await;
    }

    pub async fn emit_phase_changed(&self, phase: SessionPhase) {
        self.emit(AgentEvent::SessionPhaseChanged { phase }).await;
    }

    // ── New events for rich visualization ──────────────────────────────────────

    pub async fn emit_thinking_started(&self, iteration: u32) {
        self.emit(AgentEvent::ThinkingStarted { iteration }).await;
    }

    pub async fn emit_reasoning_delta(&self, content: impl Into<String>) {
        self.emit(AgentEvent::ReasoningDelta {
            content: content.into(),
        })
        .await;
    }

    pub async fn emit_reasoning_complete(&self, content: impl Into<String>, duration_ms: u64) {
        self.emit(AgentEvent::ReasoningComplete {
            content: content.into(),
            duration_ms,
        })
        .await;
    }

    pub async fn emit_context_warning(&self, level: impl Into<String>, used: u32, max: u32) {
        self.emit(AgentEvent::ContextWarning {
            level: level.into(),
            used_tokens: used,
            max_tokens: max,
        })
        .await;
    }

    pub async fn emit_context_compacted(&self, freed: u32, before: u32, after: u32) {
        self.emit(AgentEvent::ContextCompacted {
            freed_tokens: freed,
            before,
            after,
        })
        .await;
    }

    pub async fn emit_context_groups_evicted(
        &self,
        strategy: impl Into<String>,
        groups: Vec<caduceus_core::EvictedGroupRef>,
    ) {
        if groups.is_empty() {
            return;
        }
        let total_tokens: u32 = groups.iter().map(|g| g.token_count).sum();
        self.emit(AgentEvent::ContextGroupsEvicted {
            strategy: strategy.into(),
            groups,
            total_tokens,
        })
        .await;
    }

    pub async fn emit_loop_detected(&self, tool_name: impl Into<String>, count: u32) {
        self.emit(AgentEvent::LoopDetected {
            tool_name: tool_name.into(),
            consecutive_count: count,
        })
        .await;
    }

    pub async fn emit_circuit_breaker(&self, failures: u32, last_tools: Vec<String>) {
        self.emit(AgentEvent::CircuitBreakerTriggered {
            consecutive_failures: failures,
            last_tools,
        })
        .await;
    }

    pub async fn emit_tree_node(
        &self,
        id: impl Into<String>,
        parent_id: Option<String>,
        label: impl Into<String>,
        status: impl Into<String>,
    ) {
        self.emit(AgentEvent::ExecutionTreeNode {
            id: id.into(),
            parent_id,
            label: label.into(),
            status: status.into(),
        })
        .await;
    }

    pub async fn emit_tree_update(
        &self,
        id: impl Into<String>,
        status: impl Into<String>,
        detail: Option<String>,
    ) {
        self.emit(AgentEvent::ExecutionTreeUpdate {
            id: id.into(),
            status: status.into(),
            detail,
        })
        .await;
    }

    pub async fn emit_message_part(&self, part: caduceus_core::MessagePartType) {
        self.emit(AgentEvent::MessagePart { part_type: part }).await;
    }
}
