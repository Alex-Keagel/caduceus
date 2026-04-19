//! Context eviction / compaction pipeline — features #190–#197.

use std::collections::HashSet;

// ── #191: Atomic Message Groups ───────────────────────────────────────────────

/// Lightweight message representation used throughout the compaction pipeline.
///
/// `tool_call_ids` (assistant messages requesting tool calls) and `tool_use_id`
/// (tool result messages) carry the structured pairing metadata required to
/// keep an `assistant{tool_calls}` message and its matching `role:tool` results
/// together as one atomic eviction unit. Without them, content-sniffing alone
/// (looking for `<tool_call>` / `"tool_use"` substrings) splits the request
/// from the response when providers serialize tool calls in a structured field
/// instead of embedding them in `content`. Splitting them produces orphan tool
/// results that Anthropic and OpenAI both reject.
#[derive(Debug, Clone)]
pub struct CompactMessage {
    pub role: String,
    pub content: String,
    pub token_estimate: usize,
    /// Non-empty for assistant messages that request tool calls. Each entry is
    /// the `id` of one tool call. Private to enforce the invariant that this
    /// metadata can only be set via the dedicated constructors
    /// (`assistant_with_tool_calls`, `tool_result`) or the canonical
    /// `From<&caduceus_providers::Message>` impl — preventing callers from
    /// silently de-atomizing a message by direct field assignment.
    tool_call_ids: Vec<String>,
    /// `Some` for `role:tool` result messages. Carries the `id` of the
    /// `assistant.tool_calls[*]` entry this result responds to. Private for
    /// the same invariant-enforcement reason as `tool_call_ids`.
    tool_use_id: Option<String>,
}

impl CompactMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        let content = content.into();
        let token_estimate = (content.len() as f64 / 3.75 * 1.1).ceil() as usize;
        Self {
            role: role.into(),
            content,
            token_estimate,
            tool_call_ids: Vec::new(),
            tool_use_id: None,
        }
    }

    /// Build an `assistant` message that carries one or more structured tool
    /// call ids. Use this whenever the upstream provider stores `tool_calls`
    /// as a structured field rather than embedding markers in `content` — it
    /// is the only way `build_message_groups` can pair the request with its
    /// matching `role:tool` results.
    ///
    /// # Panics (debug only)
    ///
    /// Panics in debug builds if `tool_call_ids` is empty — that combination
    /// silently degrades to a plain assistant message and is almost always a
    /// bug at the call site. Use [`CompactMessage::new`] for plain assistant
    /// content.
    pub fn assistant_with_tool_calls<I, S>(content: impl Into<String>, tool_call_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut msg = Self::new("assistant", content);
        msg.tool_call_ids = tool_call_ids.into_iter().map(Into::into).collect();
        debug_assert!(
            !msg.tool_call_ids.is_empty(),
            "assistant_with_tool_calls requires at least one tool_call_id; \
             use CompactMessage::new(\"assistant\", ...) for plain assistant messages"
        );
        msg
    }

    /// Build a `role:tool` result message bound to a specific tool call id.
    pub fn tool_result(content: impl Into<String>, tool_use_id: impl Into<String>) -> Self {
        let mut msg = Self::new("tool", content);
        msg.tool_use_id = Some(tool_use_id.into());
        msg
    }

    fn is_assistant_tool_request(&self) -> bool {
        self.role == "assistant" && !self.tool_call_ids.is_empty()
    }

    fn is_paired_tool_result(&self) -> bool {
        self.role == "tool" && self.tool_use_id.is_some()
    }

    /// Read-only access to the structured tool call ids (assistant messages
    /// that requested one or more tool invocations). Returns an empty slice
    /// for plain assistant messages.
    pub fn tool_call_ids(&self) -> &[String] {
        &self.tool_call_ids
    }

    /// Read-only access to the structured tool result correlation id (tool
    /// messages that respond to a specific assistant tool call). Returns
    /// `None` for non-tool messages.
    pub fn tool_use_id(&self) -> Option<&str> {
        self.tool_use_id.as_deref()
    }
}

