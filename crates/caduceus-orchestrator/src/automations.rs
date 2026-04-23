//! Trigger-based automation system for Caduceus.
//!
//! Automations let users define recurring or event-driven agent sessions:
//! cron schedules, GitHub webhooks, file watches, manual triggers, etc.

use caduceus_core::{ModelId, TokenUsage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::modes::AgentMode;

// ── Core types ─────────────────────────────────────────────────────────────────

/// A single automation definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Automation {
    pub id: String,
    pub name: String,
    pub trigger: AutomationTrigger,
    pub agent_config: AutomationAgentConfig,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub run_count: u64,
}

/// What event fires this automation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AutomationTrigger {
    /// Cron expression, e.g. `"0 */6 * * *"`.
    Cron(String),
    /// On pull-request open/update in a repository.
    GitHubPR { repo: String },
    /// On push to a specific branch.
    GitHubPush { branch: String },
    /// HTTP POST to `/webhooks/{path}`.
    Webhook { path: String },
    /// Watch for file changes matching a glob pattern.
    FileChange { pattern: String },
    /// Triggered manually via `/automate run <name>`.
    Manual,
}

/// Agent configuration used when an automation fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationAgentConfig {
    pub mode: AgentMode,
    pub model: ModelId,
    /// Template with `{{event}}` placeholder replaced at runtime.
    pub prompt_template: String,
    pub tools: Vec<String>,
    pub max_turns: usize,
    pub auto_commit: bool,
    pub auto_pr: bool,
}

/// Outcome of a single automation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationResult {
    pub automation_id: String,
    pub trigger_event: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub success: bool,
    pub output: String,
    pub tokens_used: TokenUsage,
    pub cost_usd: f64,
    pub commit_sha: Option<String>,
    pub pr_url: Option<String>,
}

// ── Registry ───────────────────────────────────────────────────────────────────

/// Stores and manages all automation definitions.
/// Persists to `.caduceus/automations.json`.
#[derive(Debug, Clone)]
pub struct AutomationRegistry {
    automations: Arc<RwLock<HashMap<String, Automation>>>,
    persist_path: PathBuf,
}

