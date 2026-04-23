//! Effort level (controls detail of LLM interactions).
//!
//! Extracted from `lib.rs` — see ST-B1 Wave 0c.

use serde::{Deserialize, Serialize};

// ── P1: Effort Levels ──────────────────────────────────────────────────────────

/// Controls the detail level of LLM interactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffortLevel {
    Min,
    Low,
    Medium,
    High,
    Max,
}

impl EffortLevel {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "min" | "minimum" => Some(Self::Min),
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "max" | "maximum" => Some(Self::Max),
            _ => None,
        }
    }

    /// System prompt detail level description.
    pub fn system_prompt_detail(&self) -> &'static str {
        match self {
            Self::Min => "Be extremely concise. One sentence max.",
            Self::Low => "Be brief. Short paragraphs only.",
            Self::Medium => "Provide balanced detail with examples when helpful.",
            Self::High => "Be thorough. Include examples, edge cases, and alternatives.",
            Self::Max => {
                "Be exhaustive. Cover every detail, edge case, alternative, and implication."
            }
        }
    }

    /// Suggested max_tokens for this effort level.
    pub fn max_tokens(&self) -> u32 {
        match self {
            Self::Min => 256,
            Self::Low => 1024,
            Self::Medium => 8192,
            Self::High => 16384,
            Self::Max => 32768,
        }
    }

    /// Suggested temperature for this effort level.
    pub fn temperature(&self) -> f32 {
        match self {
            Self::Min => 0.0,
            Self::Low => 0.2,
            Self::Medium => 0.5,
            Self::High => 0.7,
            Self::Max => 0.8,
        }
    }
}