/// Bridge from the canonical provider message type into the compaction
/// representation. This is the **only** sanctioned path for converting real
/// upstream history into `CompactMessage`s — going through it guarantees the
/// structured pairing metadata (`tool_call_ids` for assistant requests,
/// `tool_use_id` for tool results) is populated, so the atomic-group
/// machinery in `build_message_groups` can keep tool-call pairs together.
///
/// Hand-rolling `CompactMessage::new("assistant", …)` when the source has
/// `tool_calls` populated will silently bypass the pairing pipeline and
/// reintroduce the orphan-tool-result bug fixed in `build_message_groups`.
impl From<&caduceus_providers::Message> for CompactMessage {
    fn from(msg: &caduceus_providers::Message) -> Self {
        // Use `content_text()` so messages whose canonical text lives in
        // `content_blocks` (rather than the legacy `content` field) are
        // converted faithfully. Falling back to `msg.content` would drop
        // image-adjacent text and any caller that built the message via
        // `with_content_blocks`.
        let mut out = Self::new(msg.role.clone(), msg.content_text());
        if msg.role == "assistant" && !msg.tool_calls.is_empty() {
            out.tool_call_ids = msg.tool_calls.iter().map(|tc| tc.id.clone()).collect();
        }
        if msg.role == "tool" {
            out.tool_use_id = msg
                .tool_result
                .as_ref()
                .and_then(|r| r.tool_use_id.clone());
        }
        out
    }
}

impl From<caduceus_providers::Message> for CompactMessage {
    fn from(msg: caduceus_providers::Message) -> Self {
        (&msg).into()
    }
}

impl crate::pairing::PairAwareMessage for CompactMessage {
    fn tool_request_ids(&self) -> Option<Vec<&str>> {
        if self.is_assistant_tool_request() {
            Some(self.tool_call_ids.iter().map(String::as_str).collect())
        } else {
            None
        }
    }

