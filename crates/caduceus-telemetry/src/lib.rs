use caduceus_core::{ModelId, ProviderId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Token counting ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
}

impl TokenUsage {
    pub fn total(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }
}

pub struct TokenCounter {
    session_usage: TokenUsage,
    total_usage: TokenUsage,
}

impl TokenCounter {
    pub fn new() -> Self {
        Self {
            session_usage: TokenUsage::default(),
            total_usage: TokenUsage::default(),
        }
    }

    pub fn record(&mut self, usage: &TokenUsage) {
        self.session_usage.input_tokens += usage.input_tokens;
        self.session_usage.output_tokens += usage.output_tokens;
        self.session_usage.cached_tokens += usage.cached_tokens;
        self.total_usage.input_tokens += usage.input_tokens;
        self.total_usage.output_tokens += usage.output_tokens;
        self.total_usage.cached_tokens += usage.cached_tokens;
    }

    pub fn session_usage(&self) -> &TokenUsage {
        &self.session_usage
    }

    pub fn total_usage(&self) -> &TokenUsage {
        &self.total_usage
    }

    pub fn reset_session(&mut self) {
        self.session_usage = TokenUsage::default();
    }
}

impl Default for TokenCounter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Cost logging ───────────────────────────────────────────────────────────────

/// Pricing per million tokens (input, output) in USD
#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl ModelPricing {
    pub fn cost(&self, usage: &TokenUsage) -> f64 {
        let input_cost = (usage.input_tokens as f64 / 1_000_000.0) * self.input_per_million;
        let output_cost = (usage.output_tokens as f64 / 1_000_000.0) * self.output_per_million;
        input_cost + output_cost
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRecord {
    pub provider_id: String,
    pub model_id: String,
    pub usage: TokenUsage,
    pub cost_usd: f64,
    pub recorded_at: DateTime<Utc>,
}

pub struct CostLogger {
    records: Vec<CostRecord>,
    pricing: Vec<ModelPricing>,
}

impl CostLogger {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            pricing: default_pricing(),
        }
    }

    pub fn log(&mut self, provider: &ProviderId, model: &ModelId, usage: TokenUsage) {
        let cost = self
            .pricing
            .iter()
            .find(|p| &p.provider_id == provider && &p.model_id == model)
            .map(|p| p.cost(&usage))
            .unwrap_or(0.0);

        self.records.push(CostRecord {
            provider_id: provider.0.clone(),
            model_id: model.0.clone(),
            usage,
            cost_usd: cost,
            recorded_at: Utc::now(),
        });
    }

    pub fn total_cost(&self) -> f64 {
        self.records.iter().map(|r| r.cost_usd).sum()
    }

    pub fn records(&self) -> &[CostRecord] {
        &self.records
    }
}

impl Default for CostLogger {
    fn default() -> Self {
        Self::new()
    }
}

fn default_pricing() -> Vec<ModelPricing> {
    vec![
        ModelPricing {
            provider_id: ProviderId::new("anthropic"),
            model_id: ModelId::new("claude-opus-4-5"),
            input_per_million: 15.0,
            output_per_million: 75.0,
        },
        ModelPricing {
            provider_id: ProviderId::new("anthropic"),
            model_id: ModelId::new("claude-sonnet-4-5"),
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        ModelPricing {
            provider_id: ProviderId::new("anthropic"),
            model_id: ModelId::new("claude-haiku-4-5"),
            input_per_million: 0.25,
            output_per_million: 1.25,
        },
    ]
}

// ── Trace spans ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSpan {
    pub name: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub attributes: std::collections::HashMap<String, String>,
}

impl TraceSpan {
    pub fn start(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            started_at: Utc::now(),
            ended_at: None,
            attributes: std::collections::HashMap::new(),
        }
    }

    pub fn finish(&mut self) {
        self.ended_at = Some(Utc::now());
    }

    pub fn with_attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    pub fn duration_ms(&self) -> Option<i64> {
        self.ended_at.map(|end| {
            (end - self.started_at).num_milliseconds()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let mut counter = TokenCounter::new();
        counter.record(&TokenUsage { input_tokens: 100, output_tokens: 50, cached_tokens: 0 });
        assert_eq!(counter.session_usage().total(), 150);
    }

    #[test]
    fn cost_calculation() {
        let pricing = ModelPricing {
            provider_id: ProviderId::new("anthropic"),
            model_id: ModelId::new("claude-opus-4-5"),
            input_per_million: 15.0,
            output_per_million: 75.0,
        };
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cached_tokens: 0,
        };
        let cost = pricing.cost(&usage);
        assert!((cost - 90.0).abs() < 0.001);
    }
}
