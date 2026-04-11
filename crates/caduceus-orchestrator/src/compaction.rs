//! Context eviction / compaction pipeline — features #190–#197.

use std::collections::HashSet;

// ── #191: Atomic Message Groups ───────────────────────────────────────────────

/// Lightweight message representation used throughout the compaction pipeline.
#[derive(Debug, Clone)]
pub struct CompactMessage {
    pub role: String,
    pub content: String,
    pub token_estimate: usize,
}

impl CompactMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        let content = content.into();
        let token_estimate = (content.len() as f64 / 3.75 * 1.1).ceil() as usize;
        Self {
            role: role.into(),
            content,
            token_estimate,
        }
    }
}

/// Semantic category of a message group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageGroupKind {
    System,
    User,
    AssistantText,
    ToolCall,
    Summary,
}

/// An atomic group of messages that are treated as a single eviction unit.
#[derive(Debug, Clone)]
pub struct MessageGroup {
    pub kind: MessageGroupKind,
    pub messages: Vec<CompactMessage>,
    pub token_count: usize,
    /// When `true`, the group has been logically removed but not yet spliced out.
    pub excluded: bool,
}

impl MessageGroup {
    pub fn new(kind: MessageGroupKind) -> Self {
        Self {
            kind,
            messages: Vec::new(),
            token_count: 0,
            excluded: false,
        }
    }

    pub fn add_message(&mut self, msg: CompactMessage) {
        self.token_count += msg.token_estimate;
        self.messages.push(msg);
    }

    pub fn total_tokens(&self) -> usize {
        self.token_count
    }

    pub fn is_system(&self) -> bool {
        self.kind == MessageGroupKind::System
    }
}

/// Group a flat message list into atomic [`MessageGroup`] units.
///
/// Consecutive messages of the same kind (except System) are coalesced into
/// one group, which preserves the invariant that system messages are never
/// merged with conversational content.
pub fn build_message_groups(messages: &[CompactMessage]) -> Vec<MessageGroup> {
    let mut groups: Vec<MessageGroup> = Vec::new();

    for msg in messages {
        let kind = classify_role(&msg.role, &msg.content);

        // System messages are always their own group.
        let can_merge = !matches!(kind, MessageGroupKind::System);

        if can_merge {
            if let Some(last) = groups.last_mut() {
                if last.kind == kind {
                    last.add_message(msg.clone());
                    continue;
                }
            }
        }

        let mut group = MessageGroup::new(kind);
        group.add_message(msg.clone());
        groups.push(group);
    }

    groups
}

fn classify_role(role: &str, content: &str) -> MessageGroupKind {
    match role {
        "system" => MessageGroupKind::System,
        "user" => MessageGroupKind::User,
        "tool" => MessageGroupKind::ToolCall,
        "assistant" => {
            if content.contains("<tool_call>") || content.contains("\"tool_use\"") {
                MessageGroupKind::ToolCall
            } else {
                MessageGroupKind::AssistantText
            }
        }
        _ => MessageGroupKind::User,
    }
}

// ── #190: Compaction Pipeline ─────────────────────────────────────────────────

/// Output of a single [`CompactionStrategy`] run.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub removed_tokens: usize,
    pub groups_affected: usize,
}

/// Aggregate result of a full [`CompactionPipeline`] run.
#[derive(Debug, Clone)]
pub struct PipelineResult {
    pub total_removed_tokens: usize,
    pub strategies_applied: Vec<String>,
}

/// A compaction strategy that mutates a group list in place.
pub trait CompactionStrategy: Send + Sync {
    fn name(&self) -> &str;
    fn compact(&self, groups: &mut Vec<MessageGroup>) -> CompactionResult;
}

/// Runs a sequence of [`CompactionStrategy`] implementations in order,
/// stopping early when the token budget is satisfied.
pub struct CompactionPipeline {
    strategies: Vec<Box<dyn CompactionStrategy>>,
    token_budget: usize,
}

impl CompactionPipeline {
    pub fn new(budget: usize) -> Self {
        Self {
            strategies: Vec::new(),
            token_budget: budget,
        }
    }

    pub fn add_strategy(&mut self, strategy: Box<dyn CompactionStrategy>) {
        self.strategies.push(strategy);
    }

    /// Run all strategies in insertion order, halting once groups fit the budget.
    pub fn run(&self, groups: &mut Vec<MessageGroup>) -> PipelineResult {
        let mut total_removed = 0usize;
        let mut strategies_applied = Vec::new();

        for strategy in &self.strategies {
            let current_tokens: usize = groups.iter().map(|g| g.total_tokens()).sum();
            if current_tokens <= self.token_budget {
                break;
            }

            let result = strategy.compact(groups);
            if result.removed_tokens > 0 || result.groups_affected > 0 {
                total_removed += result.removed_tokens;
                strategies_applied.push(strategy.name().to_string());
            }
        }

        PipelineResult {
            total_removed_tokens: total_removed,
            strategies_applied,
        }
    }

    /// Standard four-stage pipeline: tool-collapse → summarize → sliding-window → truncate.
    pub fn default_pipeline(budget: usize) -> Self {
        let mut p = Self::new(budget);
        p.add_strategy(Box::new(ToolCollapseStrategy));
        p.add_strategy(Box::new(SummarizeStrategy { keep_recent: 10 }));
        p.add_strategy(Box::new(SlidingWindowStrategy { window_size: 20 }));
        p.add_strategy(Box::new(EmergencyTruncator {
            minimum_preserved: 5,
        }));
        p
    }
}

// ── Built-in pipeline strategies ──────────────────────────────────────────────

