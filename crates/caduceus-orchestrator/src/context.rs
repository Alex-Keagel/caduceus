use caduceus_providers::Message;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Performance zones ──────────────────────────────────────────────────────────

/// Performance zones based on context fill percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextZone {
    /// 0–50%: optimal performance
    Green,
    /// 50–70%: slight degradation
    Yellow,
    /// 70–85%: noticeable context loss
    Orange,
    /// 85–95%: critical, should compact
    Red,
    /// 95%+: auto-compact triggered
    Critical,
}

impl ContextZone {
    pub fn from_percentage(pct: f64) -> Self {
        if pct >= 95.0 {
            Self::Critical
        } else if pct >= 85.0 {
            Self::Red
        } else if pct >= 70.0 {
            Self::Orange
        } else if pct >= 50.0 {
            Self::Yellow
        } else {
            Self::Green
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Green => "GREEN",
            Self::Yellow => "YELLOW",
            Self::Orange => "ORANGE",
            Self::Red => "RED",
            Self::Critical => "CRITICAL",
        }
    }

    pub fn recommendation(&self) -> &'static str {
        match self {
            Self::Green => "Context usage is healthy.",
            Self::Yellow => "Context filling up. Consider compacting soon.",
            Self::Orange => "Noticeable context loss. Compact recommended.",
            Self::Red => "Critical — compact now to avoid degradation.",
            Self::Critical => "Auto-compact triggered. Context nearly full.",
        }
    }

    pub fn ansi_color(&self) -> &'static str {
        match self {
            Self::Green => "\x1b[32m",
            Self::Yellow => "\x1b[33m",
            Self::Orange => "\x1b[38;5;208m",
            Self::Red => "\x1b[31m",
            Self::Critical => "\x1b[31;1m",
        }
    }
}

impl std::fmt::Display for ContextZone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

// ── Context breakdown ──────────────────────────────────────────────────────────

/// Token-level breakdown of each context component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBreakdown {
    pub system_prompt_tokens: u32,
    pub tool_schemas_tokens: u32,
    pub project_context_tokens: u32,
    pub conversation_tokens: u32,
    pub tool_results_tokens: u32,
    pub pinned_context_tokens: u32,
    pub total_tokens: u32,
    pub context_limit: u32,
    pub zone: ContextZone,
}

impl ContextBreakdown {
    pub fn fill_percentage(&self) -> f64 {
        if self.context_limit == 0 {
            return 0.0;
        }
        (self.total_tokens as f64 / self.context_limit as f64) * 100.0
    }

    pub fn remaining_tokens(&self) -> u32 {
        self.context_limit.saturating_sub(self.total_tokens)
    }
}

// ── Compaction strategies ──────────────────────────────────────────────────────

/// Available compaction strategies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompactionStrategy {
    /// Summarize oldest N messages into a single summary message.
    Summarize { preserve_last_n: usize },
    /// Drop oldest messages beyond a threshold.
    Truncate { keep_last_n: usize },
    /// Hybrid: summarize old, keep recent verbatim.
    Hybrid {
        summarize_before: usize,
        keep_verbatim: usize,
    },
    /// Smart: use LLM to identify and preserve high-salience content.
    Smart { budget_tokens: u32 },
}

impl CompactionStrategy {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Summarize { .. } => "summarize",
            Self::Truncate { .. } => "truncate",
            Self::Hybrid { .. } => "hybrid",
            Self::Smart { .. } => "smart",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "summarize" => Some(Self::Summarize { preserve_last_n: 5 }),
            "truncate" => Some(Self::Truncate { keep_last_n: 10 }),
            "hybrid" => Some(Self::Hybrid {
                summarize_before: 10,
                keep_verbatim: 5,
            }),
            "smart" => Some(Self::Smart {
                budget_tokens: 50_000,
            }),
            _ => None,
        }
    }
}

// ── Pinned context ─────────────────────────────────────────────────────────────

/// A pinned context item that survives compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedContext {
    pub label: String,
    pub content: String,
    pub tokens: u32,
    pub pinned_at: DateTime<Utc>,
}

// ── Context sub-commands ───────────────────────────────────────────────────────

/// Parsed sub-commands for `/context`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextCommand {
    /// `/context` — show overview
    Overview,
    /// `/context breakdown` — detailed per-component counts
    Breakdown,
    /// `/context compact` — manual compaction (default strategy)
    Compact,
    /// `/context compact --strategy <name>`
    CompactWithStrategy(String),
    /// `/context pin <label> <content>`
    Pin { label: String, content: String },
    /// `/context unpin <label>`
    Unpin { label: String },
    /// `/context pins` — list pinned items
    Pins,
    /// `/context zone` — show zone with recommendation
    Zone,
    /// `/context clear` — nuclear reset
    Clear,
}

