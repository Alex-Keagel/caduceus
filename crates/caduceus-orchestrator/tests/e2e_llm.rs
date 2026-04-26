//! E2E integration tests that call real LLM APIs.
//!
//! These tests are gated by environment variables:
//! - `ANTHROPIC_API_KEY` → Anthropic tests
//! - `OPENAI_API_KEY` → OpenAI tests
//! - `GITHUB_TOKEN` or `GH_TOKEN` → Copilot tests
//!
//! Run with: `cargo test --test e2e_llm -- --ignored`
//! Or in CI: set the env vars and run normally.

use caduceus_core::{ModelId, ProviderId, SessionPhase, SessionState, ToolSpec};
use caduceus_orchestrator::{AgentHarness, ConversationHistory};
use caduceus_providers::LlmAdapter;
use caduceus_tools::ToolRegistry;
use std::sync::Arc;

fn get_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn anthropic_key() -> Option<String> {
    get_env("ANTHROPIC_API_KEY")
}

fn openai_key() -> Option<String> {
    get_env("OPENAI_API_KEY")
}

fn copilot_key() -> Option<String> {
    get_env("GITHUB_TOKEN")
        .or_else(|| get_env("GH_TOKEN"))
        .or_else(|| get_env("COPILOT_GITHUB_TOKEN"))
}

fn make_anthropic(key: &str) -> Arc<dyn LlmAdapter> {
    Arc::new(caduceus_providers::AnthropicAdapter::new(key))
}

fn make_openai(key: &str) -> Arc<dyn LlmAdapter> {
    Arc::new(caduceus_providers::OpenAiCompatibleAdapter::new(
        "openai",
        key,
        "https://api.openai.com/v1",
    ))
}

fn make_copilot(key: &str) -> Arc<dyn LlmAdapter> {
    Arc::new(caduceus_providers::OpenAiCompatibleAdapter::new(
        "copilot",
        key,
        "https://api.githubcopilot.com",
    ))
}

fn make_registry() -> ToolRegistry {
    ToolRegistry::new()
}

fn make_state(model: &str) -> SessionState {
    SessionState::new(".", ProviderId::new("test"), ModelId::new(model))
}

fn tool_spec(name: &str, desc: &str, schema: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: desc.into(),
        input_schema: schema,
        required_capability: None,
    }
}

// ── Anthropic E2E Tests ───────────────────────────────────────────────────────

#[tokio::test]
#[ignore] // Run with --ignored or when ANTHROPIC_API_KEY is set
async fn e2e_anthropic_simple_chat() {
    let Some(key) = anthropic_key() else {
        eprintln!("Skipping: ANTHROPIC_API_KEY not set");
        return;
    };
    let provider = make_anthropic(&key);
    let request = caduceus_providers::ChatRequest {
        model: ModelId::new("claude-sonnet-4-20250514"),
        messages: vec![caduceus_providers::Message::user(
            "What is 2+2? Answer with just the number.",
        )]
        .into(),
        system: None,
        max_tokens: 32,
        temperature: Some(0.0),
        thinking_mode: false,
        tool_choice: None,
        tools: vec![].into(),
        response_format: None,
        logprobs: None,
        thread_id: None,
        prompt_id: None,
        intent: None,
        stop: vec![],
        thinking_effort: None,
        speed: None,
    };

    let response = provider.chat(request).await.expect("Anthropic chat failed");
    assert!(
        response.content.contains('4'),
        "Expected '4' in response: {}",
        response.content
    );
    assert!(response.input_tokens > 0, "Should report input tokens");
    assert!(response.output_tokens > 0, "Should report output tokens");
}

#[tokio::test]
#[ignore]
async fn e2e_anthropic_tool_call() {
    let Some(key) = anthropic_key() else {
        return;
    };
    let provider = make_anthropic(&key);
    let tools = vec![tool_spec(
        "get_weather",
        "Get weather for a city",
        serde_json::json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"]
        }),
    )];

    let request = caduceus_providers::ChatRequest {
        model: ModelId::new("claude-sonnet-4-20250514"),
        messages: vec![caduceus_providers::Message::user(
            "What's the weather in Tokyo?",
        )]
        .into(),
        system: None,
        max_tokens: 256,
        temperature: Some(0.0),
        thinking_mode: false,
        tool_choice: None,
        tools: tools.into(),
        response_format: None,
        logprobs: None,
        thread_id: None,
        prompt_id: None,
        intent: None,
        stop: vec![],
        thinking_effort: None,
        speed: None,
    };

    let response = provider
        .chat(request)
        .await
        .expect("Anthropic tool call failed");
    assert!(
        !response.tool_calls.is_empty(),
        "Should request tool call, got: {}",
        response.content
    );
    assert_eq!(response.tool_calls[0].name, "get_weather");
    let args = &response.tool_calls[0].input;
    assert!(
        args.get("city").is_some(),
        "Tool call should have 'city' arg"
    );
}

#[tokio::test]
#[ignore]
async fn e2e_anthropic_full_agent_turn() {
    let Some(key) = anthropic_key() else {
        return;
    };
    let provider = make_anthropic(&key);
    let registry = make_registry();
    let mut state = make_state("claude-sonnet-4-20250514");

    let harness = AgentHarness::new(
        provider,
        registry,
        100_000,
        "You are a helpful assistant. Be concise.",
    )
    .with_tool_timeout(std::time::Duration::from_secs(30));

    let mut history = ConversationHistory::new();
    let result = harness
        .run(
            &mut state,
            &mut history,
            "What is the capital of France? One word answer.",
        )
        .await;

    let text = result.expect("Agent turn failed");
    assert!(
        text.to_lowercase().contains("paris"),
        "Expected 'Paris' in response: {}",
        text
    );
    assert!(state.turn_count > 0, "Turn count should increment");
    assert_eq!(state.phase, SessionPhase::Idle, "Should return to idle");
}

