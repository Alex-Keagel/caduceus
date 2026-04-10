use caduceus_core::{ModelId, ProviderId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// Accumulate token usage per session, per model.
pub struct TokenCounter {
    session_usage: TokenUsage,
    total_usage: TokenUsage,
    per_model: HashMap<String, TokenUsage>,
}

impl TokenCounter {
    pub fn new() -> Self {
        Self {
            session_usage: TokenUsage::default(),
            total_usage: TokenUsage::default(),
            per_model: HashMap::new(),
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

    /// Record usage attributed to a specific model.
    pub fn record_for_model(&mut self, model: &ModelId, usage: &TokenUsage) {
        self.record(usage);
        let entry = self.per_model.entry(model.0.clone()).or_insert_with(TokenUsage::default);
        entry.input_tokens += usage.input_tokens;
        entry.output_tokens += usage.output_tokens;
        entry.cached_tokens += usage.cached_tokens;
    }

    pub fn session_usage(&self) -> &TokenUsage {
        &self.session_usage
    }

    pub fn total_usage(&self) -> &TokenUsage {
        &self.total_usage
    }

    pub fn model_usage(&self, model: &str) -> Option<&TokenUsage> {
        self.per_model.get(model)
    }

    pub fn all_model_usage(&self) -> &HashMap<String, TokenUsage> {
        &self.per_model
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

// ── Cost calculation ───────────────────────────────────────────────────────────

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

/// Estimate costs based on model pricing tables.
pub struct CostCalculator {
    pricing: Vec<ModelPricing>,
}

impl CostCalculator {
    pub fn new() -> Self {
        Self { pricing: default_pricing() }
    }

    pub fn with_pricing(pricing: Vec<ModelPricing>) -> Self {
        Self { pricing }
    }

    pub fn add_pricing(&mut self, pricing: ModelPricing) {
        self.pricing.push(pricing);
    }

    /// Calculate cost for a specific provider/model/usage combination.
    pub fn calculate(
        &self,
        provider: &ProviderId,
        model: &ModelId,
        usage: &TokenUsage,
    ) -> f64 {
        self.pricing
            .iter()
            .find(|p| &p.provider_id == provider && &p.model_id == model)
            .map(|p| p.cost(usage))
            .unwrap_or(0.0)
    }

    /// Calculate total cost for a token counter across all models.
    pub fn total_cost_for_counter(
        &self,
        provider: &ProviderId,
        counter: &TokenCounter,
    ) -> f64 {
        counter
            .all_model_usage()
            .iter()
            .map(|(model_name, usage)| {
                let model = ModelId::new(model_name);
                self.calculate(provider, &model, usage)
            })
            .sum()
    }
}

impl Default for CostCalculator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Cost logging ───────────────────────────────────────────────────────────────

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
    calculator: CostCalculator,
}

impl CostLogger {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            calculator: CostCalculator::new(),
        }
    }

    pub fn log(&mut self, provider: &ProviderId, model: &ModelId, usage: TokenUsage) {
        let cost = self.calculator.calculate(provider, model, &usage);
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

    /// Cost records filtered to a single model.
    pub fn records_for_model(&self, model: &str) -> Vec<&CostRecord> {
        self.records.iter().filter(|r| r.model_id == model).collect()
    }
}

impl Default for CostLogger {
    fn default() -> Self {
        Self::new()
    }
}

fn default_pricing() -> Vec<ModelPricing> {
    vec![
        // Anthropic
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
        // OpenAI
        ModelPricing {
            provider_id: ProviderId::new("openai"),
            model_id: ModelId::new("gpt-4o"),
            input_per_million: 2.50,
            output_per_million: 10.0,
        },
        ModelPricing {
            provider_id: ProviderId::new("openai"),
            model_id: ModelId::new("gpt-4o-mini"),
            input_per_million: 0.15,
            output_per_million: 0.60,
        },
    ]
}

// ── Trace spans ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSpan {
    pub name: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub attributes: HashMap<String, String>,
    pub children: Vec<TraceSpan>,
}

impl TraceSpan {
    pub fn start(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            started_at: Utc::now(),
            ended_at: None,
            attributes: HashMap::new(),
            children: Vec::new(),
        }
    }

    pub fn finish(&mut self) {
        self.ended_at = Some(Utc::now());
    }

    pub fn with_attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    pub fn add_child(&mut self, child: TraceSpan) {
        self.children.push(child);
    }

    pub fn duration_ms(&self) -> Option<i64> {
        self.ended_at.map(|end| {
            (end - self.started_at).num_milliseconds()
        })
    }

    pub fn is_finished(&self) -> bool {
        self.ended_at.is_some()
    }
}

/// Collects trace spans for a session.
pub struct TraceCollector {
    spans: Vec<TraceSpan>,
}

impl TraceCollector {
    pub fn new() -> Self {
        Self { spans: Vec::new() }
    }

    pub fn record(&mut self, span: TraceSpan) {
        self.spans.push(span);
    }

    pub fn spans(&self) -> &[TraceSpan] {
        &self.spans
    }

    pub fn spans_by_name(&self, name: &str) -> Vec<&TraceSpan> {
        self.spans.iter().filter(|s| s.name == name).collect()
    }
}

impl Default for TraceCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counter_basic() {
        let mut counter = TokenCounter::new();
        counter.record(&TokenUsage { input_tokens: 100, output_tokens: 50, cached_tokens: 0 });
        assert_eq!(counter.session_usage().total(), 150);
    }

    #[test]
    fn token_counter_per_model() {
        let mut counter = TokenCounter::new();
        let model = ModelId::new("claude-sonnet-4-5");
        counter.record_for_model(&model, &TokenUsage {
            input_tokens: 500,
            output_tokens: 200,
            cached_tokens: 10,
        });
        counter.record_for_model(&model, &TokenUsage {
            input_tokens: 300,
            output_tokens: 100,
            cached_tokens: 5,
        });
        let usage = counter.model_usage("claude-sonnet-4-5").unwrap();
        assert_eq!(usage.input_tokens, 800);
        assert_eq!(usage.output_tokens, 300);
        assert_eq!(usage.cached_tokens, 15);
        assert_eq!(counter.total_usage().total(), 1100);
    }

    #[test]
    fn token_counter_reset_session() {
        let mut counter = TokenCounter::new();
        counter.record(&TokenUsage { input_tokens: 100, output_tokens: 50, cached_tokens: 0 });
        counter.reset_session();
        assert_eq!(counter.session_usage().total(), 0);
        assert_eq!(counter.total_usage().total(), 150);
    }

    #[test]
    fn cost_calculation_anthropic_opus() {
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

    #[test]
    fn cost_calculation_openai_gpt4o() {
        let calc = CostCalculator::new();
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cached_tokens: 0,
        };
        let cost = calc.calculate(
            &ProviderId::new("openai"),
            &ModelId::new("gpt-4o"),
            &usage,
        );
        assert!((cost - 12.5).abs() < 0.001); // 2.50 + 10.0
    }

    #[test]
    fn cost_calculator_unknown_model_returns_zero() {
        let calc = CostCalculator::new();
        let usage = TokenUsage { input_tokens: 1000, output_tokens: 500, cached_tokens: 0 };
        let cost = calc.calculate(
            &ProviderId::new("unknown"),
            &ModelId::new("mystery-model"),
            &usage,
        );
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn cost_logger_records_and_totals() {
        let mut logger = CostLogger::new();
        logger.log(
            &ProviderId::new("anthropic"),
            &ModelId::new("claude-sonnet-4-5"),
            TokenUsage { input_tokens: 1_000_000, output_tokens: 500_000, cached_tokens: 0 },
        );
        logger.log(
            &ProviderId::new("anthropic"),
            &ModelId::new("claude-sonnet-4-5"),
            TokenUsage { input_tokens: 2_000_000, output_tokens: 1_000_000, cached_tokens: 0 },
        );
        assert_eq!(logger.records().len(), 2);
        // First: 3.0 + 7.5 = 10.5; Second: 6.0 + 15.0 = 21.0 => total 31.5
        assert!((logger.total_cost() - 31.5).abs() < 0.01);
    }

    #[test]
    fn cost_logger_filter_by_model() {
        let mut logger = CostLogger::new();
        logger.log(
            &ProviderId::new("anthropic"),
            &ModelId::new("claude-sonnet-4-5"),
            TokenUsage { input_tokens: 100, output_tokens: 50, cached_tokens: 0 },
        );
        logger.log(
            &ProviderId::new("anthropic"),
            &ModelId::new("claude-opus-4-5"),
            TokenUsage { input_tokens: 200, output_tokens: 100, cached_tokens: 0 },
        );
        let sonnet_records = logger.records_for_model("claude-sonnet-4-5");
        assert_eq!(sonnet_records.len(), 1);
    }

    #[test]
    fn trace_span_basic() {
        let mut span = TraceSpan::start("llm_call")
            .with_attr("model", "claude-sonnet-4-5")
            .with_attr("provider", "anthropic");
        assert!(!span.is_finished());
        assert!(span.duration_ms().is_none());
        span.finish();
        assert!(span.is_finished());
        assert!(span.duration_ms().unwrap() >= 0);
    }

    #[test]
    fn trace_span_children() {
        let mut parent = TraceSpan::start("agent_turn");
        let mut child1 = TraceSpan::start("llm_call");
        child1.finish();
        let mut child2 = TraceSpan::start("tool_exec");
        child2.finish();
        parent.add_child(child1);
        parent.add_child(child2);
        parent.finish();
        assert_eq!(parent.children.len(), 2);
    }

    #[test]
    fn trace_collector_basic() {
        let mut collector = TraceCollector::new();
        let mut span1 = TraceSpan::start("llm_call");
        span1.finish();
        let mut span2 = TraceSpan::start("tool_exec");
        span2.finish();
        let mut span3 = TraceSpan::start("llm_call");
        span3.finish();
        collector.record(span1);
        collector.record(span2);
        collector.record(span3);
        assert_eq!(collector.spans().len(), 3);
        assert_eq!(collector.spans_by_name("llm_call").len(), 2);
        assert_eq!(collector.spans_by_name("tool_exec").len(), 1);
    }
}
