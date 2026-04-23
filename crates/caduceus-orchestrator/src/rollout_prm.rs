//! Rollout-PRM verifier (gap G29 / P8.2): an LLM-as-judge implementation of
//! [`caduceus_core::StepVerifier`].
//!
//! Renders a [`StepView`] into a strict critic prompt, asks the configured
//! judge model to reply with a JSON object `{ reward: f32, rationale: str }`,
//! and parses the answer into a [`StepScore`]. On any failure (network,
//! timeout, malformed JSON, out-of-range reward) the verifier returns
//! [`StepScore::neutral`] with a diagnostic rationale rather than
//! propagating the error — process-reward signals are *advisory*, never
//! load-bearing.
//!
//! Inspired by Lightman et al. 2023 ("Let's Verify Step-by-Step") and Wang
//! et al. 2024 ("Math-Shepherd"), but trimmed to a single scalar so it
//! plugs into the existing self-consistency vote without a model retrain.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use caduceus_core::{ObservedToolCall, StepScore, StepVerifier, StepView};
use caduceus_providers::{
    ChatRequest, LlmAdapter, Message, ResponseFormat, StopReason as ProviderStopReason,
};

/// LLM-as-judge step verifier.
pub struct RolloutPrmVerifier {
    adapter: Arc<dyn LlmAdapter>,
    model: caduceus_core::ModelId,
    /// System prompt sent on every score request. Defaults to a built-in
    /// rubric; override via [`Self::with_system_prompt`] for domain-specific
    /// scoring (e.g. `caduceus.critic.security`).
    system_prompt: String,
    /// Hard wall-clock cap on the LLM call. Anything longer abstains.
    /// Defaults to 8 s — slow enough to allow Claude/GPT-class judges,
    /// fast enough that a stalled critic doesn't gate the agent loop.
    timeout: Duration,
}

impl RolloutPrmVerifier {
    pub fn new(adapter: Arc<dyn LlmAdapter>, model: caduceus_core::ModelId) -> Self {
        Self {
            adapter,
            model,
            system_prompt: DEFAULT_CRITIC_PROMPT.to_string(),
            timeout: Duration::from_secs(8),
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn render_user_prompt(&self, step: &StepView) -> String {
        let tools = if step.tool_calls.is_empty() {
            "(none)".to_string()
        } else {
            step.tool_calls
                .iter()
                .map(render_tool_call)
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "STEP_ID: {}\n\nPROMPT:\n{}\n\nASSISTANT_REPLY:\n{}\n\nTOOL_CALLS:\n{}\n\nReturn JSON only: {{\"reward\": <float in [-1.0, 1.0]>, \"rationale\": <short string>}}",
            step.step_id,
            step.prompt,
            if step.assistant_text.is_empty() { "(empty)" } else { &step.assistant_text },
            tools,
        )
    }
}

fn render_tool_call(t: &ObservedToolCall) -> String {
    let status = if t.is_error { "ERROR" } else { "ok" };
    format!(
        "  - {} [{}] args={} result={}",
        t.name, status, t.args_summary, t.result_summary
    )
}

#[async_trait]
impl StepVerifier for RolloutPrmVerifier {
    fn name(&self) -> &str {
        "rollout-prm"
    }

    async fn score(&self, step: &StepView) -> StepScore {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![Message::user(self.render_user_prompt(step))],
            system: Some(self.system_prompt.clone()),
            max_tokens: 256,
            temperature: Some(0.0),
            thinking_mode: false,
            tool_choice: None,
            response_format: Some(ResponseFormat::JsonObject),
            tools: Vec::new().into(),
            logprobs: None,
        };

        let source = format!("rollout-prm:{}", self.model);

        let call = self.adapter.chat(request);
        let resp = match tokio::time::timeout(self.timeout, call).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                return StepScore::neutral(source, format!("verifier error: {e}"));
            }
            Err(_) => {
                return StepScore::neutral(
                    source,
                    format!("verifier timeout after {:?}", self.timeout),
                );
            }
        };

