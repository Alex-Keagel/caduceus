use caduceus_core::{ModelId, ProviderId, Result, SessionState, TokenUsage};
use serde::{Deserialize, Serialize};

use crate::modes::AgentMode;

// ── Output format ──────────────────────────────────────────────────────────────

/// Controls the serialization format for headless CLI output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OutputFormat {
    /// Plain text output, suitable for piping.
    #[default]
    Text,
    /// Structured JSON with status, output, tokens, cost.
    Json,
    /// Compact single-line summary.
    Compact,
}

impl OutputFormat {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "text" | "txt" => Some(Self::Text),
            "json" => Some(Self::Json),
            "compact" => Some(Self::Compact),
            _ => None,
        }
    }
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text => write!(f, "text"),
            Self::Json => write!(f, "json"),
            Self::Compact => write!(f, "compact"),
        }
    }
}

// ── Headless configuration ─────────────────────────────────────────────────────

/// Configuration for non-interactive (headless) execution.
#[derive(Debug, Clone)]
pub struct HeadlessConfig {
    /// The prompt to execute in one-shot mode.
    pub prompt: String,
    /// If true, only output the final text (no streaming UI).
    pub print_only: bool,
    /// Output format (text, json, compact).
    pub output_format: OutputFormat,
    /// Agent execution mode to use.
    pub mode: Option<AgentMode>,
    /// Provider to use.
    pub provider: Option<ProviderId>,
    /// Model to use.
    pub model: Option<ModelId>,
}

impl HeadlessConfig {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            print_only: false,
            output_format: OutputFormat::Text,
            mode: None,
            provider: None,
            model: None,
        }
    }

    pub fn with_print_only(mut self, print_only: bool) -> Self {
        self.print_only = print_only;
        self
    }

    pub fn with_output_format(mut self, format: OutputFormat) -> Self {
        self.output_format = format;
        self
    }

    pub fn with_mode(mut self, mode: AgentMode) -> Self {
        self.mode = Some(mode);
        self
    }

    pub fn with_provider(mut self, provider: ProviderId) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn with_model(mut self, model: ModelId) -> Self {
        self.model = Some(model);
        self
    }
}

// ── Headless result ────────────────────────────────────────────────────────────

/// The result of a headless execution, serializable for JSON output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessResult {
    pub status: String,
    pub output: String,
    pub tokens: TokenSummary,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSummary {
    pub input: u32,
    pub output: u32,
}

impl HeadlessResult {
    pub fn success(output: impl Into<String>, usage: &TokenUsage) -> Self {
        Self {
            status: "success".into(),
            output: output.into(),
            tokens: TokenSummary {
                input: usage.input_tokens,
                output: usage.output_tokens,
            },
            cost_usd: 0.0, // Cost calculation deferred to telemetry crate
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: "error".into(),
            output: message.into(),
            tokens: TokenSummary {
                input: 0,
                output: 0,
            },
            cost_usd: 0.0,
        }
    }

    /// Format the result according to the specified output format.
    pub fn format(&self, format: OutputFormat) -> Result<String> {
        match format {
            OutputFormat::Text => Ok(self.output.clone()),
            OutputFormat::Json => serde_json::to_string_pretty(self).map_err(|e| {
                caduceus_core::CaduceusError::Config(format!("Failed to serialize result: {}", e))
            }),
            OutputFormat::Compact => Ok(format!(
                "[{}] {} (tokens: {}/{})",
                self.status, self.output, self.tokens.input, self.tokens.output
            )),
        }
    }
}

// ── Headless runner ────────────────────────────────────────────────────────────

/// Executes a single prompt in headless mode and returns the formatted result.
pub struct HeadlessRunner {
    config: HeadlessConfig,
}

impl HeadlessRunner {
    pub fn new(config: HeadlessConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &HeadlessConfig {
        &self.config
    }

    /// Build a headless result from a session state after execution.
    pub fn build_result(&self, output: &str, state: &SessionState) -> HeadlessResult {
        let usage = TokenUsage {
            input_tokens: state.token_budget.used_input,
            output_tokens: state.token_budget.used_output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        HeadlessResult::success(output, &usage)
    }

    /// Format a headless result for output.
    pub fn format_result(&self, result: &HeadlessResult) -> Result<String> {
        result.format(self.config.output_format)
    }

    /// Build an error result.
    pub fn build_error(&self, message: &str) -> HeadlessResult {
        HeadlessResult::error(message)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::TokenUsage;

    #[test]
    fn output_format_from_str() {
        assert_eq!(
            OutputFormat::from_str_loose("text"),
            Some(OutputFormat::Text)
        );
        assert_eq!(
            OutputFormat::from_str_loose("json"),
            Some(OutputFormat::Json)
        );
        assert_eq!(
            OutputFormat::from_str_loose("compact"),
            Some(OutputFormat::Compact)
        );
        assert_eq!(
            OutputFormat::from_str_loose("JSON"),
            Some(OutputFormat::Json)
        );
        assert_eq!(OutputFormat::from_str_loose("unknown"), None);
    }

    #[test]
    fn headless_result_success_json() {
        let usage = TokenUsage {
            input_tokens: 1234,
            output_tokens: 567,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let result = HeadlessResult::success("Fixed 3 tests", &usage);
        let json = result.format(OutputFormat::Json).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["output"], "Fixed 3 tests");
        assert_eq!(parsed["tokens"]["input"], 1234);
        assert_eq!(parsed["tokens"]["output"], 567);
    }

    #[test]
    fn headless_result_error() {
        let result = HeadlessResult::error("Provider connection failed");
        assert_eq!(result.status, "error");
        assert_eq!(result.output, "Provider connection failed");
        assert_eq!(result.tokens.input, 0);
    }

    #[test]
    fn headless_result_text_format() {
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let result = HeadlessResult::success("All tests pass", &usage);
        let text = result.format(OutputFormat::Text).unwrap();
        assert_eq!(text, "All tests pass");
    }

    #[test]
    fn headless_result_compact_format() {
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let result = HeadlessResult::success("Done", &usage);
        let compact = result.format(OutputFormat::Compact).unwrap();
        assert!(compact.contains("[success]"));
        assert!(compact.contains("Done"));
        assert!(compact.contains("100/50"));
    }

    #[test]
    fn headless_config_builder() {
        let config = HeadlessConfig::new("fix the tests")
            .with_print_only(true)
            .with_output_format(OutputFormat::Json)
            .with_mode(AgentMode::Autopilot);
        assert_eq!(config.prompt, "fix the tests");
        assert!(config.print_only);
        assert_eq!(config.output_format, OutputFormat::Json);
        assert_eq!(config.mode, Some(AgentMode::Autopilot));
    }

    #[test]
    fn headless_runner_build_error() {
        let config = HeadlessConfig::new("test");
        let runner = HeadlessRunner::new(config);
        let result = runner.build_error("connection failed");
        assert_eq!(result.status, "error");
        assert_eq!(result.output, "connection failed");
    }

    #[test]
    fn output_format_display() {
        assert_eq!(format!("{}", OutputFormat::Text), "text");
        assert_eq!(format!("{}", OutputFormat::Json), "json");
        assert_eq!(format!("{}", OutputFormat::Compact), "compact");
    }
}
