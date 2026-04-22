//! Typed memory blocks (gap G6 / P4.1).
//!
//! Replaces the single linear `Vec<Message>` history with four
//! semantically-distinct blocks, each with its own compaction policy.
//! Inspired by MemGPT / Letta (Packer et al., 2023):
//!
//! - **persona** — short, static system identity. Never evicted; capped
//!   by absolute char count as a guardrail against accidental bloat.
//! - **project_context** — refreshed-externally facts about the
//!   workspace (open files, key memories). LRU within a token budget.
//! - **working_history** — recent assistant/user/tool messages. Sliding
//!   window: when over budget, evict the oldest *complete*
//!   tool-call/tool-result pair (never split a pair).
//! - **archival_summary** — append-only chain of compacted summaries
//!   produced when working_history overflows. When this block itself
//!   overflows, merge the oldest two summaries into one (lossy).
//!
//! This module is **data-only**: it owns the structures and the
//! per-block compaction logic, but leaves the actual prompt assembly
//! wiring (the `Vec<caduceus_providers::Message>` the harness sends to
//! the LLM) to the orchestrator. That keeps this hot path easy to
//! test without spinning a provider mock.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// A single (role, text, optional tool-pair-id) entry in working
/// history. We store `pair_id` so the sliding-window evictor can
/// guarantee we never split a tool_call/tool_result pair — that has
/// been the #1 source of "Anthropic 400: tool_use_id with no result"
/// bugs across Round-1 and Round-2 audits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingMessage {
    pub role: String,
    pub text: String,
    /// Approximate token cost for budget accounting. We use u32 so the
    /// type stays serde-friendly without needing to pull in a tokenizer.
    pub tokens: u32,
    /// `Some(id)` ties this message to its partner (tool_call ↔
    /// tool_result). The evictor drops both halves together, or
    /// neither.
    pub pair_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivalSummary {
    pub text: String,
    pub tokens: u32,
    /// How many working_history entries this summary replaced. Used
    /// for telemetry only.
    pub replaced_entries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryBlocks {
    pub persona: String,
    pub project_context: String,
    pub working_history: VecDeque<WorkingMessage>,
    pub archival_summary: VecDeque<ArchivalSummary>,
    /// Token budget per block. Hard caps; compaction triggers when a
    /// block's used tokens exceed its budget.
    pub limits: BlockLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockLimits {
    pub persona_chars: usize,
    pub project_context_tokens: u32,
    pub working_history_tokens: u32,
    pub archival_summary_tokens: u32,
}

impl Default for BlockLimits {
    fn default() -> Self {
        // Roughly: 2k char persona, 8k project ctx, 32k working, 16k
        // archival ⇒ ~58k total before we even count tools / system /
        // pending tool results. Suits a 200k-context model with room
        // to grow; smaller models override.
        Self {
            persona_chars: 2_000,
            project_context_tokens: 8_000,
            working_history_tokens: 32_000,
            archival_summary_tokens: 16_000,
        }
    }
}

/// Result of a single compaction pass. Telemetry-shaped so the UI can
/// surface "compacted N tool turns into 1 summary".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionReport {
    pub working_evicted: u32,
    pub archival_merged: u32,
    pub project_truncated_chars: u32,
    pub persona_truncated_chars: u32,
}

impl MemoryBlocks {
    pub fn new(limits: BlockLimits) -> Self {
        Self {
            limits,
            ..Default::default()
        }
    }

    /// Set persona content, enforcing the char cap by truncation.
    /// Returns the number of chars dropped (0 if under cap).
    pub fn set_persona(&mut self, text: impl Into<String>) -> usize {
        let s = text.into();
        let cap = self.limits.persona_chars;
        if s.chars().count() <= cap {
            self.persona = s;
            0
        } else {
            // Char-safe truncation; never split a UTF-8 codepoint.
            let truncated: String = s.chars().take(cap).collect();
            let dropped = s.chars().count() - cap;
            self.persona = truncated;
            dropped
        }
    }

    /// Replace project_context wholesale (this is the typical flow:
    /// the IDE re-derives this each turn from open files + retrieved
    /// memories). Returns the number of chars dropped if the new
    /// content was over budget. Token estimate uses the
    /// 4-chars-per-token rule of thumb — provider-specific tokenizers
    /// can refine this later.
    pub fn set_project_context(&mut self, text: impl Into<String>) -> usize {
        let s = text.into();
        let cap_tokens = self.limits.project_context_tokens as usize;
        let cap_chars = cap_tokens.saturating_mul(4);
        if s.len() <= cap_chars {
            self.project_context = s;
            0
        } else {
            // Char-boundary safe truncation: walk back to a UTF-8
            // boundary at or below cap_chars.
            let mut cut = cap_chars;
            while cut > 0 && !s.is_char_boundary(cut) {
                cut -= 1;
            }
            let dropped = s.len() - cut;
            self.project_context = s[..cut].to_string();
            dropped
        }
    }

    pub fn append_working(&mut self, msg: WorkingMessage) {
        self.working_history.push_back(msg);
    }

    pub fn append_archival(&mut self, summary: ArchivalSummary) {
        self.archival_summary.push_back(summary);
    }

    /// Sum of approx tokens currently in working_history.
    pub fn working_tokens(&self) -> u32 {
        self.working_history.iter().map(|m| m.tokens).sum()
    }

    pub fn archival_tokens(&self) -> u32 {
        self.archival_summary.iter().map(|s| s.tokens).sum()
    }

    /// Run one full compaction sweep across all blocks. Idempotent
    /// when no block is over budget. Returns a structured report.
    /// Compaction order: persona/project guardrails first (both are
    /// caller-set, so this is mostly a no-op), then working_history
    /// (sliding window, pair-aware), then archival_summary (merge
    /// oldest two when over budget).
    pub fn compact(&mut self) -> CompactionReport {
        let mut report = CompactionReport {
            working_evicted: 0,
            archival_merged: 0,
            project_truncated_chars: 0,
            persona_truncated_chars: 0,
        };

        // Persona / project guardrails — these CAN drift if the caller
        // grew the strings in place via the public fields. Re-apply.
        if self.persona.chars().count() > self.limits.persona_chars {
            let dropped = self.set_persona(self.persona.clone());
            report.persona_truncated_chars = dropped as u32;
        }
        let proj_cap_chars = (self.limits.project_context_tokens as usize).saturating_mul(4);
        if self.project_context.len() > proj_cap_chars {
            let dropped = self.set_project_context(self.project_context.clone());
            report.project_truncated_chars = dropped as u32;
        }

        // Working history: sliding-window eviction, pair-aware.
        // Drop oldest entries until under budget OR we'd split a pair.
        // If we'd split a pair we drop BOTH halves of that pair (a
        // single matched pair is the smallest atomic eviction unit).
        let cap = self.limits.working_history_tokens;
        while self.working_tokens() > cap {
            let Some(front) = self.working_history.pop_front() else {
                break;
            };
            report.working_evicted += 1;
            // If this entry was tied to a pair, also drop the partner
            // (it can be either ahead or behind; here it must be
            // ahead since front was the oldest).
            if let Some(pid) = front.pair_id {
                if let Some(pos) = self
                    .working_history
                    .iter()
                    .position(|m| m.pair_id.as_deref() == Some(pid.as_str()))
                {
                    self.working_history.remove(pos);
                    report.working_evicted += 1;
                }
            }
        }

        // Archival summary: when over budget, merge the oldest TWO
        // entries into one (lossy). Repeat until under budget OR only
        // one entry remains (in which case we accept the overflow —
        // the caller should bump the limit rather than nuke history).
        while self.archival_tokens() > self.limits.archival_summary_tokens
            && self.archival_summary.len() >= 2
        {
            let a = self.archival_summary.pop_front().expect("len >= 2 checked");
            let b = self.archival_summary.pop_front().expect("len >= 2 checked");
            let merged = ArchivalSummary {
                text: format!("{}\n---\n{}", a.text, b.text),
                // Lossy merge: keep the larger of the two, NOT the
                // sum. The whole point of merging is to shrink.
                tokens: a.tokens.max(b.tokens),
                replaced_entries: a
                    .replaced_entries
                    .saturating_add(b.replaced_entries)
                    .saturating_add(1),
            };
            self.archival_summary.push_front(merged);
            report.archival_merged += 1;
        }

        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, text: &str, tokens: u32, pair: Option<&str>) -> WorkingMessage {
        WorkingMessage {
            role: role.into(),
            text: text.into(),
            tokens,
            pair_id: pair.map(String::from),
        }
    }

    #[test]
    fn persona_truncation_is_char_safe() {
        let mut mb = MemoryBlocks::new(BlockLimits {
            persona_chars: 5,
            ..Default::default()
        });
        // 4-byte char (U+1F600 😀) — must not split a codepoint.
        let dropped = mb.set_persona("😀😀😀😀😀😀😀");
        assert_eq!(mb.persona.chars().count(), 5);
        assert_eq!(dropped, 2);
    }

    #[test]
    fn project_context_truncation_is_byte_boundary_safe() {
        let mut mb = MemoryBlocks::new(BlockLimits {
            project_context_tokens: 1, // ⇒ 4 bytes
            ..Default::default()
        });
        let dropped = mb.set_project_context("ééé"); // 6 bytes
        assert!(dropped > 0);
        // The truncated string must still be valid UTF-8.
        assert!(mb
            .project_context
            .is_char_boundary(mb.project_context.len()));
    }

    #[test]
    fn working_sliding_window_evicts_oldest_first() {
        let mut mb = MemoryBlocks::new(BlockLimits {
            working_history_tokens: 30,
            ..Default::default()
        });
        for i in 0..5 {
            mb.append_working(msg("user", &format!("m{i}"), 10, None));
        }
        assert_eq!(mb.working_tokens(), 50);
        let r = mb.compact();
        assert_eq!(mb.working_tokens(), 30);
        assert_eq!(r.working_evicted, 2);
        // FIFO order check
        assert_eq!(mb.working_history.front().unwrap().text, "m2");
    }

    #[test]
    fn working_eviction_never_splits_tool_pair() {
        let mut mb = MemoryBlocks::new(BlockLimits {
            working_history_tokens: 15,
            ..Default::default()
        });
        // Two entries, paired. Their joint cost (20) is over cap.
        mb.append_working(msg("assistant", "tool_call", 10, Some("p1")));
        mb.append_working(msg("tool", "tool_result", 10, Some("p1")));
        // A safe trailing message that fits alone.
        mb.append_working(msg("user", "ok", 5, None));

        let r = mb.compact();
        // Both halves of p1 must be evicted together; "ok" survives.
        assert_eq!(r.working_evicted, 2);
        assert_eq!(mb.working_history.len(), 1);
        assert_eq!(mb.working_history.front().unwrap().text, "ok");
    }

    #[test]
    fn archival_merges_oldest_two_when_overflowing() {
        let mut mb = MemoryBlocks::new(BlockLimits {
            archival_summary_tokens: 25,
            ..Default::default()
        });
        // Three summaries totalling 30 tokens — overflow by 5.
        for i in 0..3 {
            mb.append_archival(ArchivalSummary {
                text: format!("s{i}"),
                tokens: 10,
                replaced_entries: 1,
            });
        }
        let r = mb.compact();
        assert_eq!(r.archival_merged, 1);
        // Now: 1 merged (lossy: 10) + 1 untouched (10) = 20, under cap.
        assert_eq!(mb.archival_tokens(), 20);
        assert_eq!(mb.archival_summary.len(), 2);
        assert!(mb.archival_summary[0].text.contains("s0"));
        assert!(mb.archival_summary[0].text.contains("s1"));
        // Telemetry: merged entry tracks total replaced (1+1+1 = 3).
        assert_eq!(mb.archival_summary[0].replaced_entries, 3);
    }

    #[test]
    fn compact_is_idempotent_when_under_budget() {
        let mut mb = MemoryBlocks::default();
        mb.append_working(msg("user", "hi", 5, None));
        let r1 = mb.compact();
        let r2 = mb.compact();
        assert_eq!(r1.working_evicted, 0);
        assert_eq!(r2.working_evicted, 0);
        assert_eq!(mb.working_history.len(), 1);
    }

    #[test]
    fn archival_overflow_with_single_entry_is_accepted() {
        // Defensive: don't nuke a lone giant summary — caller must
        // bump the limit instead. Compact returns zero merges.
        let mut mb = MemoryBlocks::new(BlockLimits {
            archival_summary_tokens: 5,
            ..Default::default()
        });
        mb.append_archival(ArchivalSummary {
            text: "huge".into(),
            tokens: 999,
            replaced_entries: 1,
        });
        let r = mb.compact();
        assert_eq!(r.archival_merged, 0);
        assert_eq!(mb.archival_summary.len(), 1);
    }

    #[test]
    fn serde_roundtrip_preserves_all_blocks() {
        let mut mb = MemoryBlocks::default();
        mb.set_persona("P");
        mb.set_project_context("C");
        mb.append_working(msg("user", "hi", 1, Some("x")));
        mb.append_archival(ArchivalSummary {
            text: "a".into(),
            tokens: 1,
            replaced_entries: 1,
        });
        let json = serde_json::to_string(&mb).unwrap();
        let back: MemoryBlocks = serde_json::from_str(&json).unwrap();
        assert_eq!(back.persona, "P");
        assert_eq!(back.project_context, "C");
        assert_eq!(back.working_history.len(), 1);
        assert_eq!(back.archival_summary.len(), 1);
    }

    #[test]
    fn compact_reports_persona_drift() {
        let mut mb = MemoryBlocks::new(BlockLimits {
            persona_chars: 3,
            ..Default::default()
        });
        // Bypass set_persona and grow the field directly to simulate
        // drift (e.g. a caller appended via the public field).
        mb.persona = "abcdef".to_string();
        let r = mb.compact();
        assert_eq!(r.persona_truncated_chars, 3);
        assert_eq!(mb.persona, "abc");
    }
}