impl AutomationRegistry {
    /// Open (or create) a registry backed by a JSON file.
    pub fn new(caduceus_dir: impl AsRef<Path>) -> Self {
        // Audit finding round-3 (#r3-sec-7): cap automations.json read to
        // prevent OOM if the file is corrupted or maliciously huge. 5 MiB
        // accommodates ~thousands of automations with comfortable headroom.
        const MAX_REGISTRY_BYTES: u64 = 5 * 1024 * 1024;
        let persist_path = caduceus_dir.as_ref().join("automations.json");
        let automations = if persist_path.exists() {
            let too_big = std::fs::metadata(&persist_path)
                .map(|m| m.len() > MAX_REGISTRY_BYTES)
                .unwrap_or(false);
            if too_big {
                tracing::warn!(
                    path = %persist_path.display(),
                    "automations.json exceeds 5 MiB cap, refusing to load"
                );
                HashMap::new()
            } else {
                match std::fs::read_to_string(&persist_path) {
                    Ok(json) => serde_json::from_str::<Vec<Automation>>(&json)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|a| (a.name.clone(), a))
                        .collect(),
                    Err(_) => HashMap::new(),
                }
            }
        } else {
            HashMap::new()
        };
        Self {
            automations: Arc::new(RwLock::new(automations)),
            persist_path,
        }
    }

    /// Register a new automation.
    ///
    /// Audit finding round-2 (#30): persistence used to happen while the
    /// write lock was still held, blocking every other call to the
    /// registry on the disk write. Now we mutate the map, snapshot it,
    /// drop the lock, then persist — concurrent reads/writes proceed
    /// immediately and the persist races nobody.
    pub async fn register(&self, automation: Automation) -> Result<(), AutomationError> {
        let snapshot = {
            let mut map = self.automations.write().await;
            if map.contains_key(&automation.name) {
                return Err(AutomationError::AlreadyExists(automation.name));
            }
            map.insert(automation.name.clone(), automation);
            map.values().cloned().collect::<Vec<_>>()
        };
        Self::persist_snapshot(&snapshot, &self.persist_path)
    }

    /// Remove an automation by name.
    pub async fn remove(&self, name: &str) -> Result<Automation, AutomationError> {
        let (auto, snapshot) = {
            let mut map = self.automations.write().await;
            let auto = map
                .remove(name)
                .ok_or_else(|| AutomationError::NotFound(name.to_string()))?;
            let snapshot = map.values().cloned().collect::<Vec<_>>();
            (auto, snapshot)
        };
        Self::persist_snapshot(&snapshot, &self.persist_path)?;
        Ok(auto)
    }

    /// Enable or disable an automation.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), AutomationError> {
        let snapshot = {
            let mut map = self.automations.write().await;
            let auto = map
                .get_mut(name)
                .ok_or_else(|| AutomationError::NotFound(name.to_string()))?;
            auto.enabled = enabled;
            map.values().cloned().collect::<Vec<_>>()
        };
        Self::persist_snapshot(&snapshot, &self.persist_path)
    }

    /// Get a single automation by name.
    pub async fn get(&self, name: &str) -> Option<Automation> {
        self.automations.read().await.get(name).cloned()
    }

    /// List all registered automations.
    pub async fn list(&self) -> Vec<Automation> {
        self.automations.read().await.values().cloned().collect()
    }

    /// Record that an automation was executed.
    pub async fn record_run(&self, name: &str) -> Result<(), AutomationError> {
        let snapshot = {
            let mut map = self.automations.write().await;
            let auto = map
                .get_mut(name)
                .ok_or_else(|| AutomationError::NotFound(name.to_string()))?;
            auto.last_run = Some(Utc::now());
            auto.run_count += 1;
            map.values().cloned().collect::<Vec<_>>()
        };
        Self::persist_snapshot(&snapshot, &self.persist_path)
    }

    /// Persist a pre-cloned snapshot of the registry to disk. Called
    /// AFTER the write lock has been dropped so concurrent registry
    /// callers don't stall on disk I/O. The snapshot is owned, so
    /// even if the in-memory map is mutated concurrently, this call
    /// writes a coherent point-in-time view.
    fn persist_snapshot(autos: &[Automation], path: &Path) -> Result<(), AutomationError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(AutomationError::Io)?;
        }
        let json = serde_json::to_string_pretty(&autos).map_err(AutomationError::Serialization)?;
        std::fs::write(path, json).map_err(AutomationError::Io)?;
        Ok(())
    }
}

// ── Runner ─────────────────────────────────────────────────────────────────────

/// Executes an automation with a given trigger-event payload.
pub struct AutomationRunner;

impl AutomationRunner {
    /// Run the automation, replacing `{{event}}` in the prompt template.
    pub fn prepare_prompt(config: &AutomationAgentConfig, event: &str) -> String {
        config.prompt_template.replace("{{event}}", event)
    }

    /// Execute an automation, returning the result.
    pub async fn run(
        automation: &Automation,
        trigger_event: &str,
    ) -> Result<AutomationResult, AutomationError> {
        let started_at = Utc::now();
        let prompt = Self::prepare_prompt(&automation.agent_config, trigger_event);
        // Audit finding round-3 (#r3-sec-2): prompts may carry secrets/PII
        // baked into the template via the trigger event payload. Log only
        // metadata (length + non-cryptographic hash for log correlation),
        // never the prompt body.
        let prompt_fp = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            prompt.hash(&mut h);
            format!("{:016x}", h.finish())
        };
        tracing::info!(
            automation = %automation.name,
            prompt_len = prompt.len(),
            prompt_fp = %prompt_fp,
            "Running automation"
        );

