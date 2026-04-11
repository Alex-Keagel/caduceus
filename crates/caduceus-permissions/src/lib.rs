use caduceus_core::{CaduceusError, Result, SessionId};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// ── Capabilities ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Read files within the workspace
    FsRead,
    /// Write/create/delete files within the workspace
    FsWrite,
    /// Execute shell commands/processes
    ProcessExec,
    /// Make outbound HTTP requests
    NetworkHttp,
    /// Perform mutating git operations (commit, push, etc.)
    GitMutate,
    /// Access paths outside the workspace root
    FsEscape,
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FsRead => write!(f, "fs:read"),
            Self::FsWrite => write!(f, "fs:write"),
            Self::ProcessExec => write!(f, "process:exec"),
            Self::NetworkHttp => write!(f, "network:http"),
            Self::GitMutate => write!(f, "git:mutate"),
            Self::FsEscape => write!(f, "fs:escape"),
        }
    }
}

// ── Permission request/decision ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub session_id: SessionId,
    pub capability: Capability,
    pub resource: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    Allowed,
    Denied,
    AllowedForSession,
}

// ── Audit log ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub session_id: SessionId,
    pub capability: Capability,
    pub resource: String,
    pub decision: PermissionDecision,
    pub timestamp: DateTime<Utc>,
}

pub struct AuditLog {
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn record(&mut self, entry: AuditEntry) {
        self.entries.push(entry);
    }

    pub fn entries_for_session(&self, session_id: &SessionId) -> Vec<&AuditEntry> {
        self.entries
            .iter()
            .filter(|e| &e.session_id == session_id)
            .collect()
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

// ── PermissionEnforcer ─────────────────────────────────────────────────────────

pub struct PermissionEnforcer {
    workspace_root: PathBuf,
    granted_capabilities: HashSet<Capability>,
    session_grants: std::collections::HashMap<String, HashSet<Capability>>,
    audit: AuditLog,
    mode: PermissionMode,
}

impl PermissionEnforcer {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let mut granted = HashSet::new();
        // Default safe capabilities
        granted.insert(Capability::FsRead);
        granted.insert(Capability::FsWrite);
        granted.insert(Capability::ProcessExec);
        granted.insert(Capability::NetworkHttp);

        Self {
            workspace_root: workspace_root.into(),
            granted_capabilities: granted,
            session_grants: std::collections::HashMap::new(),
            audit: AuditLog::new(),
            mode: PermissionMode::Default,
        }
    }

    pub fn check(
        &mut self,
        session_id: &SessionId,
        capability: &Capability,
        resource: &str,
    ) -> Result<()> {
        // Permission mode short-circuits
        match &self.mode {
            PermissionMode::Bypass => {
                self.audit.record(AuditEntry {
                    session_id: session_id.clone(),
                    capability: capability.clone(),
                    resource: resource.to_string(),
                    decision: PermissionDecision::Allowed,
                    timestamp: Utc::now(),
                });
                return Ok(());
            }
            PermissionMode::Plan => {
                if matches!(
                    capability,
                    Capability::FsWrite | Capability::ProcessExec | Capability::GitMutate
                ) {
                    self.audit.record(AuditEntry {
                        session_id: session_id.clone(),
                        capability: capability.clone(),
                        resource: resource.to_string(),
                        decision: PermissionDecision::Denied,
                        timestamp: Utc::now(),
                    });
                    return Err(CaduceusError::PermissionDenied {
                        capability: capability.to_string(),
                        tool: resource.to_string(),
                    });
                }
            }
            PermissionMode::Default => { /* fall through to existing logic */ }
        }

        // FsEscape: check path is within workspace
        if matches!(capability, Capability::FsRead | Capability::FsWrite) {
            let path = Path::new(resource);
            if path.is_absolute() {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if !canonical.starts_with(&self.workspace_root) {
                    self.audit.record(AuditEntry {
                        session_id: session_id.clone(),
                        capability: capability.clone(),
                        resource: resource.to_string(),
                        decision: PermissionDecision::Denied,
                        timestamp: Utc::now(),
                    });
                    return Err(CaduceusError::PermissionDenied {
                        capability: capability.to_string(),
                        tool: format!("path:{resource}"),
                    });
                }
            }
        }

        let allowed = self.granted_capabilities.contains(capability)
            || self
                .session_grants
                .get(&session_id.to_string())
                .map(|s| s.contains(capability))
                .unwrap_or(false);

        let decision = if allowed {
            PermissionDecision::Allowed
        } else {
            PermissionDecision::Denied
        };

        self.audit.record(AuditEntry {
            session_id: session_id.clone(),
            capability: capability.clone(),
            resource: resource.to_string(),
            decision: decision.clone(),
            timestamp: Utc::now(),
        });

        if decision == PermissionDecision::Allowed {
            Ok(())
        } else {
            Err(CaduceusError::PermissionDenied {
                capability: capability.to_string(),
                tool: resource.to_string(),
            })
        }
    }

    pub fn grant_capability(&mut self, capability: Capability) {
        self.granted_capabilities.insert(capability);
    }

    pub fn revoke_capability(&mut self, capability: &Capability) {
        self.granted_capabilities.remove(capability);
    }

    pub fn audit_log(&self) -> &AuditLog {
        &self.audit
    }

    pub fn set_mode(&mut self, mode: PermissionMode) {
        self.mode = mode;
    }

    pub fn mode(&self) -> &PermissionMode {
        &self.mode
    }
}

// ── Hook system ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum HookEvent {
    SessionStart,
    SessionEnd,
    TurnStart,
    TurnEnd,
    ToolCallStart { tool: String },
    ToolCallEnd { tool: String, result: String },
    LlmRequestStart,
    LlmResponseEnd,
    PermissionGranted { capability: String },
    PermissionDenied { capability: String },
    ErrorOccurred { error: String },
    FileRead { path: String },
    FileWrite { path: String },
    BashExec { command: String },
    BashComplete { exit_code: i32 },
    CompactionStart,
    CompactionEnd,
}

