use caduceus_core::{AgentEvent, ModelId, ProviderId, Result, SessionState, TokenUsage};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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

// ── Interactive REPL mode ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplState {
    Idle,
    Executing,
    WaitingApproval,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplAction {
    Execute(String),
    SlashCommand(String, Vec<String>),
    Empty,
    ContinueMultiline,
}

#[derive(Debug, Clone)]
pub struct ReplMode {
    pub state: ReplState,
    pub history: Vec<String>,
    pub history_index: usize,
    pub multiline_buffer: String,
    pub is_multiline: bool,
}

impl ReplMode {
    pub fn new() -> Self {
        Self {
            state: ReplState::Idle,
            history: Vec::new(),
            history_index: 0,
            multiline_buffer: String::new(),
            is_multiline: false,
        }
    }

    pub fn add_input(&mut self, line: &str) -> ReplAction {
        if self.is_multiline {
            if !self.multiline_buffer.is_empty() {
                self.multiline_buffer.push('\n');
            }
            self.multiline_buffer.push_str(line);
            return ReplAction::ContinueMultiline;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            return ReplAction::Empty;
        }

        self.history.push(line.to_string());
        self.history_index = self.history.len();

        if let Some(command) = trimmed.strip_prefix('/') {
            let mut parts = command.split_whitespace();
            let name = parts.next().unwrap_or_default().to_string();
            let args = parts.map(ToString::to_string).collect();
            return ReplAction::SlashCommand(name, args);
        }

        ReplAction::Execute(line.to_string())
    }

    pub fn history_prev(&mut self) -> Option<&str> {
        if self.history.is_empty() {
            return None;
        }

        self.history_index = self.history_index.saturating_sub(1);
        self.history.get(self.history_index).map(String::as_str)
    }

    pub fn history_next(&mut self) -> Option<&str> {
        if self.history.is_empty() || self.history_index >= self.history.len() {
            return None;
        }

        self.history_index += 1;
        self.history.get(self.history_index).map(String::as_str)
    }

    pub fn start_multiline(&mut self) {
        self.is_multiline = true;
        self.multiline_buffer.clear();
    }

    pub fn end_multiline(&mut self) -> String {
        self.is_multiline = false;
        let completed = std::mem::take(&mut self.multiline_buffer);
        if !completed.trim().is_empty() {
            self.history.push(completed.clone());
            self.history_index = self.history.len();
        }
        completed
    }

    pub fn transition(&mut self, new_state: ReplState) {
        self.state = new_state;
    }

    pub fn complete_slash_command(&self, partial: &str) -> Vec<String> {
        let normalized = partial.trim_start_matches('/');
        let mut matches: Vec<String> = slash_commands()
            .into_iter()
            .filter(|command| command.trim_start_matches('/').starts_with(normalized))
            .map(ToString::to_string)
            .collect();
        matches.sort();
        matches
    }
}

impl Default for ReplMode {
    fn default() -> Self {
        Self::new()
    }
}

fn slash_commands() -> [&'static str; 8] {
    [
        "/approve", "/clear", "/compact", "/context", "/deny", "/exit", "/help", "/quit",
    ]
}

// ── Compact output mode ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompactOutputFilter {
    pub enabled: bool,
    pub show_tool_names: bool,
    pub show_errors: bool,
}

impl CompactOutputFilter {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            show_tool_names: false,
            show_errors: true,
        }
    }

    pub fn filter_output(&self, events: &[AgentEvent]) -> Vec<String> {
        if !self.enabled {
            return events
                .iter()
                .filter_map(|event| match event {
                    AgentEvent::TextDelta { text } => Some(text.clone()),
                    AgentEvent::ToolCallStart { name, .. } if self.show_tool_names => {
                        Some(name.clone())
                    }
                    AgentEvent::ToolResultEnd {
                        content, is_error, ..
                    } if *is_error && self.show_errors => Some(content.clone()),
                    AgentEvent::Error { message } if self.show_errors => Some(message.clone()),
                    _ => None,
                })
                .map(|line| self.format_compact(&line))
                .collect();
        }

        let mut lines = Vec::new();
        let mut final_text = String::new();

        for event in events {
            match event {
                AgentEvent::TextDelta { text } => final_text.push_str(text),
                AgentEvent::ToolCallStart { name, .. } if self.show_tool_names => {
                    lines.push(self.format_compact(name));
                }
                AgentEvent::ToolResultEnd {
                    content, is_error, ..
                } if *is_error && self.show_errors => {
                    lines.push(self.format_compact(content));
                }
                AgentEvent::Error { message } if self.show_errors => {
                    lines.push(self.format_compact(message));
                }
                _ => {}
            }
        }

        if !final_text.is_empty() {
            lines.push(self.format_compact(&final_text));
        }

        lines
    }

    pub fn format_compact(&self, response: &str) -> String {
        strip_ansi_sequences(response).trim().to_string()
    }
}

