//! Shared utilities for keeping tool-call request/response pairs atomic.

/// A message that may participate in a tool-call ↔ tool-result pairing.
///
/// The compaction pipeline and provider message history both need the same
/// "pair-aware" scan: when an assistant message requests one or more tool calls,
/// any immediately-following tool result messages that match those call ids must
/// be treated as a single atomic unit.
pub(crate) trait PairAwareMessage {
    /// If this message initiates a tool-call unit, return the expected tool call ids.
    fn tool_request_ids(&self) -> Option<Vec<&str>>;

    /// If this message is a tool result that belongs to a tool-call unit, return its tool id.
    fn tool_result_id(&self) -> Option<&str>;
}

/// Partition `messages` into non-overlapping `(start, end_exclusive)` ranges.
///
/// If a message at `i` is a tool-call request (per [`PairAwareMessage::tool_request_ids`]),
/// the range extends through all *immediately following* tool-result messages whose
/// ids match one of the expected ids. Grouping stops at the first non-matching tool
/// result or first non-tool message.
///
/// All other messages are emitted as size-1 ranges.
pub(crate) fn pair_aware_units<M: PairAwareMessage>(messages: &[M]) -> Vec<(usize, usize)> {
    let mut units = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        if let Some(expected_ids) = messages[i].tool_request_ids() {
            let mut j = i + 1;
            while j < messages.len() {
                match messages[j].tool_result_id() {
                    Some(id) if expected_ids.contains(&id) => j += 1,
                    _ => break,
                }
            }
            units.push((i, j));
            i = j;
        } else {
            units.push((i, i + 1));
            i += 1;
        }
    }

    units
}

impl PairAwareMessage for caduceus_providers::Message {
    fn tool_request_ids(&self) -> Option<Vec<&str>> {
        if self.role == "assistant" && !self.tool_calls.is_empty() {
            Some(self.tool_calls.iter().map(|tc| tc.id.as_str()).collect())
        } else {
            None
        }
    }

    fn tool_result_id(&self) -> Option<&str> {
        if self.role != "tool" {
            return None;
        }
        self.tool_result.as_ref()?.tool_use_id.as_deref()
    }
}

/// Transparent pairing view through any smart pointer (Arc / Rc / Box) so
/// `ConversationHistory` can store `Arc<Message>` without reimplementing the
/// trait for each wrapper (ST-C2 Phase 2).
impl<M: PairAwareMessage> PairAwareMessage for std::sync::Arc<M> {
    fn tool_request_ids(&self) -> Option<Vec<&str>> {
        (**self).tool_request_ids()
    }

    fn tool_result_id(&self) -> Option<&str> {
        (**self).tool_result_id()
    }
}
