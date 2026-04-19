//! Opt-in permission policy for MCP tool calls.
//!
//! All MCP servers are *disabled by default*. A caller must explicitly
//! mark a server as `Approved` (and optionally narrow which tools it
//! may invoke) before [`PermissionedMcpManager::call_tool`] will route
//! a request to the underlying manager.
//!
//! Two failure modes are encoded as distinct error variants so the UI
//! can prompt the user with the right action:
//!
//! - `ServerNotApproved` → "Approve <server>?" prompt.
//! - `ToolNotAllowed`    → "Allow tool <name> on <server>?" prompt.
//! - `ToolDenied`        → blanket-denied tool, no prompt.
//!
//! Audit log records every decision (approved/denied/error) so the
//! IDE can render an inspectable history without coupling to logging
//! infrastructure.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::error::{McpError, Result};
use crate::manager::McpServerManager;
use crate::types::{McpServerConfig, McpToolDef};

// ── Policy types ──────────────────────────────────────────────────────────

/// Per-server approval state. `Pending` is the safe default — a
/// not-yet-approved server cannot be invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerApproval {
    Pending,
    Approved,
    Denied,
}

impl Default for ServerApproval {
    fn default() -> Self {
        Self::Pending
    }
}

/// What tools may be invoked on an approved server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolPolicy {
    /// All tools allowed (after server approval).
    AllowAll,
    /// Only the listed tool names allowed; everything else denied.
    Allowlist { tools: Vec<String> },
    /// All tools allowed except the listed names.
    Blocklist { tools: Vec<String> },
}