// ── OpenAI E2E Tests ──────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn e2e_openai_simple_chat() {
    let Some(key) = openai_key() else {
        eprintln!("Skipping: OPENAI_API_KEY not set");
        return;
    };
    let provider = make_openai(&key);
    let request = caduceus_providers::ChatRequest {
        model: ModelId::new("gpt-4o-mini"),
        messages: vec![caduceus_providers::Message::user(
            "What is 3+3? Answer with just the number.",
        )]
        .into(),
        system: None,
        max_tokens: 32,
        temperature: Some(0.0),
        thinking_mode: false,
        tool_choice: None,
        tools: vec![].into(),
        response_format: None,
        logprobs: None,
        thread_id: None,
        prompt_id: None,
        intent: None,
        stop: vec![],
        thinking_effort: None,
        speed: None,
    };

    let response = provider.chat(request).await.expect("OpenAI chat failed");
    assert!(
        response.content.contains('6'),
        "Expected '6' in response: {}",
        response.content
    );
}

#[tokio::test]
#[ignore]
async fn e2e_openai_tool_call() {
    let Some(key) = openai_key() else {
        return;
    };
    let provider = make_openai(&key);
    let tools = vec![tool_spec(
        "calculate",
        "Perform a calculation",
        serde_json::json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string" }
            },
            "required": ["expression"]
        }),
    )];

    let request = caduceus_providers::ChatRequest {
        model: ModelId::new("gpt-4o-mini"),
        messages: vec![caduceus_providers::Message::user("Calculate 15 * 7")].into(),
        system: None,
        max_tokens: 256,
        temperature: Some(0.0),
        thinking_mode: false,
        tool_choice: None,
        tools: tools.into(),
        response_format: None,
        logprobs: None,
        thread_id: None,
        prompt_id: None,
        intent: None,
        stop: vec![],
        thinking_effort: None,
        speed: None,
    };

    let response = provider
        .chat(request)
        .await
        .expect("OpenAI tool call failed");
    assert!(!response.tool_calls.is_empty(), "Should request tool call");
    assert_eq!(response.tool_calls[0].name, "calculate");
}

// ── Copilot E2E Tests ─────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn e2e_copilot_simple_chat() {
    let Some(key) = copilot_key() else {
        eprintln!("Skipping: GITHUB_TOKEN not set");
        return;
    };
    let provider = make_copilot(&key);
    let request = caduceus_providers::ChatRequest {
        model: ModelId::new("gpt-4o"),
        messages: vec![caduceus_providers::Message::user(
            "What is 5+5? Answer with just the number.",
        )]
        .into(),
        system: None,
        max_tokens: 32,
        temperature: Some(0.0),
        thinking_mode: false,
        tool_choice: None,
        tools: vec![].into(),
        response_format: None,
        logprobs: None,
        thread_id: None,
        prompt_id: None,
        intent: None,
        stop: vec![],
        thinking_effort: None,
        speed: None,
    };

    let response = provider.chat(request).await.expect("Copilot chat failed");
    assert!(
        response.content.contains("10"),
        "Expected '10' in response: {}",
        response.content
    );
}

// ── Cross-Provider Tests ──────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn e2e_agent_harness_with_tools() {
    // Use whichever provider has a key
    let (provider, model) = if let Some(key) = anthropic_key() {
        (make_anthropic(&key), "claude-sonnet-4-20250514")
    } else if let Some(key) = openai_key() {
        (make_openai(&key), "gpt-4o-mini")
    } else {
        eprintln!("Skipping: no LLM API key available");
        return;
    };

    let registry = make_registry();
    let mut state = make_state(model);

    let harness = AgentHarness::new(
        provider,
        registry,
        100_000,
        "You are a coding assistant. Use tools when asked to read or write files.",
    )
    .with_tool_timeout(std::time::Duration::from_secs(30))
    .with_max_turns(5);

    let mut history = ConversationHistory::new();
    let result = harness
        .run(
            &mut state,
            &mut history,
            "List the files in the current directory using the bash tool.",
        )
        .await;

    let text = result.expect("Agent with tools failed");
    // Should have used at least one tool and returned file listing
    assert!(!text.is_empty(), "Response should not be empty");
    assert!(
        history.len() >= 3,
        "Should have user + assistant + tool messages, got {}",
        history.len()
    );
}

#[tokio::test]
#[ignore]
async fn e2e_run_turn_stateless() {
    let (provider, model) = if let Some(key) = anthropic_key() {
        (make_anthropic(&key), "claude-sonnet-4-20250514")
    } else if let Some(key) = openai_key() {
        (make_openai(&key), "gpt-4o-mini")
    } else {
        return;
    };

    let registry = make_registry();
    let mut state = make_state(model);

    let harness = AgentHarness::new(provider, registry, 100_000, "Be concise.")
        .with_tool_timeout(std::time::Duration::from_secs(30));

    let result = harness.run_turn(&mut state, "Say hello").await;
    let text = result.expect("run_turn failed");
    assert!(!text.is_empty(), "Should return a response");
}