impl ContextCommand {
    pub fn parse(args: &str) -> Self {
        let parts: Vec<&str> = args.split_whitespace().collect();
        match parts.first().map(|s| s.to_lowercase()).as_deref() {
            None | Some("") => Self::Overview,
            Some("breakdown") => Self::Breakdown,
            Some("compact") => {
                // Check for --strategy flag
                if parts.len() >= 3 && parts[1] == "--strategy" {
                    Self::CompactWithStrategy(parts[2].to_string())
                } else {
                    Self::Compact
                }
            }
            Some("pin") => {
                if parts.len() >= 3 {
                    let label = parts[1].to_string();
                    let content = parts[2..].join(" ");
                    Self::Pin { label, content }
                } else {
                    Self::Overview
                }
            }
            Some("unpin") => {
                if parts.len() >= 2 {
                    Self::Unpin {
                        label: parts[1].to_string(),
                    }
                } else {
                    Self::Overview
                }
            }
            Some("pins") => Self::Pins,
            Some("zone") => Self::Zone,
            Some("clear") => Self::Clear,
            _ => Self::Overview,
        }
    }
}

// ── Token estimation ───────────────────────────────────────────────────────────

/// Estimate token count for a string using a character-based heuristic.
/// Code averages ~3.5 chars/token; prose ~4 chars/token. We use 3.75 as a
/// compromise and apply a 10% safety margin.
pub fn estimate_tokens(text: &str) -> u32 {
    let chars = text.len() as f64;
    let estimate = chars / 3.75;
    // 10% safety margin
    (estimate * 1.1).ceil() as u32
}

// ── Context assembly / attunement ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextSource {
    SystemPrompt(String),
    Instructions(String),
    FileContext {
        path: String,
        content: String,
        priority: u8,
    },
    GitDiff(String),
    MemoryBank(String),
    ConversationHistory(Vec<String>),
    Pinned(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssembledContext {
    pub content: String,
    pub total_tokens: usize,
    pub sources_included: Vec<String>,
    pub sources_truncated: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextAssembler {
    pub max_tokens: usize,
    pub sources: Vec<ContextSource>,
}

impl ContextAssembler {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            max_tokens,
            sources: Vec::new(),
        }
    }

    pub fn add_source(&mut self, source: ContextSource) {
        self.sources.push(source);
    }

    pub fn assemble(&self) -> AssembledContext {
        let mut indexed_sources: Vec<(usize, &ContextSource)> =
            self.sources.iter().enumerate().collect();
        indexed_sources.sort_by(|(left_idx, left), (right_idx, right)| {
            context_source_order(*left_idx, left).cmp(&context_source_order(*right_idx, right))
        });

        let mut included_sections = Vec::new();
        let mut sources_included = Vec::new();
        let mut sources_truncated = Vec::new();
        let mut total_tokens = 0usize;

        for (_, source) in indexed_sources {
            let label = context_source_label(source);
            let rendered = render_context_source(source);
            let source_tokens = Self::estimate_tokens(&rendered);

            if total_tokens + source_tokens <= self.max_tokens {
                included_sections.push(rendered);
                sources_included.push(label);
                total_tokens += source_tokens;
                continue;
            }

            let remaining_tokens = self.max_tokens.saturating_sub(total_tokens);
            let truncated = truncate_context_source(source, remaining_tokens);

            if !truncated.is_empty() {
                total_tokens += Self::estimate_tokens(&truncated);
                included_sections.push(truncated);
                sources_included.push(label.clone());
            }

            sources_truncated.push(label);
        }

        AssembledContext {
            content: included_sections.join("\n\n"),
            total_tokens,
            sources_included,
            sources_truncated,
        }
    }

    pub fn estimate_tokens(text: &str) -> usize {
        estimate_tokens(text) as usize
    }
}

fn context_source_order(index: usize, source: &ContextSource) -> (u8, u8, usize) {
    match source {
        ContextSource::SystemPrompt(_) => (0, 0, index),
        ContextSource::Instructions(_) => (1, 0, index),
        ContextSource::Pinned(_) => (2, 0, index),
        ContextSource::FileContext { priority, .. } => (3, u8::MAX - *priority, index),
        ContextSource::GitDiff(_) => (4, 0, index),
        ContextSource::MemoryBank(_) => (5, 0, index),
        ContextSource::ConversationHistory(_) => (6, 0, index),
    }
}

fn context_source_label(source: &ContextSource) -> String {
    match source {
        ContextSource::SystemPrompt(_) => "system_prompt".to_string(),
        ContextSource::Instructions(_) => "instructions".to_string(),
        ContextSource::FileContext { path, .. } => format!("file:{path}"),
        ContextSource::GitDiff(_) => "git_diff".to_string(),
        ContextSource::MemoryBank(_) => "memory_bank".to_string(),
        ContextSource::ConversationHistory(_) => "conversation_history".to_string(),
        ContextSource::Pinned(_) => "pinned".to_string(),
    }
}

