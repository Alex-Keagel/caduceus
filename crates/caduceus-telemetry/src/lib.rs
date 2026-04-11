use caduceus_core::{ModelId, ProviderId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

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
        let entry = self.per_model.entry(model.0.clone()).or_default();
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
        Self {
            pricing: default_pricing(),
        }
    }

    pub fn with_pricing(pricing: Vec<ModelPricing>) -> Self {
        Self { pricing }
    }

    pub fn add_pricing(&mut self, pricing: ModelPricing) {
        self.pricing.push(pricing);
    }

    /// Calculate cost for a specific provider/model/usage combination.
    pub fn calculate(&self, provider: &ProviderId, model: &ModelId, usage: &TokenUsage) -> f64 {
        self.pricing
            .iter()
            .find(|p| &p.provider_id == provider && &p.model_id == model)
            .map(|p| p.cost(usage))
            .unwrap_or(0.0)
    }

    /// Calculate total cost for a token counter across all models.
    pub fn total_cost_for_counter(&self, provider: &ProviderId, counter: &TokenCounter) -> f64 {
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
        self.records
            .iter()
            .filter(|r| r.model_id == model)
            .collect()
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
        self.ended_at
            .map(|end| (end - self.started_at).num_milliseconds())
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

// ── Budget enforcement ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetExceeded {
    pub limit_usd: f64,
    pub spent_usd: f64,
    pub attempted_cost: f64,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Budget exceeded: ${:.4} spent of ${:.4} limit (attempted ${:.4} more)",
            self.spent_usd, self.limit_usd, self.attempted_cost
        )
    }
}

pub struct BudgetEnforcer {
    max_usd: f64,
    spent_usd: f64,
}

impl BudgetEnforcer {
    pub fn new(max_usd: f64) -> Self {
        Self {
            max_usd,
            spent_usd: 0.0,
        }
    }

    pub fn set_limit(&mut self, max_usd: f64) {
        self.max_usd = max_usd;
    }

    pub fn limit(&self) -> f64 {
        self.max_usd
    }

    pub fn spent(&self) -> f64 {
        self.spent_usd
    }

    pub fn remaining(&self) -> f64 {
        (self.max_usd - self.spent_usd).max(0.0)
    }

    /// Record a cost and check if budget is exceeded. Returns Err if budget would be exceeded.
    pub fn check_and_record(&mut self, cost: f64) -> std::result::Result<(), BudgetExceeded> {
        if self.spent_usd + cost > self.max_usd {
            return Err(BudgetExceeded {
                limit_usd: self.max_usd,
                spent_usd: self.spent_usd,
                attempted_cost: cost,
            });
        }
        self.spent_usd += cost;
        Ok(())
    }

    /// Record cost unconditionally (for tracking purposes).
    pub fn record(&mut self, cost: f64) {
        self.spent_usd += cost;
    }

    pub fn reset(&mut self) {
        self.spent_usd = 0.0;
    }
}

impl Default for BudgetEnforcer {
    fn default() -> Self {
        Self::new(f64::MAX)
    }
}

// ── OTLP Telemetry Export ──────────────────────────────────────────────────────

/// A single telemetry event/metric to export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    /// Metric name following Claude Code convention, e.g. "caduceus.token.usage"
    pub name: String,
    pub timestamp: DateTime<Utc>,
    /// Key/value attributes, e.g. {"type": "input", "model": "claude-sonnet-4-5"}
    pub attributes: HashMap<String, String>,
    /// The numeric metric value
    pub value: f64,
}

impl TelemetryEvent {
    pub fn new(name: impl Into<String>, value: f64) -> Self {
        Self {
            name: name.into(),
            timestamp: Utc::now(),
            attributes: HashMap::new(),
            value,
        }
    }