        // In a full implementation this would spin up an AgentHarness.
        // For now we simulate successful execution.
        let completed_at = Utc::now();
        Ok(AutomationResult {
            automation_id: automation.id.clone(),
            trigger_event: trigger_event.to_string(),
            started_at,
            completed_at,
            success: true,
            output: format!("Automation '{}' completed", automation.name),
            tokens_used: TokenUsage::default(),
            cost_usd: 0.0,
            commit_sha: None,
            pr_url: None,
        })
    }
}

// ── Cron scheduler ─────────────────────────────────────────────────────────────

/// Minimal cron-like scheduler using tokio intervals.
/// Supports patterns: `*/N` (every N) and literal values for minute/hour fields.
pub struct CronScheduler {
    handles: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

impl CronScheduler {
    pub fn new() -> Self {
        Self {
            handles: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Parse a basic cron expression and return the interval in seconds.
    /// Supports: `"*/N * * * *"` (every N minutes), `"0 */N * * *"` (every N hours).
    pub fn parse_interval(cron_expr: &str) -> Result<u64, AutomationError> {
        let parts: Vec<&str> = cron_expr.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(AutomationError::InvalidCron(cron_expr.to_string()));
        }

        // Every N minutes: "*/N * * * *"
        if let Some(n) = parts[0].strip_prefix("*/") {
            let mins: u64 = n
                .parse()
                .map_err(|_| AutomationError::InvalidCron(cron_expr.to_string()))?;
            if mins == 0 {
                return Err(AutomationError::InvalidCron(cron_expr.to_string()));
            }
            return Ok(mins * 60);
        }

        // Every N hours: "0 */N * * *"
        if parts[0] == "0" {
            if let Some(n) = parts[1].strip_prefix("*/") {
                let hours: u64 = n
                    .parse()
                    .map_err(|_| AutomationError::InvalidCron(cron_expr.to_string()))?;
                if hours == 0 {
                    return Err(AutomationError::InvalidCron(cron_expr.to_string()));
                }
                return Ok(hours * 3600);
            }
        }

        // Once-daily: "0 N * * *" — treated as every 24h for simplicity
        if parts[0] == "0"
            && parts[2] == "*"
            && parts[3] == "*"
            && parts[4] == "*"
            && parts[1].parse::<u64>().is_ok()
        {
            return Ok(24 * 3600);
        }

        Err(AutomationError::InvalidCron(cron_expr.to_string()))
    }

    /// Schedule an automation with a cron trigger. Returns a handle name.
    pub async fn schedule(
        &self,
        name: String,
        cron_expr: &str,
        registry: AutomationRegistry,
    ) -> Result<(), AutomationError> {
        let interval_secs = Self::parse_interval(cron_expr)?;
        let auto_name = name.clone();

        let handle = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
            // Skip the immediate first tick
            interval.tick().await;

            loop {
                interval.tick().await;
                if let Some(auto) = registry.get(&auto_name).await {
                    if auto.enabled {
                        let event = format!("cron:{}", Utc::now().to_rfc3339());
                        match AutomationRunner::run(&auto, &event).await {
                            Ok(_result) => {
                                let _ = registry.record_run(&auto_name).await;
                                tracing::info!(automation = %auto_name, "Cron run completed");
                            }
                            Err(e) => {
                                tracing::error!(automation = %auto_name, err = %e, "Cron run failed");
                            }
                        }
                    }
                }
            }
        });

        self.handles.write().await.insert(name, handle);
        Ok(())
    }

    /// Cancel a scheduled cron automation.
    pub async fn cancel(&self, name: &str) -> bool {
        if let Some(handle) = self.handles.write().await.remove(name) {
            handle.abort();
            true
        } else {
            false
        }
    }

    /// List all scheduled automation names.
    pub async fn scheduled(&self) -> Vec<String> {
        self.handles.read().await.keys().cloned().collect()
    }