fn render_context_source(source: &ContextSource) -> String {
    match source {
        ContextSource::SystemPrompt(text) => format!("[System Prompt]\n{text}"),
        ContextSource::Instructions(text) => format!("[Instructions]\n{text}"),
        ContextSource::FileContext { path, content, .. } => {
            format!("[File Context: {path}]\n{content}")
        }
        ContextSource::GitDiff(text) => format!("[Git Diff]\n{text}"),
        ContextSource::MemoryBank(text) => format!("[Memory Bank]\n{text}"),
        ContextSource::ConversationHistory(messages) => {
            format!("[Conversation History]\n{}", messages.join("\n"))
        }
        ContextSource::Pinned(text) => format!("[Pinned]\n{text}"),
    }
}

fn truncate_context_source(source: &ContextSource, remaining_tokens: usize) -> String {
    if remaining_tokens == 0 {
        return String::new();
    }

    match source {
        ContextSource::ConversationHistory(messages) => {
            let header = "[Conversation History]\n";
            let mut lines = Vec::new();
            let mut used_tokens = ContextAssembler::estimate_tokens(header);

            for message in messages.iter().rev() {
                let line = if lines.is_empty() {
                    message.clone()
                } else {
                    format!("\n{message}")
                };
                let line_tokens = ContextAssembler::estimate_tokens(&line);
                if used_tokens + line_tokens > remaining_tokens {
                    break;
                }
                used_tokens += line_tokens;
                lines.push(message.clone());
            }

            if lines.is_empty() {
                truncate_text(&render_context_source(source), remaining_tokens)
            } else {
                lines.reverse();
                format!("{header}{}", lines.join("\n"))
            }
        }
        _ => truncate_text(&render_context_source(source), remaining_tokens),
    }
}

fn truncate_text(text: &str, remaining_tokens: usize) -> String {
    if remaining_tokens == 0 {
        return String::new();
    }

    let max_chars = remaining_tokens.saturating_mul(4);
    if max_chars == 0 {
        return String::new();
    }
    let mut truncated_chars: Vec<char> = text.chars().collect();

    if truncated_chars.len() > max_chars {
        if max_chars == 1 {
            return "…".to_string();
        }
        truncated_chars.truncate(max_chars - 1);
        truncated_chars.push('…');
    }

    while !truncated_chars.is_empty()
        && ContextAssembler::estimate_tokens(&truncated_chars.iter().collect::<String>())
            > remaining_tokens
    {
        if truncated_chars.last() == Some(&'…') {
            truncated_chars.pop();
        }
        truncated_chars.pop();
        if !truncated_chars.is_empty() {
            truncated_chars.push('…');
        }
    }

    truncated_chars.into_iter().collect()
}

// ── Context manager ────────────────────────────────────────────────────────────

pub struct ContextManager {
    limit: u32,
    strategy: CompactionStrategy,
    pinned: Vec<PinnedContext>,
    auto_compact_threshold: f64,
    #[allow(dead_code)]
    warning_thresholds: Vec<(f64, ContextZone)>,
}

impl ContextManager {
    pub fn new(context_limit: u32) -> Self {
        Self {
            limit: context_limit,
            strategy: CompactionStrategy::Hybrid {
                summarize_before: 10,
                keep_verbatim: 5,
            },
            pinned: Vec::new(),
            auto_compact_threshold: 85.0,
            warning_thresholds: vec![
                (95.0, ContextZone::Critical),
                (85.0, ContextZone::Red),
                (70.0, ContextZone::Orange),
                (50.0, ContextZone::Yellow),
                (0.0, ContextZone::Green),
            ],
        }
    }