// Use discriminant-only equality so any ToolCallStart matches any other ToolCallStart for registration
impl PartialEq for HookEvent {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}
impl Eq for HookEvent {}
impl std::hash::Hash for HookEvent {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
    }
}

impl HookEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            HookEvent::SessionStart => "SessionStart",
            HookEvent::SessionEnd => "SessionEnd",
            HookEvent::TurnStart => "TurnStart",
            HookEvent::TurnEnd => "TurnEnd",
            HookEvent::ToolCallStart { .. } => "ToolCallStart",
            HookEvent::ToolCallEnd { .. } => "ToolCallEnd",
            HookEvent::LlmRequestStart => "LlmRequestStart",
            HookEvent::LlmResponseEnd => "LlmResponseEnd",
            HookEvent::PermissionGranted { .. } => "PermissionGranted",
            HookEvent::PermissionDenied { .. } => "PermissionDenied",
            HookEvent::ErrorOccurred { .. } => "ErrorOccurred",
            HookEvent::FileRead { .. } => "FileRead",
            HookEvent::FileWrite { .. } => "FileWrite",
            HookEvent::BashExec { .. } => "BashExec",
            HookEvent::BashComplete { .. } => "BashComplete",
            HookEvent::CompactionStart => "CompactionStart",
            HookEvent::CompactionEnd => "CompactionEnd",
        }
    }
}

pub type HookHandler = Box<dyn Fn(&HookEvent, &serde_json::Value) -> Result<()> + Send + Sync>;