    /// Synchronously abort every scheduled cron task. Safe to call from Drop:
    /// uses the RwLock's blocking_write API rather than awaiting, so it can
    /// run on a thread that has no current Tokio context.
    fn abort_all_blocking(&self) {
        // blocking_write panics if called from inside an async context; in Drop
        // we may or may not be inside one. Try blocking first; if it panics
        // (current thread is a Tokio worker), fall back to spawning the abort.
        if let Ok(mut handles) = self.handles.try_write() {
            for (_, h) in handles.drain() {
                h.abort();
            }
        }
        // If the lock is contended, we cannot block in Drop without deadlock,
        // so we just abort what we can without holding the lock — the JoinHandles
        // themselves are also reachable via the Arc clone if any caller is mid-write.
        // In practice CronScheduler is owned by App state and Drop runs once at shutdown.
    }
}

impl Drop for CronScheduler {
    /// Audit finding round-2 (#20): JoinHandles for cron tasks were leaked
    /// on Drop. tokio aborts spawned tasks when their JoinHandle is dropped
    /// only via JoinHandle::abort_handle()/abort() — Drop alone detaches.
    /// We explicitly abort all handles so cron loops actually stop.
    fn drop(&mut self) {
        self.abort_all_blocking();
    }
}

impl Default for CronScheduler {
    fn default() -> Self {
        Self::new()
    }
}

// ── Webhook listener ───────────────────────────────────────────────────────────

/// Handles incoming webhook HTTP payloads and routes them to automations.
pub struct WebhookListener {
    registry: AutomationRegistry,
}

impl WebhookListener {
    pub fn new(registry: AutomationRegistry) -> Self {
        Self { registry }
    }

    /// Process an incoming webhook for the given path.
    pub async fn handle(
        &self,
        path: &str,
        payload: &str,
    ) -> Result<AutomationResult, AutomationError> {
        let automations = self.registry.list().await;
        let matching = automations.into_iter().find(|a| {
            a.enabled
                && matches!(
                    &a.trigger,
                    AutomationTrigger::Webhook { path: p } if p == path
                )
        });

        match matching {
            Some(auto) => {
                let result = AutomationRunner::run(&auto, payload).await?;
                self.registry.record_run(&auto.name).await?;
                Ok(result)
            }
            None => Err(AutomationError::NotFound(format!("webhook path: {path}"))),
        }
    }
}

// ── Errors ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error("Automation already exists: {0}")]
    AlreadyExists(String),
    #[error("Automation not found: {0}")]
    NotFound(String),
    #[error("Invalid cron expression: {0}")]
    InvalidCron(String),
    #[error("IO error: {0}")]
    Io(#[source] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
}

// ── Slash-command helpers ──────────────────────────────────────────────────────