    pub fn with_attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

/// Configuration for the OTLP exporter.
#[derive(Debug, Clone)]
pub struct OtelExporterConfig {
    /// OTLP HTTP endpoint, e.g. "http://localhost:4318"
    pub endpoint: String,
    pub enabled: bool,
    pub export_interval: Duration,
    /// Privacy: do not log prompt contents by default
    pub log_prompts: bool,
}

impl Default for OtelExporterConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:4318".into(),
            enabled: std::env::var("CADUCEUS_ENABLE_TELEMETRY")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            export_interval: Duration::from_secs(30),
            log_prompts: false,
        }
    }
}

/// OTLP JSON exporter that sends metrics to an OpenTelemetry Collector
/// via HTTP POST to the `/v1/metrics` endpoint.
pub struct OtelExporter {
    pub endpoint: String,
    pub enabled: bool,
    pub export_interval: Duration,
    pub log_prompts: bool,
    client: reqwest::Client,
    pending: tokio::sync::Mutex<Vec<TelemetryEvent>>,
}

impl OtelExporter {
    pub fn new(config: OtelExporterConfig) -> Self {
        Self {
            endpoint: config.endpoint,
            enabled: config.enabled,
            export_interval: config.export_interval,
            log_prompts: config.log_prompts,
            client: reqwest::Client::new(),
            pending: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn from_env() -> Self {
        Self::new(OtelExporterConfig::default())
    }

    /// Export a single metric event. Batches internally; use `flush()` to send.
    pub async fn export_metric(&self, event: TelemetryEvent) {
        if !self.enabled {
            return;
        }
        self.pending.lock().await.push(event);
    }

    /// Export a batch of events immediately.
    pub async fn export_batch(&self, events: Vec<TelemetryEvent>) -> anyhow::Result<()> {
        if !self.enabled || events.is_empty() {
            return Ok(());
        }
        let payload = build_otlp_payload(&events);
        let url = format!("{}/v1/metrics", self.endpoint.trim_end_matches('/'));
        self.client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&payload)?)
            .send()
            .await?;
        Ok(())
    }

    /// Flush all pending metrics to the OTLP endpoint.
    pub async fn flush(&self) -> anyhow::Result<()> {
        let events: Vec<TelemetryEvent> = {
            let mut pending = self.pending.lock().await;
            std::mem::take(&mut *pending)
        };
        self.export_batch(events).await
    }

    // ── Convenience constructors ───────────────────────────────────────────────

    pub fn token_usage_event(
        token_type: &str,
        count: u64,
        model: &str,
        provider: &str,
    ) -> TelemetryEvent {
        TelemetryEvent::new("caduceus.token.usage", count as f64)
            .with_attr("type", token_type)
            .with_attr("model", model)
            .with_attr("provider", provider)
    }

    pub fn cost_event(usd: f64, model: &str, provider: &str) -> TelemetryEvent {
        TelemetryEvent::new("caduceus.cost.usage", usd)
            .with_attr("model", model)
            .with_attr("provider", provider)
    }

    pub fn session_count_event(count: u64) -> TelemetryEvent {
        TelemetryEvent::new("caduceus.session.count", count as f64)
    }

    pub fn tool_execution_event(
        tool_name: &str,
        duration_ms: u64,
        success: bool,
    ) -> TelemetryEvent {
        TelemetryEvent::new("caduceus.tool.execution", duration_ms as f64)
            .with_attr("tool", tool_name)
            .with_attr("success", if success { "true" } else { "false" })
    }

    pub fn active_time_event(total_ms: u64) -> TelemetryEvent {
        TelemetryEvent::new("caduceus.active_time.total", total_ms as f64)
    }
}