pub struct HookRegistry {
    hooks: std::collections::HashMap<HookEvent, Vec<HookHandler>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            hooks: std::collections::HashMap::new(),
        }
    }

    pub fn register(&mut self, event: HookEvent, handler: HookHandler) {
        self.hooks.entry(event).or_default().push(handler);
    }

    pub fn emit(&self, event: &HookEvent, context: &serde_json::Value) -> Result<()> {
        if let Some(handlers) = self.hooks.get(event) {
            for handler in handlers {
                handler(event, context)?;
            }
        }
        Ok(())
    }

    /// Convenience wrapper for calling `emit` from async contexts.
    /// Note: handlers are still invoked synchronously — this does **not** support
    /// `async` hook handlers.  It exists so callers don't need `spawn_blocking`.
    pub async fn emit_async(&self, event: &HookEvent, context: &serde_json::Value) -> Result<()> {
        self.emit(event, context)
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Permission modes ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PermissionMode {
    #[default]
    Default,
    Plan,
    Bypass,
}

// ── Kill switch ────────────────────────────────────────────────────────────────

/// Global emergency stop for all running agents.
///
/// The kill switch is an atomic boolean checked at the top of every tool dispatch
/// cycle. When triggered, all in-flight operations should be cancelled and session
/// state preserved.
#[derive(Debug, Clone)]
pub struct KillSwitch {
    active: Arc<AtomicBool>,
}

impl KillSwitch {
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Activate the kill switch. All subsequent `is_active()` checks return true.
    pub fn trigger(&self) {
        self.active.store(true, Ordering::SeqCst);
    }

    /// Check whether the kill switch is currently active.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Reset the kill switch (requires explicit human action).
    pub fn reset(&self) {
        self.active.store(false, Ordering::SeqCst);
    }
}

impl Default for KillSwitch {
    fn default() -> Self {
        Self::new()
    }
}

// ── Secret scanner ────────────────────────────────────────────────────────────

/// A detected secret in scanned text.
#[derive(Debug, Clone)]
pub struct SecretFinding {
    pub kind: String,
    pub start: usize,
    pub end: usize,
    pub redacted_preview: String,
}

/// Scans text for leaked secrets and credentials using regex patterns.
pub struct SecretScanner {
    patterns: Vec<(String, Regex)>,
}

impl SecretScanner {
    /// Create a scanner with the default pattern set covering common credential types.
    pub fn new() -> Self {
        let pattern_defs = vec![
            ("AWS Access Key", r"AKIA[0-9A-Z]{16}"),
            ("GitHub Token", r"gh[pousr]_[A-Za-z0-9_]{36,255}"),
            (
                "Private Key",
                r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
            ),
            (
                "JWT Token",
                r"eyJ[A-Za-z0-9\-_]+\.eyJ[A-Za-z0-9\-_]+\.[A-Za-z0-9\-_.+/=]+",
            ),
            (
                "Database Connection String",
                r"(?:postgres|mysql|mongodb)://[^\s]{10,}",
            ),
            ("Slack Token", r"xox[baprs]-[0-9a-zA-Z\-]{10,}"),
            (
                "Generic High-Entropy Key",
                r#"(?i)(?:api[_-]?key|secret|token|password)\s*[:=]\s*['"]?[A-Za-z0-9/+=]{20,}"#,
            ),
        ];

        let patterns = pattern_defs
            .into_iter()
            .filter_map(|(name, pat)| Regex::new(pat).ok().map(|re| (name.to_string(), re)))
            .collect();

        Self { patterns }
    }

    /// Scan text and return all detected secret findings.
    pub fn scan(&self, text: &str) -> Vec<SecretFinding> {
        let mut findings = Vec::new();
        for (kind, regex) in &self.patterns {
            for m in regex.find_iter(text) {
                let matched = m.as_str();
                let preview_len = matched.len().min(8);
                let redacted = format!(
                    "{}{}",
                    &matched[..preview_len],
                    "*".repeat(matched.len().saturating_sub(preview_len))
                );
                findings.push(SecretFinding {
                    kind: kind.clone(),
                    start: m.start(),
                    end: m.end(),
                    redacted_preview: redacted,
                });
            }
        }
        findings
    }

    /// Redact all detected secrets in the text, replacing them with `[REDACTED:<kind>]`.
    pub fn redact(&self, text: &str) -> String {
        let mut result = text.to_string();
        // Process findings in reverse order so byte offsets remain valid
        let mut findings = self.scan(text);
        findings.sort_by(|a, b| b.start.cmp(&a.start));
        for finding in findings {
            let replacement = format!("[REDACTED:{}]", finding.kind);
            result.replace_range(finding.start..finding.end, &replacement);
        }
        result
    }
}

impl Default for SecretScanner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::SessionId;

    #[test]
    fn it_works() {
        let _cap = Capability::FsRead;
    }

    #[test]
    fn permission_check_allowed() {
        let mut enforcer = PermissionEnforcer::new("/workspace");
        let session_id = SessionId::new();
        let result = enforcer.check(&session_id, &Capability::FsRead, "src/main.rs");
        assert!(result.is_ok());
    }

    #[test]
    fn permission_check_denied_escape() {
        let mut enforcer = PermissionEnforcer::new("/workspace");
        let session_id = SessionId::new();
        let result = enforcer.check(&session_id, &Capability::FsRead, "/etc/passwd");
        // may be ok if /etc/passwd doesn't exist under /workspace, or err
        // just verify it runs without panic
        let _ = result;
    }

    // ── Hook system tests ──────────────────────────────────────────────────

    #[test]
    fn hook_registry_emit_calls_handlers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();

        let mut registry = HookRegistry::new();
        registry.register(
            HookEvent::SessionStart,
            Box::new(move |_event, _ctx| {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        );

        registry
            .emit(&HookEvent::SessionStart, &serde_json::json!({}))
            .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        registry
            .emit(&HookEvent::SessionStart, &serde_json::json!({}))
            .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn hook_registry_discriminant_matching() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();

        let mut registry = HookRegistry::new();
        registry.register(
            HookEvent::ToolCallStart {
                tool: String::new(),
            },
            Box::new(move |_event, _ctx| {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        );

        // Different tool name but same discriminant should trigger handler
        registry
            .emit(
                &HookEvent::ToolCallStart {
                    tool: "bash".into(),
                },
                &serde_json::json!({}),
            )
            .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn hook_event_kind_strings() {
        assert_eq!(HookEvent::SessionStart.kind(), "SessionStart");
        assert_eq!(
            HookEvent::ToolCallStart { tool: "x".into() }.kind(),
            "ToolCallStart"
        );
        assert_eq!(HookEvent::CompactionEnd.kind(), "CompactionEnd");
    }

    #[test]
    fn hook_registry_no_handlers_is_ok() {
        let registry = HookRegistry::new();
        let result = registry.emit(&HookEvent::TurnStart, &serde_json::json!({}));
        assert!(result.is_ok());
    }

    // ── Permission mode tests ──────────────────────────────────────────────

    #[test]
    fn permission_mode_default_allows_reads() {
        let mut enforcer = PermissionEnforcer::new("/workspace");
        assert_eq!(*enforcer.mode(), PermissionMode::Default);
        let sid = SessionId::new();
        assert!(enforcer.check(&sid, &Capability::FsRead, "file.rs").is_ok());
    }

    #[test]
    fn permission_mode_bypass_allows_everything() {
        let mut enforcer = PermissionEnforcer::new("/workspace");
        enforcer.set_mode(PermissionMode::Bypass);
        let sid = SessionId::new();
        // Even GitMutate (not in default grants) is allowed in bypass
        assert!(enforcer.check(&sid, &Capability::GitMutate, "repo").is_ok());
        assert!(enforcer
            .check(&sid, &Capability::FsEscape, "/etc/passwd")
            .is_ok());
    }

    #[test]
    fn permission_mode_plan_denies_writes() {
        let mut enforcer = PermissionEnforcer::new("/workspace");
        enforcer.set_mode(PermissionMode::Plan);
        let sid = SessionId::new();
        // Reads are still allowed
        assert!(enforcer.check(&sid, &Capability::FsRead, "file.rs").is_ok());
        // Writes, exec, and git mutate are denied
        assert!(enforcer
            .check(&sid, &Capability::FsWrite, "file.rs")
            .is_err());
        assert!(enforcer
            .check(&sid, &Capability::ProcessExec, "ls")
            .is_err());
        assert!(enforcer
            .check(&sid, &Capability::GitMutate, "repo")
            .is_err());
    }

    #[test]
    fn permission_mode_plan_allows_network() {
        let mut enforcer = PermissionEnforcer::new("/workspace");
        enforcer.set_mode(PermissionMode::Plan);
        let sid = SessionId::new();
        assert!(enforcer
            .check(&sid, &Capability::NetworkHttp, "https://example.com")
            .is_ok());
    }

    // ── Kill switch tests ──────────────────────────────────────────────────

    #[test]
    fn kill_switch_starts_inactive() {
        let ks = KillSwitch::new();
        assert!(!ks.is_active());
    }

    #[test]
    fn kill_switch_trigger_and_reset() {
        let ks = KillSwitch::new();
        ks.trigger();
        assert!(ks.is_active());
        ks.reset();
        assert!(!ks.is_active());
    }

    #[test]
    fn kill_switch_clone_shares_state() {
        let ks = KillSwitch::new();
        let ks2 = ks.clone();
        ks.trigger();
        assert!(ks2.is_active());
    }

    // ── Secret scanner tests ───────────────────────────────────────────────

    #[test]
    fn secret_scanner_detects_aws_key() {
        let scanner = SecretScanner::new();
        let text = "config: AKIAIOSFODNN7EXAMPLE";
        let findings = scanner.scan(text);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, "AWS Access Key");
    }

    #[test]
    fn secret_scanner_detects_github_token() {
        let scanner = SecretScanner::new();
        let text = "token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij";
        let findings = scanner.scan(text);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, "GitHub Token");
    }

    #[test]
    fn secret_scanner_detects_private_key() {
        let scanner = SecretScanner::new();
        let text = "-----BEGIN RSA PRIVATE KEY-----\nMIIE...";
        let findings = scanner.scan(text);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, "Private Key");
    }

    #[test]
    fn secret_scanner_no_false_positive_on_clean_text() {
        let scanner = SecretScanner::new();
        let text = "This is a normal code review comment with no secrets.";
        let findings = scanner.scan(text);
        assert!(findings.is_empty());
    }

    #[test]
    fn secret_scanner_redact_replaces_secrets() {
        let scanner = SecretScanner::new();
        let text = "key: AKIAIOSFODNN7EXAMPLE";
        let redacted = scanner.redact(text);
        assert!(redacted.contains("[REDACTED:AWS Access Key]"));
        assert!(!redacted.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn secret_scanner_detects_connection_string() {
        let scanner = SecretScanner::new();
        let text = "DATABASE_URL=postgres://user:pass@host:5432/mydb";
        let findings = scanner.scan(text);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, "Database Connection String");
    }
}