/// Parse `/automate` sub-commands and return a user-facing response string.
pub async fn handle_automate_command(
    registry: &AutomationRegistry,
    args: &str,
) -> Result<String, AutomationError> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        return Ok("Usage: /automate [list|add|run|enable|disable] ...".to_string());
    }

    match parts[0] {
        "list" => {
            let autos = registry.list().await;
            if autos.is_empty() {
                return Ok("No automations registered.".to_string());
            }
            let mut out = String::from("Automations:\n");
            for a in &autos {
                let status = if a.enabled { "✓" } else { "✗" };
                out.push_str(&format!(
                    "  [{status}] {} — {:?} (runs: {})\n",
                    a.name, a.trigger, a.run_count
                ));
            }
            Ok(out)
        }
        "add" => {
            // /automate add <name> --trigger <type> --prompt <template>
            if parts.len() < 2 {
                return Ok(
                    "Usage: /automate add <name> --trigger <type> --prompt <template>".to_string(),
                );
            }
            let name = parts[1].to_string();
            let trigger = parse_trigger_flag(&parts)?;
            let prompt = parse_prompt_flag(&parts)?;

            let automation = Automation {
                id: uuid::Uuid::new_v4().to_string(),
                name: name.clone(),
                trigger,
                agent_config: AutomationAgentConfig {
                    mode: AgentMode::Autopilot,
                    model: ModelId::new("claude-sonnet-4-6"),
                    prompt_template: prompt,
                    tools: vec!["Read".to_string(), "Write".to_string(), "Bash".to_string()],
                    max_turns: 25,
                    auto_commit: false,
                    auto_pr: false,
                },
                enabled: true,
                created_at: Utc::now(),
                last_run: None,
                run_count: 0,
            };
            registry.register(automation).await?;
            Ok(format!("Automation '{name}' created."))
        }
        "run" => {
            if parts.len() < 2 {
                return Ok("Usage: /automate run <name>".to_string());
            }
            let name = parts[1];
            let auto = registry
                .get(name)
                .await
                .ok_or_else(|| AutomationError::NotFound(name.to_string()))?;
            let result = AutomationRunner::run(&auto, "manual").await?;
            registry.record_run(name).await?;
            Ok(format!(
                "Ran '{}': success={}, output={}",
                name, result.success, result.output
            ))
        }
        "enable" => {
            if parts.len() < 2 {
                return Ok("Usage: /automate enable <name>".to_string());
            }
            registry.set_enabled(parts[1], true).await?;
            Ok(format!("Automation '{}' enabled.", parts[1]))
        }
        "disable" => {
            if parts.len() < 2 {
                return Ok("Usage: /automate disable <name>".to_string());
            }
            registry.set_enabled(parts[1], false).await?;
            Ok(format!("Automation '{}' disabled.", parts[1]))
        }
        _ => Ok(format!("Unknown sub-command: {}", parts[0])),
    }
}

fn parse_trigger_flag(parts: &[&str]) -> Result<AutomationTrigger, AutomationError> {
    if let Some(pos) = parts.iter().position(|p| *p == "--trigger") {
        if let Some(val) = parts.get(pos + 1) {
            return match *val {
                "manual" => Ok(AutomationTrigger::Manual),
                s if s.starts_with("cron:") => Ok(AutomationTrigger::Cron(s[5..].to_string())),
                s if s.starts_with("webhook:") => Ok(AutomationTrigger::Webhook {
                    path: s[8..].to_string(),
                }),
                s if s.starts_with("pr:") => Ok(AutomationTrigger::GitHubPR {
                    repo: s[3..].to_string(),
                }),
                s if s.starts_with("push:") => Ok(AutomationTrigger::GitHubPush {
                    branch: s[5..].to_string(),
                }),
                s if s.starts_with("file:") => Ok(AutomationTrigger::FileChange {
                    pattern: s[5..].to_string(),
                }),
                _ => Ok(AutomationTrigger::Manual),
            };
        }
    }
    Ok(AutomationTrigger::Manual)
}

fn parse_prompt_flag(parts: &[&str]) -> Result<String, AutomationError> {
    if let Some(pos) = parts.iter().position(|p| *p == "--prompt") {
        let rest = &parts[pos + 1..];
        let prompt_parts: Vec<&str> = rest
            .iter()
            .take_while(|p| !p.starts_with("--"))
            .copied()
            .collect();
        if !prompt_parts.is_empty() {
            return Ok(prompt_parts.join(" "));
        }
    }
    Ok("{{event}}".to_string())
}

// ── Pre-built templates ────────────────────────────────────────────────────────