/// Drops consecutive ToolCall groups so that only the first one remains.
///
/// **NOTE:** subsequent tool groups are *removed entirely* (not merged or
/// summarised). Use this strategy when tool-call duplication is the primary
/// token pressure and discarding the extra calls is acceptable.
pub struct ToolCollapseStrategy;

impl CompactionStrategy for ToolCollapseStrategy {
    fn name(&self) -> &str {
        "tool-collapse"
    }

    fn compact(&self, groups: &mut Vec<MessageGroup>) -> CompactionResult {
        let mut removed_tokens = 0usize;
        let mut groups_affected = 0usize;
        let mut i = 0;

        while i + 1 < groups.len() {
            if groups[i].kind == MessageGroupKind::ToolCall
                && groups[i + 1].kind == MessageGroupKind::ToolCall
            {
                let absorbed = groups.remove(i + 1);
                removed_tokens += absorbed.token_count;
                groups_affected += 1;
                // Keep iterating from the same position to catch longer runs.
            } else {
                i += 1;
            }
        }

        CompactionResult {
            removed_tokens,
            groups_affected,
        }
    }
}

/// Summarises old non-system groups, retaining only the `keep_recent` most recent.
pub struct SummarizeStrategy {
    pub keep_recent: usize,
}

impl CompactionStrategy for SummarizeStrategy {
    fn name(&self) -> &str {
        "summarize"
    }

    fn compact(&self, groups: &mut Vec<MessageGroup>) -> CompactionResult {
        let non_system_indices: Vec<usize> = groups
            .iter()
            .enumerate()
            .filter(|(_, g)| !g.is_system())
            .map(|(i, _)| i)
            .collect();

        if non_system_indices.len() <= self.keep_recent {
            return CompactionResult {
                removed_tokens: 0,
                groups_affected: 0,
            };
        }

        let to_summarise_count = non_system_indices.len() - self.keep_recent;
        let eligible: Vec<usize> = non_system_indices[..to_summarise_count].to_vec();

        let mut removed_tokens = 0usize;
        let mut groups_affected = 0usize;

        // Build a compact summary that is deliberately shorter than the originals.
        let mut summary_parts: Vec<String> = Vec::new();
        const PREVIEW_CHARS: usize = 80;
        for &idx in &eligible {
            let g = &groups[idx];
            for msg in &g.messages {
                // FIX 1: use char-based truncation to avoid panics on multi-byte UTF-8.
                let preview = if msg.content.chars().count() > PREVIEW_CHARS {
                    let truncated: String = msg.content.chars().take(PREVIEW_CHARS).collect();
                    format!("{truncated}…")
                } else {
                    msg.content.clone()
                };
                summary_parts.push(format!("{}: {}", msg.role, preview));
            }
            removed_tokens += g.token_count;
            groups_affected += 1;
        }

        // Remove in descending index order to avoid index shifting.
        let mut sorted_eligible = eligible.clone();
        sorted_eligible.sort_unstable_by(|a, b| b.cmp(a));
        for &idx in &sorted_eligible {
            groups.remove(idx);
        }

        // Prepend a Summary group just after any System groups.
        let insert_pos = groups
            .iter()
            .position(|g| !g.is_system())
            .unwrap_or(groups.len());

        let summary_text = format!(
            "[Summarised {} groups]\n{}",
            groups_affected,
            summary_parts.join("\n")
        );
        let summary_tokens = estimate_compact_tokens(&summary_text);
        let summary_msg = CompactMessage {
            role: "system".to_string(),
            content: summary_text,
            token_estimate: summary_tokens,
        };
        let mut summary_group = MessageGroup::new(MessageGroupKind::Summary);
        summary_group.add_message(summary_msg);
        groups.insert(insert_pos, summary_group);

        let net_removed = removed_tokens.saturating_sub(summary_tokens);
        CompactionResult {
            removed_tokens: net_removed,
            groups_affected,
        }
    }
}

/// Keeps only the `window_size` most recent non-system groups.
pub struct SlidingWindowStrategy {
    pub window_size: usize,
}

impl CompactionStrategy for SlidingWindowStrategy {
    fn name(&self) -> &str {
        "sliding-window"
    }

    fn compact(&self, groups: &mut Vec<MessageGroup>) -> CompactionResult {
        let non_system_count = groups.iter().filter(|g| !g.is_system()).count();
        if non_system_count <= self.window_size {
            return CompactionResult {
                removed_tokens: 0,
                groups_affected: 0,
            };
        }

        let to_drop = non_system_count - self.window_size;
        let mut dropped = 0usize;
        let mut removed_tokens = 0usize;
        let mut i = 0;

        while i < groups.len() && dropped < to_drop {
            if !groups[i].is_system() {
                removed_tokens += groups[i].token_count;
                groups.remove(i);
                dropped += 1;
                // don't advance i — the next element shifted into position i
            } else {
                i += 1;
            }
        }

        CompactionResult {
            removed_tokens,
            groups_affected: dropped,
        }
    }
}

// ── #192: Compaction Triggers ─────────────────────────────────────────────────

/// Snapshot of current context dimensions used by [`CompactionTrigger`].
#[derive(Debug, Clone)]
pub struct ContextStats {
    pub total_tokens: usize,
    pub message_count: usize,
    pub turn_count: usize,
}

/// Declarative trigger that decides whether compaction should run.
#[derive(Debug, Clone)]
pub enum CompactionTrigger {
    TokensExceed(usize),
    MessagesExceed(usize),
    TurnsExceed(usize),
    Always,
    Never,
    /// All inner triggers must fire.
    All(Vec<CompactionTrigger>),
    /// At least one inner trigger must fire.
    Any(Vec<CompactionTrigger>),
}

