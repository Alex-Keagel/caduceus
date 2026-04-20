//! P13.12 (G‑R2.1 / G‑R4.1 / G‑R5.1) — `RetryAdapter` wrapping any
//! [`LlmAdapter`] with exponential backoff + multi‑adapter failover.
//!
//! HTTP‑layer retries already exist inside the Anthropic / OpenAI /
//! Gemini adapters, but they only handle network‑level transient
//! errors. `RetryAdapter` sits ABOVE the adapter trait so it can:
//!
//! * Retry whole `chat()` calls when the adapter itself returns a
//!   classified‑transient error ([`is_transient_error`]).
//! * Fall over to one or more backup adapters after the primary's
//!   retry budget is exhausted — useful for "Anthropic → OpenAI"
//!   degradation under regional outages.
//! * Emit an optional callback on every fallover so callers can wire
//!   it into their telemetry pipeline (the orchestrator does this
//!   to emit a `ProviderFailover` event).
//!
//! The wrapper is intentionally minimal: it does NOT attempt to
//! re‑shape requests for backup providers. Callers must ensure the
//! backup adapters can serve the same `ChatRequest` (typically by
//! using the same model family or a translation layer).

use crate::{ChatRequest, ChatResponse, LlmAdapter, StreamResult};
use async_trait::async_trait;
use caduceus_core::{CaduceusError, ModelId, ProviderId, Result};
use std::sync::Arc;
use std::time::Duration;

/// Retry / failover policy for [`RetryAdapter`].
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum attempts AGAINST EACH ADAPTER (primary + each backup).
    /// `1` disables retry but still allows failover. `0` is treated
    /// as `1` (we always make at least one attempt).
    pub max_attempts: u32,
    /// Delay before attempt #2.
    pub base_delay: Duration,
    /// Cap on per‑attempt delay (exponential growth saturates here).
    pub max_delay: Duration,
    /// Multiplier between consecutive delays.
    pub multiplier: f32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(8),
            multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    /// Compute the sleep duration before retry attempt `attempt`
    /// (1‑indexed against the next attempt: `delay_before(1)` is the
    /// wait between attempt 1 and attempt 2).
    pub fn delay_before(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let exp = (attempt - 1) as i32;
        let mul = (self.multiplier as f64).powi(exp);
        let nanos = (self.base_delay.as_nanos() as f64 * mul) as u128;
        let cap = self.max_delay.as_nanos();
        Duration::from_nanos(nanos.min(cap) as u64)
    }
}

/// Optional fallover callback signature: `(from_provider, to_provider, attempts_used)`.
pub type FailoverHook = Arc<dyn Fn(&str, &str, u32) + Send + Sync>;

/// Wraps a primary [`LlmAdapter`] with retry + optional failover to
/// a chain of backup adapters. Calls the wrapped adapter's `chat`
/// up to `policy.max_attempts` times on transient errors, then
/// moves to the next backup. Non‑transient errors short‑circuit
/// (no point retrying a 400 Bad Request).
pub struct RetryAdapter {
    primary: Arc<dyn LlmAdapter>,
    backups: Vec<Arc<dyn LlmAdapter>>,
    policy: RetryPolicy,
    on_failover: Option<FailoverHook>,
}

impl RetryAdapter {
    pub fn new(primary: Arc<dyn LlmAdapter>) -> Self {
        Self {
            primary,
            backups: Vec::new(),
            policy: RetryPolicy::default(),
            on_failover: None,
        }
    }

    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Append a single backup adapter to the failover chain.
    /// Backups are tried in registration order.
    pub fn with_failover(mut self, backup: Arc<dyn LlmAdapter>) -> Self {
        self.backups.push(backup);
        self
    }

    /// Bulk‑register a chain of backups.
    pub fn with_failover_chain(mut self, backups: Vec<Arc<dyn LlmAdapter>>) -> Self {
        self.backups.extend(backups);
        self
    }

    /// Attach a callback fired ONCE per fallover transition. Useful
    /// for emitting a `ProviderFailover` telemetry event.
    pub fn with_failover_hook(mut self, hook: FailoverHook) -> Self {
        self.on_failover = Some(hook);
        self
    }
}

/// Returns `true` for errors the wrapper should retry / fall over on.
/// Conservative: only well‑known transient categories qualify.
/// Anything else (4xx, schema errors, cancellation, IO) is bubbled
/// immediately so the caller sees the real problem.
pub fn is_transient_error(err: &CaduceusError) -> bool {
    match err {
        CaduceusError::RateLimited { .. } => true,
        CaduceusError::Provider(msg) => {
            let lower = msg.to_lowercase();
            // Match any of the canonical transient signals across
            // Anthropic / OpenAI / Gemini error strings.
            lower.contains("overloaded")
                || lower.contains("timeout")
                || lower.contains("timed out")
                || lower.contains("503")
                || lower.contains("502")
                || lower.contains("504")
                || lower.contains("internal server error")
                || lower.contains("service unavailable")
                || lower.contains("connection reset")
                || lower.contains("connection refused")
        }
        CaduceusError::Other(e) => {
            let lower = format!("{e:#}").to_lowercase();
            lower.contains("timeout") || lower.contains("connection")
        }
        _ => false,
    }
}

