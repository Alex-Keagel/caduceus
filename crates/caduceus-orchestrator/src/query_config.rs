//! Per-query model overrides (parses `/config` command args).
//!
//! Extracted from `lib.rs` — see ST-B1 Wave 0c.

use caduceus_core::ModelId;
use serde::{Deserialize, Serialize};

// ── P1: Query Configuration ────────────────────────────────────────────────────

/// Per-query overrides for model parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryConfig {
    pub model: Option<ModelId>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl QueryConfig {
    /// Parse from `/config` command args like `model=gpt-4 temp=0.5 tokens=8192`.
    pub fn parse(args: &str) -> Self {
        let mut config = Self::default();
        for part in args.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                match key {
                    "model" => config.model = Some(ModelId::new(value)),
                    "temp" | "temperature" => config.temperature = value.parse().ok(),
                    "tokens" | "max_tokens" => config.max_tokens = value.parse().ok(),
                    _ => {}
                }
            }
        }
        config
    }
}