/// Build a simplified OTLP JSON payload for the `/v1/metrics` endpoint.
fn build_otlp_payload(events: &[TelemetryEvent]) -> serde_json::Value {
    let data_points: Vec<serde_json::Value> = events
        .iter()
        .map(|e| {
            let attrs: Vec<serde_json::Value> = e
                .attributes
                .iter()
                .map(|(k, v)| {
                    serde_json::json!({
                        "key": k,
                        "value": {"stringValue": v}
                    })
                })
                .collect();

            serde_json::json!({
                "attributes": attrs,
                "startTimeUnixNano": e.timestamp.timestamp_nanos_opt().unwrap_or(0).to_string(),
                "timeUnixNano": e.timestamp.timestamp_nanos_opt().unwrap_or(0).to_string(),
                "asDouble": e.value
            })
        })
        .collect();

    // Group by metric name
    let mut grouped: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for (i, e) in events.iter().enumerate() {
        grouped
            .entry(e.name.clone())
            .or_default()
            .push(data_points[i].clone());
    }

    let metrics: Vec<serde_json::Value> = grouped
        .into_iter()
        .map(|(name, points)| {
            serde_json::json!({
                "name": name,
                "gauge": {
                    "dataPoints": points
                }
            })
        })
        .collect();

    serde_json::json!({
        "resourceMetrics": [{
            "resource": {
                "attributes": [{
                    "key": "service.name",
                    "value": {"stringValue": "caduceus"}
                }]
            },
            "scopeMetrics": [{
                "scope": {"name": "caduceus-telemetry"},
                "metrics": metrics
            }]
        }]
    })
}

/// Simple in-session metrics tracker for the `/telemetry` slash command.
#[derive(Debug, Default)]
pub struct SessionMetrics {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
    pub session_count: u64,
    pub tool_executions: u64,
    pub active_time_ms: u64,
}

impl SessionMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn summary(&self) -> String {
        format!(
            "Session Telemetry:\n  Input tokens:      {}\n  Output tokens:     {}\n  Cache read:        {}\n  Cache write:       {}\n  Cost (USD):        ${:.6}\n  Sessions:          {}\n  Tool executions:   {}\n  Active time (ms):  {}",
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
            self.cost_usd,
            self.session_count,
            self.tool_executions,
            self.active_time_ms,
        )
    }

    pub fn to_events(&self, model: &str, provider: &str) -> Vec<TelemetryEvent> {
        vec![
            OtelExporter::token_usage_event("input", self.input_tokens, model, provider),
            OtelExporter::token_usage_event("output", self.output_tokens, model, provider),
            OtelExporter::token_usage_event("cache_read", self.cache_read_tokens, model, provider),
            OtelExporter::token_usage_event(
                "cache_write",
                self.cache_write_tokens,
                model,
                provider,
            ),
            OtelExporter::cost_event(self.cost_usd, model, provider),
            OtelExporter::session_count_event(self.session_count),
            OtelExporter::active_time_event(self.active_time_ms),
        ]
    }
}

pub struct SloMonitor {
    objectives: Vec<Slo>,
}

