//! Context assembler — builds the LLM-ready message list for a turn.
//!
//! Walks `ConversationHistory` newest-first in pair-aware *units* (an
//! assistant message + its tool_use children + their tool_result siblings
//! count as a single atomic unit), including each whole unit only if its
//! full token cost still fits the budget. This guarantees the request
//! never contains an orphaned `tool_use` without its `tool_result` (or
//! vice-versa) — Anthropic in particular rejects such payloads with
//! HTTP 400.
//!
//! Extracted from `lib.rs` (ST-B1 Wave 1).

use crate::ConversationHistory;

/// Assembles the full message list for an LLM request within a token budget.
/// Uses a simple char-based heuristic (1 token ~ 4 chars) to estimate token usage.
pub struct MessageAssembler {
    max_context_tokens: u32,
    system_prompt: String,
    project_context: Option<String>,
}

impl MessageAssembler {
    pub fn new(max_context_tokens: u32, system_prompt: impl Into<String>) -> Self {
        Self {
            max_context_tokens,
            system_prompt: system_prompt.into(),
            project_context: None,
        }
    }

    pub fn with_project_context(mut self, ctx: impl Into<String>) -> Self {
        self.project_context = Some(ctx.into());
        self
    }

    fn estimate_tokens(text: &str) -> u32 {
        crate::context::estimate_tokens(text)
    }

    pub(crate) fn message_tokens(msg: &caduceus_providers::Message) -> u32 {
        let mut tokens = Self::estimate_tokens(&msg.role) + Self::estimate_tokens(&msg.content);
        // Tool call args and results can be large — count them
        for tc in &msg.tool_calls {
            tokens += Self::estimate_tokens(&tc.input.to_string());
        }
        if let Some(ref tr) = msg.tool_result {
            tokens += Self::estimate_tokens(&tr.content);
        }
        tokens
    }

    /// Build the final message list that fits within the token budget.
    ///
    /// Strategy: always include system prompt + project context, then walk
    /// pair-aware *units* (assistant+tool_calls+tool_results = 1 unit) from
    /// most recent backward, including each whole unit only if its full
    /// token cost still fits the budget. This guarantees no orphaned
    /// tool_use without its tool_result and no orphaned tool_result without
    /// its assistant tool_use — a malformed pair would otherwise cause
    /// providers (especially Anthropic) to reject the request with HTTP 400.
    pub fn assemble(&self, history: &ConversationHistory) -> Vec<caduceus_providers::Message> {
        let mut result = Vec::new();

        let mut full_system = self.system_prompt.clone();
        if let Some(ref ctx) = self.project_context {
            full_system.push_str("\n\n<project_context>\n");
            full_system.push_str(ctx);
            full_system.push_str("\n</project_context>");
        }

        let system_msg = caduceus_providers::Message::system(&full_system);
        let mut budget_used = Self::message_tokens(&system_msg);
        result.push(system_msg);

        // Reserve 25% of budget for output
        let available = self.max_context_tokens.saturating_mul(3) / 4;

        let messages = history.messages();
        let units = crate::pairing::pair_aware_units(messages);

        // Walk units newest-first, stop when next unit doesn't fit.
        let mut included_units: Vec<(usize, usize)> = Vec::new();
        for &(start, end) in units.iter().rev() {
            let unit_cost: u32 = messages[start..end]
                .iter()
                .map(|m| Self::message_tokens(m.as_ref()))
                .sum();
            if budget_used + unit_cost > available {
                // Stop on first non-fitting unit so chronological order is
                // preserved (we never want a gap in the middle of history).
                break;
            }
            budget_used += unit_cost;
            included_units.push((start, end));
        }

        // Restore chronological order and flatten unit ranges into messages.
        included_units.reverse();
        for (start, end) in included_units {
            for msg in &messages[start..end] {
                result.push(msg.as_ref().clone());
            }
        }

        // Defensive: if the very first included message is an orphan
        // tool-role (only possible if the history itself was malformed and
        // started with one), drop it. pair_aware_units emits orphans as
        // size-1 units so this fallback only ever triggers on bad input.
        while result.get(1).is_some_and(|m| m.role == "tool") {
            result.remove(1);
        }

        result
    }
}