#[async_trait]
impl LlmAdapter for RetryAdapter {
    fn provider_id(&self) -> &ProviderId {
        self.primary.provider_id()
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let mut last_err: Option<CaduceusError> = None;

        // Iterate primary then each backup in order.
        let chain: Vec<&Arc<dyn LlmAdapter>> = std::iter::once(&self.primary)
            .chain(self.backups.iter())
            .collect();

        for (chain_idx, adapter) in chain.iter().enumerate() {
            let attempts = self.policy.max_attempts.max(1);
            for attempt in 0..attempts {
                if attempt > 0 {
                    tokio::time::sleep(self.policy.delay_before(attempt)).await;
                }
                match adapter.chat(request.clone()).await {
                    Ok(resp) => return Ok(resp),
                    Err(e) => {
                        let transient = is_transient_error(&e);
                        last_err = Some(e);
                        if !transient {
                            // Hard error — don't retry, but DO try next
                            // adapter in case it has different
                            // capabilities (e.g., 400 from one provider
                            // might succeed on another).
                            break;
                        }
                    }
                }
            }
            // Exhausted this adapter; if there's another, fire hook.
            if chain_idx + 1 < chain.len() {
                if let Some(hook) = &self.on_failover {
                    let from = adapter.provider_id().0.as_str();
                    let to = chain[chain_idx + 1].provider_id().0.as_str();
                    hook(from, to, attempts);
                }
            }
        }

        Err(last_err.unwrap_or(CaduceusError::Provider(
            "RetryAdapter: no attempts made (empty chain?)".into(),
        )))
    }

    async fn stream(&self, request: ChatRequest) -> Result<StreamResult> {
        // Streaming retry is genuinely tricky (partial output already
        // flushed), so for now we delegate to primary only and let
        // upper layers handle stream-restart policy.
        self.primary.stream(request).await
    }