#[derive(Debug, Clone)]
pub struct Slo {
    pub name: String,
    pub description: String,
    pub target: f64,
    pub window_secs: u64,
    pub metric: SloMetric,
    pub measurements: Vec<SloMeasurement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SloMetric {
    SuccessRate,
    Latency { p99_ms: u64 },
    ErrorRate,
    Availability,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SloMeasurement {
    pub timestamp: u64,
    pub value: f64,
    pub is_good: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SloStatus {
    pub name: String,
    pub target: f64,
    pub current: f64,
    pub is_met: bool,
    pub error_budget_remaining: f64,
}

impl SloMonitor {
    pub fn new() -> Self {
        Self {
            objectives: Vec::new(),
        }
    }

    pub fn add_slo(&mut self, slo: Slo) {
        self.objectives.push(slo);
    }

    pub fn record_measurement(&mut self, slo_name: &str, value: f64) {
        let timestamp = self
            .objectives
            .iter()
            .flat_map(|slo| {
                slo.measurements
                    .iter()
                    .map(|measurement| measurement.timestamp)
            })
            .max()
            .unwrap_or(0)
            + 1;

        if let Some(slo) = self.objectives.iter_mut().find(|slo| slo.name == slo_name) {
            let is_good = slo.metric.is_good(value, slo.target);
            slo.measurements.push(SloMeasurement {
                timestamp,
                value,
                is_good,
            });
        }
    }

    pub fn check_slo(&self, name: &str) -> Option<SloStatus> {
        self.objectives
            .iter()
            .find(|slo| slo.name == name)
            .map(Slo::status)
    }

    pub fn all_statuses(&self) -> Vec<SloStatus> {
        self.objectives.iter().map(Slo::status).collect()
    }

    pub fn error_budget_remaining(&self, name: &str) -> Option<f64> {
        self.check_slo(name)
            .map(|status| status.error_budget_remaining)
    }
}

impl Default for SloMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Slo {
    fn windowed_measurements(&self) -> &[SloMeasurement] {
        let Some(latest) = self
            .measurements
            .last()
            .map(|measurement| measurement.timestamp)
        else {
            return &self.measurements;
        };
        let window_start = latest.saturating_sub(self.window_secs.saturating_sub(1));
        let first_in_window = self
            .measurements
            .iter()
            .position(|measurement| measurement.timestamp >= window_start)
            .unwrap_or(self.measurements.len());
        &self.measurements[first_in_window..]
    }

    fn compliance_ratio(&self) -> f64 {
        let measurements = self.windowed_measurements();
        if measurements.is_empty() {
            return 0.0;
        }

        let good = measurements
            .iter()
            .filter(|measurement| measurement.is_good)
            .count() as f64;
        good / measurements.len() as f64
    }

    fn error_budget_remaining(&self, current: f64) -> f64 {
        if self.target >= 1.0 {
            return if current >= 1.0 { 1.0 } else { 0.0 };
        }

        ((current - self.target) / (1.0 - self.target)).clamp(0.0, 1.0)
    }

    fn status(&self) -> SloStatus {
        let current = self.compliance_ratio();
        SloStatus {
            name: self.name.clone(),
            target: self.target,
            current,
            is_met: current >= self.target,
            error_budget_remaining: self.error_budget_remaining(current),
        }
    }
}

impl SloMetric {
    fn is_good(&self, value: f64, target: f64) -> bool {
        match self {
            Self::SuccessRate | Self::Availability => value >= target,
            Self::Latency { p99_ms } => value <= *p99_ms as f64,
            Self::ErrorRate => value <= (1.0 - target).clamp(0.0, 1.0),
        }
    }
}

pub struct GovernanceAttestor {
    controls: Vec<GovernanceControl>,
}

#[derive(Debug, Clone)]
pub struct GovernanceControl {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: ControlStatus,
    pub evidence: Vec<String>,
    pub last_verified: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlStatus {
    Active,
    Inactive,
    Degraded,
    Unknown,
}

impl GovernanceAttestor {
    pub fn new() -> Self {
        Self {
            controls: Vec::new(),
        }
    }

    pub fn register_control(&mut self, control: GovernanceControl) {
        self.controls.push(control);
    }

    pub fn verify_control(&mut self, id: &str) -> ControlStatus {
        let next_verified = self
            .controls
            .iter()
            .map(|control| control.last_verified)
            .max()
            .unwrap_or(0)
            + 1;

        if let Some(control) = self.controls.iter_mut().find(|control| control.id == id) {
            control.last_verified = next_verified;
            control.status = match control.status {
                ControlStatus::Unknown if control.evidence.is_empty() => ControlStatus::Inactive,
                ControlStatus::Unknown => ControlStatus::Active,
                status => status,
            };
            control.status
        } else {
            ControlStatus::Unknown
        }
    }

    pub fn generate_report(&self) -> String {
        let mut report = format!(
            "# Governance Attestation\n\nCompliance: {:.1}%\n\n| ID | Name | Status | Last Verified | Evidence |\n| --- | --- | --- | --- | --- |\n",
            self.compliance_percentage()
        );

        for control in &self.controls {
            let evidence = if control.evidence.is_empty() {
                "None".to_string()
            } else {
                control.evidence.join("<br/>")
            };
            report.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                control.id,
                control.name,
                control.status.as_str(),
                control.last_verified,
                evidence
            ));
            report.push_str(&format!(
                "\n- **{}**: {}\n",
                control.name, control.description
            ));
        }

        report
    }

    pub fn compliance_percentage(&self) -> f64 {
        if self.controls.is_empty() {
            return 0.0;
        }

        let compliant = self
            .controls
            .iter()
            .filter(|control| control.status == ControlStatus::Active)
            .count() as f64;
        (compliant / self.controls.len() as f64) * 100.0
    }

    pub fn active_controls(&self) -> Vec<&GovernanceControl> {
        self.controls
            .iter()
            .filter(|control| control.status == ControlStatus::Active)
            .collect()
    }

    pub fn default_controls() -> Self {
        let mut attestor = Self::new();
        for control in [
            GovernanceControl {
                id: "policy-enforcement".to_string(),
                name: "Policy Enforcement".to_string(),
                description: "Critical policy checks run before agent actions execute.".to_string(),
                status: ControlStatus::Active,
                evidence: vec!["Policy engine enabled".to_string()],
                last_verified: 1,
            },
            GovernanceControl {
                id: "audit-logging".to_string(),
                name: "Audit Logging".to_string(),
                description: "Agent actions are captured in an immutable audit log.".to_string(),
                status: ControlStatus::Active,
                evidence: vec!["Audit sink configured".to_string()],
                last_verified: 1,
            },
            GovernanceControl {
                id: "approval-gates".to_string(),
                name: "Approval Gates".to_string(),
                description: "High-risk operations require explicit approval gates.".to_string(),
                status: ControlStatus::Active,
                evidence: vec!["Manual approval workflow present".to_string()],
                last_verified: 1,
            },
            GovernanceControl {
                id: "model-allowlist".to_string(),
                name: "Model Allowlist".to_string(),
                description: "Only approved models can be used in production flows.".to_string(),
                status: ControlStatus::Degraded,
                evidence: vec!["Fallback model entered degraded mode".to_string()],
                last_verified: 1,
            },
        ] {
            attestor.register_control(control);
        }
        attestor
    }
}

impl Default for GovernanceAttestor {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Inactive => "Inactive",
            Self::Degraded => "Degraded",
            Self::Unknown => "Unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counter_basic() {
        let mut counter = TokenCounter::new();
        counter.record(&TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cached_tokens: 0,
        });
        assert_eq!(counter.session_usage().total(), 150);
    }