        // We intentionally accept either EndTurn or any other stop reason —
        // the body is what matters. Refuse on completely empty text.
        if resp.content.trim().is_empty() {
            let reason = match resp.stop_reason {
                ProviderStopReason::MaxTokens => "max_tokens",
                ProviderStopReason::Error => "error",
                _ => "empty",
            };
            return StepScore::neutral(
                source,
                format!("verifier returned no usable text ({reason})"),
            );
        }

        match parse_critic_response(&resp.content) {
            Ok(parsed) => StepScore::new(parsed.reward, parsed.rationale, source),
            Err(e) => StepScore::neutral(source, format!("malformed critic JSON: {e}")),
        }
    }
}

#[derive(Debug)]
struct ParsedScore {
    reward: f32,
    rationale: String,
}

fn parse_critic_response(body: &str) -> Result<ParsedScore, String> {
    let trimmed = strip_code_fence(body);
    let v: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("not JSON: {e}"))?;
    let reward = v
        .get("reward")
        .and_then(|r| r.as_f64())
        .ok_or_else(|| "missing or non-numeric `reward`".to_string())?;
    let rationale = v
        .get("rationale")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string();
    Ok(ParsedScore {
        reward: reward as f32,
        rationale,
    })
}

/// Some judge models return ```json …``` even when asked for raw JSON.
/// Strip a single leading code fence if present.
fn strip_code_fence(body: &str) -> &str {
    let trimmed = body.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        rest.trim().trim_end_matches("```").trim()
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest.trim().trim_end_matches("```").trim()
    } else {
        trimmed
    }
}