fn strip_ansi_sequences(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                let _ = chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        result.push(ch);
    }

    result
}

// ── Typo suggestions ────────────────────────────────────────────────────────────

pub struct TypoSuggester {
    known_commands: Vec<String>,
    known_flags: Vec<String>,
}

impl TypoSuggester {
    pub fn new(commands: Vec<String>, flags: Vec<String>) -> Self {
        Self {
            known_commands: commands,
            known_flags: flags,
        }
    }

    pub fn suggest_command(&self, unknown: &str) -> Vec<(String, f64)> {
        self.suggest_from(&self.known_commands, unknown)
    }

    pub fn suggest_flag(&self, unknown: &str) -> Vec<(String, f64)> {
        self.suggest_from(&self.known_flags, unknown)
    }

    pub fn levenshtein_distance(a: &str, b: &str) -> usize {
        if a == b {
            return 0;
        }
        if a.is_empty() {
            return b.chars().count();
        }
        if b.is_empty() {
            return a.chars().count();
        }

        let b_chars: Vec<char> = b.chars().collect();
        let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
        let mut curr = vec![0; b_chars.len() + 1];

        for (i, a_char) in a.chars().enumerate() {
            curr[0] = i + 1;
            for (j, b_char) in b_chars.iter().enumerate() {
                let cost = usize::from(a_char != *b_char);
                curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
            }
            prev.clone_from_slice(&curr);
        }

        prev[b_chars.len()]
    }