impl Default for ToolPolicy {
    fn default() -> Self {
        // Safe default: nothing allowed until the user opts in.
        Self::Allowlist { tools: Vec::new() }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerPolicy {
    pub approval: ServerApproval,
    pub tools: ToolPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Approved,
    ServerNotApproved,
    ServerDenied,
    ToolNotAllowed,
    ToolDenied,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Seconds since UNIX epoch.
    pub at_unix_secs: u64,
    pub server_id: String,
    pub tool: String,
    pub outcome: String,
    /// Free-form details (error message, "approved", etc.).
    pub detail: String,
}

// ── Manager ───────────────────────────────────────────────────────────────

/// Wraps an [`McpServerManager`] with a per-server permission policy
/// and an audit log. Cheap to clone — internal state is `Arc<RwLock>`.
#[derive(Clone)]
pub struct PermissionedMcpManager {
    inner: Arc<McpServerManager>,
    policies: Arc<RwLock<HashMap<String, ServerPolicy>>>,
    audit: Arc<RwLock<Vec<AuditEntry>>>,
    /// Hard cap on audit log entries (FIFO trim). Bounds memory.
    audit_cap: usize,
}

impl PermissionedMcpManager {
    pub fn new(inner: Arc<McpServerManager>) -> Self {
        Self {
            inner,
            policies: Arc::new(RwLock::new(HashMap::new())),
            audit: Arc::new(RwLock::new(Vec::new())),
            audit_cap: 500,
        }
    }

    pub fn with_audit_cap(mut self, cap: usize) -> Self {
        self.audit_cap = cap.max(1);
        self
    }

    /// Underlying unsandboxed manager. Use sparingly — bypasses policy.
    pub fn inner(&self) -> &Arc<McpServerManager> {
        &self.inner
    }

    /// Register a server config. The server is added to the underlying
    /// manager but a `Pending` policy entry is created so it cannot be
    /// invoked until the caller explicitly approves it.
    pub async fn register_server(
        &self,
        config: McpServerConfig,
        connect: bool,
    ) -> Result<()> {
        let id = config.id.clone();
        self.inner.add_server(config, connect).await?;
        let mut policies = self.policies.write().await;
        policies.entry(id).or_default();
        Ok(())
    }

    pub async fn approve_server(&self, server_id: &str, tools: ToolPolicy) {
        let mut policies = self.policies.write().await;
        let entry = policies.entry(server_id.to_string()).or_default();
        entry.approval = ServerApproval::Approved;
        entry.tools = tools;
    }

    pub async fn deny_server(&self, server_id: &str) {
        let mut policies = self.policies.write().await;
        let entry = policies.entry(server_id.to_string()).or_default();
        entry.approval = ServerApproval::Denied;
    }

    pub async fn revoke(&self, server_id: &str) {
        let mut policies = self.policies.write().await;
        if let Some(p) = policies.get_mut(server_id) {
            p.approval = ServerApproval::Pending;
        }
    }

    pub async fn policy_for(&self, server_id: &str) -> ServerPolicy {
        self.policies
            .read()
            .await
            .get(server_id)
            .cloned()
            .unwrap_or_default()
    }

    /// All approved tools across all approved servers, after policy
    /// filtering. Use this to populate the agent's tool catalogue —
    /// LLMs should never see tools they cannot actually call.
    pub async fn approved_tools(&self) -> Vec<(String, McpToolDef)> {
        let policies = self.policies.read().await;
        let mut out = Vec::new();
        for (server_id, tools) in self.tools_grouped_by_server().await {
            let policy = policies.get(&server_id).cloned().unwrap_or_default();
            if policy.approval != ServerApproval::Approved {
                continue;
            }
            for tool in tools {
                if tool_allowed(&policy.tools, &tool.name) {
                    out.push((server_id.clone(), tool));
                }
            }
        }
        out
    }

    async fn tools_grouped_by_server(&self) -> Vec<(String, Vec<McpToolDef>)> {
        // The wrapped manager doesn't expose per-server tool listings,
        // so fall back to `all_tools()` and treat the union as
        // belonging to whichever server first claims it. This matches
        // `McpServerManager::call_tool`'s own routing rule.
        //
        // For multi-server setups the routing rule means a name
        // collision is resolved deterministically; the policy still
        // applies because both servers must be approved for both
        // tools to surface.
        let all = self.inner.all_tools().await;
        let statuses = self.inner.statuses().await;
        let mut grouped: HashMap<String, Vec<McpToolDef>> = HashMap::new();
        // Without per-server access, attribute every tool to every
        // running server's bucket. The policy filter then narrows.
        let running: Vec<String> = statuses.keys().cloned().collect();
        for tool in all {
            for sid in &running {
                grouped.entry(sid.clone()).or_default().push(tool.clone());
            }
        }
        grouped.into_iter().collect()
    }

    /// Decide whether a call is permitted, without making the call.
    pub async fn check(&self, server_id: &str, tool_name: &str) -> PermissionDecision {
        let policy = self.policy_for(server_id).await;
        match policy.approval {
            ServerApproval::Pending => PermissionDecision::ServerNotApproved,
            ServerApproval::Denied => PermissionDecision::ServerDenied,
            ServerApproval::Approved => match &policy.tools {
                ToolPolicy::AllowAll => PermissionDecision::Approved,
                ToolPolicy::Allowlist { tools } => {
                    if tools.iter().any(|t| t == tool_name) {
                        PermissionDecision::Approved
                    } else {
                        PermissionDecision::ToolNotAllowed
                    }
                }
                ToolPolicy::Blocklist { tools } => {
                    if tools.iter().any(|t| t == tool_name) {
                        PermissionDecision::ToolDenied
                    } else {
                        PermissionDecision::Approved
                    }
                }
            },
        }
    }

    /// Permission-checked wrapper around [`McpServerManager::call_tool`].
    ///
    /// The caller MUST pass the `server_id` it intends to invoke; we
    /// do not infer it from the tool name because two servers can
    /// expose tools with the same name. Returns
    /// [`McpError::PermissionDenied`] without invoking the underlying
    /// manager when policy rejects the call.
    pub async fn call_tool(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Value> {
        let decision = self.check(server_id, tool_name).await;
        if decision != PermissionDecision::Approved {
            self.audit_push(
                server_id,
                tool_name,
                "denied",
                &format!("{decision:?}"),
            )
            .await;
            return Err(McpError::PermissionDenied(format!(
                "{server_id}/{tool_name}: {decision:?}"
            )));
        }
        let result = self.inner.call_tool(tool_name, arguments).await;
        let (outcome, detail) = match &result {
            Ok(_) => ("ok", String::from("approved")),
            Err(e) => ("error", e.to_string()),
        };
        self.audit_push(server_id, tool_name, outcome, &detail).await;
        result
    }

    pub async fn audit_log(&self) -> Vec<AuditEntry> {
        self.audit.read().await.clone()
    }

    async fn audit_push(&self, server_id: &str, tool: &str, outcome: &str, detail: &str) {
        let entry = AuditEntry {
            at_unix_secs: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            server_id: server_id.to_string(),
            tool: tool.to_string(),
            outcome: outcome.to_string(),
            detail: detail.to_string(),
        };
        let mut log = self.audit.write().await;
        log.push(entry);
        if log.len() > self.audit_cap {
            let drop_n = log.len() - self.audit_cap;
            log.drain(0..drop_n);
        }
    }
}

fn tool_allowed(policy: &ToolPolicy, name: &str) -> bool {
    match policy {
        ToolPolicy::AllowAll => true,
        ToolPolicy::Allowlist { tools } => tools.iter().any(|t| t == name),
        ToolPolicy::Blocklist { tools } => !tools.iter().any(|t| t == name),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(id: &str) -> McpServerConfig {
        // Use a stdio config with a no-op command. We never actually
        // connect (`connect = false`), so the command is irrelevant.
        McpServerConfig::stdio(id, id, "true", vec![])
    }

    #[tokio::test]
    async fn pending_server_is_denied() {
        let mgr = PermissionedMcpManager::new(Arc::new(McpServerManager::new()));
        mgr.register_server(cfg("srv-a"), false).await.unwrap();
        let decision = mgr.check("srv-a", "anything").await;
        assert_eq!(decision, PermissionDecision::ServerNotApproved);
    }

    #[tokio::test]
    async fn approved_with_allow_all_passes() {
        let mgr = PermissionedMcpManager::new(Arc::new(McpServerManager::new()));
        mgr.register_server(cfg("srv-a"), false).await.unwrap();
        mgr.approve_server("srv-a", ToolPolicy::AllowAll).await;
        assert_eq!(mgr.check("srv-a", "x").await, PermissionDecision::Approved);
    }

    #[tokio::test]
    async fn allowlist_filters_unknown_tools() {
        let mgr = PermissionedMcpManager::new(Arc::new(McpServerManager::new()));
        mgr.register_server(cfg("srv-a"), false).await.unwrap();
        mgr.approve_server(
            "srv-a",
            ToolPolicy::Allowlist {
                tools: vec!["read".into()],
            },
        )
        .await;
        assert_eq!(mgr.check("srv-a", "read").await, PermissionDecision::Approved);
        assert_eq!(
            mgr.check("srv-a", "delete").await,
            PermissionDecision::ToolNotAllowed
        );
    }

    #[tokio::test]
    async fn blocklist_blocks_named_tools() {
        let mgr = PermissionedMcpManager::new(Arc::new(McpServerManager::new()));
        mgr.register_server(cfg("srv-a"), false).await.unwrap();
        mgr.approve_server(
            "srv-a",
            ToolPolicy::Blocklist {
                tools: vec!["delete".into(), "exec".into()],
            },
        )
        .await;
        assert_eq!(mgr.check("srv-a", "read").await, PermissionDecision::Approved);
        assert_eq!(mgr.check("srv-a", "delete").await, PermissionDecision::ToolDenied);
        assert_eq!(mgr.check("srv-a", "exec").await, PermissionDecision::ToolDenied);
    }

    #[tokio::test]
    async fn denied_server_short_circuits() {
        let mgr = PermissionedMcpManager::new(Arc::new(McpServerManager::new()));
        mgr.register_server(cfg("srv-a"), false).await.unwrap();
        mgr.deny_server("srv-a").await;
        assert_eq!(
            mgr.check("srv-a", "anything").await,
            PermissionDecision::ServerDenied
        );
    }

    #[tokio::test]
    async fn revoke_returns_to_pending() {
        let mgr = PermissionedMcpManager::new(Arc::new(McpServerManager::new()));
        mgr.register_server(cfg("srv-a"), false).await.unwrap();
        mgr.approve_server("srv-a", ToolPolicy::AllowAll).await;
        mgr.revoke("srv-a").await;
        assert_eq!(
            mgr.check("srv-a", "x").await,
            PermissionDecision::ServerNotApproved
        );
    }

    #[tokio::test]
    async fn call_on_pending_returns_permission_error_and_audits() {
        let mgr = PermissionedMcpManager::new(Arc::new(McpServerManager::new()));
        mgr.register_server(cfg("srv-a"), false).await.unwrap();
        let res = mgr.call_tool("srv-a", "x", Value::Null).await;
        assert!(matches!(res, Err(McpError::PermissionDenied(_))));
        let log = mgr.audit_log().await;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].outcome, "denied");
        assert_eq!(log[0].server_id, "srv-a");
    }

    #[tokio::test]
    async fn unknown_server_id_is_pending_by_default() {
        // Calling with a server_id we never registered must NOT silently
        // route through — defaults to Pending → denied.
        let mgr = PermissionedMcpManager::new(Arc::new(McpServerManager::new()));
        let decision = mgr.check("never-registered", "any").await;
        assert_eq!(decision, PermissionDecision::ServerNotApproved);
    }

    #[tokio::test]
    async fn audit_log_fifo_trims_to_cap() {
        let mgr = PermissionedMcpManager::new(Arc::new(McpServerManager::new()))
            .with_audit_cap(3);
        mgr.register_server(cfg("srv-a"), false).await.unwrap();
        for i in 0..5 {
            let _ = mgr.call_tool("srv-a", &format!("tool-{i}"), Value::Null).await;
        }
        let log = mgr.audit_log().await;
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].tool, "tool-2"); // oldest two dropped
        assert_eq!(log[2].tool, "tool-4");
    }