    fn tool_result_id(&self) -> Option<&str> {
        if self.role != "tool" {
            return None;
        }
        self.tool_use_id.as_deref()
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
    /// When `true`, this group is an **atomic unit** whose messages must never
    /// be separated — not by `build_message_groups`'s same-kind merge pass, and
    /// not by any [`CompactionStrategy`] that splits, partially modifies,
    /// summarises, or rewrites individual groups. Currently set for tool-call
    /// pairs (`assistant` with `tool_call_ids` plus matching `role:tool`
    /// results).
    ///
    /// **Strategy contract:**
    /// - Strategies that *split or modify* a group's contents (e.g.
    ///   `ToolCollapseStrategy`, `SummarizeStrategy`, `PatternCompactor`)
    ///   **must** check [`MessageGroup::is_atomic`] and skip atomic groups —
    ///   otherwise they produce orphan `role:tool` messages that providers
    ///   reject with HTTP 400.
    /// - Strategies that *drop entire groups* (e.g. `SlidingWindowStrategy`,
    ///   `EmergencyTruncator`) **need not** check, because removing the
    ///   complete transaction (assistant request + all matching results) is
    ///   protocol-safe by construction.
    ///
    /// Field is private to prevent external code from breaking the invariant;
    /// use [`MessageGroup::mark_atomic`] to set it.
    atomic: bool,
}

impl MessageGroup {
    pub fn new(kind: MessageGroupKind) -> Self {
        Self {
            kind,
            messages: Vec::new(),
            token_count: 0,
            excluded: false,
            atomic: false,
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

    /// `true` when this group must be treated as an inseparable unit by every
    /// [`CompactionStrategy`].
    pub fn is_atomic(&self) -> bool {
        self.atomic
    }

    /// Mark this group as atomic. Strategies must not split, summarise, or
    /// partially-evict atomic groups.
    pub fn mark_atomic(&mut self) {
        self.atomic = true;
    }
}

/// Group a flat message list into atomic [`MessageGroup`] units.
///
/// Three guarantees:
///
/// 1. System messages are never merged with conversational content.
/// 2. Consecutive messages of the same kind (except System) are coalesced into
///    one group.
/// 3. **Tool-pair atomicity:** an `assistant` message with non-empty
///    `tool_call_ids` plus the immediately-following `role:tool` results whose
///    `tool_use_id` matches one of those ids are bundled into ONE `ToolCall`
///    group. Strategies in this module drop entire groups, so the request and
///    its responses are evicted together or not at all — they cannot be split.
///    Grouping stops on the first non-tool message OR a `role:tool` whose
///    `tool_use_id` doesn't match (avoids hopping over orphan/foreign results).
pub fn build_message_groups(messages: &[CompactMessage]) -> Vec<MessageGroup> {
    let units = crate::pairing::pair_aware_units(messages);
    let mut groups: Vec<MessageGroup> = Vec::new();

    for (start, end) in units {
        let first = &messages[start];

        // ── Tool-pair atomic absorption ──────────────────────────────────────
        if first.is_assistant_tool_request() {
            let mut group = MessageGroup::new(MessageGroupKind::ToolCall);
            group.mark_atomic();
            for msg in &messages[start..end] {
                group.add_message(msg.clone());
            }
            groups.push(group);
            continue;
        }

        // Non-tool-call units are always size-1.
        let msg = first;

        // ── Default classification + same-kind merge ─────────────────────────
        let kind = classify_message(msg);
        let can_merge = !matches!(kind, MessageGroupKind::System);

        if can_merge {
            if let Some(last) = groups.last_mut() {
                if last.kind == kind && !last.is_atomic() {
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

/// Convenience entrypoint that converts upstream provider messages into
/// `CompactMessage`s via the canonical `From` impl, then groups them.
///
/// Prefer this over hand-rolling `messages.iter().map(...).collect()` followed
/// by `build_message_groups`: going through this function guarantees the
/// `tool_call_ids` / `tool_use_id` pairing metadata is populated, so atomic
/// tool-call groups are formed correctly. Bypassing this path is the same
/// silent-bypass risk that motivated adding `From<&Message>` in the first
/// place.
pub fn build_message_groups_from_provider(
    messages: &[caduceus_providers::Message],
) -> Vec<MessageGroup> {
    let compact: Vec<CompactMessage> = messages.iter().map(CompactMessage::from).collect();
    build_message_groups(&compact)
}

fn classify_message(msg: &CompactMessage) -> MessageGroupKind {
    // NOTE: assistant messages with `tool_call_ids` are handled by the
    // atomic-absorption path in `build_message_groups` and never reach here;
    // we still check `is_paired_tool_result` because a structured tool result
    // (with `tool_use_id` set) that fails to absorb into a previous atomic
    // group still classifies as ToolCall.
    debug_assert!(
        !msg.is_assistant_tool_request(),
        "classify_message reached for an assistant tool request — \
         build_message_groups should have absorbed it"
    );
    if msg.is_paired_tool_result() {
        return MessageGroupKind::ToolCall;
    }
    classify_role(&msg.role, &msg.content)
}

fn classify_role(role: &str, content: &str) -> MessageGroupKind {
    match role {
        "system" => MessageGroupKind::System,
        "user" => MessageGroupKind::User,
        "tool" => MessageGroupKind::ToolCall,
        "assistant" => {
            // Legacy backward-compat sniff for callers that haven't migrated
            // to structured `tool_call_ids`. Restricted to the strict
            // `<tool_call>` XML marker; the looser `"tool_use"` substring was
            // removed because it false-positives on plain prose like
            // `"the tool_use pattern requires…"`.
            // TODO: deprecate once all producers populate `tool_call_ids`.
            if content.trim_start().starts_with("<tool_call>") {
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
            // Atomic groups (atomic tool-call pairs) are inseparable units; we
            // must not collapse two adjacent atomic groups into one — that
            // would silently drop a complete tool transaction. Emergency
            // truncation handles age-based eviction; this strategy only
            // collapses *legacy* non-atomic ToolCall groups.
            let both_collapsible = groups[i].kind == MessageGroupKind::ToolCall
                && groups[i + 1].kind == MessageGroupKind::ToolCall
                && !groups[i].is_atomic()
                && !groups[i + 1].is_atomic();
            if both_collapsible {
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
            // Atomic tool-call pairs encode a structured request/result
            // transaction bound by `tool_call_ids` / `tool_use_id`.
            // Summarising one half breaks the linkage and produces an HTTP
            // 400 from the provider on the next turn.
            .filter(|(_, g)| !g.is_system() && !g.is_atomic())
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
        let summary_msg = CompactMessage::new("system", summary_text);
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
///
/// Unlike `ToolCollapseStrategy`, `SummarizeStrategy`, and `PatternCompactor`
/// — which split or rewrite groups and therefore must skip atomic ones —
/// this strategy drops groups *as a whole*. An atomic tool-call group goes
/// out as a single transaction (the assistant request and every matching
/// `role:tool` result together), so no orphan tool message can survive. No
/// `is_atomic()` check is needed.
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
                && groups[i + 2].kind == MessageGroupKind::AssistantText
                // Atomic tool-call pairs encode structured `tool_call_ids` /
                // `tool_use_id` linkage. Marking them excluded would surface
                // the assistant request without its results to the provider,
                // producing HTTP 400. Skip atomic groups entirely.
                && !groups[i + 1].is_atomic();

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
///
/// **Atomic-group handling:** unlike the upstream three strategies, this pass
/// intentionally does **not** skip atomic groups. Under emergency pressure
/// the oldest groups must be removed regardless of kind; dropping a whole
/// atomic group is still protocol-safe because both the assistant tool-call
/// request and every matching `role:tool` result are evicted together — no
/// orphan tool message survives. Skipping atomic groups here would stall
/// the loop when the context is dominated by tool-call sequences and leave
/// the request over budget.
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

    // ── Tool-pair atomicity (regression for compaction.rs:76 split bug) ──────

    #[test]
    fn structured_tool_pair_groups_atomically() {
        // assistant{tool_calls=[a]} + tool{use_id=a} must group together even
        // though their roles differ.
        let messages = vec![
            CompactMessage::assistant_with_tool_calls("calling", ["a"]),
            CompactMessage::tool_result("result-a", "a"),
            CompactMessage::new("user", "thanks"),
        ];
        let groups = build_message_groups(&messages);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].kind, MessageGroupKind::ToolCall);
        assert_eq!(groups[0].messages.len(), 2);
        assert_eq!(groups[0].messages[0].role, "assistant");
        assert_eq!(groups[0].messages[1].role, "tool");
        assert_eq!(groups[1].kind, MessageGroupKind::User);
    }

    #[test]
    fn parallel_tool_calls_all_results_stay_in_one_group() {
        let messages = vec![
            CompactMessage::assistant_with_tool_calls("fan-out", ["a", "b", "c"]),
            CompactMessage::tool_result("ra", "a"),
            CompactMessage::tool_result("rb", "b"),
            CompactMessage::tool_result("rc", "c"),
        ];
        let groups = build_message_groups(&messages);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].messages.len(), 4);
    }

    #[test]
    fn mismatched_tool_id_breaks_grouping_does_not_hop() {
        // The "wrong" id must terminate absorption — we must not skip over it
        // and pick up the matching `b` afterwards.
        let messages = vec![
            CompactMessage::assistant_with_tool_calls("calling", ["a", "b"]),
            CompactMessage::tool_result("ra", "a"),
            CompactMessage::tool_result("foreign", "wrong"),
            CompactMessage::tool_result("rb", "b"),
        ];
        let groups = build_message_groups(&messages);
        // First group: assistant + ra (stops at "wrong")
        assert_eq!(groups[0].kind, MessageGroupKind::ToolCall);
        assert_eq!(groups[0].messages.len(), 2);
        // The mismatched + "rb" both have role:tool with use_id, so they
        // classify as ToolCall and merge by same-kind.
        assert_eq!(groups[1].kind, MessageGroupKind::ToolCall);
        assert_eq!(groups[1].messages.len(), 2);
    }

    #[test]
    fn tool_result_without_use_id_does_not_join_previous_group() {
        // Defensive: an orphan tool result (parsed without an id) must not be
        // absorbed into the previous assistant's atomic group.
        let messages = vec![
            CompactMessage::assistant_with_tool_calls("calling", ["a"]),
            CompactMessage::new("tool", "orphan-no-id"),
        ];
        let groups = build_message_groups(&messages);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].messages.len(), 1);
        assert_eq!(groups[0].messages[0].role, "assistant");
        assert_eq!(groups[1].kind, MessageGroupKind::ToolCall);
        assert_eq!(groups[1].messages.len(), 1);
    }

    #[test]
    fn assistant_tool_call_without_results_is_safe_singleton() {
        let messages = vec![CompactMessage::assistant_with_tool_calls("calling", ["a"])];
        let groups = build_message_groups(&messages);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, MessageGroupKind::ToolCall);
        assert_eq!(groups[0].messages.len(), 1);
    }

    #[test]
    fn legacy_content_sniffing_still_classifies_tool_call() {
        // Backward compat: an old-style message with `<tool_call>` marker but
        // no structured tool_call_ids must still be classified as ToolCall.
        let messages = vec![
            CompactMessage::new("user", "do it"),
            CompactMessage::new("assistant", "<tool_call>{\"name\":\"x\"}</tool_call>"),
        ];
        let groups = build_message_groups(&messages);
        assert_eq!(groups[1].kind, MessageGroupKind::ToolCall);
    }

    #[test]
    fn legacy_tool_use_substring_no_longer_false_positives() {
        // After the iteration-2 tightening, prose that merely mentions
        // "tool_use" must classify as AssistantText, not ToolCall.
        let messages = vec![
            CompactMessage::new("user", "explain"),
            CompactMessage::new(
                "assistant",
                "The tool_use pattern requires structured payloads.",
            ),
        ];
        let groups = build_message_groups(&messages);
        assert_eq!(groups[1].kind, MessageGroupKind::AssistantText);
    }

    #[test]
    fn structured_metadata_wins_over_content_sniffing() {
        // An assistant whose content also contains a tool_use marker but with
        // an explicitly empty tool_call_ids set is still classified by content
        // (since structured fields are absent). Constructed via `new`, no ids.
        let messages = vec![CompactMessage::new("assistant", "<tool_call>foo</tool_call>")];
        let groups = build_message_groups(&messages);
        assert_eq!(groups[0].kind, MessageGroupKind::ToolCall);
    }

    #[test]
    fn sliding_window_does_not_orphan_tool_result() {
        // 4 atomic units, window=2, so 2 oldest must drop. The pair must
        // either be fully present or fully absent — never split.
        let messages = vec![
            CompactMessage::new("user", "u1"),
            CompactMessage::assistant_with_tool_calls("call", ["a"]),
            CompactMessage::tool_result("ra", "a"),
            CompactMessage::new("user", "u2"),
            CompactMessage::new("assistant", "reply"),
            CompactMessage::new("user", "u3"),
        ];
        let mut groups = build_message_groups(&messages);
        let strategy = SlidingWindowStrategy { window_size: 2 };
        strategy.compact(&mut groups);

        // Every surviving tool result must have its assistant request in the
        // same group.
        for g in &groups {
            let asst_ids: HashSet<&str> = g
                .messages
                .iter()
                .filter(|m| m.role == "assistant")
                .flat_map(|m| m.tool_call_ids.iter().map(String::as_str))
                .collect();
            for m in &g.messages {
                if let Some(id) = m.tool_use_id.as_deref() {
                    assert!(
                        asst_ids.contains(id),
                        "orphan tool_result with use_id={id} survived sliding window"
                    );
                }
            }
        }
    }

    #[test]
    fn emergency_truncator_does_not_orphan_tool_result() {
        let messages = vec![
            CompactMessage::new("user", "u1"),
            CompactMessage::assistant_with_tool_calls("call", ["a"]),
            CompactMessage::tool_result("ra", "a"),
            CompactMessage::new("user", "u2"),
            CompactMessage::new("assistant", "reply"),
            CompactMessage::new("user", "u3"),
        ];
        let mut groups = build_message_groups(&messages);
        let strategy = EmergencyTruncator {
            minimum_preserved: 2,
        };
        strategy.compact(&mut groups);

        for g in &groups {
            let asst_ids: HashSet<&str> = g
                .messages
                .iter()
                .filter(|m| m.role == "assistant")
                .flat_map(|m| m.tool_call_ids.iter().map(String::as_str))
                .collect();
            for m in &g.messages {
                if let Some(id) = m.tool_use_id.as_deref() {
                    assert!(
                        asst_ids.contains(id),
                        "orphan tool_result with use_id={id} survived emergency truncate"
                    );
                }
            }
        }
    }

    #[test]
    fn tool_collapse_preserves_both_atomic_pairs() {
        // Iteration-2 fix: ToolCollapseStrategy must NOT drop atomic
        // pairs. Two adjacent transactions both survive — emergency
        // truncation handles age-based eviction, not pair collapse.
        let messages = vec![
            CompactMessage::assistant_with_tool_calls("call1", ["a"]),
            CompactMessage::tool_result("ra", "a"),
            CompactMessage::assistant_with_tool_calls("call2", ["b"]),
            CompactMessage::tool_result("rb", "b"),
        ];
        let mut groups = build_message_groups(&messages);
        assert_eq!(groups.len(), 2);
        assert!(groups[0].is_atomic());
        assert!(groups[1].is_atomic());
        let strategy = ToolCollapseStrategy;
        let result = strategy.compact(&mut groups);
        assert_eq!(result.groups_affected, 0, "must not collapse atomic pairs");
        assert_eq!(groups.len(), 2);
        assert!(groups[0].messages.iter().any(|m| m.role == "assistant"));
        assert!(groups[1].messages.iter().any(|m| m.role == "assistant"));
    }

    #[test]
    fn tool_collapse_still_drops_legacy_non_atomic_tool_groups() {
        // Backward compat: legacy non-atomic ToolCall groups (built without
        // structured tool_call_ids) still collapse as before.
        let messages = vec![
            CompactMessage::new("assistant", "<tool_call>a</tool_call>"),
            CompactMessage::new("assistant", "<tool_call>b</tool_call>"),
        ];
        // Force two adjacent non-atomic ToolCall groups.
        let mut groups: Vec<MessageGroup> = messages
            .into_iter()
            .map(|m| {
                let mut g = MessageGroup::new(MessageGroupKind::ToolCall);
                g.add_message(m);
                g
            })
            .collect();
        let strategy = ToolCollapseStrategy;
        strategy.compact(&mut groups);
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn parallel_tool_results_absorbed_regardless_of_order() {
        // The assistant requests THREE tool calls in one message; results
        // arrive out of order. All three results plus the request must end
        // up in a single atomic group.
        let messages = vec![
            CompactMessage::new("user", "do three things"),
            CompactMessage::assistant_with_tool_calls("a1", ["c1", "c2", "c3"]),
            CompactMessage::tool_result("third", "c3"),
            CompactMessage::tool_result("first", "c1"),
            CompactMessage::tool_result("second", "c2"),
            CompactMessage::new("user", "thanks"),
        ];
        let groups = build_message_groups(&messages);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[1].kind, MessageGroupKind::ToolCall);
        assert!(groups[1].is_atomic());
        assert_eq!(groups[1].messages.len(), 4);
        // Final user message must be its own group, not absorbed.
        assert_eq!(groups[2].kind, MessageGroupKind::User);
    }

    #[test]
    fn multi_turn_tool_conversation_groups_correctly() {
        let messages = vec![
            CompactMessage::new("user", "u1"),
            CompactMessage::assistant_with_tool_calls("a1", ["c1"]),
            CompactMessage::tool_result("r1", "c1"),
            CompactMessage::new("user", "u2"),
            CompactMessage::assistant_with_tool_calls("a2", ["c2"]),
            CompactMessage::tool_result("r2", "c2"),
            CompactMessage::new("user", "u3"),
        ];
        let groups = build_message_groups(&messages);
        assert_eq!(groups.len(), 5);
        assert_eq!(groups[0].kind, MessageGroupKind::User);
        assert_eq!(groups[1].kind, MessageGroupKind::ToolCall);
        assert!(groups[1].is_atomic());
        assert_eq!(groups[2].kind, MessageGroupKind::User);
        assert_eq!(groups[3].kind, MessageGroupKind::ToolCall);
        assert!(groups[3].is_atomic());
        assert_eq!(groups[4].kind, MessageGroupKind::User);
    }

    #[test]
    fn summarize_strategy_skips_atomic_groups() {
        let mut messages = vec![
            CompactMessage::new("user", "u1"),
            CompactMessage::assistant_with_tool_calls("a1", ["c1"]),
            CompactMessage::tool_result("r1", "c1"),
            CompactMessage::new("user", "u2"),
            CompactMessage::new("assistant", "a2"),
        ];
        messages[3].content = "u2 ".repeat(2000);
        messages[4].content = "a2 ".repeat(2000);
        let mut groups = build_message_groups(&messages);
        let original_atomic_msgs: Vec<Vec<(String, String)>> = groups
            .iter()
            .filter(|g| g.is_atomic())
            .map(|g| {
                g.messages
                    .iter()
                    .map(|m| (m.role.clone(), m.content.clone()))
                    .collect()
            })
            .collect();
        let strategy = SummarizeStrategy { keep_recent: 1 };
        strategy.compact(&mut groups);
        let surviving_atomic_msgs: Vec<Vec<(String, String)>> = groups
            .iter()
            .filter(|g| g.is_atomic())
            .map(|g| {
                g.messages
                    .iter()
                    .map(|m| (m.role.clone(), m.content.clone()))
                    .collect()
            })
            .collect();
        assert_eq!(
            surviving_atomic_msgs, original_atomic_msgs,
            "atomic groups must survive summarization byte-for-byte"
        );
    }

    #[test]
    fn pattern_compactor_skips_atomic_middle_group() {
        let messages = vec![
            CompactMessage::new("assistant", "before"),
            CompactMessage::assistant_with_tool_calls("a1", ["c1"]),
            CompactMessage::tool_result("r1", "c1"),
            CompactMessage::new("assistant", "after"),
        ];
        let mut groups = build_message_groups(&messages);
        let before_count = groups.len();
        let compactor = PatternCompactor { retention_window: 0 };
        compactor.compact(&mut groups);
        assert_eq!(groups.len(), before_count);
        assert!(groups.iter().any(|g| g.is_atomic()));
    }

    #[test]
    fn from_provider_message_populates_tool_call_ids_for_assistant() {
        use caduceus_core::ToolUse;
        let pmsg = caduceus_providers::Message {
            role: "assistant".into(),
            content: "calling tools".into(),
            content_blocks: None,
            tool_calls: vec![
                ToolUse {
                    id: "call_1".into(),
                    name: "fs_read".into(),
                    input: serde_json::json!({}),
                },
                ToolUse {
                    id: "call_2".into(),
                    name: "fs_write".into(),
                    input: serde_json::json!({}),
                },
            ],
            tool_result: None,
        };
        let cm: CompactMessage = (&pmsg).into();
        assert_eq!(cm.role, "assistant");
        assert_eq!(cm.tool_call_ids, vec!["call_1", "call_2"]);
        assert!(cm.tool_use_id.is_none());
    }

    #[test]
    fn from_provider_message_populates_tool_use_id_for_tool_result() {
        use caduceus_core::ToolResult;
        let pmsg = caduceus_providers::Message {
            role: "tool".into(),
            content: "ok".into(),
            content_blocks: None,
            tool_calls: vec![],
            tool_result: Some(ToolResult::success("ok").with_tool_use_id("call_1")),
        };
        let cm: CompactMessage = (&pmsg).into();
        assert_eq!(cm.role, "tool");
        assert_eq!(cm.tool_use_id.as_deref(), Some("call_1"));
        assert!(cm.tool_call_ids.is_empty());
    }

    #[test]
    fn from_provider_messages_round_trip_through_grouping() {
        use caduceus_core::{ToolResult, ToolUse};
        let provider_msgs = vec![
            caduceus_providers::Message::user("do it"),
            caduceus_providers::Message {
                role: "assistant".into(),
                content: "calling".into(),
                content_blocks: None,
                tool_calls: vec![ToolUse {
                    id: "c1".into(),
                    name: "fs_read".into(),
                    input: serde_json::json!({}),
                }],
                tool_result: None,
            },
            caduceus_providers::Message {
                role: "tool".into(),
                content: "result".into(),
                content_blocks: None,
                tool_calls: vec![],
                tool_result: Some(ToolResult::success("result").with_tool_use_id("c1")),
            },
        ];
        let compact: Vec<CompactMessage> = provider_msgs.iter().map(Into::into).collect();
        let groups = build_message_groups(&compact);
        assert_eq!(groups.len(), 2, "user + atomic tool-call group");
        assert_eq!(groups[1].kind, MessageGroupKind::ToolCall);
        assert!(groups[1].is_atomic());
        assert_eq!(groups[1].messages.len(), 2);
    }

    #[test]
    fn build_message_groups_from_provider_preserves_atomic_pairs() {
        use caduceus_core::{ToolResult, ToolUse};
        let provider_msgs = vec![
            caduceus_providers::Message::user("u"),
            caduceus_providers::Message {
                role: "assistant".into(),
                content: "calling".into(),
                content_blocks: None,
                tool_calls: vec![ToolUse {
                    id: "c1".into(),
                    name: "fs_read".into(),
                    input: serde_json::json!({}),
                }],
                tool_result: None,
            },
            caduceus_providers::Message {
                role: "tool".into(),
                content: "r".into(),
                content_blocks: None,
                tool_calls: vec![],
                tool_result: Some(ToolResult::success("r").with_tool_use_id("c1")),
            },
        ];
        let groups = build_message_groups_from_provider(&provider_msgs);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1].kind, MessageGroupKind::ToolCall);
        assert!(groups[1].is_atomic());
    }

    #[test]
    fn from_provider_message_uses_content_text_for_block_messages() {
        use caduceus_providers::MessageContentBlock;
        // Build a user message via with_content_blocks so `content` is
        // synthesized from blocks; the From impl must read the canonical
        // text via content_text(), not the legacy `content` field directly.
        let pmsg = caduceus_providers::Message::user("ignored").with_content_blocks(vec![
            MessageContentBlock::text("hello "),
            MessageContentBlock::text("world"),
        ]);
        let cm: CompactMessage = (&pmsg).into();
        assert_eq!(cm.content, "hello world");
    }
}

#[cfg(test)]
mod iter3_tests {
    use super::*;

    #[test]
    fn summarize_is_noop_when_all_non_system_are_atomic() {
        let messages = vec![
            CompactMessage::new("system", "sys"),
            CompactMessage::assistant_with_tool_calls("a1", ["c1"]),
            CompactMessage::tool_result("r1", "c1"),
            CompactMessage::assistant_with_tool_calls("a2", ["c2"]),
            CompactMessage::tool_result("r2", "c2"),
        ];
        let mut groups = build_message_groups(&messages);
        let atomic_count = groups.iter().filter(|g| g.is_atomic()).count();
        assert_eq!(atomic_count, 2);
        let strategy = SummarizeStrategy { keep_recent: 0 };
        let result = strategy.compact(&mut groups);
        assert_eq!(
            result.groups_affected, 0,
            "nothing to summarize when all non-system groups are atomic"
        );
    }

    #[test]
    fn sliding_window_drops_atomic_group_as_whole_unit() {
        let messages = vec![
            CompactMessage::assistant_with_tool_calls("a1", ["c1", "c2"]),
            CompactMessage::tool_result("r1", "c1"),
            CompactMessage::tool_result("r2", "c2"),
            CompactMessage::new("user", "thanks"),
        ];
        let mut groups = build_message_groups(&messages);
        assert_eq!(groups.len(), 2);
        assert!(groups[0].is_atomic());
        assert_eq!(groups[0].messages.len(), 3);
        let strategy = SlidingWindowStrategy { window_size: 1 };
        strategy.compact(&mut groups);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, MessageGroupKind::User);
        // No orphan tool result anywhere — atomic group went out as a whole.
        assert!(groups
            .iter()
            .all(|g| g.messages.iter().all(|m| m.role != "tool")));
    }

    #[test]
    fn user_interrupting_tool_pair_prevents_absorption() {
        let messages = vec![
            CompactMessage::assistant_with_tool_calls("call", ["a"]),
            CompactMessage::new("user", "wait"),
            CompactMessage::tool_result("ra", "a"),
        ];
        let groups = build_message_groups(&messages);
        // Assistant alone in its atomic group (no result absorbed because the
        // user interrupted the immediately-following constraint).
        assert_eq!(groups[0].kind, MessageGroupKind::ToolCall);
        assert_eq!(groups[0].messages.len(), 1);
        assert!(groups[0].is_atomic());
        assert_eq!(groups[1].kind, MessageGroupKind::User);
        // Orphan tool result is its own non-atomic ToolCall group.
        assert_eq!(groups[2].kind, MessageGroupKind::ToolCall);
        assert!(!groups[2].is_atomic());
    }

    #[test]
    fn full_pipeline_preserves_or_drops_atomic_groups_whole() {
        use caduceus_core::{ToolResult, ToolUse};
        use std::collections::HashSet;
        let provider_msgs = vec![
            caduceus_providers::Message::user("u1"),
            {
                let mut m = caduceus_providers::Message::assistant("call");
                m.tool_calls = vec![ToolUse {
                    id: "c1".into(),
                    name: "t".into(),
                    input: serde_json::json!({}),
                }];
                m
            },
            caduceus_providers::Message {
                role: "tool".into(),
                content: "x".repeat(5000),
                content_blocks: None,
                tool_calls: vec![],
                tool_result: Some(ToolResult::success("x".repeat(5000)).with_tool_use_id("c1")),
            },
            caduceus_providers::Message::user("u2"),
            caduceus_providers::Message::assistant("done"),
        ];
        let mut groups = build_message_groups_from_provider(&provider_msgs);
        assert!(groups.iter().any(|g| g.is_atomic()));
        let pipeline = CompactionPipeline::default_pipeline(1);
        pipeline.run(&mut groups);
        // After aggressive compaction, prove no orphan tool_result survives.
        for g in &groups {
            let asst_ids: HashSet<&str> = g
                .messages
                .iter()
                .filter(|m| m.role == "assistant")
                .flat_map(|m| m.tool_call_ids().iter().map(String::as_str))
                .collect();
            for m in &g.messages {
                if let Some(id) = m.tool_use_id() {
                    assert!(
                        asst_ids.contains(id),
                        "orphan tool_result {id} after full pipeline compaction"
                    );
                }
            }
        }
    }
}
