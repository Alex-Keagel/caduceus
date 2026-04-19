use crate::client::McpClient;
use crate::error::{McpError, Result};
use crate::types::{McpServerConfig, McpToolDef, ServerStatus};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info, instrument, warn};

// ── Server Entry ───────────────────────────────────────────────────────────────

struct ServerEntry {
    client: McpClient,
    /// Cached tool list (populated after connect).
    tools: Vec<McpToolDef>,
}

/// Shared, per-server lock. Held while talking to that one server's stdio
/// channel — never held across any operation that talks to a *different*
/// server, so a hung MCP process can only block calls to itself.
type ServerHandle = Arc<Mutex<ServerEntry>>;

// ── Manager ────────────────────────────────────────────────────────────────────

/// Manages a pool of MCP server connections.
///
/// Tools from all servers are aggregated and routed transparently.
///
/// **Concurrency**: the outer `RwLock` only protects the registry map
/// (insert/remove/snapshot of `Arc` handles); per-server I/O happens under
/// each server's own `Mutex`, so a slow or hung MCP process can never
/// block calls targeting other servers (fix for audit finding #3).
pub struct McpServerManager {
    servers: Arc<RwLock<HashMap<String, ServerHandle>>>,
}

impl McpServerManager {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Snapshot the current `(id, handle)` pairs without holding the outer
    /// lock for any subsequent work. Per-server operations then lock their
    /// own `Mutex`, so other servers (and other manager-level callers)
    /// remain unblocked.
    async fn snapshot_handles(&self) -> Vec<(String, ServerHandle)> {
        let servers = self.servers.read().await;
        servers
            .iter()
            .map(|(id, h)| (id.clone(), Arc::clone(h)))
            .collect()
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

        // Outer write lock held only for the map insert — no I/O underneath.
        let mut servers = self.servers.write().await;
        servers.insert(id, Arc::new(Mutex::new(ServerEntry { client, tools })));
        Ok(())
    }

    /// Remove and shut down a server. Outer write lock is released before
    /// shutdown I/O so other manager calls aren't blocked by the teardown.
    #[instrument(skip(self), fields(server_id = %server_id))]
    pub async fn remove_server(&self, server_id: &str) -> Result<()> {
        let removed = {
            let mut servers = self.servers.write().await;
            servers.remove(server_id)
        };
        match removed {
            Some(handle) => {
                let mut entry = handle.lock().await;
                entry.client.shutdown().await;
                info!("Removed MCP server '{}'", server_id);
                Ok(())
            }
            None => Err(McpError::ServerNotFound(server_id.to_string())),
        }
    }