    pub fn with_strategy(mut self, strategy: CompactionStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    pub fn with_auto_compact_threshold(mut self, threshold: f64) -> Self {
        self.auto_compact_threshold = threshold.clamp(50.0, 99.0);
        self
    }

    pub fn limit(&self) -> u32 {
        self.limit
    }

    pub fn strategy(&self) -> &CompactionStrategy {
        &self.strategy
    }

    pub fn set_strategy(&mut self, strategy: CompactionStrategy) {
        self.strategy = strategy;
    }

    // ── Breakdown ──────────────────────────────────────────────────────────

    /// Compute a context breakdown from the current conversation state.
    pub fn get_breakdown(
        &self,
        messages: &[Message],
        system_prompt: &str,
        tool_schemas_json: &str,
    ) -> ContextBreakdown {
        let system_prompt_tokens = estimate_tokens(system_prompt);
        let tool_schemas_tokens = estimate_tokens(tool_schemas_json);
        let pinned_context_tokens: u32 = self.pinned.iter().map(|p| p.tokens).sum();

        let mut conversation_tokens: u32 = 0;
        let mut tool_results_tokens: u32 = 0;
        let mut project_context_tokens: u32 = 0;

        for msg in messages {
            let tokens = estimate_tokens(&msg.content);
            match msg.role.as_str() {
                "system" => project_context_tokens += tokens,
                "tool" => tool_results_tokens += tokens,
                _ => conversation_tokens += tokens,
            }
        }

        let total_tokens = system_prompt_tokens
            + tool_schemas_tokens
            + project_context_tokens
            + conversation_tokens
            + tool_results_tokens
            + pinned_context_tokens;

        let pct = if self.limit == 0 {
            0.0
        } else {
            (total_tokens as f64 / self.limit as f64) * 100.0
        };

        ContextBreakdown {
            system_prompt_tokens,
            tool_schemas_tokens,
            project_context_tokens,
            conversation_tokens,
            tool_results_tokens,
            pinned_context_tokens,
            total_tokens,
            context_limit: self.limit,
            zone: ContextZone::from_percentage(pct),
        }
    }

    // ── Compaction decision ────────────────────────────────────────────────

    pub fn should_compact(&self, breakdown: &ContextBreakdown) -> bool {
        let pct = breakdown.fill_percentage();
        matches!(
            ContextZone::from_percentage(pct),
            ContextZone::Orange | ContextZone::Red | ContextZone::Critical
        )
    }

    // ── Compaction ─────────────────────────────────────────────────────────

    /// Apply the given compaction strategy to a message list.
    pub fn compact(&self, messages: &[Message], strategy: &CompactionStrategy) -> Vec<Message> {
        if messages.is_empty() {
            return Vec::new();
        }

        match strategy {
            CompactionStrategy::Summarize { preserve_last_n } => {
                let keep = (*preserve_last_n).min(messages.len());
                let to_summarize = &messages[..messages.len() - keep];
                let kept = &messages[messages.len() - keep..];

                if to_summarize.is_empty() {
                    return messages.to_vec();
                }

                let summary = Self::build_summary(to_summarize);
                let mut result = vec![Message::system(summary)];
                result.extend_from_slice(kept);
                result
            }
            CompactionStrategy::Truncate { keep_last_n } => {
                let keep = (*keep_last_n).min(messages.len());
                messages[messages.len() - keep..].to_vec()
            }
            CompactionStrategy::Hybrid {
                summarize_before,
                keep_verbatim,
            } => {
                let total = messages.len();
                let verbatim = (*keep_verbatim).min(total);
                let summarize_count = (*summarize_before).min(total.saturating_sub(verbatim));

                if summarize_count == 0 {
                    return messages[total - verbatim..].to_vec();
                }

                let to_summarize = &messages[..summarize_count];
                let kept = &messages[total - verbatim..];

                let summary = Self::build_summary(to_summarize);
                let mut result = vec![Message::system(summary)];
                result.extend_from_slice(kept);
                result
            }
            CompactionStrategy::Smart { budget_tokens } => {
                // Smart strategy: keep messages whose cumulative tokens fit the budget.
                // Iterate from the end (most recent) and include until budget exhausted.
                let mut budget_remaining = *budget_tokens;
                let mut kept_indices: Vec<usize> = Vec::new();

                for (i, msg) in messages.iter().enumerate().rev() {
                    let tokens = estimate_tokens(&msg.content);
                    if tokens <= budget_remaining {
                        budget_remaining -= tokens;
                        kept_indices.push(i);
                    }
                }

                kept_indices.reverse();
                if kept_indices.is_empty() && !messages.is_empty() {
                    // Always keep at least the last message
                    return vec![messages.last().unwrap().clone()];
                }

                kept_indices.iter().map(|&i| messages[i].clone()).collect()
            }
        }
    }

    /// Auto-compact if context usage exceeds the threshold.
    pub fn auto_compact_if_needed(
        &self,
        messages: &[Message],
        system_prompt: &str,
        tool_schemas_json: &str,
    ) -> Option<Vec<Message>> {
        let breakdown = self.get_breakdown(messages, system_prompt, tool_schemas_json);
        if breakdown.fill_percentage() >= self.auto_compact_threshold {
            Some(self.compact(messages, &self.strategy))
        } else {
            None
        }
    }

    /// Build a plain-text summary of a slice of messages.
    fn build_summary(messages: &[Message]) -> String {
        let mut summary = String::from("[Compacted conversation summary]\n");
        let turn_count = messages.len();
        let total_chars: usize = messages.iter().map(|m| m.content.len()).sum();

        summary.push_str(&format!(
            "({turn_count} messages, ~{total_chars} chars compacted)\n\n"
        ));

        // Include first and last message content as anchors
        if let Some(first) = messages.first() {
            let preview = truncate_str(&first.content, 200);
            summary.push_str(&format!("First ({role}): {preview}\n", role = first.role));
        }
        if messages.len() > 1 {
            if let Some(last) = messages.last() {
                let preview = truncate_str(&last.content, 200);
                summary.push_str(&format!("Last ({role}): {preview}\n", role = last.role));
            }
        }

        summary
    }

    // ── Pinned context ─────────────────────────────────────────────────────

    pub fn pin(&mut self, label: impl Into<String>, content: impl Into<String>) {
        let label = label.into();
        let content = content.into();
        let tokens = estimate_tokens(&content);

        // Remove existing pin with the same label
        self.pinned.retain(|p| p.label != label);

        self.pinned.push(PinnedContext {
            label,
            content,
            tokens,
            pinned_at: Utc::now(),
        });
    }

    pub fn unpin(&mut self, label: &str) -> bool {
        let before = self.pinned.len();
        self.pinned.retain(|p| p.label != label);
        self.pinned.len() < before
    }

    pub fn list_pins(&self) -> &[PinnedContext] {
        &self.pinned
    }

    pub fn pinned_tokens(&self) -> u32 {
        self.pinned.iter().map(|p| p.tokens).sum()
    }

    // ── Visualization ──────────────────────────────────────────────────────

    /// Render an ASCII visualization of context usage.
    pub fn format_visualization(&self, breakdown: &ContextBreakdown) -> String {
        let bar_width = 50u32;
        let pct = breakdown.fill_percentage();
        let zone = &breakdown.zone;
        let reset = "\x1b[0m";
        let color = zone.ansi_color();

        let mut out = String::new();
        out.push_str(&format!(
            "Context: {color}[{zone}]{reset} {:.1}% ({} / {} tokens)\n",
            pct, breakdown.total_tokens, breakdown.context_limit
        ));

        // Bar
        let filled = ((pct / 100.0) * bar_width as f64).round() as u32;
        let empty = bar_width.saturating_sub(filled);
        out.push_str(&format!(
            "  {color}[{}{}]{reset}\n",
            "█".repeat(filled as usize),
            "░".repeat(empty as usize)
        ));

        // Component breakdown
        let components = [
            ("System prompt", breakdown.system_prompt_tokens),
            ("Tool schemas", breakdown.tool_schemas_tokens),
            ("Project context", breakdown.project_context_tokens),
            ("Conversation", breakdown.conversation_tokens),
            ("Tool results", breakdown.tool_results_tokens),
            ("Pinned context", breakdown.pinned_context_tokens),
        ];

        for (label, tokens) in &components {
            let comp_pct = if breakdown.context_limit == 0 {
                0.0
            } else {
                (*tokens as f64 / breakdown.context_limit as f64) * 100.0
            };
            out.push_str(&format!("  {label:<18} {tokens:>8} ({comp_pct:>5.1}%)\n"));
        }

        out.push_str(&format!(
            "  {:<18} {:>8}\n",
            "Remaining",
            breakdown.remaining_tokens()
        ));
        out.push_str(&format!("\n{color}{}{reset}\n", zone.recommendation()));

        out
    }

    /// Render detailed breakdown without ANSI colors (for serialization).
    pub fn format_breakdown_plain(&self, breakdown: &ContextBreakdown) -> String {
        let pct = breakdown.fill_percentage();
        let mut out = String::new();

        out.push_str(&format!(
            "Context Breakdown — {} zone ({:.1}%)\n",
            breakdown.zone, pct
        ));
        out.push_str(&format!(
            "Total: {} / {}\n\n",
            breakdown.total_tokens, breakdown.context_limit
        ));

        let components = [
            ("System prompt", breakdown.system_prompt_tokens),
            ("Tool schemas", breakdown.tool_schemas_tokens),
            ("Project context", breakdown.project_context_tokens),
            ("Conversation", breakdown.conversation_tokens),
            ("Tool results", breakdown.tool_results_tokens),
            ("Pinned context", breakdown.pinned_context_tokens),
        ];

        for (label, tokens) in &components {
            out.push_str(&format!("  {label}: {tokens}\n"));
        }

        out.push_str(&format!("\nRemaining: {}\n", breakdown.remaining_tokens()));
        out
    }
}

/// Truncate a string to at most `max_len` characters, appending "…" if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut result: String = s.chars().take(max_len).collect();
        result.push('…');
        result
    }
}