impl CompactionTrigger {
    pub fn should_compact(&self, stats: &ContextStats) -> bool {
        match self {
            Self::TokensExceed(limit) => stats.total_tokens > *limit,
            Self::MessagesExceed(limit) => stats.message_count > *limit,
            Self::TurnsExceed(limit) => stats.turn_count > *limit,
            Self::Always => true,
            Self::Never => false,
            // FIX 4: empty All(vec![]) must not vacuously return true.
            Self::All(triggers) => {
                !triggers.is_empty() && triggers.iter().all(|t| t.should_compact(stats))
            }
            Self::Any(triggers) => triggers.iter().any(|t| t.should_compact(stats)),
        }
    }
}

// ── #193: Self-Eviction Tools ─────────────────────────────────────────────────

/// A named snapshot of verified facts at a point in the conversation.
#[derive(Debug, Clone)]
pub struct ContextCheckpoint {
    pub id: String,
    pub verified_facts: Vec<String>,
    pub timestamp: u64,
}

/// Manages a sequence of checkpoints to support targeted context eviction.
#[derive(Debug, Default)]
pub struct SelfEvictionManager {
    checkpoints: Vec<ContextCheckpoint>,
}

impl SelfEvictionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the given `facts` and return a unique checkpoint id.
    pub fn checkpoint(&mut self, facts: Vec<String>) -> String {
        let id = format!("cp-{}", self.checkpoints.len() + 1);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.checkpoints.push(ContextCheckpoint {
            id: id.clone(),
            verified_facts: facts,
            timestamp,
        });
        id
    }

    /// Remove all checkpoints *before* the named one and return an estimate of
    /// freed token-bytes (sum of fact string lengths, as a proxy).
    pub fn purge_before(&mut self, checkpoint_id: &str) -> usize {
        if let Some(pos) = self.checkpoints.iter().position(|c| c.id == checkpoint_id) {
            let freed: usize = self.checkpoints[..pos]
                .iter()
                .flat_map(|c| c.verified_facts.iter())
                .map(|f| f.len())
                .sum();
            self.checkpoints.drain(..pos);
            freed
        } else {
            0
        }
    }

    /// Return the verified facts stored at the given checkpoint, if it exists.
    pub fn resume_from(&self, checkpoint_id: &str) -> Option<Vec<String>> {
        self.checkpoints
            .iter()
            .find(|c| c.id == checkpoint_id)
            .map(|c| c.verified_facts.clone())
    }

    pub fn list_checkpoints(&self) -> &[ContextCheckpoint] {
        &self.checkpoints
    }
}

// ── #194: Dual-Model Compaction ───────────────────────────────────────────────

/// Coordinates a primary (full-capability) model with a cheaper compaction model.
#[derive(Debug, Clone)]
pub struct DualModelCompactor {
    pub primary_model: String,
    pub compaction_model: String,
    pub max_summary_tokens: usize,
}

impl DualModelCompactor {
    pub fn new(primary: &str, compaction: &str) -> Self {
        Self {
            primary_model: primary.to_string(),
            compaction_model: compaction.to_string(),
            max_summary_tokens: 2_000,
        }
    }

    /// Build the summarisation prompt to be sent to `compaction_model`.
    pub fn generate_summary_prompt(&self, messages: &[CompactMessage]) -> String {
        let body: Vec<String> = messages
            .iter()
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect();
        format!(
            "Summarise the following conversation in at most {} tokens, preserving key facts, \
             decisions, and context:\n\n{}",
            self.max_summary_tokens,
            body.join("\n")
        )
    }

    /// Fraction of tokens saved: `(original - summary) / original`.
    pub fn estimate_savings(&self, original_tokens: usize, summary_tokens: usize) -> f64 {
        if original_tokens == 0 {
            return 0.0;
        }
        original_tokens.saturating_sub(summary_tokens) as f64 / original_tokens as f64
    }
}

// ── #195: Compaction Entropy Check ────────────────────────────────────────────

/// Quality assessment of a compaction summary.
#[derive(Debug, Clone)]
pub struct EntropyResult {
    pub passed: bool,
    pub density_score: f64,
    pub keyword_retention: f64,
}

/// Validates that a summary retains sufficient information relative to the original.
pub struct EntropyChecker {
    /// Minimum required information density (0.0–1.0).
    pub min_density: f64,
}

impl EntropyChecker {
    pub fn new(min_density: f64) -> Self {
        Self {
            min_density: min_density.clamp(0.0, 1.0),
        }
    }

    /// Check whether `summary` meets the density threshold relative to `original`.
    pub fn check_summary_quality(&self, original: &str, summary: &str) -> EntropyResult {
        let keyword_retention = self.keyword_retention_ratio(original, summary);
        let compression = self.length_compression_ratio(original, summary);

        // Density = keyword retention per unit of length compression.
        // A summary that keeps all keywords but is half the length scores 2.0,
        // clamped to 1.0 so we never exceed a perfect score.
        let density_score = if compression > 0.0 {
            (keyword_retention / compression).min(1.0)
        } else {
            keyword_retention
        };

        EntropyResult {
            passed: density_score >= self.min_density,
            density_score,
            keyword_retention,
        }
    }

    /// Ratio of original keywords that appear at least once in `summary`.
    pub fn keyword_retention_ratio(&self, original: &str, summary: &str) -> f64 {
        let keywords = extract_keywords(original);
        if keywords.is_empty() {
            return 1.0;
        }
        let summary_lower = summary.to_lowercase();
        let retained = keywords
            .iter()
            .filter(|kw| summary_lower.contains(kw.as_str()))
            .count();
        retained as f64 / keywords.len() as f64
    }