    async fn list_models(&self) -> Result<Vec<ModelId>> {
        self.primary.list_models().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatRequest;
    use caduceus_core::StopReason;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Test adapter that returns a programmable sequence of results
    /// and records its provider_id and call count.
    struct ScriptedAdapter {
        id: ProviderId,
        results: Mutex<Vec<Result<ChatResponse>>>,
        calls: AtomicUsize,
    }

    impl ScriptedAdapter {
        fn new(id: &str, results: Vec<Result<ChatResponse>>) -> Self {
            Self {
                id: ProviderId::new(id),
                results: Mutex::new(results),
                calls: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LlmAdapter for ScriptedAdapter {
        fn provider_id(&self) -> &ProviderId {
            &self.id
        }
        async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.results
                .lock()
                .unwrap()
                .pop()
                .map(|r| {
                    r.map_err(|e| match e {
                        CaduceusError::RateLimited { retry_after_secs } => {
                            CaduceusError::RateLimited { retry_after_secs }
                        }
                        CaduceusError::Provider(s) => CaduceusError::Provider(s),
                        other => CaduceusError::Provider(format!("{other}")),
                    })
                })
                .unwrap_or_else(|| Err(CaduceusError::Provider("ScriptedAdapter exhausted".into())))
        }
        async fn stream(&self, _req: ChatRequest) -> Result<StreamResult> {
            unimplemented!("streaming not used in retry tests")
        }
        async fn list_models(&self) -> Result<Vec<ModelId>> {
            Ok(vec![])
        }
    }

    fn ok_response(text: &str) -> ChatResponse {
        ChatResponse {
            content: text.into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: StopReason::EndTurn,
            tool_calls: vec![],
            logprobs: None,
        }
    }

    fn fast_policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            multiplier: 2.0,
        }
    }

    fn req() -> ChatRequest {
        ChatRequest {
            model: ModelId::new("m"),
            messages: vec![],
            system: None,
            max_tokens: 8,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
            tools: vec![],
            logprobs: None,
        }
    }

    #[test]
    fn p13_12_classifies_rate_limit_as_transient() {
        assert!(is_transient_error(&CaduceusError::RateLimited {
            retry_after_secs: 1
        }));
    }

    #[test]
    fn p13_12_classifies_overloaded_as_transient() {
        assert!(is_transient_error(&CaduceusError::Provider(
            "Anthropic returned 529 Overloaded".into()
        )));
    }

    #[test]
    fn p13_12_classifies_400_as_non_transient() {
        assert!(!is_transient_error(&CaduceusError::Provider(
            "400 Bad Request: malformed JSON".into()
        )));
        assert!(!is_transient_error(&CaduceusError::Provider(
            "Authentication failed: invalid API key".into()
        )));
    }

    #[tokio::test]
    async fn p13_12_retries_transient_then_succeeds() {
        // pop() returns LAST first → reverse order: [error, error, ok]
        let adapter = Arc::new(ScriptedAdapter::new(
            "primary",
            vec![
                Ok(ok_response("ok")), // 3rd attempt
                Err(CaduceusError::RateLimited {
                    retry_after_secs: 0,
                }), // 2nd
                Err(CaduceusError::RateLimited {
                    retry_after_secs: 0,
                }), // 1st
            ],
        ));
        let retry = RetryAdapter::new(adapter.clone()).with_policy(fast_policy());
        let r = retry.chat(req()).await.unwrap();
        assert_eq!(r.content, "ok");
        assert_eq!(adapter.calls(), 3, "should have retried twice");
    }

    #[tokio::test]
    async fn p13_12_fails_over_after_primary_exhausted() {
        // Primary always rate-limited (3 attempts), backup succeeds.
        let primary = Arc::new(ScriptedAdapter::new(
            "primary",
            vec![
                Err(CaduceusError::RateLimited {
                    retry_after_secs: 0,
                }),
                Err(CaduceusError::RateLimited {
                    retry_after_secs: 0,
                }),
                Err(CaduceusError::RateLimited {
                    retry_after_secs: 0,
                }),
            ],
        ));
        let backup = Arc::new(ScriptedAdapter::new(
            "backup",
            vec![Ok(ok_response("backup-ok"))],
        ));
        let hook_calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let hook_calls_inner = hook_calls.clone();
        let retry = RetryAdapter::new(primary.clone())
            .with_policy(fast_policy())
            .with_failover(backup.clone())
            .with_failover_hook(Arc::new(move |from, to, attempts| {
                hook_calls_inner
                    .lock()
                    .unwrap()
                    .push(format!("{from}->{to}@{attempts}"));
            }));
        let r = retry.chat(req()).await.unwrap();
        assert_eq!(r.content, "backup-ok");
        assert_eq!(primary.calls(), 3);
        assert_eq!(backup.calls(), 1);
        let calls = hook_calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "hook fires exactly once at the transition");
        assert_eq!(calls[0], "primary->backup@3");
    }

    #[tokio::test]
    async fn p13_12_non_transient_short_circuits_to_failover_immediately() {
        // Primary returns a 400 (non-transient) → don't waste retries,
        // jump straight to backup.
        let primary = Arc::new(ScriptedAdapter::new(
            "primary",
            vec![Err(CaduceusError::Provider("400 Bad Request".into()))],
        ));
        let backup = Arc::new(ScriptedAdapter::new("backup", vec![Ok(ok_response("ok"))]));
        let retry = RetryAdapter::new(primary.clone())
            .with_policy(fast_policy())
            .with_failover(backup.clone());
        let r = retry.chat(req()).await.unwrap();
        assert_eq!(r.content, "ok");
        assert_eq!(primary.calls(), 1, "non-transient must not retry");
        assert_eq!(backup.calls(), 1);
    }

    #[tokio::test]
    async fn p13_12_all_adapters_exhausted_returns_last_error() {
        let primary = Arc::new(ScriptedAdapter::new(
            "primary",
            vec![Err(CaduceusError::Provider("primary down".into()))],
        ));
        let backup = Arc::new(ScriptedAdapter::new(
            "backup",
            vec![Err(CaduceusError::Provider("backup down".into()))],
        ));
        let retry = RetryAdapter::new(primary.clone())
            .with_policy(fast_policy())
            .with_failover(backup.clone());
        let err = retry.chat(req()).await.unwrap_err();
        let s = format!("{err}");
        assert!(
            s.contains("backup down"),
            "must surface LAST error, got: {s}"
        );
    }

    #[test]
    fn p13_12_policy_delay_grows_then_caps() {
        let p = RetryPolicy {
            max_attempts: 6,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(40),
            multiplier: 2.0,
        };
        assert_eq!(p.delay_before(0), Duration::ZERO);
        assert_eq!(p.delay_before(1), Duration::from_millis(10));
        assert_eq!(p.delay_before(2), Duration::from_millis(20));
        assert_eq!(p.delay_before(3), Duration::from_millis(40));
        // Caps at max_delay.
        assert_eq!(p.delay_before(4), Duration::from_millis(40));
        assert_eq!(p.delay_before(10), Duration::from_millis(40));
    }
}