// ── Context-aware retrieval ────────────────────────────────────────────────────

/// A retrieved code chunk with token cost, for budget-constrained injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedChunk {
    pub file_path: String,
    pub symbol_name: String,
    pub content: String,
    pub score: f32,
    pub tokens: u32,
}

/// Retrieve relevant code chunks that fit within a token budget.
///
/// Takes search results from `caduceus-omniscience` and filters to fit
/// the available budget.
pub fn retrieve_relevant_context(
    results: Vec<(String, String, String, f32)>, // (file_path, symbol, content, score)
    budget_tokens: u32,
) -> Vec<RetrievedChunk> {
    let mut remaining = budget_tokens;
    let mut chunks = Vec::new();

    for (file_path, symbol_name, content, score) in results {
        let tokens = estimate_tokens(&content);
        if tokens <= remaining {
            remaining -= tokens;
            chunks.push(RetrievedChunk {
                file_path,
                symbol_name,
                content,
                score,
                tokens,
            });
        }
        if remaining == 0 {
            break;
        }
    }

    chunks
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_messages(count: usize) -> Vec<Message> {
        (0..count)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(format!("User message {i}"))
                } else {
                    Message::assistant(format!("Assistant reply {i}"))
                }
            })
            .collect()
    }

    #[test]
    fn zone_from_percentage() {
        assert_eq!(ContextZone::from_percentage(0.0), ContextZone::Green);
        assert_eq!(ContextZone::from_percentage(25.0), ContextZone::Green);
        assert_eq!(ContextZone::from_percentage(49.9), ContextZone::Green);
        assert_eq!(ContextZone::from_percentage(50.0), ContextZone::Yellow);
        assert_eq!(ContextZone::from_percentage(69.9), ContextZone::Yellow);
        assert_eq!(ContextZone::from_percentage(70.0), ContextZone::Orange);
        assert_eq!(ContextZone::from_percentage(84.9), ContextZone::Orange);
        assert_eq!(ContextZone::from_percentage(85.0), ContextZone::Red);
        assert_eq!(ContextZone::from_percentage(94.9), ContextZone::Red);
        assert_eq!(ContextZone::from_percentage(95.0), ContextZone::Critical);
        assert_eq!(ContextZone::from_percentage(100.0), ContextZone::Critical);
    }

    #[test]
    fn zone_display_and_labels() {
        assert_eq!(ContextZone::Green.label(), "GREEN");
        assert_eq!(ContextZone::Critical.label(), "CRITICAL");
        assert_eq!(format!("{}", ContextZone::Orange), "ORANGE");
    }

    #[test]
    fn estimate_tokens_basic() {
        // ~3.75 chars/token with 10% safety margin
        let tokens = estimate_tokens("hello world");
        assert!(tokens > 0);
        // 11 chars / 3.75 * 1.1 ≈ 3.23 → 4
        assert!(tokens >= 3 && tokens <= 5, "got {tokens}");
    }

    #[test]
    fn estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn assembled_context_enforces_token_budget() {
        let mut assembler = ContextAssembler::new(18);
        assembler.add_source(ContextSource::SystemPrompt("You are helpful.".repeat(3)));
        assembler.add_source(ContextSource::ConversationHistory(vec![
            "First turn".to_string(),
            "Second turn".to_string(),
            "Third turn".to_string(),
        ]));

        let assembled = assembler.assemble();

        assert!(assembled.total_tokens <= 18);
        assert!(!assembled.sources_included.is_empty());
    }

    #[test]
    fn assembled_context_respects_priority_ordering() {
        let mut assembler = ContextAssembler::new(200);
        assembler.add_source(ContextSource::ConversationHistory(vec![
            "recent turn".to_string()
        ]));
        assembler.add_source(ContextSource::FileContext {
            path: "b.rs".to_string(),
            content: "lower priority".to_string(),
            priority: 1,
        });
        assembler.add_source(ContextSource::SystemPrompt("system".to_string()));
        assembler.add_source(ContextSource::FileContext {
            path: "a.rs".to_string(),
            content: "higher priority".to_string(),
            priority: 10,
        });

        let assembled = assembler.assemble();

        assert_eq!(
            assembled.sources_included,
            vec![
                "system_prompt".to_string(),
                "file:a.rs".to_string(),
                "file:b.rs".to_string(),
                "conversation_history".to_string(),
            ]
        );
    }

    #[test]
    fn assembled_context_tracks_truncation() {
        let mut assembler = ContextAssembler::new(10);
        assembler.add_source(ContextSource::Pinned(
            "Pinned context is important.".repeat(2),
        ));
        assembler.add_source(ContextSource::GitDiff("+".repeat(200)));

        let assembled = assembler.assemble();

        assert!(assembled.total_tokens <= 10);
        assert!(assembled
            .sources_truncated
            .contains(&"git_diff".to_string()));
    }

    #[test]
    fn breakdown_fill_percentage() {
        let b = ContextBreakdown {
            system_prompt_tokens: 100,
            tool_schemas_tokens: 50,
            project_context_tokens: 0,
            conversation_tokens: 350,
            tool_results_tokens: 0,
            pinned_context_tokens: 0,
            total_tokens: 500,
            context_limit: 1000,
            zone: ContextZone::Yellow,
        };
        assert!((b.fill_percentage() - 50.0).abs() < 0.01);
        assert_eq!(b.remaining_tokens(), 500);
    }

    #[test]
    fn breakdown_zero_limit() {
        let b = ContextBreakdown {
            system_prompt_tokens: 0,
            tool_schemas_tokens: 0,
            project_context_tokens: 0,
            conversation_tokens: 0,
            tool_results_tokens: 0,
            pinned_context_tokens: 0,
            total_tokens: 0,
            context_limit: 0,
            zone: ContextZone::Green,
        };
        assert!((b.fill_percentage() - 0.0).abs() < 0.01);
    }

    #[test]
    fn should_compact_zones() {
        let mgr = ContextManager::new(1000);

        // Green — no compact
        let green = ContextBreakdown {
            total_tokens: 300,
            context_limit: 1000,
            zone: ContextZone::Green,
            ..default_breakdown()
        };
        assert!(!mgr.should_compact(&green));

        // Yellow — no compact
        let yellow = ContextBreakdown {
            total_tokens: 550,
            context_limit: 1000,
            zone: ContextZone::Yellow,
            ..default_breakdown()
        };
        assert!(!mgr.should_compact(&yellow));

        // Orange — compact
        let orange = ContextBreakdown {
            total_tokens: 750,
            context_limit: 1000,
            zone: ContextZone::Orange,
            ..default_breakdown()
        };
        assert!(mgr.should_compact(&orange));

        // Red — compact
        let red = ContextBreakdown {
            total_tokens: 900,
            context_limit: 1000,
            zone: ContextZone::Red,
            ..default_breakdown()
        };
        assert!(mgr.should_compact(&red));

        // Critical — compact
        let critical = ContextBreakdown {
            total_tokens: 960,
            context_limit: 1000,
            zone: ContextZone::Critical,
            ..default_breakdown()
        };
        assert!(mgr.should_compact(&critical));
    }

    #[test]
    fn compact_truncate() {
        let mgr = ContextManager::new(200_000);
        let msgs = make_messages(10);
        let strategy = CompactionStrategy::Truncate { keep_last_n: 3 };
        let result = mgr.compact(&msgs, &strategy);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].content, "Assistant reply 7");
        assert_eq!(result[2].content, "Assistant reply 9");
    }

    #[test]
    fn compact_summarize() {
        let mgr = ContextManager::new(200_000);
        let msgs = make_messages(10);
        let strategy = CompactionStrategy::Summarize { preserve_last_n: 3 };
        let result = mgr.compact(&msgs, &strategy);
        // 1 summary message + 3 preserved
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].role, "system");
        assert!(result[0].content.contains("Compacted conversation summary"));
    }

    #[test]
    fn compact_hybrid() {
        let mgr = ContextManager::new(200_000);
        let msgs = make_messages(10);
        let strategy = CompactionStrategy::Hybrid {
            summarize_before: 5,
            keep_verbatim: 3,
        };
        let result = mgr.compact(&msgs, &strategy);
        // 1 summary + 3 verbatim
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].role, "system");
    }

    #[test]
    fn compact_smart_budget() {
        let mgr = ContextManager::new(200_000);
        let msgs = make_messages(6);
        // Each message is short (~5 tokens), budget of 20 should fit several
        let strategy = CompactionStrategy::Smart { budget_tokens: 20 };
        let result = mgr.compact(&msgs, &strategy);
        assert!(!result.is_empty());
        assert!(result.len() <= msgs.len());
    }

    #[test]
    fn compact_empty() {
        let mgr = ContextManager::new(200_000);
        let result = mgr.compact(&[], &CompactionStrategy::Truncate { keep_last_n: 5 });
        assert!(result.is_empty());
    }

    #[test]
    fn pin_and_unpin() {
        let mut mgr = ContextManager::new(200_000);
        mgr.pin("task", "Implement context management");
        assert_eq!(mgr.list_pins().len(), 1);
        assert_eq!(mgr.list_pins()[0].label, "task");
        assert!(mgr.list_pins()[0].tokens > 0);

        // Re-pin with same label replaces
        mgr.pin("task", "Updated task description");
        assert_eq!(mgr.list_pins().len(), 1);
        assert!(mgr.list_pins()[0].content.contains("Updated"));

        // Add another pin
        mgr.pin("notes", "Some important notes");
        assert_eq!(mgr.list_pins().len(), 2);

        // Unpin
        assert!(mgr.unpin("task"));
        assert_eq!(mgr.list_pins().len(), 1);
        assert_eq!(mgr.list_pins()[0].label, "notes");

        // Unpin non-existent
        assert!(!mgr.unpin("nonexistent"));
    }

    #[test]
    fn pinned_tokens_counted() {
        let mut mgr = ContextManager::new(1000);
        mgr.pin("info", "Some pinned information that matters");

        let msgs = make_messages(2);
        let breakdown = mgr.get_breakdown(&msgs, "system prompt", "{}");

        assert!(breakdown.pinned_context_tokens > 0);
        assert!(breakdown.total_tokens > 0);
    }

    #[test]
    fn auto_compact_trigger() {
        let mgr = ContextManager::new(100).with_auto_compact_threshold(50.0);

        // Create messages that exceed 50% of 100 tokens
        let msgs: Vec<Message> = (0..20)
            .map(|i| {
                Message::user(format!(
                    "This is a reasonably long message number {i} with enough content"
                ))
            })
            .collect();

        let result = mgr.auto_compact_if_needed(&msgs, "", "");
        assert!(result.is_some(), "Should trigger compaction");
        let compacted = result.unwrap();
        assert!(compacted.len() < msgs.len());
    }

    #[test]
    fn auto_compact_no_trigger() {
        let mgr = ContextManager::new(1_000_000);

        let msgs = make_messages(2);
        let result = mgr.auto_compact_if_needed(&msgs, "short", "{}");
        assert!(result.is_none(), "Should not trigger with tiny usage");
    }

    #[test]
    fn visualization_output() {
        let mgr = ContextManager::new(10_000);
        let breakdown = ContextBreakdown {
            system_prompt_tokens: 500,
            tool_schemas_tokens: 200,
            project_context_tokens: 100,
            conversation_tokens: 3000,
            tool_results_tokens: 1500,
            pinned_context_tokens: 200,
            total_tokens: 5500,
            context_limit: 10_000,
            zone: ContextZone::Yellow,
        };

        let vis = mgr.format_visualization(&breakdown);
        assert!(vis.contains("YELLOW"));
        assert!(vis.contains("System prompt"));
        assert!(vis.contains("Conversation"));
        assert!(vis.contains("Remaining"));
        assert!(vis.contains("█")); // filled bar
        assert!(vis.contains("░")); // empty bar
    }

    #[test]
    fn context_command_parse() {
        assert_eq!(ContextCommand::parse(""), ContextCommand::Overview);
        assert_eq!(
            ContextCommand::parse("breakdown"),
            ContextCommand::Breakdown
        );
        assert_eq!(ContextCommand::parse("compact"), ContextCommand::Compact);
        assert_eq!(
            ContextCommand::parse("compact --strategy summarize"),
            ContextCommand::CompactWithStrategy("summarize".into())
        );
        assert_eq!(
            ContextCommand::parse("pin task Build the feature"),
            ContextCommand::Pin {
                label: "task".into(),
                content: "Build the feature".into(),
            }
        );
        assert_eq!(
            ContextCommand::parse("unpin task"),
            ContextCommand::Unpin {
                label: "task".into()
            }
        );
        assert_eq!(ContextCommand::parse("pins"), ContextCommand::Pins);
        assert_eq!(ContextCommand::parse("zone"), ContextCommand::Zone);
        assert_eq!(ContextCommand::parse("clear"), ContextCommand::Clear);
    }

    #[test]
    fn strategy_from_name() {
        assert!(CompactionStrategy::from_name("summarize").is_some());
        assert!(CompactionStrategy::from_name("truncate").is_some());
        assert!(CompactionStrategy::from_name("hybrid").is_some());
        assert!(CompactionStrategy::from_name("smart").is_some());
        assert!(CompactionStrategy::from_name("HYBRID").is_some());
        assert!(CompactionStrategy::from_name("invalid").is_none());
    }

    #[test]
    fn retrieve_relevant_context_budget() {
        let results = vec![
            ("a.rs".into(), "fn_a".into(), "fn a() {}".into(), 0.9),
            (
                "b.rs".into(),
                "fn_b".into(),
                "fn b() { let x = 1; let y = 2; }".into(),
                0.8,
            ),
            ("c.rs".into(), "fn_c".into(), "fn c() {}".into(), 0.7),
        ];
        let chunks = retrieve_relevant_context(results, 10);
        // Budget is tight — should include only chunks that fit
        assert!(!chunks.is_empty());
        let total: u32 = chunks.iter().map(|c| c.tokens).sum();
        assert!(total <= 10);
    }

    fn default_breakdown() -> ContextBreakdown {
        ContextBreakdown {
            system_prompt_tokens: 0,
            tool_schemas_tokens: 0,
            project_context_tokens: 0,
            conversation_tokens: 0,
            tool_results_tokens: 0,
            pinned_context_tokens: 0,
            total_tokens: 0,
            context_limit: 1000,
            zone: ContextZone::Green,
        }
    }
}