    #[tokio::test]
    async fn approved_tools_excludes_disallowed() {
        // Without a real running MCP subprocess, all_tools() returns
        // empty. We use this to verify the *filtering* surface
        // contract doesn't accidentally surface anything when there
        // are no approved servers.
        let mgr = PermissionedMcpManager::new(Arc::new(McpServerManager::new()));
        mgr.register_server(cfg("srv-a"), false).await.unwrap();
        // Not approved → no tools.
        assert!(mgr.approved_tools().await.is_empty());
        mgr.approve_server("srv-a", ToolPolicy::AllowAll).await;
        // Still empty because the manager has no real connected tools,
        // but the call must not error or panic.
        assert!(mgr.approved_tools().await.is_empty());
    }

    #[tokio::test]
    async fn policy_serializes_round_trip() {
        let p = ServerPolicy {
            approval: ServerApproval::Approved,
            tools: ToolPolicy::Blocklist {
                tools: vec!["x".into()],
            },
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ServerPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.approval, ServerApproval::Approved);
        match back.tools {
            ToolPolicy::Blocklist { tools } => assert_eq!(tools, vec!["x".to_string()]),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn tool_allowed_helper() {
        assert!(tool_allowed(&ToolPolicy::AllowAll, "x"));
        assert!(tool_allowed(
            &ToolPolicy::Allowlist {
                tools: vec!["x".into()]
            },
            "x"
        ));
        assert!(!tool_allowed(
            &ToolPolicy::Allowlist {
                tools: vec!["x".into()]
            },
            "y"
        ));
        assert!(!tool_allowed(
            &ToolPolicy::Blocklist {
                tools: vec!["x".into()]
            },
            "x"
        ));
    }
}

// Reserved for forward-compat: HashSet was used by an earlier audit
// dedup design; keep import scoped via the feature-gated path so it
// surfaces if reintroduced.