    #[test]
    fn token_counter_per_model() {
        let mut counter = TokenCounter::new();
        let model = ModelId::new("claude-sonnet-4-5");
        counter.record_for_model(
            &model,
            &TokenUsage {
                input_tokens: 500,
                output_tokens: 200,
                cached_tokens: 10,
            },
        );
        counter.record_for_model(
            &model,
            &TokenUsage {
                input_tokens: 300,
                output_tokens: 100,
                cached_tokens: 5,
            },
        );
        let usage = counter.model_usage("claude-sonnet-4-5").unwrap();
        assert_eq!(usage.input_tokens, 800);
        assert_eq!(usage.output_tokens, 300);
        assert_eq!(usage.cached_tokens, 15);
        assert_eq!(counter.total_usage().total(), 1100);
    }

    #[test]
    fn token_counter_reset_session() {
        let mut counter = TokenCounter::new();
        counter.record(&TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cached_tokens: 0,
        });
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
        let cost = calc.calculate(&ProviderId::new("openai"), &ModelId::new("gpt-4o"), &usage);
        assert!((cost - 12.5).abs() < 0.001); // 2.50 + 10.0
    }

    #[test]
    fn cost_calculator_unknown_model_returns_zero() {
        let calc = CostCalculator::new();
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cached_tokens: 0,
        };
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
            TokenUsage {
                input_tokens: 1_000_000,
                output_tokens: 500_000,
                cached_tokens: 0,
            },
        );
        logger.log(
            &ProviderId::new("anthropic"),
            &ModelId::new("claude-sonnet-4-5"),
            TokenUsage {
                input_tokens: 2_000_000,
                output_tokens: 1_000_000,
                cached_tokens: 0,
            },
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
            TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                cached_tokens: 0,
            },
        );
        logger.log(
            &ProviderId::new("anthropic"),
            &ModelId::new("claude-opus-4-5"),
            TokenUsage {
                input_tokens: 200,
                output_tokens: 100,
                cached_tokens: 0,
            },
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

    // ── BudgetEnforcer tests ──────────────────────────────────────────────

    #[test]
    fn budget_enforcer_basic() {
        let mut budget = BudgetEnforcer::new(1.0);
        assert_eq!(budget.limit(), 1.0);
        assert_eq!(budget.spent(), 0.0);
        assert!((budget.remaining() - 1.0).abs() < f64::EPSILON);

        assert!(budget.check_and_record(0.5).is_ok());
        assert!((budget.spent() - 0.5).abs() < f64::EPSILON);
        assert!((budget.remaining() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn budget_enforcer_exceeds() {
        let mut budget = BudgetEnforcer::new(1.0);
        budget.check_and_record(0.8).unwrap();
        let err = budget.check_and_record(0.5).unwrap_err();
        assert!((err.limit_usd - 1.0).abs() < f64::EPSILON);
        assert!((err.spent_usd - 0.8).abs() < f64::EPSILON);
        assert!((err.attempted_cost - 0.5).abs() < f64::EPSILON);
        // Spent should not have increased
        assert!((budget.spent() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn budget_enforcer_record_unconditional() {
        let mut budget = BudgetEnforcer::new(1.0);
        budget.record(2.0);
        assert!((budget.spent() - 2.0).abs() < f64::EPSILON);
        assert!((budget.remaining()).abs() < f64::EPSILON);
    }

    #[test]
    fn budget_enforcer_reset() {
        let mut budget = BudgetEnforcer::new(1.0);
        budget.check_and_record(0.5).unwrap();
        budget.reset();
        assert!((budget.spent()).abs() < f64::EPSILON);
        assert!((budget.remaining() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn budget_enforcer_set_limit() {
        let mut budget = BudgetEnforcer::new(1.0);
        budget.set_limit(5.0);
        assert!((budget.limit() - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn budget_enforcer_default_is_unlimited() {
        let budget = BudgetEnforcer::default();
        assert!(budget.limit() > 1_000_000.0);
    }

    #[test]
    fn budget_exceeded_display() {
        let exceeded = BudgetExceeded {
            limit_usd: 1.0,
            spent_usd: 0.8,
            attempted_cost: 0.5,
        };
        let msg = exceeded.to_string();
        assert!(msg.contains("Budget exceeded"));
        assert!(msg.contains("0.8000"));
        assert!(msg.contains("1.0000"));
    }

    // ── OtelExporter tests ─────────────────────────────────────────────────────

    #[test]
    fn telemetry_event_new() {
        let event = TelemetryEvent::new("caduceus.token.usage", 42.0)
            .with_attr("type", "input")
            .with_attr("model", "claude-sonnet-4-5");
        assert_eq!(event.name, "caduceus.token.usage");
        assert_eq!(event.value, 42.0);
        assert_eq!(
            event.attributes.get("type").map(String::as_str),
            Some("input")
        );
        assert_eq!(
            event.attributes.get("model").map(String::as_str),
            Some("claude-sonnet-4-5")
        );
    }

    #[test]
    fn otel_exporter_disabled_by_default() {
        // Without CADUCEUS_ENABLE_TELEMETRY env var, should be disabled
        std::env::remove_var("CADUCEUS_ENABLE_TELEMETRY");
        let config = OtelExporterConfig::default();
        assert!(!config.enabled);
        assert!(!config.log_prompts);
    }

    #[test]
    fn otel_convenience_constructors() {
        let token_ev = OtelExporter::token_usage_event("input", 1000, "sonnet", "anthropic");
        assert_eq!(token_ev.name, "caduceus.token.usage");
        assert_eq!(token_ev.value, 1000.0);
        assert_eq!(
            token_ev.attributes.get("type").map(String::as_str),
            Some("input")
        );

        let cost_ev = OtelExporter::cost_event(0.005, "gpt-4o", "openai");
        assert_eq!(cost_ev.name, "caduceus.cost.usage");
        assert!((cost_ev.value - 0.005).abs() < f64::EPSILON);

        let session_ev = OtelExporter::session_count_event(3);
        assert_eq!(session_ev.name, "caduceus.session.count");
        assert_eq!(session_ev.value, 3.0);

        let tool_ev = OtelExporter::tool_execution_event("bash", 120, true);
        assert_eq!(tool_ev.name, "caduceus.tool.execution");
        assert_eq!(tool_ev.value, 120.0);
        assert_eq!(
            tool_ev.attributes.get("success").map(String::as_str),
            Some("true")
        );

        let time_ev = OtelExporter::active_time_event(5000);
        assert_eq!(time_ev.name, "caduceus.active_time.total");
        assert_eq!(time_ev.value, 5000.0);
    }

    #[test]
    fn build_otlp_payload_structure() {
        let events = vec![
            TelemetryEvent::new("caduceus.token.usage", 100.0).with_attr("type", "input"),
            TelemetryEvent::new("caduceus.cost.usage", 0.001),
        ];
        let payload = build_otlp_payload(&events);
        assert!(payload["resourceMetrics"].is_array());
        let rm = &payload["resourceMetrics"][0];
        assert_eq!(rm["resource"]["attributes"][0]["key"], "service.name");
        let metrics = &rm["scopeMetrics"][0]["metrics"];
        assert!(metrics.is_array());
        assert!(metrics.as_array().unwrap().len() >= 1);
    }

    #[test]
    fn session_metrics_summary() {
        let mut m = SessionMetrics::new();
        m.input_tokens = 500;
        m.output_tokens = 200;
        m.cost_usd = 0.0025;
        m.tool_executions = 7;
        let s = m.summary();
        assert!(s.contains("500"));
        assert!(s.contains("200"));
        assert!(s.contains("0.002500"));
        assert!(s.contains("7"));
    }

    #[test]
    fn session_metrics_to_events() {
        let mut m = SessionMetrics::new();
        m.input_tokens = 100;
        m.cache_read_tokens = 50;
        m.cost_usd = 0.001;
        m.session_count = 2;
        let events = m.to_events("claude-sonnet-4-5", "anthropic");
        assert_eq!(events.len(), 7);
        let names: Vec<&str> = events.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"caduceus.token.usage"));
        assert!(names.contains(&"caduceus.cost.usage"));
        assert!(names.contains(&"caduceus.session.count"));
        assert!(names.contains(&"caduceus.active_time.total"));
    }

    #[tokio::test]
    async fn otel_exporter_disabled_does_not_queue() {
        let config = OtelExporterConfig {
            endpoint: "http://localhost:4318".into(),
            enabled: false,
            export_interval: Duration::from_secs(30),
            log_prompts: false,
        };
        let exporter = OtelExporter::new(config);
        exporter
            .export_metric(TelemetryEvent::new("test.metric", 1.0))
            .await;
        let pending = exporter.pending.lock().await;
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn otel_exporter_queues_when_enabled() {
        let config = OtelExporterConfig {
            endpoint: "http://localhost:4318".into(),
            enabled: true,
            export_interval: Duration::from_secs(30),
            log_prompts: false,
        };
        let exporter = OtelExporter::new(config);
        exporter
            .export_metric(TelemetryEvent::new("caduceus.token.usage", 99.0))
            .await;
        exporter
            .export_metric(TelemetryEvent::new("caduceus.cost.usage", 0.005))
            .await;
        let pending = exporter.pending.lock().await;
        assert_eq!(pending.len(), 2);
    }
    #[test]
    fn slo_monitor_records_measurements() {
        let mut monitor = SloMonitor::new();
        monitor.add_slo(Slo {
            name: "success".to_string(),
            description: "Successful agent runs".to_string(),
            target: 0.75,
            window_secs: 10,
            metric: SloMetric::SuccessRate,
            measurements: Vec::new(),
        });

        monitor.record_measurement("success", 0.8);
        monitor.record_measurement("success", 0.7);

        let status = monitor.check_slo("success").unwrap();
        assert!((status.current - 0.5).abs() < 1e-9);
        assert!(!status.is_met);
    }

    #[test]
    fn slo_monitor_checks_status_and_windowed_budget() {
        let mut monitor = SloMonitor::new();
        monitor.add_slo(Slo {
            name: "latency".to_string(),
            description: "P99 latency under 250ms".to_string(),
            target: 0.5,
            window_secs: 2,
            metric: SloMetric::Latency { p99_ms: 250 },
            measurements: Vec::new(),
        });

        monitor.record_measurement("latency", 200.0);
        monitor.record_measurement("latency", 300.0);
        monitor.record_measurement("latency", 180.0);

        let status = monitor.check_slo("latency").unwrap();
        assert!((status.current - 0.5).abs() < 1e-9);
        assert!(status.is_met);
        assert_eq!(monitor.all_statuses().len(), 1);
    }

    #[test]
    fn slo_monitor_calculates_error_budget_remaining() {
        let mut monitor = SloMonitor::new();
        monitor.add_slo(Slo {
            name: "availability".to_string(),
            description: "Availability target".to_string(),
            target: 0.8,
            window_secs: 10,
            metric: SloMetric::Availability,
            measurements: Vec::new(),
        });

        monitor.record_measurement("availability", 0.95);
        monitor.record_measurement("availability", 0.85);
        monitor.record_measurement("availability", 0.90);
        monitor.record_measurement("availability", 0.10);

        let remaining = monitor.error_budget_remaining("availability").unwrap();
        assert!((remaining - 0.0).abs() < 1e-9);
    }

    #[test]
    fn governance_attestor_registers_and_verifies_controls() {
        let mut attestor = GovernanceAttestor::new();
        attestor.register_control(GovernanceControl {
            id: "ctrl-1".to_string(),
            name: "Control One".to_string(),
            description: "Verifies policy wiring.".to_string(),
            status: ControlStatus::Unknown,
            evidence: vec!["checklist complete".to_string()],
            last_verified: 0,
        });

        let status = attestor.verify_control("ctrl-1");
        assert_eq!(status, ControlStatus::Active);
        assert_eq!(attestor.active_controls().len(), 1);
    }

    #[test]
    fn governance_attestor_generates_markdown_report() {
        let mut attestor = GovernanceAttestor::new();
        attestor.register_control(GovernanceControl {
            id: "ctrl-2".to_string(),
            name: "Audit Trail".to_string(),
            description: "Captures user and agent actions.".to_string(),
            status: ControlStatus::Active,
            evidence: vec!["logs enabled".to_string()],
            last_verified: 7,
        });

        let report = attestor.generate_report();
        assert!(report.contains("# Governance Attestation"));
        assert!(report.contains("Audit Trail"));
        assert!(report.contains("logs enabled"));
    }

    #[test]
    fn governance_attestor_reports_compliance_percentage() {
        let mut attestor = GovernanceAttestor::new();
        attestor.register_control(GovernanceControl {
            id: "ctrl-a".to_string(),
            name: "A".to_string(),
            description: "A".to_string(),
            status: ControlStatus::Active,
            evidence: vec![],
            last_verified: 1,
        });
        attestor.register_control(GovernanceControl {
            id: "ctrl-b".to_string(),
            name: "B".to_string(),
            description: "B".to_string(),
            status: ControlStatus::Degraded,
            evidence: vec![],
            last_verified: 1,
        });

        assert!((attestor.compliance_percentage() - 50.0).abs() < 1e-9);

        let defaults = GovernanceAttestor::default_controls();
        assert_eq!(defaults.active_controls().len(), 3);
    }
}