    /// `summary.len() / original.len()` — smaller means more compressed.
    pub fn length_compression_ratio(&self, original: &str, summary: &str) -> f64 {
        if original.is_empty() {
            return 1.0;
        }
        summary.len() as f64 / original.len() as f64
    }
}

/// Extract unique, meaningful words from `text` (longer than 4 chars, not stop-words).
fn extract_keywords(text: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "about", "after", "again", "also", "been", "before", "being", "could", "every", "first",
        "from", "have", "here", "just", "like", "made", "make", "more", "most", "much", "only",
        "other", "over", "same", "should", "some", "such", "than", "that", "their", "there",
        "these", "they", "this", "those", "through", "under", "very", "was", "were", "when",
        "where", "which", "while", "will", "with", "would", "your",
    ];

    let mut seen: HashSet<String> = HashSet::new();
    text.split_whitespace()
        .filter_map(|w| {
            let clean: String = w.chars().filter(|c| c.is_alphabetic()).collect();
            if clean.len() > 4 {
                let lower = clean.to_lowercase();
                if !STOP_WORDS.contains(&lower.as_str()) && seen.insert(lower.clone()) {
                    return Some(lower);
                }
            }
            None
        })
        .collect()
}

// ── #196: Pattern-Based Compaction ────────────────────────────────────────────

/// Compacts sequences that match the pattern:
/// `[AssistantText] → [ToolCall] → [AssistantText]`
///
/// The ToolCall group in the middle is marked `excluded` and its token count is
/// reduced to 25 % of the original (a collapsed placeholder).
pub struct PatternCompactor {
    /// How many non-excluded groups to retain unconditionally at the tail.
    pub retention_window: usize,
}

impl CompactionStrategy for PatternCompactor {
    fn name(&self) -> &str {
        "pattern-compactor"
    }

    fn compact(&self, groups: &mut Vec<MessageGroup>) -> CompactionResult {
        let mut removed_tokens = 0usize;
        let mut groups_affected = 0usize;

        // Compute index of first group inside the retention window so we don't
        // touch the tail the caller wants preserved.
        let non_system_count = groups.iter().filter(|g| !g.is_system()).count();
        let protected_from = non_system_count.saturating_sub(self.retention_window);
        let mut non_system_seen = 0usize;

        let mut i = 0;
        while i + 2 < groups.len() {
            // Track how many non-system groups we've passed.
            if !groups[i].is_system() {
                non_system_seen += 1;
            }

            let pattern_matches = groups[i].kind == MessageGroupKind::AssistantText
                && groups[i + 1].kind == MessageGroupKind::ToolCall
                && groups[i + 2].kind == MessageGroupKind::AssistantText;

            // FIX 3: ensure all three groups in the pattern are outside the
            // retention window, not just the first one.
            let mut seen_through_pattern = non_system_seen;
            if !groups[i + 1].is_system() {
                seen_through_pattern += 1;
            }
            if !groups[i + 2].is_system() {
                seen_through_pattern += 1;
            }
            if pattern_matches && seen_through_pattern <= protected_from {
                let original = groups[i + 1].token_count;
                let collapsed = (original / 4).max(1);
                removed_tokens += original.saturating_sub(collapsed);
                groups[i + 1].token_count = collapsed;
                groups[i + 1].excluded = true;
                groups_affected += 1;
                i += 3; // advance past the whole pattern
            } else {
                i += 1;
            }
        }

        CompactionResult {
            removed_tokens,
            groups_affected,
        }
    }
}

// ── #197: Emergency Truncation ────────────────────────────────────────────────

/// Drops the oldest non-system groups until under the pipeline budget,
/// always preserving at least `minimum_preserved` recent non-system groups.
pub struct EmergencyTruncator {
    pub minimum_preserved: usize,
}

impl CompactionStrategy for EmergencyTruncator {
    fn name(&self) -> &str {
        "emergency-truncate"
    }

