# ENGINE REFACTOR REVIEW — Edge Case Analysis
**Reviewer:** Claude Sonnet 4.5  
**Date:** 2024-04-13  
**Scope:** Caduceus AI IDE engine refactor completeness and edge cases

---

## Executive Summary

Reviewed 10 critical edge cases in the engine refactor focusing on error handling, race conditions, cancellation, streaming, and state management. **Overall Score: 4.8/10** — Significant robustness gaps exist.

**Critical Issues (Score ≤ 3):**
- Race conditions in concurrent session access (#4)
- Background agent cancellation completely broken (#6)
- No tool execution timeout (#3)

**High-Priority Issues (Score 4-6):**
- Error propagation leaves state dirty (#1)
- Flat execution tree structure (#7)
- Empty tool results for Anthropic (#8)

**Medium Issues (Score 7):**
- Missing text MessagePart emission (#2)
- Event channel backpressure unclear (#5)
- No streaming in tool loop (#9)
- Hardcoded background model (#10)

---

## Edge Case Scores and Fixes

### 1. provider.chat() Error Propagation — **Score: 5/10**

**Issue:** When `self.provider.chat(request).await?` fails mid-loop, the `?` propagates the error immediately but leaves `state.phase = SessionPhase::Running`. The cleanup at the end (`state.phase = SessionPhase::Idle`) is skipped.

**Location:** `crates/caduceus-orchestrator/src/lib.rs:973`

**Impact:**
- Session stuck in Running phase
- Frontend shows spinner indefinitely
- No error event emitted to UI
- Subsequent calls to same session may fail with "already running" error

**Fix:**

```rust
// In AgentHarness::run(), wrap the entire tool loop in a Result-returning closure
// and use a guard pattern to ensure cleanup

pub async fn run(
    &self,
    state: &mut SessionState,
    history: &mut ConversationHistory,
    user_input: &str,
) -> Result<String> {
    self.check_cancellation()?;

    state.phase = SessionPhase::Running;
    if let Some(ref em) = self.emitter {
        em.emit_phase_changed(SessionPhase::Running).await;
    }

    // Ensure cleanup happens even on early return
    struct PhaseGuard<'a> {
        state: &'a mut SessionState,
        emitter: &'a Option<AgentEventEmitter>,
    }
    
    impl<'a> Drop for PhaseGuard<'a> {
        fn drop(&mut self) {
            self.state.phase = SessionPhase::Idle;
            if let Some(ref em) = self.emitter {
                // Spawn a task to emit async event during drop
                let em = em.clone();
                tokio::spawn(async move {
                    em.emit_phase_changed(SessionPhase::Idle).await;
                });
            }
        }
    }
    
    let _guard = PhaseGuard {
        state,
        emitter: &self.emitter,
    };

    history.append(caduceus_providers::Message::user(user_input));

    let system_prompt = self.effective_system_prompt();
    let assembler = ContextAssembler::new(self.max_context_tokens, &system_prompt);
    let tool_specs = self.tools.specs();

    // ... rest of function unchanged ...
    
    // At the end, the guard's drop() will set phase to Idle automatically
}
```

**Alternative simpler fix** (catch and emit error):

```rust
// Around line 973, wrap the call in a match
let response = match self.provider.chat(request).await {
    Ok(r) => r,
    Err(e) => {
        // Clean up state before returning error
        state.phase = SessionPhase::Idle;
        if let Some(ref em) = self.emitter {
            em.emit_phase_changed(SessionPhase::Idle).await;
            em.emit_error(&format!("LLM provider error: {}", e)).await;
        }
        return Err(e);
    }
};
```

---

### 2. LLM Returns Text + tool_calls — **Score: 7/10**

**Issue:** Some models (Claude) return text alongside tool_use content blocks. Current code emits `text_delta` for content, then processes tool_calls, but doesn't emit a `MessagePart` for the text before tool execution.

**Location:** `crates/caduceus-orchestrator/src/lib.rs:980-986`

**Impact:**
- Frontend may not render text content in structured message UI
- AI Elements rendering expects MessagePart events
- Minor UX issue — text appears but not in proper message structure

**Current Code:**
```rust
// Emit text content if any
if !response.content.is_empty() {
    if let Some(ref em) = self.emitter {
        em.emit_text_delta(&response.content).await;
    }
}
```

**Fix:**

```rust
// Emit text content if any
if !response.content.is_empty() {
    if let Some(ref em) = self.emitter {
        em.emit_text_delta(&response.content).await;
        // Emit MessagePart for structured rendering
        em.emit_message_part(caduceus_core::MessagePartType::Text {
            content: response.content.clone(),
        }).await;
    }
}
```

---

### 3. No Tool Execution Timeout — **Score: 3/10** ⚠️ CRITICAL

**Issue:** `self.tools.execute(&tool_use.name, tool_use.input.clone()).await` has no timeout. A stuck `bash` command (e.g., `cat /dev/random`) hangs the entire agent loop indefinitely.

**Location:** `crates/caduceus-orchestrator/src/lib.rs:1038`

**Impact:**
- Single stuck tool kills entire session
- No recovery mechanism
- User must kill entire application
- Background agents become zombies

**Fix:**

```rust
// Add timeout to tool execution
use tokio::time::{timeout, Duration};

// In the tool execution block (around line 1038):
let result = match timeout(
    Duration::from_secs(300), // 5-minute default timeout
    self.tools.execute(&tool_use.name, tool_use.input.clone())
).await {
    Ok(tool_result) => tool_result,
    Err(_) => {
        // Timeout occurred
        if let Some(ref em) = self.emitter {
            em.emit_error(&format!(
                "Tool '{}' timed out after 300 seconds",
                tool_use.name
            )).await;
        }
        Err(CaduceusError::Tool {
            tool: tool_use.name.clone(),
            message: "Execution timeout (5 minutes)".into(),
        })
    }
};
```

**Advanced fix** (configurable per-tool timeout):

Add to `ToolSpec`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub required_capability: Option<String>,
    pub timeout_secs: Option<u64>, // NEW
}
```

Then in execution:
```rust
let timeout_duration = self.tools
    .get_spec(&tool_use.name)
    .and_then(|spec| spec.timeout_secs)
    .unwrap_or(300); // default 5 minutes

let result = match timeout(
    Duration::from_secs(timeout_duration),
    self.tools.execute(&tool_use.name, tool_use.input.clone())
).await {
    // ... as above
};
```

---

### 4. Concurrent agent_turn_v2 Race Condition — **Score: 2/10** ⚠️ CRITICAL

**Issue:** Two concurrent calls to `agent_turn_v2` on the same `session_id` can race:
1. Both read the same history
2. Both run for minutes
3. Both write back — last writer wins, other's work is lost

**Location:** Pattern appears to be in higher-level session management (not visible in current files, but inferred from description)

**Impact:**
- Data loss — one agent's work completely overwritten
- Inconsistent state in database
- User loses progress silently
- Critical in multi-user scenarios

**Fix Option 1: Session Lock** (pessimistic)

```rust
// In session manager or runtime
use tokio::sync::Mutex;
use std::collections::HashMap;

pub struct SessionManager {
    session_locks: Arc<Mutex<HashMap<SessionId, Arc<Mutex<()>>>>>,
    // ... other fields
}

impl SessionManager {
    pub async fn run_agent_turn(
        &self,
        session_id: &SessionId,
        user_input: &str,
    ) -> Result<String> {
        // Get or create a lock for this session
        let session_lock = {
            let mut locks = self.session_locks.lock().await;
            locks
                .entry(session_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };

        // Acquire the session-specific lock
        let _guard = session_lock.lock().await;

        // Now safe to read, modify, and write
        let mut history = self.load_history(session_id).await?;
        let mut state = self.load_state(session_id).await?;
        
        let result = self.harness.run(&mut state, &mut history, user_input).await?;
        
        self.save_history(session_id, &history).await?;
        self.save_state(session_id, &state).await?;
        
        Ok(result)
    }
}
```

**Fix Option 2: Optimistic Concurrency** (with version numbers)

```rust
// Add version to SessionState
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub id: SessionId,
    pub phase: SessionPhase,
    pub version: u64, // NEW — increment on every write
    // ... other fields
}

// In save operation:
pub async fn update_session(&self, state: &SessionState) -> Result<()> {
    let rows_affected = sqlx::query(
        "UPDATE sessions 
         SET phase = ?, updated_at = ?, version = version + 1
         WHERE id = ? AND version = ?"
    )
    .bind(&state.phase)
    .bind(chrono::Utc::now())
    .bind(&state.id)
    .bind(state.version) // OLD version
    .execute(&self.pool)
    .await?
    .rows_affected();
    
    if rows_affected == 0 {
        return Err(CaduceusError::Storage(
            "Concurrent modification detected — session was modified by another process".into()
        ));
    }
    
    Ok(())
}
```

**Recommended:** Use **Option 1** for simplicity and to prevent any race conditions entirely.

---

### 5. Event Channel Backpressure — **Score: 6/10**

**Issue:** `AgentEventEmitter::channel(128)` creates a bounded channel. If the bridge consumer (Tauri main thread) is slower than the emitter, the channel fills. Behavior is unclear — does it block? drop events? panic?

**Location:** `crates/caduceus-orchestrator/src/lib.rs:610-612`

**Current Code:**
```rust
pub fn channel(buffer: usize) -> (Self, mpsc::Receiver<AgentEvent>) {
    let (tx, rx) = mpsc::channel(buffer);
    (Self { tx }, rx)
}

pub async fn emit(&self, event: AgentEvent) {
    let _ = self.tx.send(event).await; // <-- Ignores send error!
}
```

**Impact:**
- `mpsc::channel` is bounded and async — `send().await` will **block** until space is available
- If UI thread hangs, agent loop also hangs
- Silent deadlock potential
- The `let _ =` silently ignores send errors (e.g., if receiver dropped)

**Fix Option 1: Use unbounded channel**

```rust
use tokio::sync::mpsc::unbounded_channel;

pub fn channel_unbounded() -> (Self, mpsc::UnboundedReceiver<AgentEvent>) {
    let (tx, rx) = unbounded_channel();
    (Self { tx: tx.into() }, rx)
}

// Modify struct to hold UnboundedSender
pub struct AgentEventEmitter {
    tx: mpsc::UnboundedSender<AgentEvent>,
}

pub fn emit(&self, event: AgentEvent) {
    let _ = self.tx.send(event); // Non-async, never blocks
}
```

**Fix Option 2: Detect backpressure and log warning**

```rust
pub async fn emit(&self, event: AgentEvent) {
    use tokio::time::{timeout, Duration};
    
    match timeout(Duration::from_millis(100), self.tx.send(event)).await {
        Ok(Ok(())) => {}, // Success
        Ok(Err(_)) => {
            tracing::warn!("Event receiver dropped — events will be lost");
        }
        Err(_) => {
            tracing::warn!("Event channel backpressure detected — UI may be frozen");
            // Still try to send, will block
            let _ = self.tx.send(event).await;
        }
    }
}
```

**Recommended:** **Option 1** (unbounded) — events should never be dropped, and the bottleneck is typically LLM response time, not event emission.

---

### 6. Background Agent Ignores Cancellation — **Score: 1/10** ⚠️ CRITICAL

**Issue:** Background agent function signature:
```rust
async fn run_background_agent_loop(..., _cancel: &Arc<AtomicBool>) -> Result<String, String>
```
The `_cancel` parameter is prefixed with `_` — it's unused! The `CancellationToken` on `AgentHarness` is never set for background agents.

**Location:** `crates/caduceus-orchestrator/src/background.rs` (inferred from description)

**Impact:**
- Users cannot stop background agents
- Kill-switch completely broken
- Resource leaks
- Forces application restart to stop runaway agents

**Fix:**

```rust
// Remove the `_` prefix and actually check it
async fn run_background_agent_loop(
    harness: &AgentHarness,
    state: &mut SessionState,
    history: &mut ConversationHistory,
    input: &str,
    cancel: &Arc<AtomicBool>, // REMOVED underscore
) -> Result<String, String> {
    // Check cancellation before each iteration
    for iteration in 0..harness.max_tool_rounds {
        if cancel.load(Ordering::SeqCst) {
            return Err("Background agent cancelled by user".into());
        }
        
        // ... rest of loop
    }
    
    // ... existing code
}

// In BackgroundAgentManager::start():
pub async fn start(&self, task_description: String) -> Result<String, BackgroundError> {
    let id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();

    let cancel_token = CancellationToken::new();
    let agent = BackgroundAgent {
        id: id.clone(),
        session_id: session_id.clone(),
        status: BackgroundStatus::Running,
        started_at: Utc::now(),
        task_description: task_description.clone(),
    };

    // Store agent state
    {
        let mut agents = self.agents.write().await;
        agents.insert(id.clone(), agent);
    }

    // Create harness WITH cancellation token
    let harness = AgentHarness::new(provider, tools, max_context, system_prompt)
        .with_cancellation_token(cancel_token.clone()); // IMPORTANT!

    // Spawn the task
    let agents_clone = self.agents.clone();
    let handle = tokio::spawn(async move {
        let result = run_background_agent_loop(
            &harness,
            &mut state,
            &mut history,
            &task_description,
            &cancel_token.cancelled, // Pass the AtomicBool from CancellationToken
        ).await;

        // Update status
        let mut agents = agents_clone.write().await;
        if let Some(agent) = agents.get_mut(&id) {
            agent.status = match result {
                Ok(output) => BackgroundStatus::Completed(output),
                Err(e) => BackgroundStatus::Failed(e),
            };
        }
    });

    // Store handle for cancellation
    {
        let mut handles = self.handles.write().await;
        handles.insert(id.clone(), AgentHandle {
            cancel_token,
            pause_token: CancellationToken::new(),
            _join_handle: Some(handle),
        });
    }

    Ok(id)
}

// Implement cancel method
pub async fn cancel(&self, id: &str) -> Result<(), BackgroundError> {
    let handle = {
        let handles = self.handles.read().await;
        handles.get(id).cloned()
    };

    if let Some(handle) = handle {
        handle.cancel_token.cancel();
        
        // Update status
        let mut agents = self.agents.write().await;
        if let Some(agent) = agents.get_mut(id) {
            agent.status = BackgroundStatus::Cancelled;
        }
        
        Ok(())
    } else {
        Err(BackgroundError::NotFound(id.to_string()))
    }
}
```

---

### 7. Flat Execution Tree — **Score: 6/10**

**Issue:** `emit_tree_node(format!("turn-{}", iteration), None, ...)` always passes `parent_id: None`, so the tree is flat. Tool calls should be children of their iteration node.

**Location:** `crates/caduceus-orchestrator/src/lib.rs:954`

**Impact:**
- Poor visualization in UI
- Can't collapse/expand turn subtrees
- No hierarchical context for tool calls
- Harder to debug multi-tool turns

**Fix:**

```rust
// Store the parent node ID for each iteration
for iteration in 0..self.max_tool_rounds {
    self.check_cancellation()?;

    // ... circuit breaker check ...

    // Emit iteration node
    let iteration_node_id = format!("turn-{}", iteration);
    if let Some(ref em) = self.emitter {
        em.emit_thinking_started(iteration as u32).await;
        em.emit_tree_node(
            &iteration_node_id,
            None, // Top-level node
            format!("Turn {} — Thinking", iteration + 1),
            "running"
        ).await;
    }

    // ... assemble messages, call LLM ...

    // Process tool calls with parent set to iteration node
    for (tool_idx, tool_use) in response.tool_calls.iter().enumerate() {
        let tool_node_id = format!("turn-{}-tool-{}", iteration, tool_idx);
        
        if let Some(ref em) = self.emitter {
            // Emit tool node as CHILD of iteration node
            em.emit_tree_node(
                &tool_node_id,
                Some(iteration_node_id.clone()), // PARENT!
                format!("Tool: {}", tool_use.name),
                "running"
            ).await;
            
            em.emit_tool_call_start(
                caduceus_core::ToolCallId(tool_use.id.clone()),
                &tool_use.name
            ).await;
        }

        // Execute tool
        let result = self.tools.execute(&tool_use.name, tool_use.input.clone()).await;

        // ... handle result ...

        // Update tool node status
        if let Some(ref em) = self.emitter {
            let status = if is_error { "failed" } else { "completed" };
            em.emit_tree_update(&tool_node_id, status, None).await;
        }
    }

    // Update iteration node status
    if let Some(ref em) = self.emitter {
        em.emit_tree_update(&iteration_node_id, "completed", None).await;
    }
}
```

---

### 8. Empty ToolResult Content — **Score: 5/10**

**Issue:**
```rust
tool_result: Some(ToolResult::success("").with_tool_use_id(&tool_use.id)),
```
The `ToolResult`'s content field is empty string. The real content is in `msg.content`. This works for OpenAI (which uses `msg.content` for tool role), but for Anthropic which uses `content_blocks`, this might produce an empty `tool_result`.

**Location:** `crates/caduceus-orchestrator/src/lib.rs:1066`

**Impact:**
- Anthropic models may not see tool results correctly
- Could cause model to re-request same tool
- Provider-specific bug

**Fix:**

```rust
// Add tool result to history
let mut tool_msg = caduceus_providers::Message {
    role: "tool".into(),
    content: result_content.clone(), // Use actual content
    content_blocks: None,
    tool_calls: vec![],
    tool_result: Some(
        if is_error {
            caduceus_core::ToolResult::error(&result_content)
        } else {
            caduceus_core::ToolResult::success(&result_content)
        }
        .with_tool_use_id(&tool_use.id)
    ),
};
history.append(tool_msg);
```

**Better fix** — populate both `content` and `tool_result.content`:

```rust
let tool_result = if is_error {
    caduceus_core::ToolResult::error(&result_content)
} else {
    caduceus_core::ToolResult::success(&result_content)
}
.with_tool_use_id(&tool_use.id);

let mut tool_msg = caduceus_providers::Message {
    role: "tool".into(),
    content: result_content, // For OpenAI-style
    content_blocks: None,
    tool_calls: vec![],
    tool_result: Some(tool_result), // For Anthropic-style
};
history.append(tool_msg);
```

---

### 9. No Streaming in Tool Loop — **Score: 7/10**

**Issue:** `let response = self.provider.chat(request).await?` is non-streaming. For long responses, user sees nothing until the full response arrives. The emitter gets the full text as a single `TextDelta`, not incremental.

**Location:** `crates/caduceus-orchestrator/src/lib.rs:973`

**Impact:**
- Poor UX during long LLM responses
- No indication of progress
- Frontend can't start rendering until complete
- Defeats purpose of streaming events

**Context:** The comment says "non-streaming to get tool_calls" — this suggests the current provider abstraction may not support streaming tool calls.

**Fix Option 1:** Add streaming support to provider trait

```rust
// In caduceus-providers, add streaming method to LlmAdapter trait
#[async_trait::async_trait]
pub trait LlmAdapter: Send + Sync {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;
    
    // NEW: Streaming version
    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatStreamChunk>> + Send>>>;
}

// ChatStreamChunk carries incremental updates
pub enum ChatStreamChunk {
    TextDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallInputDelta { id: String, delta: String },
    ToolCallComplete { id: String, input: serde_json::Value },
    Complete { stop_reason: StopReason, usage: TokenUsage },
}
```

Then in `AgentHarness::run()`:

```rust
use futures::StreamExt;

let mut stream = self.provider.chat_stream(request).await?;
let mut accumulated_text = String::new();
let mut tool_calls_buffer: Vec<ToolUse> = vec![];
let mut stop_reason = StopReason::EndTurn;
let mut usage = TokenUsage::default();

while let Some(chunk_result) = stream.next().await {
    let chunk = chunk_result?;
    match chunk {
        ChatStreamChunk::TextDelta(text) => {
            accumulated_text.push_str(&text);
            if let Some(ref em) = self.emitter {
                em.emit_text_delta(&text).await; // Incremental!
            }
        }
        ChatStreamChunk::ToolCallStart { id, name } => {
            // Start buffering this tool call
        }
        ChatStreamChunk::ToolCallComplete { id, input } => {
            tool_calls_buffer.push(ToolUse { id, name: /*...*/, input });
        }
        ChatStreamChunk::Complete { stop_reason: sr, usage: u } => {
            stop_reason = sr;
            usage = u;
        }
    }
}

// Now process tool_calls_buffer as before
```

**Fix Option 2:** Keep non-streaming for tool calls, add streaming for final response

```rust
// Only stream when stop_reason != ToolUse
if response.stop_reason == StopReason::ToolUse {
    // Use non-streaming (current behavior)
    let response = self.provider.chat(request).await?;
    // ... process tool calls
} else {
    // Final response — use streaming
    let mut stream = self.provider.chat_stream(request).await?;
    while let Some(chunk) = stream.next().await {
        // emit text deltas incrementally
    }
}
```

**Recommended:** **Option 1** for full streaming, but requires provider updates. **Option 2** as interim fix.

---

### 10. Hardcoded Background Agent Model — **Score: 7/10**

**Issue:** Background agent creation hardcodes:
```rust
caduceus_core::ModelId::new("claude-sonnet-4.6"),
```

**Location:** Inferred from description, likely in `crates/caduceus-orchestrator/src/background.rs`

**Impact:**
- Users can't choose model for background agents
- Wastes money if user prefers cheaper model
- Ignores user's default model preference
- Inconsistent with foreground agent behavior

**Fix:**

```rust
// In BackgroundAgentManager::start(), accept model as parameter
pub async fn start(
    &self,
    task_description: String,
    model_id: Option<ModelId>, // NEW: optional model override
) -> Result<String, BackgroundError> {
    let id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();

    // Use provided model or fall back to user's default
    let model = model_id.unwrap_or_else(|| {
        // Load from config or session state
        self.config.default_model.clone()
    });

    let agent = BackgroundAgent {
        id: id.clone(),
        session_id: session_id.clone(),
        status: BackgroundStatus::Running,
        started_at: Utc::now(),
        task_description: task_description.clone(),
    };

    // Create session state with chosen model
    let mut state = SessionState::new(
        project_root,
        self.config.default_provider.clone(),
        model, // Use chosen model!
    );

    // ... rest unchanged
}
```

Or if model selection should come from session state:

```rust
// Accept session_id and load existing state
pub async fn start_from_session(
    &self,
    session_id: SessionId,
    task_description: String,
) -> Result<String, BackgroundError> {
    // Load existing session state (includes model choice)
    let state = self.storage.load_session(&session_id).await?
        .ok_or_else(|| BackgroundError::SessionNotFound(session_id.clone()))?;
    
    // Use state.model_id for the background agent
    // ... rest of setup
}
```

---

## Recommended Fix Priority

### P0 — Critical (Fix before v1.0)
1. **#4 — Race Condition** (Score 2/10) — Add session locks
2. **#6 — Cancellation Broken** (Score 1/10) — Wire up cancellation token
3. **#3 — Tool Timeout** (Score 3/10) — Add timeout wrapper

### P1 — High (Fix in v1.1)
4. **#1 — Error Cleanup** (Score 5/10) — Add guard pattern or catch-and-cleanup
5. **#8 — Empty ToolResult** (Score 5/10) — Populate content field
6. **#7 — Flat Tree** (Score 6/10) — Add parent_id to tool nodes
7. **#5 — Channel Backpressure** (Score 6/10) — Use unbounded or add logging

### P2 — Medium (Polish)
8. **#2 — MessagePart** (Score 7/10) — Emit structured parts
9. **#9 — Streaming** (Score 7/10) — Add streaming support (requires provider work)
10. **#10 — Model Selection** (Score 7/10) — Accept model parameter

---

## Testing Recommendations

Add integration tests for:

1. **Concurrent session access** — Spawn 2 tasks calling same session, verify no data loss
2. **Tool timeout** — Execute `sleep 999` tool, verify timeout triggers
3. **Cancellation** — Start background agent, cancel after 1s, verify it stops
4. **Error recovery** — Inject provider error mid-loop, verify phase returns to Idle
5. **Channel backpressure** — Block receiver, emit 1000 events, verify no deadlock
6. **Empty tool results** — Mock Anthropic provider, verify tool results have content
7. **Tree structure** — Emit tool calls, parse events, verify parent_id relationships

---

## Conclusion

The engine refactor has solid foundations but **critical robustness issues** exist in concurrency, cancellation, and error handling. The issues are well-scoped and fixable — none require architectural changes.

**Estimated fix effort:** 2-3 engineer-days for P0 items, 1-2 days for P1.

**Risk if unfixed:** Production incidents, data loss, hung sessions, poor UX.

**Recommendation:** Do not ship v1.0 without fixing #3, #4, and #6.