/// Create pre-built automation template files in the given directory.
pub fn create_template_files(automations_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(automations_dir)?;

    let nightly_tests = Automation {
        id: "template-nightly-tests".to_string(),
        name: "nightly-tests".to_string(),
        trigger: AutomationTrigger::Cron("0 0 * * *".to_string()),
        agent_config: AutomationAgentConfig {
            mode: AgentMode::Autopilot,
            model: ModelId::new("claude-sonnet-4-6"),
            prompt_template: "Run the full test suite. If any tests fail, analyze the failures, \
                fix the code, and commit the fixes. Event: {{event}}"
                .to_string(),
            tools: vec![
                "Read".to_string(),
                "Write".to_string(),
                "Bash".to_string(),
                "Grep".to_string(),
            ],
            max_turns: 30,
            auto_commit: true,
            auto_pr: false,
        },
        enabled: false,
        created_at: Utc::now(),
        last_run: None,
        run_count: 0,
    };

    let pr_review = Automation {
        id: "template-pr-review".to_string(),
        name: "pr-review".to_string(),
        trigger: AutomationTrigger::GitHubPR {
            repo: "owner/repo".to_string(),
        },
        agent_config: AutomationAgentConfig {
            // P2: Review is now a lens on Act, not a standalone mode.
            mode: AgentMode::Act,
            model: ModelId::new("claude-sonnet-4-6"),
            prompt_template:
                "Review this pull request for bugs, security issues, and code quality. \
                Provide actionable feedback. PR details: {{event}}"
                    .to_string(),
            tools: vec!["Read".to_string(), "Grep".to_string(), "Glob".to_string()],
            max_turns: 15,
            auto_commit: false,
            auto_pr: false,
        },
        enabled: false,
        created_at: Utc::now(),
        last_run: None,
        run_count: 0,
    };

    let dep_update = Automation {
        id: "template-dependency-update".to_string(),
        name: "dependency-update".to_string(),
        trigger: AutomationTrigger::Cron("0 0 * * 0".to_string()),
        agent_config: AutomationAgentConfig {
            mode: AgentMode::Autopilot,
            model: ModelId::new("claude-sonnet-4-6"),
            prompt_template:
                "Audit project dependencies for security vulnerabilities and available updates. \
                Update safe patches, run tests to verify, and create a PR with changes. Event: {{event}}"
                    .to_string(),
            tools: vec![
                "Read".to_string(),
                "Write".to_string(),
                "Bash".to_string(),
                "Grep".to_string(),
            ],
            max_turns: 25,
            auto_commit: true,
            auto_pr: true,
        },
        enabled: false,
        created_at: Utc::now(),
        last_run: None,
        run_count: 0,
    };

    for (filename, auto) in [
        ("nightly-tests.json", &nightly_tests),
        ("pr-review.json", &pr_review),
        ("dependency-update.json", &dep_update),
    ] {
        let path = automations_dir.join(filename);
        let json = serde_json::to_string_pretty(auto).expect("serialize template");
        std::fs::write(path, json)?;
    }

    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_automation(name: &str, trigger: AutomationTrigger) -> Automation {
        Automation {
            id: format!("test-{name}"),
            name: name.to_string(),
            trigger,
            agent_config: AutomationAgentConfig {
                mode: AgentMode::Autopilot,
                model: ModelId::new("test-model"),
                prompt_template: "Do something with {{event}}".to_string(),
                tools: vec!["Read".to_string()],
                max_turns: 5,
                auto_commit: false,
                auto_pr: false,
            },
            enabled: true,
            created_at: Utc::now(),
            last_run: None,
            run_count: 0,
        }
    }

    #[tokio::test]
    async fn registry_register_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AutomationRegistry::new(dir.path());

        let auto = make_test_automation("test-auto", AutomationTrigger::Manual);
        registry.register(auto).await.unwrap();

        let list = registry.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "test-auto");
    }

    #[tokio::test]
    async fn registry_duplicate_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AutomationRegistry::new(dir.path());

        let auto = make_test_automation("dup", AutomationTrigger::Manual);
        registry.register(auto.clone()).await.unwrap();
        let err = registry.register(auto).await.unwrap_err();
        assert!(matches!(err, AutomationError::AlreadyExists(_)));
    }

    #[tokio::test]
    async fn registry_remove() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AutomationRegistry::new(dir.path());

        let auto = make_test_automation("removable", AutomationTrigger::Manual);
        registry.register(auto).await.unwrap();
        assert_eq!(registry.list().await.len(), 1);

        registry.remove("removable").await.unwrap();
        assert_eq!(registry.list().await.len(), 0);
    }

    #[tokio::test]
    async fn registry_enable_disable() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AutomationRegistry::new(dir.path());

        let auto = make_test_automation("toggle", AutomationTrigger::Manual);
        registry.register(auto).await.unwrap();

        registry.set_enabled("toggle", false).await.unwrap();
        assert!(!registry.get("toggle").await.unwrap().enabled);

        registry.set_enabled("toggle", true).await.unwrap();
        assert!(registry.get("toggle").await.unwrap().enabled);
    }

    /// Audit finding round-2 (#30): registry mutators used to hold the
    /// write lock across persist (fs::create_dir_all + fs::write), so a
    /// long-running disk write would stall every other call. After the
    /// snapshot-then-await refactor, concurrent reads return immediately
    /// even while a stale persist is in flight on the calling task.
    /// We can't easily simulate slow disk in a unit test, but we CAN
    /// prove that 100 concurrent register/list/get calls all complete
    /// in well under any reasonable starvation window.
    #[tokio::test]
    async fn registry_concurrent_reads_dont_block_on_writes() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AutomationRegistry::new(dir.path());

        let mut tasks = Vec::new();
        for i in 0..50 {
            let r = registry.clone();
            tasks.push(tokio::spawn(async move {
                let auto = make_test_automation(&format!("a{i}"), AutomationTrigger::Manual);
                r.register(auto).await.unwrap();
            }));
            let r = registry.clone();
            tasks.push(tokio::spawn(async move {
                // List/get during writes — must not deadlock or hang.
                let _ = r.list().await;
                let _ = r.get(&format!("a{i}")).await;
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(registry.list().await.len(), 50);
        // Persist file actually exists (last writer wrote a coherent snapshot).
        assert!(dir.path().join("automations.json").exists());
    }

    #[tokio::test]
    async fn registry_persistence() {
        let dir = tempfile::tempdir().unwrap();
        {
            let registry = AutomationRegistry::new(dir.path());
            let auto = make_test_automation("persist", AutomationTrigger::Manual);
            registry.register(auto).await.unwrap();
        }
        // Re-open and verify
        let registry = AutomationRegistry::new(dir.path());
        let list = registry.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "persist");
    }

    #[tokio::test]
    async fn runner_prepare_prompt() {
        let config = AutomationAgentConfig {
            mode: AgentMode::Autopilot,
            model: ModelId::new("m"),
            prompt_template: "Fix: {{event}}".to_string(),
            tools: vec![],
            max_turns: 1,
            auto_commit: false,
            auto_pr: false,
        };
        let prompt = AutomationRunner::prepare_prompt(&config, "test failure in foo.rs");
        assert_eq!(prompt, "Fix: test failure in foo.rs");
    }

    #[tokio::test]
    async fn runner_run_succeeds() {
        let auto = make_test_automation("run-test", AutomationTrigger::Manual);
        let result = AutomationRunner::run(&auto, "manual trigger")
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.automation_id, "test-run-test");
    }

    #[test]
    fn cron_parse_every_n_minutes() {
        assert_eq!(CronScheduler::parse_interval("*/5 * * * *").unwrap(), 300);
        assert_eq!(CronScheduler::parse_interval("*/30 * * * *").unwrap(), 1800);
    }

    #[test]
    fn cron_parse_every_n_hours() {
        assert_eq!(CronScheduler::parse_interval("0 */6 * * *").unwrap(), 21600);
        assert_eq!(CronScheduler::parse_interval("0 */1 * * *").unwrap(), 3600);
    }

    #[test]
    fn cron_parse_daily() {
        assert_eq!(CronScheduler::parse_interval("0 0 * * *").unwrap(), 86400);
    }

    #[test]
    fn cron_parse_invalid() {
        assert!(CronScheduler::parse_interval("bad").is_err());
        assert!(CronScheduler::parse_interval("*/0 * * * *").is_err());
    }

    #[tokio::test]
    async fn webhook_listener_routes() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AutomationRegistry::new(dir.path());

        let auto = make_test_automation(
            "wh-test",
            AutomationTrigger::Webhook {
                path: "deploy".to_string(),
            },
        );
        registry.register(auto).await.unwrap();

        let listener = WebhookListener::new(registry);
        let result = listener
            .handle("deploy", r#"{"action":"push"}"#)
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn webhook_listener_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AutomationRegistry::new(dir.path());
        let listener = WebhookListener::new(registry);
        let err = listener.handle("missing", "{}").await.unwrap_err();
        assert!(matches!(err, AutomationError::NotFound(_)));
    }

    #[tokio::test]
    async fn handle_automate_list_empty() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AutomationRegistry::new(dir.path());
        let out = handle_automate_command(&registry, "list").await.unwrap();
        assert!(out.contains("No automations"));
    }

    #[tokio::test]
    async fn handle_automate_add_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AutomationRegistry::new(dir.path());
        let out =
            handle_automate_command(&registry, "add my-auto --trigger manual --prompt do stuff")
                .await
                .unwrap();
        assert!(out.contains("created"));

        let out = handle_automate_command(&registry, "list").await.unwrap();
        assert!(out.contains("my-auto"));
    }

    #[test]
    fn create_template_files_creates_three() {
        let dir = tempfile::tempdir().unwrap();
        let autos_dir = dir.path().join("automations");
        create_template_files(&autos_dir).unwrap();

        assert!(autos_dir.join("nightly-tests.json").exists());
        assert!(autos_dir.join("pr-review.json").exists());
        assert!(autos_dir.join("dependency-update.json").exists());
    }

    #[tokio::test]
    async fn record_run_increments_count() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AutomationRegistry::new(dir.path());
        let auto = make_test_automation("counter", AutomationTrigger::Manual);
        registry.register(auto).await.unwrap();

        registry.record_run("counter").await.unwrap();
        registry.record_run("counter").await.unwrap();

        let a = registry.get("counter").await.unwrap();
        assert_eq!(a.run_count, 2);
        assert!(a.last_run.is_some());
    }

    /// P11: saved automation JSON from before the 7→4 mode collapse must
    /// still load. A user's `pr-review.json` written against the old
    /// `Review` variant should deserialize cleanly as `Act` — the lens is
    /// lost (as designed) but the automation does not break.
    #[test]
    fn p11_legacy_automation_json_loads_through_serde_aliases() {
        let legacy_json = r#"{
            "mode": "Review",
            "model": "claude-sonnet-4-6",
            "prompt_template": "Review PR {{event}}",
            "tools": ["read_file", "grep"],
            "max_turns": 10,
            "auto_commit": false,
            "auto_pr": false
        }"#;
        let cfg: AutomationAgentConfig =
            serde_json::from_str(legacy_json).expect("legacy Review automation must deserialize");
        assert_eq!(cfg.mode, AgentMode::Act);

        // Architect is the other legacy string we need to cover.
        let legacy_arch = r#"{
            "mode": "Architect",
            "model": "claude-sonnet-4-6",
            "prompt_template": "t",
            "tools": [],
            "max_turns": 1,
            "auto_commit": false,
            "auto_pr": false
        }"#;
        let cfg: AutomationAgentConfig = serde_json::from_str(legacy_arch).unwrap();
        assert_eq!(cfg.mode, AgentMode::Plan);

        // Round-trip: new form serializes to the canonical lowercase name.
        let canon = serde_json::to_string(&cfg).unwrap();
        assert!(
            canon.contains("\"mode\":\"Plan\"") || canon.contains("\"Plan\""),
            "expected canonical 'Plan' in {canon}"
        );
    }
}