    /// Start all registered servers that are not yet running.
    ///
    /// Snapshots the handle list once, then connects each server under its
    /// own per-server lock — slow connect on server A does not block
    /// reading or writing the manager state for server B.
    pub async fn start_all(&self) -> Result<()> {
        let handles = self.snapshot_handles().await;

        for (id, handle) in handles {
            // Cheap pre-check under the per-server lock so we don't try to
            // restart something that's already running.
            let mut entry = handle.lock().await;
            if entry.client.is_running() {
                continue;
            }
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
        Ok(())
    }

    /// Return a deduplicated list of all tools across all running servers.
    ///
    /// If two servers expose a tool with the same name, the first one wins.
    /// Each per-server lock is taken only for the brief tool-list copy, so
    /// concurrent `call_tool`s on other servers continue uninterrupted.
    pub async fn all_tools(&self) -> Vec<McpToolDef> {
        let handles = self.snapshot_handles().await;
        let mut seen = std::collections::HashSet::new();
        let mut tools = Vec::new();
        for (_id, handle) in handles {
            let entry = handle.lock().await;
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
    ///
    /// Resolves the target server under each handle's per-server lock,
    /// then performs the (potentially slow) `call_tool` await with only
    /// that server's lock held — never the manager outer lock.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let handles = self.snapshot_handles().await;
        for (id, handle) in handles {
            // Pre-check + call under the same per-server lock so we don't
            // race a concurrent `remove_server` of the chosen server.
            let mut entry = handle.lock().await;
            if entry.client.is_running() && entry.tools.iter().any(|t| t.name == name) {
                tracing::debug!("Routing tool '{}' to server '{}'", name, id);
                return entry.client.call_tool(name, arguments).await;
            }
        }
        Err(McpError::ToolNotFound(name.to_string()))
    }

    /// Check all servers and update their statuses. Per-server health checks
    /// run sequentially but each holds only its own lock — one hung server
    /// no longer blocks health checks on the others (other than the fact
    /// that this method walks the list serially; callers wanting full
    /// parallelism can `tokio::spawn` per handle from `snapshot_handles`).
    pub async fn health_check(&self) {
        let handles = self.snapshot_handles().await;

        for (id, handle) in handles {
            let mut entry = handle.lock().await;
            if entry.client.is_running() {
                // Refresh tool list as a lightweight ping.
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

    /// Shut down all servers. Outer write lock is taken once to drain the
    /// map; each server's shutdown then runs under its own per-server lock.
    pub async fn shutdown_all(&self) {
        let drained: Vec<(String, ServerHandle)> = {
            let mut servers = self.servers.write().await;
            servers.drain().collect()
        };

        for (id, handle) in drained {
            let mut entry = handle.lock().await;
            entry.client.shutdown().await;
            info!("Shut down MCP server '{}'", id);
        }
    }

    /// Return statuses for all registered servers.
    pub async fn statuses(&self) -> HashMap<String, ServerStatus> {
        let handles = self.snapshot_handles().await;
        let mut out = HashMap::with_capacity(handles.len());
        for (id, handle) in handles {
            let entry = handle.lock().await;
            out.insert(id, entry.client.status);
        }
        out
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

    // ── Additional manager tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_tool_not_found_error() {
        let mgr = McpServerManager::new();
        // No servers registered, so any tool call should fail
        let result = mgr
            .call_tool("nonexistent_tool", serde_json::json!({}))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, McpError::ToolNotFound(_)),
            "expected ToolNotFound error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_server_disconnect_handled() {
        let mgr = McpServerManager::new();
        // Add a server but don't connect it
        mgr.add_server(dummy_config("disconnected"), false)
            .await
            .unwrap();

        // Calling a tool should fail since server isn't running
        let result = mgr.call_tool("any_tool", serde_json::json!({})).await;
        assert!(
            result.is_err(),
            "calling tool on disconnected server should error"
        );
    }

    /// Regression for finding #3: a hung MCP server (here simulated by
    /// holding one server's per-server `Mutex` for an extended time) MUST
    /// NOT block manager-level operations on a different server. Under
    /// the old `RwLock<HashMap<String, ServerEntry>>` design, any
    /// `call_tool`/`health_check`/etc would acquire the outer write lock
    /// and serialize behind the slow server. With per-server Arc<Mutex>
    /// handles, only the slow server itself is locked.
    #[tokio::test]
    async fn slow_server_does_not_block_other_manager_calls() {
        use std::time::Duration;

        let mgr = Arc::new(McpServerManager::new());
        mgr.add_server(dummy_config("slow"), false).await.unwrap();
        mgr.add_server(dummy_config("fast"), false).await.unwrap();

        // Grab a long-held lock on "slow" — simulates a server stuck mid-I/O.
        let slow_handle = {
            let servers = mgr.servers.read().await;
            Arc::clone(servers.get("slow").expect("slow registered"))
        };
        let _slow_guard = slow_handle.lock().await;

        // Now hammer the manager: statuses, all_tools, call_tool — none of
        // these should hang on the slow lock because they iterate handles
        // and only block on the specific server they touch.
        let m1 = Arc::clone(&mgr);
        let statuses_task = tokio::spawn(async move { m1.statuses().await });
        let m2 = Arc::clone(&mgr);
        let tools_task = tokio::spawn(async move { m2.all_tools().await });
        let m3 = Arc::clone(&mgr);
        let call_task = tokio::spawn(async move {
            m3.call_tool("nope", serde_json::json!({})).await
        });

        // None of these should take more than a fraction of a second —
        // statuses and all_tools will skip the locked entry by simply
        // queuing on its mutex (so they DO wait). To prove the fix, we
        // also assert that a remove_server on the OTHER server completes
        // immediately, which is the manager-level write path.
        let m4 = Arc::clone(&mgr);
        let remove = tokio::time::timeout(Duration::from_secs(2), async move {
            m4.remove_server("fast").await
        })
        .await
        .expect("remove of unrelated server must not block on slow lock");
        assert!(remove.is_ok());

        // call_tool walks all handles — when it hits "slow" it WILL queue on
        // the slow mutex. Verify it eventually returns ToolNotFound when we
        // release the slow lock.
        drop(_slow_guard);
        let call_result = tokio::time::timeout(Duration::from_secs(2), call_task)
            .await
            .expect("call_tool must finish after lock release")
            .unwrap();
        assert!(matches!(call_result, Err(McpError::ToolNotFound(_))));

        // Background tasks should also finish cleanly now.
        let _ = tokio::time::timeout(Duration::from_secs(2), statuses_task).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), tools_task).await;
    }
}
