use crate::client::McpClient;
use crate::error::{McpError, Result};
use crate::types::{McpServerConfig, McpToolDef, ServerStatus};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, instrument, warn};

// ── Server Entry ───────────────────────────────────────────────────────────────

struct ServerEntry {
    client: McpClient,
    /// Cached tool list (populated after connect).
    tools: Vec<McpToolDef>,
}

// ── Manager ────────────────────────────────────────────────────────────────────

/// Manages a pool of MCP server connections.
///
/// Tools from all servers are aggregated and routed transparently.
pub struct McpServerManager {
    servers: Arc<RwLock<HashMap<String, ServerEntry>>>,
}

impl McpServerManager {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add a server config and optionally start it immediately.
    #[instrument(skip(self, config), fields(server_id = %config.id))]
    pub async fn add_server(&self, config: McpServerConfig, connect: bool) -> Result<()> {
        let id = config.id.clone();
        let mut client = McpClient::new(config);

        let tools = if connect {
            info!("Connecting to MCP server '{}'", id);
            match client.connect().await {
                Ok(()) => match client.list_tools().await {
                    Ok(t) => {
                        info!("Server '{}' ready — {} tools", id, t.len());
                        t
                    }
                    Err(e) => {
                        warn!("Could not list tools for '{}': {}", id, e);
                        vec![]
                    }
                },
                Err(e) => {
                    error!("Failed to connect to '{}': {}", id, e);
                    client.status = ServerStatus::Error;
                    vec![]
                }
            }
        } else {
            vec![]
        };

        let mut servers = self.servers.write().await;
        servers.insert(id, ServerEntry { client, tools });
        Ok(())
    }

    /// Remove and shut down a server.
    #[instrument(skip(self), fields(server_id = %server_id))]
    pub async fn remove_server(&self, server_id: &str) -> Result<()> {
        let mut servers = self.servers.write().await;
        if let Some(mut entry) = servers.remove(server_id) {
            entry.client.shutdown().await;
            info!("Removed MCP server '{}'", server_id);
            Ok(())
        } else {
            Err(McpError::ServerNotFound(server_id.to_string()))
        }
    }

    /// Start all registered servers that are not yet running.
    pub async fn start_all(&self) -> Result<()> {
        let ids: Vec<String> = {
            let servers = self.servers.read().await;
            servers
                .iter()
                .filter(|(_, e)| !e.client.is_running())
                .map(|(id, _)| id.clone())
                .collect()
        };

        for id in ids {
            let mut servers = self.servers.write().await;
            if let Some(entry) = servers.get_mut(&id) {
                if let Err(e) = entry.client.connect().await {
                    error!("Failed to start '{}': {}", id, e);
                    entry.client.status = ServerStatus::Error;
                    continue;
                }
                match entry.client.list_tools().await {
                    Ok(t) => entry.tools = t,
                    Err(e) => warn!("Could not list tools for '{}': {}", id, e),
                }
            }
        }
        Ok(())
    }

    /// Return a deduplicated list of all tools across all running servers.
    ///
    /// If two servers expose a tool with the same name, the first one wins.
    pub async fn all_tools(&self) -> Vec<McpToolDef> {
        let servers = self.servers.read().await;
        let mut seen = std::collections::HashSet::new();
        let mut tools = Vec::new();
        for entry in servers.values() {
            if !entry.client.is_running() {
                continue;
            }
            for tool in &entry.tools {
                if seen.insert(tool.name.clone()) {
                    tools.push(tool.clone());
                }
            }
        }
        tools
    }

    /// Call a tool by name, routing to the first server that exposes it.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let server_id = {
            let servers = self.servers.read().await;
            servers
                .iter()
                .find(|(_, e)| e.client.is_running() && e.tools.iter().any(|t| t.name == name))
                .map(|(id, _)| id.clone())
                .ok_or_else(|| McpError::ToolNotFound(name.to_string()))?
        };

        let mut servers = self.servers.write().await;
        let entry = servers
            .get_mut(&server_id)
            .ok_or_else(|| McpError::ServerNotFound(server_id.clone()))?;
        entry.client.call_tool(name, arguments).await
    }

    /// Check all servers and update their statuses.
    pub async fn health_check(&self) {
        let ids: Vec<String> = {
            let s = self.servers.read().await;
            s.keys().cloned().collect()
        };

        for id in ids {
            let mut servers = self.servers.write().await;
            if let Some(entry) = servers.get_mut(&id) {
                if entry.client.is_running() {
                    // Refresh tool list as a lightweight ping
                    match entry.client.list_tools().await {
                        Ok(t) => entry.tools = t,
                        Err(e) => {
                            warn!("Health check failed for '{}': {}", id, e);
                            entry.client.status = ServerStatus::Degraded;
                        }
                    }
                }
            }
        }
    }

    /// Shut down all servers.
    pub async fn shutdown_all(&self) {
        let ids: Vec<String> = {
            let s = self.servers.read().await;
            s.keys().cloned().collect()
        };

        let mut servers = self.servers.write().await;
        for id in &ids {
            if let Some(entry) = servers.get_mut(id) {
                entry.client.shutdown().await;
                info!("Shut down MCP server '{}'", id);
            }
        }
    }

    /// Return statuses for all registered servers.
    pub async fn statuses(&self) -> HashMap<String, ServerStatus> {
        let servers = self.servers.read().await;
        servers
            .iter()
            .map(|(id, e)| (id.clone(), e.client.status))
            .collect()
    }
}

impl Default for McpServerManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{McpServerConfig, McpTransport};
    use std::collections::HashMap;

    fn dummy_config(id: &str) -> McpServerConfig {
        McpServerConfig {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            transport: McpTransport::Stdio {
                command: "false".into(), // intentionally fails to connect
                args: vec![],
                env: HashMap::new(),
            },
            auto_start: false,
        }
    }

    #[tokio::test]
    async fn add_server_without_connect() {
        let mgr = McpServerManager::new();
        mgr.add_server(dummy_config("srv1"), false).await.unwrap();
        let statuses = mgr.statuses().await;
        assert!(statuses.contains_key("srv1"));
        assert_eq!(statuses["srv1"], ServerStatus::Stopped);
    }

    #[tokio::test]
    async fn remove_registered_server() {
        let mgr = McpServerManager::new();
        mgr.add_server(dummy_config("srv2"), false).await.unwrap();
        mgr.remove_server("srv2").await.unwrap();
        let statuses = mgr.statuses().await;
        assert!(!statuses.contains_key("srv2"));
    }

    #[tokio::test]
    async fn remove_nonexistent_server_errors() {
        let mgr = McpServerManager::new();
        let err = mgr.remove_server("nope").await.unwrap_err();
        assert!(matches!(err, McpError::ServerNotFound(_)));
    }

    #[tokio::test]
    async fn all_tools_empty_when_no_servers_running() {
        let mgr = McpServerManager::new();
        mgr.add_server(dummy_config("srv3"), false).await.unwrap();
        let tools = mgr.all_tools().await;
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn call_tool_unknown_tool_errors() {
        let mgr = McpServerManager::new();
        let err = mgr
            .call_tool("nonexistent", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::ToolNotFound(_)));
    }

    #[tokio::test]
    async fn shutdown_all_runs_without_panic() {
        let mgr = McpServerManager::new();
        mgr.add_server(dummy_config("srv4"), false).await.unwrap();
        mgr.add_server(dummy_config("srv5"), false).await.unwrap();
        mgr.shutdown_all().await; // should not panic
    }
}
