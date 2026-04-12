use caduceus_core::{ModelId, ProviderId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
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

// ── Agent resilience telemetry ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct RotMeasurement {
    pub turn: usize,
    pub recall_score: f64,
    pub repetition_count: usize,
    pub hallucination_markers: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotTrend {
    Stable,
    Declining,
    Critical,
}

pub struct ContextRotDetector {
    window_size: usize,
    rot_threshold: f64,
    measurements: VecDeque<RotMeasurement>,
}

impl ContextRotDetector {
    pub fn new(threshold: f64) -> Self {
        Self {
            window_size: 6,
            rot_threshold: threshold.clamp(0.0, 1.0),
            measurements: VecDeque::new(),
        }
    }

    pub fn record_turn(&mut self, recall: f64, repetitions: usize, hallucinations: usize) {
        let turn = self
            .measurements
            .back()
            .map_or(1, |measurement| measurement.turn + 1);
        self.measurements.push_back(RotMeasurement {
            turn,
            recall_score: recall.clamp(0.0, 1.0),
            repetition_count: repetitions,
            hallucination_markers: hallucinations,
        });

        if self.measurements.len() > self.window_size {
            self.measurements.pop_front();
        }
    }

    pub fn is_rotting(&self) -> bool {
        let Some(latest) = self.measurements.back() else {
            return false;
        };

        latest.recall_score < self.rot_threshold && !matches!(self.trend(), RotTrend::Stable)
    }

    pub fn rot_score(&self) -> f64 {
        if self.measurements.is_empty() {
            return 0.0;
        }

        self.measurements
            .iter()
            .map(Self::measurement_score)
            .sum::<f64>()
            / self.measurements.len() as f64
    }

    pub fn trend(&self) -> RotTrend {
        if self.measurements.len() < 2 {
            return RotTrend::Stable;
        }

        let midpoint = (self.measurements.len() / 2).max(1);
        let earlier_recall = self
            .measurements
            .iter()
            .take(midpoint)
            .map(|measurement| measurement.recall_score)
            .sum::<f64>()
            / midpoint as f64;
        let later_count = self.measurements.len().saturating_sub(midpoint).max(1);
        let later_recall = self
            .measurements
            .iter()
            .skip(midpoint)
            .map(|measurement| measurement.recall_score)
            .sum::<f64>()
            / later_count as f64;
        let decline = earlier_recall - later_recall;
        let latest = self.measurements.back().expect("latest measurement exists");
        let latest_score = Self::measurement_score(latest);

        if latest.recall_score <= (self.rot_threshold * 0.8)
            || latest_score >= 0.75
            || decline >= 0.25
        {
            RotTrend::Critical
        } else if latest.recall_score < self.rot_threshold
            || latest_score >= 0.45
            || decline >= 0.08
        {
            RotTrend::Declining
        } else {
            RotTrend::Stable
        }
    }

    fn measurement_score(measurement: &RotMeasurement) -> f64 {
        let recall_penalty = 1.0 - measurement.recall_score.clamp(0.0, 1.0);
        let repetition_penalty = (measurement.repetition_count.min(6) as f64) / 6.0;
        let hallucination_penalty = (measurement.hallucination_markers.min(4) as f64) / 4.0;

        (recall_penalty * 0.6 + repetition_penalty * 0.25 + hallucination_penalty * 0.15)
            .clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriftMeasurement {
    pub turn: usize,
    pub similarity_to_baseline: f64,
    pub skipped_steps: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BehavioralDriftDetector {
    baseline_patterns: Vec<String>,
    drift_measurements: Vec<DriftMeasurement>,
}

const MAX_DRIFT_MEASUREMENTS: usize = 100;

impl BehavioralDriftDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_baseline(&mut self, patterns: Vec<String>) {
        self.baseline_patterns = patterns;
    }

    pub fn record_behavior(&mut self, turn: usize, behavior: &str) {
        let similarity = if self.baseline_patterns.is_empty() {
            0.0
        } else {
            self.baseline_patterns
                .iter()
                .map(|pattern| Self::jaccard_similarity(pattern, behavior))
                .fold(0.0, f64::max)
        };

        let skipped_steps = self
            .baseline_patterns
            .iter()
            .filter(|pattern| Self::jaccard_similarity(pattern, behavior) < 0.25)
            .count();

        self.drift_measurements.push(DriftMeasurement {
            turn,
            similarity_to_baseline: similarity,
            skipped_steps,
        });
        let overflow = self
            .drift_measurements
            .len()
            .saturating_sub(MAX_DRIFT_MEASUREMENTS);
        if overflow > 0 {
            self.drift_measurements.drain(..overflow);
        }
    }

    pub fn drift_score(&self) -> f64 {
        if self.drift_measurements.is_empty() {
            return 0.0;
        }

        self.drift_measurements
            .iter()
            .map(|measurement| {
                let skipped_ratio = if self.baseline_patterns.is_empty() {
                    0.0
                } else {
                    measurement.skipped_steps as f64 / self.baseline_patterns.len() as f64
                };

                ((1.0 - measurement.similarity_to_baseline) * 0.8 + skipped_ratio * 0.2)
                    .clamp(0.0, 1.0)
            })
            .sum::<f64>()
            / self.drift_measurements.len() as f64
    }

    pub fn is_drifting(&self, threshold: f64) -> bool {
        self.drift_score() >= threshold.clamp(0.0, 1.0)
    }

    pub fn jaccard_similarity(a: &str, b: &str) -> f64 {
        let a_tokens = tokenize(a);
        let b_tokens = tokenize(b);

        if a_tokens.is_empty() && b_tokens.is_empty() {
            return 1.0;
        }

        if a_tokens.is_empty() || b_tokens.is_empty() {
            return 0.0;
        }

        let intersection = a_tokens.intersection(&b_tokens).count() as f64;
        let union = a_tokens.union(&b_tokens).count() as f64;
        if union == 0.0 {
            0.0
        } else {
            intersection / union
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DegradationStage {
    Healthy = 0,
    TriggerInjection = 1,
    ResourceStarvation = 2,
    BehavioralDrift = 3,
    MemoryEntrenchment = 4,
    FunctionalOverride = 5,
    SystemicCollapse = 6,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DegradationIndicators {
    pub context_utilization: f64,
    pub error_rate: f64,
    pub repetition_rate: f64,
    pub drift_score: f64,
}

pub struct CognitiveDegradationBreaker {
    stage: DegradationStage,
    stage_history: Vec<(DegradationStage, u64)>,
    auto_reset_threshold: DegradationStage,
}

const MAX_STAGE_HISTORY: usize = 100;

impl CognitiveDegradationBreaker {
    pub fn new() -> Self {
        let now = current_unix_timestamp();
        Self {
            stage: DegradationStage::Healthy,
            stage_history: vec![(DegradationStage::Healthy, now)],
            auto_reset_threshold: DegradationStage::FunctionalOverride,
        }
    }

    pub fn update_stage(&mut self, indicators: &DegradationIndicators) {
        let next_stage = Self::stage_from(indicators);
        if next_stage != self.stage {
            self.stage = next_stage;
            self.stage_history
                .push((next_stage, current_unix_timestamp()));
            self.trim_stage_history();
        }
    }

    pub fn current_stage(&self) -> &DegradationStage {
        &self.stage
    }

    pub fn should_reset(&self) -> bool {
        self.stage >= self.auto_reset_threshold
    }

    pub fn reset(&mut self) {
        self.stage = DegradationStage::Healthy;
        self.stage_history
            .push((DegradationStage::Healthy, current_unix_timestamp()));
        self.trim_stage_history();
    }

    pub fn stage_duration(&self) -> u64 {
        let started_at = self
            .stage_history
            .last()
            .map(|(_, timestamp)| *timestamp)
            .unwrap_or_else(current_unix_timestamp);
        current_unix_timestamp().saturating_sub(started_at)
    }

    fn stage_from(indicators: &DegradationIndicators) -> DegradationStage {
        let severity = [
            stage_severity(
                indicators.context_utilization,
                &[0.45, 0.6, 0.75, 0.85, 0.92, 0.98],
            ),
            stage_severity(indicators.error_rate, &[0.08, 0.16, 0.28, 0.4, 0.52, 0.65]),
            stage_severity(
                indicators.repetition_rate,
                &[0.08, 0.18, 0.3, 0.45, 0.6, 0.75],
            ),
            stage_severity(indicators.drift_score, &[0.15, 0.3, 0.45, 0.6, 0.78, 0.92]),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);

        match severity {
            1 => DegradationStage::TriggerInjection,
            2 => DegradationStage::ResourceStarvation,
            3 => DegradationStage::BehavioralDrift,
            4 => DegradationStage::MemoryEntrenchment,
            5 => DegradationStage::FunctionalOverride,
            6 => DegradationStage::SystemicCollapse,
            _ => DegradationStage::Healthy,
        }
    }

    fn trim_stage_history(&mut self) {
        let overflow = self.stage_history.len().saturating_sub(MAX_STAGE_HISTORY);
        if overflow > 0 {
            self.stage_history.drain(..overflow);
        }
    }
}

impl Default for CognitiveDegradationBreaker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttentionMeasurement {
    pub tokens_used: usize,
    pub estimated_attention: f64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionZone {
    Green,
    Yellow,
    Orange,
    Red,
    Critical,
}

pub struct AttentionBudgetTracker {
    max_tokens: usize,
    effective_attention: f64,
    measurements: Vec<AttentionMeasurement>,
}

impl AttentionBudgetTracker {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            max_tokens: max_tokens.max(1),
            effective_attention: 1.0,
            measurements: Vec::new(),
        }
    }

    pub fn record_usage(&mut self, tokens_used: usize) {
        let usage_ratio = (tokens_used as f64 / self.max_tokens as f64).clamp(0.0, 1.5);
        let estimated_attention = (1.0 - usage_ratio.powf(1.3)).clamp(0.0, 1.0);
        self.effective_attention = estimated_attention;
        self.measurements.push(AttentionMeasurement {
            tokens_used,
            estimated_attention,
            timestamp: current_unix_timestamp(),
        });
    }

    pub fn remaining_attention(&self) -> f64 {
        self.effective_attention
    }

    pub fn attention_zone(&self) -> AttentionZone {
        match self.remaining_attention() {
            attention if attention > 0.75 => AttentionZone::Green,
            attention if attention > 0.5 => AttentionZone::Yellow,
            attention if attention > 0.3 => AttentionZone::Orange,
            attention if attention > 0.1 => AttentionZone::Red,
            _ => AttentionZone::Critical,
        }
    }

    pub fn recommend_action(&self) -> Option<String> {
        match self.attention_zone() {
            AttentionZone::Green | AttentionZone::Yellow => None,
            AttentionZone::Orange => Some("summarize active context".to_string()),
            AttentionZone::Red => Some("compact conversation state".to_string()),
            AttentionZone::Critical => Some("reset or aggressively summarize context".to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeStatus {
    Active,
    Pruned(String),
    Succeeded,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub id: usize,
    pub parent: Option<usize>,
    pub hypothesis: String,
    pub status: NodeStatus,
    pub error_log: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgenticTreeTracker {
    nodes: Vec<TreeNode>,
    active_branch: Vec<usize>,
}

impl AgenticTreeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_branch(&mut self, parent: Option<usize>, hypothesis: &str) -> usize {
        let parent = parent.filter(|candidate| *candidate < self.nodes.len());
        let id = self.nodes.len();
        self.nodes.push(TreeNode {
            id,
            parent,
            hypothesis: hypothesis.to_string(),
            status: NodeStatus::Active,
            error_log: None,
        });
        self.active_branch = self.path_ids(id);
        id
    }

    pub fn prune(&mut self, node_id: usize, reason: &str) {
        if let Some(node) = self.nodes.get_mut(node_id) {
            node.status = NodeStatus::Pruned(reason.to_string());
            node.error_log = None;
            self.refresh_active_branch();
        }
    }

    pub fn succeed(&mut self, node_id: usize) {
        if let Some(node) = self.nodes.get_mut(node_id) {
            node.status = NodeStatus::Succeeded;
            node.error_log = None;
            self.refresh_active_branch();
        }
    }

    pub fn fail(&mut self, node_id: usize, error: &str) {
        if let Some(node) = self.nodes.get_mut(node_id) {
            node.status = NodeStatus::Failed(error.to_string());
            node.error_log = Some(error.to_string());
            self.refresh_active_branch();
        }
    }

    pub fn active_branches(&self) -> Vec<&TreeNode> {
        self.nodes
            .iter()
            .filter(|node| matches!(node.status, NodeStatus::Active))
            .collect()
    }

    pub fn depth(&self) -> usize {
        self.nodes
            .iter()
            .map(|node| self.path_ids(node.id).len())
            .max()
            .unwrap_or(0)
    }

    pub fn best_path(&self) -> Vec<&TreeNode> {
        let Some(node) = self
            .nodes
            .iter()
            .find(|node| matches!(node.status, NodeStatus::Succeeded))
        else {
            return Vec::new();
        };

        self.path_ids(node.id)
            .into_iter()
            .filter_map(|id| self.nodes.get(id))
            .collect()
    }

    fn path_ids(&self, node_id: usize) -> Vec<usize> {
        let mut path = Vec::new();
        let mut visited = HashSet::new();
        let mut current = self.nodes.get(node_id);
        while let Some(node) = current {
            if !visited.insert(node.id) {
                break;
            }
            path.push(node.id);
            current = node.parent.and_then(|parent| self.nodes.get(parent));
        }
        path.reverse();
        path
    }

    fn refresh_active_branch(&mut self) {
        if let Some(node) = self
            .nodes
            .iter()
            .rev()
            .find(|node| matches!(node.status, NodeStatus::Active))
        {
            self.active_branch = self.path_ids(node.id);
        } else {
            self.active_branch.clear();
        }
    }
}

fn current_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn stage_severity(value: f64, thresholds: &[f64; 6]) -> u8 {
    thresholds
        .iter()
        .position(|threshold| value < *threshold)
        .map_or(6, |index| index as u8)
}

fn tokenize(input: &str) -> HashSet<String> {
    input
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

/// Captures a full telemetry snapshot as a JSON value for SQLite persistence.
/// Call this at the end of each agent turn or session to persist state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySnapshot {
    pub context_rot_score: f64,
    pub context_rot_trend: String,
    pub drift_score: f64,
    pub degradation_stage: String,
    pub attention_remaining: f64,
    pub attention_zone: String,
    pub active_tree_branches: usize,
    pub tree_depth: usize,
    pub total_tokens_used: usize,
    pub timestamp_ms: u64,
}

impl TelemetrySnapshot {
    /// Build a snapshot from the live telemetry trackers
    pub fn capture(
        rot: &ContextRotDetector,
        drift: &BehavioralDriftDetector,
        degradation: &CognitiveDegradationBreaker,
        attention: &AttentionBudgetTracker,
        tree: &AgenticTreeTracker,
    ) -> Self {
        Self {
            context_rot_score: rot.rot_score(),
            context_rot_trend: format!("{:?}", rot.trend()),
            drift_score: drift.drift_score(),
            degradation_stage: format!("{:?}", degradation.current_stage()),
            attention_remaining: attention.remaining_attention(),
            attention_zone: format!("{:?}", attention.attention_zone()),
            active_tree_branches: tree.active_branches().len(),
            tree_depth: tree.depth(),
            total_tokens_used: attention
                .measurements
                .last()
                .map(|m| m.tokens_used)
                .unwrap_or(0),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }

    /// Convert to serde_json::Value for storage
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
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

    #[test]
    fn context_rot_detector_flags_decline() {
        let mut detector = ContextRotDetector::new(0.7);
        detector.record_turn(0.92, 0, 0);
        detector.record_turn(0.81, 1, 0);
        detector.record_turn(0.63, 3, 1);

        assert!(detector.is_rotting());
        assert_eq!(detector.trend(), RotTrend::Declining);
        assert!(detector.rot_score() > 0.15);
    }

    #[test]
    fn context_rot_detector_enters_critical_state() {
        let mut detector = ContextRotDetector::new(0.75);
        detector.record_turn(0.88, 0, 0);
        detector.record_turn(0.58, 5, 2);
        detector.record_turn(0.42, 6, 4);

        assert_eq!(detector.trend(), RotTrend::Critical);
        assert!(detector.is_rotting());
        assert!(detector.rot_score() > 0.45);
    }

    #[test]
    fn behavioral_drift_detector_scores_baseline_divergence() {
        let mut detector = BehavioralDriftDetector::new();
        detector.set_baseline(vec![
            "analyze requirements".to_string(),
            "run tests".to_string(),
            "summarize results".to_string(),
        ]);
        detector.record_behavior(1, "analyze requirements and run tests");
        detector.record_behavior(2, "rewrite everything and skip validation");

        assert!(detector.drift_score() > 0.3);
        assert!(detector.is_drifting(0.3));
        assert!(
            BehavioralDriftDetector::jaccard_similarity("run tests", "run tests quickly") > 0.5
        );
    }

    #[test]
    fn cognitive_degradation_breaker_tracks_stage_changes() {
        let mut breaker = CognitiveDegradationBreaker::new();
        let severe = DegradationIndicators {
            context_utilization: 0.95,
            error_rate: 0.55,
            repetition_rate: 0.2,
            drift_score: 0.35,
        };
        breaker.update_stage(&severe);

        assert_eq!(
            *breaker.current_stage(),
            DegradationStage::FunctionalOverride
        );
        assert!(breaker.should_reset());
        assert_eq!(breaker.stage_history.len(), 2);
        assert_eq!(
            breaker.stage_history[1].0,
            DegradationStage::FunctionalOverride
        );
        assert_eq!(breaker.stage_duration(), 0);

        breaker.reset();
        assert_eq!(*breaker.current_stage(), DegradationStage::Healthy);
    }

    #[test]
    fn attention_budget_tracker_recommends_escalating_actions() {
        let mut tracker = AttentionBudgetTracker::new(1_000);
        tracker.record_usage(200);
        assert_eq!(tracker.attention_zone(), AttentionZone::Green);
        assert!(tracker.recommend_action().is_none());

        tracker.record_usage(700);
        assert_eq!(tracker.attention_zone(), AttentionZone::Orange);
        assert_eq!(
            tracker.recommend_action().as_deref(),
            Some("summarize active context")
        );

        tracker.record_usage(980);
        assert_eq!(tracker.attention_zone(), AttentionZone::Critical);
        assert_eq!(
            tracker.recommend_action().as_deref(),
            Some("reset or aggressively summarize context")
        );
    }

    #[test]
    fn agentic_tree_tracker_reports_best_path() {
        let mut tracker = AgenticTreeTracker::new();
        let root = tracker.add_branch(None, "investigate failing test");
        let branch = tracker.add_branch(Some(root), "inspect telemetry heuristics");
        let sibling = tracker.add_branch(Some(root), "inspect storage heuristics");
        tracker.fail(sibling, "storage path was irrelevant");
        tracker.succeed(branch);

        let active = tracker.active_branches();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, root);
        assert_eq!(tracker.depth(), 2);

        let best_path = tracker.best_path();
        let hypotheses: Vec<&str> = best_path
            .iter()
            .map(|node| node.hypothesis.as_str())
            .collect();
        assert_eq!(
            hypotheses,
            vec!["investigate failing test", "inspect telemetry heuristics"]
        );
        assert!(matches!(
            tracker.nodes[sibling].status,
            NodeStatus::Failed(_)
        ));
    }

    #[test]
    fn agentic_tree_tracker_path_ids_stops_on_cycles() {
        let mut tracker = AgenticTreeTracker::new();
        let root = tracker.add_branch(None, "root");
        let child = tracker.add_branch(Some(root), "child");
        tracker.nodes[root].parent = Some(child);

        assert_eq!(tracker.path_ids(child), vec![root, child]);
    }
}