    pub fn format_suggestion(unknown: &str, suggestions: &[(String, f64)]) -> String {
        if suggestions.is_empty() {
            return format!("Unknown input '{unknown}'.");
        }

        let formatted = suggestions
            .iter()
            .map(|(suggestion, score)| format!("{suggestion} ({score:.2})"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("Unknown input '{unknown}'. Did you mean: {formatted}?")
    }

    fn suggest_from(&self, known_values: &[String], unknown: &str) -> Vec<(String, f64)> {
        let mut suggestions = known_values
            .iter()
            .filter_map(|candidate| {
                let distance = Self::levenshtein_distance(unknown, candidate);
                let max_len = unknown.chars().count().max(candidate.chars().count()) as f64;
                if max_len == 0.0 {
                    return None;
                }

                let transposition_bonus = if is_single_adjacent_transposition(unknown, candidate) {
                    0.2
                } else {
                    0.0
                };
                let similarity = (1.0 - (distance as f64 / max_len) + transposition_bonus).min(1.0);
                if distance <= 2 || similarity >= 0.75 {
                    Some((candidate.clone(), similarity))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        suggestions.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        suggestions
    }
}

fn is_single_adjacent_transposition(left: &str, right: &str) -> bool {
    if left.chars().count() != right.chars().count() {
        return false;
    }

    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    let differing_indices = left_chars
        .iter()
        .zip(right_chars.iter())
        .enumerate()
        .filter_map(|(index, (left_char, right_char))| (left_char != right_char).then_some(index))
        .collect::<Vec<_>>();

    if differing_indices.len() != 2 {
        return false;
    }

    let first = differing_indices[0];
    let second = differing_indices[1];
    second == first + 1
        && left_chars[first] == right_chars[second]
        && left_chars[second] == right_chars[first]
}

// ── Summary compression ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct SummaryCompressor {
    pub max_lines: Option<usize>,
    pub max_chars: Option<usize>,
}

impl SummaryCompressor {
    pub fn new() -> Self {
        Self {
            max_lines: None,
            max_chars: None,
        }
    }

    pub fn with_line_budget(mut self, lines: usize) -> Self {
        self.max_lines = Some(lines);
        self
    }

    pub fn with_char_budget(mut self, chars: usize) -> Self {
        self.max_chars = Some(chars);
        self
    }

    pub fn compress(&self, summary: &str) -> String {
        if summary.trim().is_empty() {
            return String::new();
        }

        let mut lines = Self::remove_redundant_lines(&summary.lines().collect::<Vec<_>>());
        if let Some(max_lines) = self.max_lines {
            lines.truncate(max_lines);
        }

        let compressed = lines.join("\n");
        match self.max_chars {
            Some(max_chars) => Self::truncate_to_budget(&compressed, max_chars),
            None => compressed,
        }
    }

    fn remove_redundant_lines(lines: &[&str]) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();

        for line in lines {
            let normalized = line.trim();
            if normalized.is_empty() || !seen.insert(normalized.to_string()) {
                continue;
            }
            result.push(normalized.to_string());
        }

        result
    }

    fn truncate_to_budget(text: &str, max_chars: usize) -> String {
        if text.chars().count() <= max_chars {
            return text.to_string();
        }
        if max_chars == 0 {
            return String::new();
        }
        if max_chars == 1 {
            return "…".to_string();
        }

        let mut truncated: String = text.chars().take(max_chars - 1).collect();
        truncated.push('…');
        truncated
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

    #[test]
    fn repl_state_transitions() {
        let mut repl = ReplMode::new();
        assert_eq!(repl.state, ReplState::Idle);

        repl.transition(ReplState::Executing);
        assert_eq!(repl.state, ReplState::Executing);

        repl.transition(ReplState::WaitingApproval);
        assert_eq!(repl.state, ReplState::WaitingApproval);
    }

    #[test]
    fn repl_history_navigation() {
        let mut repl = ReplMode::new();
        let _ = repl.add_input("first");
        let _ = repl.add_input("second");

        assert_eq!(repl.history_prev(), Some("second"));
        assert_eq!(repl.history_prev(), Some("first"));
        assert_eq!(repl.history_next(), Some("second"));
        assert_eq!(repl.history_next(), None);
    }

    #[test]
    fn repl_multiline_mode() {
        let mut repl = ReplMode::new();
        repl.start_multiline();

        assert_eq!(repl.add_input("line one"), ReplAction::ContinueMultiline);
        assert_eq!(repl.add_input("line two"), ReplAction::ContinueMultiline);

        let completed = repl.end_multiline();
        assert_eq!(completed, "line one\nline two");
        assert!(!repl.is_multiline);
    }

    #[test]
    fn repl_slash_completion() {
        let repl = ReplMode::new();
        let completions = repl.complete_slash_command("/co");

        assert_eq!(
            completions,
            vec!["/compact".to_string(), "/context".to_string()]
        );
    }

    #[test]
    fn compact_filter_removes_tool_events_and_keeps_final_text() {
        let filter = CompactOutputFilter::new(true);
        let events = vec![
            AgentEvent::ToolCallStart {
                id: caduceus_core::ToolCallId::new("tool-1"),
                name: "read_file".to_string(),
            },
            AgentEvent::TextDelta {
                text: "Hello".to_string(),
            },
            AgentEvent::TextDelta {
                text: " world".to_string(),
            },
        ];

        assert_eq!(
            filter.filter_output(&events),
            vec!["Hello world".to_string()]
        );
    }

    #[test]
    fn compact_filter_handles_errors() {
        let filter = CompactOutputFilter::new(true);
        let events = vec![AgentEvent::Error {
            message: "\u{1b}[31mboom\u{1b}[0m".to_string(),
        }];

        assert_eq!(filter.filter_output(&events), vec!["boom".to_string()]);
    }

    #[test]
    fn typo_suggester_matches_exact_and_close_values() {
        let suggester = TypoSuggester::new(
            vec!["/compact".to_string(), "/context".to_string()],
            vec!["--model".to_string(), "--mode".to_string()],
        );

        let exact = suggester.suggest_command("/compact");
        let close = suggester.suggest_flag("--modle");

        assert_eq!(exact[0].0, "/compact");
        assert_eq!(exact[0].1, 1.0);
        assert_eq!(close[0].0, "--model");
        assert_eq!(TypoSuggester::levenshtein_distance("mode", "modle"), 1);
    }

    #[test]
    fn typo_suggester_ignores_distant_values() {
        let suggester =
            TypoSuggester::new(vec!["/compact".to_string()], vec!["--model".to_string()]);
        assert!(suggester.suggest_command("/xyz").is_empty());
        assert!(TypoSuggester::format_suggestion("/xyz", &[]).contains("Unknown input"));
    }

    #[test]
    fn summary_compressor_applies_line_and_char_budgets() {
        let summary = "alpha\nbeta\nbeta\ngamma";
        let compressor = SummaryCompressor::new()
            .with_line_budget(2)
            .with_char_budget(9);

        assert_eq!(compressor.compress(summary), "alpha\nbe…");
    }

    #[test]
    fn summary_compressor_handles_empty_input() {
        let compressor = SummaryCompressor::new();
        assert!(compressor.compress("   ").is_empty());
    }
}