const DEFAULT_CRITIC_PROMPT: &str =
    "You are a strict judge of a single step taken by an autonomous coding agent. \
You reply with ONE JSON object and nothing else: \
{\"reward\": <float in [-1.0, 1.0]>, \"rationale\": <short single-sentence string>}. \
Score 1.0 only if the step is clearly correct, useful, and matches the prompt. \
Score 0.0 if the step is plausible but unverifiable. \
Score -1.0 only if the step is clearly wrong, harmful, or off-topic. \
Tool errors that the agent then handled gracefully are not by themselves a negative score.";

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_providers::{mock::MockLlmAdapter, ChatResponse};

    fn judge_response(json: &str) -> ChatResponse {
        ChatResponse {
            content: json.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            stop_reason: caduceus_core::StopReason::EndTurn,
            tool_calls: Vec::new(),
            logprobs: None,
            thinking: String::new(),
        }
    }
    fn make_verifier(scripted: Vec<ChatResponse>) -> RolloutPrmVerifier {
        let adapter = Arc::new(MockLlmAdapter::new(scripted));
        let model = caduceus_core::ModelId::new("test-judge");
        RolloutPrmVerifier::new(adapter, model)
    }

    fn step() -> StepView {
        StepView::new(1, "do the thing", "ok did the thing")
    }

    #[tokio::test]
    async fn happy_path_parses_reward_and_rationale() {
        let v = make_verifier(vec![judge_response(
            r#"{"reward": 0.75, "rationale": "looks good"}"#,
        )]);
        let s = v.score(&step()).await;
        assert!((s.reward - 0.75).abs() < f32::EPSILON);
        assert_eq!(s.rationale, "looks good");
        assert!(s.source.starts_with("rollout-prm:"));
    }

    #[tokio::test]
    async fn out_of_range_reward_is_clamped_by_step_score() {
        let v = make_verifier(vec![judge_response(
            r#"{"reward": 12.0, "rationale": "x"}"#,
        )]);
        let s = v.score(&step()).await;
        assert_eq!(s.reward, 1.0, "must clamp via StepScore::new");
    }

    #[tokio::test]
    async fn nan_reward_snaps_to_zero() {
        // JSON can't carry NaN literally; simulate via an absurd-but-finite
        // value parsed as f32 then clamped. NaN snap is covered by core
        // tests; here we ensure a missing reward field abstains.
        let v = make_verifier(vec![judge_response(r#"{"rationale": "no reward field"}"#)]);
        let s = v.score(&step()).await;
        assert_eq!(s.reward, 0.0);
        assert!(s.rationale.contains("malformed"));
    }

    #[tokio::test]
    async fn malformed_json_returns_neutral_with_diagnostic() {
        let v = make_verifier(vec![judge_response("not json at all")]);
        let s = v.score(&step()).await;
        assert_eq!(s.reward, 0.0);
        assert!(s.rationale.contains("malformed"), "got: {}", s.rationale);
    }

    #[tokio::test]
    async fn code_fenced_json_is_accepted() {
        let v = make_verifier(vec![judge_response(
            "```json\n{\"reward\": -0.4, \"rationale\": \"meh\"}\n```",
        )]);
        let s = v.score(&step()).await;
        assert!((s.reward - (-0.4)).abs() < 1e-5);
        assert_eq!(s.rationale, "meh");
    }

    #[tokio::test]
    async fn empty_response_returns_neutral() {
        let mut resp = judge_response("   ");
        resp.stop_reason = caduceus_core::StopReason::MaxTokens;
        let v = make_verifier(vec![resp]);
        let s = v.score(&step()).await;
        assert_eq!(s.reward, 0.0);
        assert!(s.rationale.contains("max_tokens"));
    }

    #[tokio::test]
    async fn provider_error_returns_neutral_with_error_rationale() {
        // No scripted response → MockLlmAdapter returns a Provider error.
        let v = make_verifier(vec![]);
        let s = v.score(&step()).await;
        assert_eq!(s.reward, 0.0);
        assert!(
            s.rationale.contains("verifier error"),
            "got: {}",
            s.rationale
        );
    }

    #[tokio::test]
    async fn timeout_returns_neutral_with_timeout_rationale() {
        // Adapter that never returns within the verifier's timeout.
        struct StallAdapter {
            pid: caduceus_core::ProviderId,
        }
        #[async_trait]
        impl LlmAdapter for StallAdapter {
            fn provider_id(&self) -> &caduceus_core::ProviderId {
                &self.pid
            }
            async fn chat(&self, _: ChatRequest) -> caduceus_core::Result<ChatResponse> {
                tokio::time::sleep(Duration::from_secs(60)).await;
                unreachable!()
            }
            async fn stream(
                &self,
                _: ChatRequest,
            ) -> caduceus_core::Result<caduceus_providers::StreamResult> {
                unreachable!()
            }
            async fn list_models(&self) -> caduceus_core::Result<Vec<caduceus_core::ModelId>> {
                Ok(Vec::new())
            }
        }
        let v = RolloutPrmVerifier::new(
            Arc::new(StallAdapter {
                pid: caduceus_core::ProviderId::new("stall"),
            }),
            caduceus_core::ModelId::new("test-judge"),
        )
        .with_timeout(Duration::from_millis(50));
        let s = v.score(&step()).await;
        assert_eq!(s.reward, 0.0);
        assert!(s.rationale.contains("timeout"), "got: {}", s.rationale);
    }

    #[tokio::test]
    async fn rendered_prompt_includes_tool_calls_when_present() {
        let adapter = Arc::new(MockLlmAdapter::new(vec![judge_response(
            r#"{"reward": 0.0, "rationale": "ok"}"#,
        )]));
        let v = RolloutPrmVerifier::new(adapter.clone(), caduceus_core::ModelId::new("test-judge"));
        let step = StepView::new(1, "p", "a").with_tool_calls(vec![ObservedToolCall {
            name: "shell".into(),
            args_summary: "ls".into(),
            result_summary: "out".into(),
            is_error: false,
        }]);
        let _ = v.score(&step).await;
        let recorded = adapter.recorded_requests();
        assert_eq!(recorded.len(), 1);
        let body = &recorded[0].messages[0].content;
        assert!(body.contains("shell"), "rendered prompt missing tool name");
        assert!(body.contains("[ok]"), "rendered prompt missing status");
    }
}