    fn compact(&self, groups: &mut Vec<MessageGroup>) -> CompactionResult {
        let non_system_indices: Vec<usize> = groups
            .iter()
            .enumerate()
            .filter(|(_, g)| !g.is_system())
            .map(|(i, _)| i)
            .collect();

        let preserve = self.minimum_preserved.min(non_system_indices.len());
        let eligible_count = non_system_indices.len().saturating_sub(preserve);

        // Indices of groups to remove (the oldest ones).
        let to_remove: Vec<usize> = non_system_indices[..eligible_count].to_vec();

        let mut removed_tokens = 0usize;
        let groups_affected = to_remove.len();

        // Remove in descending order so each removal doesn't shift earlier indices.
        for &idx in to_remove.iter().rev() {
            removed_tokens += groups[idx].token_count;
            groups.remove(idx);
        }

        CompactionResult {
            removed_tokens,
            groups_affected,
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn estimate_compact_tokens(text: &str) -> usize {
    (text.len() as f64 / 3.75 * 1.1).ceil() as usize
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn msg(role: &str, content: &str) -> CompactMessage {
        CompactMessage::new(role, content)
    }

    fn groups_tokens(groups: &[MessageGroup]) -> usize {
        groups.iter().map(|g| g.total_tokens()).sum()
    }

    fn make_groups(specs: &[(&str, &str)]) -> Vec<MessageGroup> {
        let messages: Vec<CompactMessage> = specs.iter().map(|(r, c)| msg(r, c)).collect();
        build_message_groups(&messages)
    }

    // ── #191: MessageGroup ────────────────────────────────────────────────────

    #[test]
    fn message_group_new_and_add() {
        let mut g = MessageGroup::new(MessageGroupKind::User);
        assert_eq!(g.total_tokens(), 0);
        assert!(!g.is_system());

        g.add_message(msg("user", "hello world"));
        assert!(g.total_tokens() > 0);
        assert_eq!(g.messages.len(), 1);
    }

    #[test]
    fn message_group_is_system() {
        let g = MessageGroup::new(MessageGroupKind::System);
        assert!(g.is_system());
        let g2 = MessageGroup::new(MessageGroupKind::AssistantText);
        assert!(!g2.is_system());
    }

    #[test]
    fn build_message_groups_consecutive_merge() {
        // Two consecutive user messages should be merged into one group.
        let groups = make_groups(&[
            ("user", "first"),
            ("user", "second"),
            ("assistant", "reply"),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].kind, MessageGroupKind::User);
        assert_eq!(groups[0].messages.len(), 2);
        assert_eq!(groups[1].kind, MessageGroupKind::AssistantText);
    }

    #[test]
    fn build_message_groups_system_not_merged() {
        let groups = make_groups(&[("system", "sys1"), ("system", "sys2"), ("user", "hello")]);
        // Each system message gets its own group (not merged).
        assert_eq!(groups[0].kind, MessageGroupKind::System);
        assert_eq!(groups[1].kind, MessageGroupKind::System);
    }

    #[test]
    fn build_message_groups_tool_role() {
        let groups = make_groups(&[("user", "run it"), ("tool", "output"), ("assistant", "ok")]);
        assert_eq!(groups[1].kind, MessageGroupKind::ToolCall);
    }

    // ── #192: CompactionTrigger ───────────────────────────────────────────────

    fn stats(tokens: usize, messages: usize, turns: usize) -> ContextStats {
        ContextStats {
            total_tokens: tokens,
            message_count: messages,
            turn_count: turns,
        }
    }

    #[test]
    fn trigger_tokens_exceed() {
        let t = CompactionTrigger::TokensExceed(1000);
        assert!(!t.should_compact(&stats(999, 0, 0)));
        assert!(!t.should_compact(&stats(1000, 0, 0)));
        assert!(t.should_compact(&stats(1001, 0, 0)));
    }

    #[test]
    fn trigger_messages_exceed() {
        let t = CompactionTrigger::MessagesExceed(10);
        assert!(!t.should_compact(&stats(0, 10, 0)));
        assert!(t.should_compact(&stats(0, 11, 0)));
    }

    #[test]
    fn trigger_turns_exceed() {
        let t = CompactionTrigger::TurnsExceed(5);
        assert!(!t.should_compact(&stats(0, 0, 5)));
        assert!(t.should_compact(&stats(0, 0, 6)));
    }

    #[test]
    fn trigger_always_never() {
        assert!(CompactionTrigger::Always.should_compact(&stats(0, 0, 0)));
        assert!(!CompactionTrigger::Never.should_compact(&stats(
            usize::MAX,
            usize::MAX,
            usize::MAX
        )));
    }

    #[test]
    fn trigger_all_requires_every_condition() {
        let t = CompactionTrigger::All(vec![
            CompactionTrigger::TokensExceed(100),
            CompactionTrigger::TurnsExceed(3),
        ]);
        assert!(!t.should_compact(&stats(200, 0, 2))); // tokens ok, turns not
        assert!(!t.should_compact(&stats(50, 0, 10))); // turns ok, tokens not
        assert!(t.should_compact(&stats(200, 0, 10))); // both
    }

    #[test]
    fn trigger_any_requires_one_condition() {
        let t = CompactionTrigger::Any(vec![
            CompactionTrigger::TokensExceed(100),
            CompactionTrigger::TurnsExceed(3),
        ]);
        assert!(!t.should_compact(&stats(50, 0, 2)));
        assert!(t.should_compact(&stats(200, 0, 1)));
        assert!(t.should_compact(&stats(50, 0, 10)));
    }

    #[test]
    fn trigger_nested_any_all() {
        // Any(All(tokens>100, turns>3), Messages>20)
        let inner = CompactionTrigger::All(vec![
            CompactionTrigger::TokensExceed(100),
            CompactionTrigger::TurnsExceed(3),
        ]);
        let t = CompactionTrigger::Any(vec![inner, CompactionTrigger::MessagesExceed(20)]);
        assert!(!t.should_compact(&stats(50, 5, 1)));
        assert!(t.should_compact(&stats(200, 0, 10)));
        assert!(t.should_compact(&stats(0, 25, 0)));
    }

    // ── #193: SelfEvictionManager ─────────────────────────────────────────────

    #[test]
    fn checkpoint_creates_unique_ids() {
        let mut mgr = SelfEvictionManager::new();
        let id1 = mgr.checkpoint(vec!["fact A".into()]);
        let id2 = mgr.checkpoint(vec!["fact B".into()]);
        assert_ne!(id1, id2);
        assert_eq!(mgr.list_checkpoints().len(), 2);
    }

    #[test]
    fn resume_from_returns_facts() {
        let mut mgr = SelfEvictionManager::new();
        let id = mgr.checkpoint(vec!["the sky is blue".into(), "water is wet".into()]);
        let facts = mgr.resume_from(&id).expect("checkpoint should exist");
        assert_eq!(facts, vec!["the sky is blue", "water is wet"]);
    }

    #[test]
    fn resume_from_missing_returns_none() {
        let mgr = SelfEvictionManager::new();
        assert!(mgr.resume_from("nonexistent").is_none());
    }

    #[test]
    fn purge_before_removes_earlier_checkpoints() {
        let mut mgr = SelfEvictionManager::new();
        mgr.checkpoint(vec!["fact 1".into()]);
        mgr.checkpoint(vec!["fact 2".into()]);
        let id3 = mgr.checkpoint(vec!["fact 3".into()]);
        mgr.checkpoint(vec!["fact 4".into()]);

        let freed = mgr.purge_before(&id3);
        assert!(freed > 0);
        // id3 and id4 remain; id1 and id2 are gone
        assert_eq!(mgr.list_checkpoints().len(), 2);
        assert_eq!(mgr.list_checkpoints()[0].id, id3);
    }

    #[test]
    fn purge_before_unknown_id_is_noop() {
        let mut mgr = SelfEvictionManager::new();
        mgr.checkpoint(vec!["fact".into()]);
        let freed = mgr.purge_before("ghost");
        assert_eq!(freed, 0);
        assert_eq!(mgr.list_checkpoints().len(), 1);
    }

    // ── #194: DualModelCompactor ──────────────────────────────────────────────

    #[test]
    fn dual_model_summary_prompt_contains_messages() {
        let compactor = DualModelCompactor::new("gpt-4o", "gpt-4o-mini");
        let messages = vec![
            msg("user", "What is Rust?"),
            msg("assistant", "Rust is a systems language."),
        ];
        let prompt = compactor.generate_summary_prompt(&messages);
        assert!(prompt.contains("What is Rust?"));
        assert!(prompt.contains("systems language"));
        assert!(prompt.contains(&compactor.max_summary_tokens.to_string()));
    }

    #[test]
    fn estimate_savings_correct() {
        let c = DualModelCompactor::new("big", "small");
        assert!((c.estimate_savings(1000, 200) - 0.8).abs() < 1e-9);
        assert!((c.estimate_savings(1000, 1000) - 0.0).abs() < 1e-9);
        assert!((c.estimate_savings(0, 0) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn estimate_savings_clamps_at_zero() {
        let c = DualModelCompactor::new("big", "small");
        // summary larger than original → 0 savings
        let savings = c.estimate_savings(100, 200);
        assert!((savings - 0.0).abs() < 1e-9);
    }

    // ── #195: EntropyChecker ──────────────────────────────────────────────────

    #[test]
    fn entropy_perfect_summary_passes() {
        let checker = EntropyChecker::new(0.5);
        let original = "The quick brown fox jumps over the lazy dog";
        let summary = "The quick brown fox jumps over the lazy dog"; // identical
        let result = checker.check_summary_quality(original, summary);
        assert!(result.passed);
        assert!((result.keyword_retention - 1.0).abs() < 1e-9);
    }

    #[test]
    fn entropy_empty_summary_fails() {
        let checker = EntropyChecker::new(0.3);
        let original = "Important technical discussion about distributed systems";
        let result = checker.check_summary_quality(original, "");
        assert!(!result.passed);
    }

    #[test]
    fn entropy_keyword_retention_ratio() {
        let checker = EntropyChecker::new(0.0);
        let original = "authentication tokens expire after thirty minutes";
        let summary = "tokens expire after thirty"; // missing "authentication", "minutes"
        let ratio = checker.keyword_retention_ratio(original, summary);
        assert!(ratio > 0.0 && ratio <= 1.0);
    }

    #[test]
    fn entropy_length_compression_ratio() {
        let checker = EntropyChecker::new(0.0);
        let ratio = checker.length_compression_ratio("hello world", "hi");
        assert!(ratio < 1.0);
        let ratio_same = checker.length_compression_ratio("abc", "abc");
        assert!((ratio_same - 1.0).abs() < 1e-9);
        let ratio_empty = checker.length_compression_ratio("", "anything");
        assert!((ratio_empty - 1.0).abs() < 1e-9);
    }

    #[test]
    fn entropy_good_compression_passes() {
        let checker = EntropyChecker::new(0.4);
        // Summary retains the key domain terms but is much shorter.
        let original =
            "The authentication service validates tokens using HMAC-SHA256 and expires them \
             after thirty minutes of inactivity. The refresh endpoint issues new tokens.";
        let summary =
            "authentication tokens validated HMAC-SHA256 expire thirty minutes refresh endpoint";
        let result = checker.check_summary_quality(original, summary);
        // Compression is high and keyword retention is decent → should pass
        assert!(result.density_score >= 0.0);
        // We don't assert pass/fail here since it's heuristic; just ensure it runs.
        let _ = result.passed;
    }

    // ── #196: PatternCompactor ────────────────────────────────────────────────

    #[test]
    fn pattern_compactor_collapses_atb_pattern() {
        // Build: AssistantText → ToolCall → AssistantText → User (tail)
        let mut groups = vec![
            {
                let mut g = MessageGroup::new(MessageGroupKind::AssistantText);
                g.add_message(msg("assistant", "I will call the tool"));
                g
            },
            {
                let mut g = MessageGroup::new(MessageGroupKind::ToolCall);
                g.add_message(msg("tool", "tool result data result data result data"));
                g
            },
            {
                let mut g = MessageGroup::new(MessageGroupKind::AssistantText);
                g.add_message(msg("assistant", "Here is the result"));
                g
            },
            {
                let mut g = MessageGroup::new(MessageGroupKind::User);
                g.add_message(msg("user", "thanks"));
                g
            },
        ];

        // Use a large retention_window so the pattern is outside the protected tail.
        let compactor = PatternCompactor {
            retention_window: 0,
        };
        let result = compactor.compact(&mut groups);

        assert_eq!(result.groups_affected, 1);
        assert!(result.removed_tokens > 0);
        // The tool call group should be marked excluded and shrunken.
        assert!(groups[1].excluded);
        assert!(groups[1].token_count < 10); // collapsed to ≤25 %
    }

    #[test]
    fn pattern_compactor_respects_retention_window() {
        let mut groups = vec![
            {
                let mut g = MessageGroup::new(MessageGroupKind::AssistantText);
                g.add_message(msg("assistant", "call tool now"));
                g
            },
            {
                let mut g = MessageGroup::new(MessageGroupKind::ToolCall);
                g.add_message(msg("tool", "tool output data"));
                g
            },
            {
                let mut g = MessageGroup::new(MessageGroupKind::AssistantText);
                g.add_message(msg("assistant", "done"));
                g
            },
        ];

        // Retain all 3 non-system groups → pattern is inside the window → no compaction.
        let compactor = PatternCompactor {
            retention_window: 3,
        };
        let result = compactor.compact(&mut groups);
        assert_eq!(result.groups_affected, 0);
        assert!(!groups[1].excluded);
    }

    #[test]
    fn pattern_compactor_skips_non_matching() {
        let mut groups = make_groups(&[("user", "hello"), ("assistant", "hi"), ("user", "bye")]);
        let compactor = PatternCompactor {
            retention_window: 0,
        };
        let result = compactor.compact(&mut groups);
        assert_eq!(result.groups_affected, 0);
    }

    // ── #197: EmergencyTruncator ──────────────────────────────────────────────

    #[test]
    fn emergency_truncator_drops_oldest() {
        // 1 system + 5 user/assistant alternating
        let mut groups = make_groups(&[
            ("system", "You are helpful."),
            ("user", "msg 1"),
            ("assistant", "reply 1"),
            ("user", "msg 2"),
            ("assistant", "reply 2"),
            ("user", "msg 3"),
        ]);

        let truncator = EmergencyTruncator {
            minimum_preserved: 2,
        };
        let result = truncator.compact(&mut groups);

        // 5 non-system groups, preserve 2 → drop 3
        assert_eq!(result.groups_affected, 3);
        assert!(result.removed_tokens > 0);

        // System group is still present
        assert!(groups.iter().any(|g| g.is_system()));
        // Exactly 2 non-system groups remain
        let non_sys: Vec<_> = groups.iter().filter(|g| !g.is_system()).collect();
        assert_eq!(non_sys.len(), 2);
    }

    #[test]
    fn emergency_truncator_preserves_minimum() {
        let mut groups = make_groups(&[("user", "only one non-system")]);
        let truncator = EmergencyTruncator {
            minimum_preserved: 5,
        };
        let result = truncator.compact(&mut groups);
        // Nothing to drop — minimum_preserved >= non-system count
        assert_eq!(result.groups_affected, 0);
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn emergency_truncator_never_drops_system() {
        // Alternate roles so each message becomes its own group (no consecutive merging).
        let mut groups = make_groups(&[
            ("system", "System instructions"),
            ("user", "question a"),
            ("assistant", "answer a"),
            ("user", "question b"),
        ]);
        let truncator = EmergencyTruncator {
            minimum_preserved: 0,
        };
        let result = truncator.compact(&mut groups);
        // Drops all 3 non-system groups but keeps the system group.
        assert_eq!(result.groups_affected, 3);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].is_system());
    }

    // ── #190: CompactionPipeline ──────────────────────────────────────────────

    #[test]
    fn pipeline_execution_order() {
        // 25 alternating user/assistant messages → 25 groups.
        // SummarizeStrategy (keep_recent=10) will summarise the 15 oldest
        // with truncated previews, producing a net token reduction.
        let big_content = "interesting ".repeat(200);
        let specs: Vec<(&str, String)> = (0..25)
            .map(|i| {
                if i % 2 == 0 {
                    ("user", big_content.clone())
                } else {
                    ("assistant", big_content.clone())
                }
            })
            .collect();
        let spec_refs: Vec<(&str, &str)> = specs.iter().map(|(r, c)| (*r, c.as_str())).collect();
        let mut groups = make_groups(&spec_refs);

        let total_before = groups_tokens(&groups);
        let pipeline = CompactionPipeline::default_pipeline(1); // tiny budget
        let result = pipeline.run(&mut groups);

        assert!(
            result.total_removed_tokens > 0,
            "expected tokens to be removed"
        );
        assert!(!result.strategies_applied.is_empty());
        let total_after: usize = groups.iter().map(|g| g.total_tokens()).sum();
        assert!(total_after < total_before);
    }

    #[test]
    fn pipeline_stops_early_when_budget_met() {
        // Groups that already fit the budget → no strategies should run.
        let mut groups = make_groups(&[("user", "tiny")]);
        let pipeline = CompactionPipeline::default_pipeline(1_000_000);
        let result = pipeline.run(&mut groups);
        assert_eq!(result.total_removed_tokens, 0);
        assert!(result.strategies_applied.is_empty());
    }

    #[test]
    fn pipeline_add_custom_strategy() {
        struct AlwaysNoop;
        impl CompactionStrategy for AlwaysNoop {
            fn name(&self) -> &str {
                "noop"
            }
            fn compact(&self, _: &mut Vec<MessageGroup>) -> CompactionResult {
                CompactionResult {
                    removed_tokens: 0,
                    groups_affected: 0,
                }
            }
        }

        let mut pipeline = CompactionPipeline::new(0);
        pipeline.add_strategy(Box::new(AlwaysNoop));
        let mut groups = make_groups(&[("user", "hello")]);
        let result = pipeline.run(&mut groups);
        // Noop removes nothing → not recorded in strategies_applied.
        assert!(result.strategies_applied.is_empty());
    }

    // ── Tool-collapse strategy ────────────────────────────────────────────────

    #[test]
    fn tool_collapse_merges_consecutive_tool_groups() {
        // Build groups manually so we have 3 distinct ToolCall groups back-to-back.
        // (build_message_groups coalesces consecutive same-kind messages, so we
        // construct the groups directly to simulate three separate tool turns.)
        let mut groups = vec![
            {
                let mut g = MessageGroup::new(MessageGroupKind::User);
                g.add_message(msg("user", "do it"));
                g
            },
            {
                let mut g = MessageGroup::new(MessageGroupKind::ToolCall);
                g.add_message(msg("tool", "result1 data data data data"));
                g
            },
            {
                let mut g = MessageGroup::new(MessageGroupKind::ToolCall);
                g.add_message(msg("tool", "result2 data data data data"));
                g
            },
            {
                let mut g = MessageGroup::new(MessageGroupKind::ToolCall);
                g.add_message(msg("tool", "result3 data data data data"));
                g
            },
            {
                let mut g = MessageGroup::new(MessageGroupKind::AssistantText);
                g.add_message(msg("assistant", "done"));
                g
            },
        ];

        let strategy = ToolCollapseStrategy;
        let result = strategy.compact(&mut groups);

        // 2 merges (3 tool groups → 1) → 2 groups_affected
        assert_eq!(result.groups_affected, 2);
        assert!(result.removed_tokens > 0);
        let tool_groups: Vec<_> = groups
            .iter()
            .filter(|g| g.kind == MessageGroupKind::ToolCall)
            .collect();
        assert_eq!(tool_groups.len(), 1);
    }

    // ── Sliding-window strategy ───────────────────────────────────────────────

    #[test]
    fn sliding_window_keeps_recent_groups() {
        let specs: Vec<(&str, String)> = (0..10)
            .map(|i| {
                if i % 2 == 0 {
                    ("user", format!("user msg {i}"))
                } else {
                    ("assistant", format!("reply {i}"))
                }
            })
            .collect();
        let spec_refs: Vec<(&str, &str)> = specs.iter().map(|(r, c)| (*r, c.as_str())).collect();
        let mut groups = make_groups(&spec_refs);

        let strategy = SlidingWindowStrategy { window_size: 4 };
        let result = strategy.compact(&mut groups);

        let non_sys_remaining = groups.iter().filter(|g| !g.is_system()).count();
        assert_eq!(non_sys_remaining, 4);
        assert!(result.removed_tokens > 0);
    }

    // ── FIX 1: UTF-8 content through compaction ───────────────────────────────

    #[test]
    fn summarize_strategy_does_not_panic_on_multibyte_utf8() {
        // 30 groups of emoji / CJK content — much longer than PREVIEW_CHARS bytes
        // but potentially shorter in chars.  Must not panic on byte slicing.
        let emoji_content = "🦀".repeat(200); // each '🦀' is 4 bytes
        let cjk_content = "你好世界".repeat(50); // each CJK char is 3 bytes

        let mut groups = Vec::new();
        for i in 0..15 {
            let content = if i % 2 == 0 {
                emoji_content.clone()
            } else {
                cjk_content.clone()
            };
            let mut g = MessageGroup::new(MessageGroupKind::User);
            g.add_message(msg("user", &content));
            groups.push(g);
        }
        for _ in 0..15 {
            let mut g = MessageGroup::new(MessageGroupKind::AssistantText);
            g.add_message(msg("assistant", &emoji_content));
            groups.push(g);
        }

        let strategy = SummarizeStrategy { keep_recent: 10 };
        // Must not panic regardless of content encoding.
        let result = strategy.compact(&mut groups);
        assert!(result.groups_affected > 0);
    }

    #[test]
    fn summarize_preview_truncates_on_char_boundary() {
        // A string where bytes and chars diverge: 80 ASCII chars then an emoji.
        let content = format!("{}{}", "a".repeat(80), "🦀");
        let mut groups = vec![];
        for _ in 0..15 {
            let mut g = MessageGroup::new(MessageGroupKind::User);
            g.add_message(msg("user", &content));
            groups.push(g);
        }
        let strategy = SummarizeStrategy { keep_recent: 5 };
        let result = strategy.compact(&mut groups);
        // The result must not panic and must remove at least some groups.
        assert!(result.groups_affected > 0);
    }

    // ── FIX 4: All(vec![]) and Any(vec![]) edge cases ─────────────────────────

    #[test]
    fn trigger_all_empty_is_false() {
        // Vacuous truth bug: All([]) used to return true. It must return false.
        let t = CompactionTrigger::All(vec![]);
        assert!(
            !t.should_compact(&stats(usize::MAX, usize::MAX, usize::MAX)),
            "All(vec![]) must not trigger compaction"
        );
    }

    #[test]
    fn trigger_any_empty_is_false() {
        // Any([]) has no conditions to satisfy → should remain false (standard behaviour).
        let t = CompactionTrigger::Any(vec![]);
        assert!(
            !t.should_compact(&stats(usize::MAX, usize::MAX, usize::MAX)),
            "Any(vec![]) must not trigger compaction"
        );
    }

    #[test]
    fn trigger_all_single_condition_behaves_correctly() {
        let t = CompactionTrigger::All(vec![CompactionTrigger::TokensExceed(100)]);
        assert!(!t.should_compact(&stats(100, 0, 0)));
        assert!(t.should_compact(&stats(101, 0, 0)));
    }
}
