use crate::error::{McpError, Result};
use crate::types::{McpResource, McpServerConfig, McpToolDef, McpTransport, ServerStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, instrument, warn};

// ── JSON-RPC 2.0 ───────────────────────────────────────────────────────────────

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: next_id(),
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn into_result(self) -> Result<Value> {
        if let Some(err) = self.error {
            return Err(McpError::JsonRpc {
                code: err.code,
                message: err.message,
            });
        }
        self.result.ok_or(McpError::EmptyResult)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

// ── Transport layer ────────────────────────────────────────────────────────────

enum Transport {
    Stdio(StdioTransport),
    Http(HttpTransport),
}

impl Transport {
    async fn send(&mut self, req: &JsonRpcRequest) -> Result<JsonRpcResponse> {
        match self {
            Transport::Stdio(t) => t.send(req).await,
            Transport::Http(t) => t.send(req).await,
        }
    }
}

struct StdioTransport {
    stdin: tokio::process::ChildStdin,
    stdout_lines: Arc<Mutex<tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>>>,
}

impl StdioTransport {
    async fn send(&mut self, req: &JsonRpcRequest) -> Result<JsonRpcResponse> {
        use tokio::io::AsyncWriteExt;

        let payload = serde_json::to_string(req)?;
        debug!(method = %req.method, id = req.id, "→ stdio");

        self.stdin
            .write_all(format!("{}\n", payload).as_bytes())
            .await
            .map_err(McpError::Io)?;
        self.stdin.flush().await.map_err(McpError::Io)?;

        let mut guard = self.stdout_lines.lock().await;
        loop {
            let line = guard
                .next_line()
                .await
                .map_err(McpError::Io)?
                .ok_or(McpError::ServerClosed)?;

            if line.trim().is_empty() {
                continue;
            }

            let resp: JsonRpcResponse = serde_json::from_str(&line)?;
            if resp.id == Some(req.id) {
                return Ok(resp);
            }
            // Notification or out-of-order — skip
            warn!(got_id = ?resp.id, want_id = req.id, "out-of-order JSON-RPC response, skipping");
        }
    }
}

struct HttpTransport {
    url: String,
    client: reqwest::Client,
}

impl HttpTransport {
    async fn send(&mut self, req: &JsonRpcRequest) -> Result<JsonRpcResponse> {
        debug!(method = %req.method, id = req.id, url = %self.url, "→ HTTP");
        let resp = self
            .client
            .post(&self.url)
            .json(req)
            .send()
            .await
            .map_err(McpError::Http)?
            .error_for_status()
            .map_err(McpError::Http)?
            .json::<JsonRpcResponse>()
            .await
            .map_err(McpError::Http)?;
        Ok(resp)
    }
}

// ── MCP Client ─────────────────────────────────────────────────────────────────

pub struct McpClient {
    pub config: McpServerConfig,
    pub status: ServerStatus,
    transport: Option<Transport>,
    _child: Option<tokio::process::Child>,
}

impl McpClient {
    /// Create a new client from config (not yet connected).
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            status: ServerStatus::Stopped,
            transport: None,
            _child: None,
        }
    }

    /// Establish the connection (spawn process or open HTTP session).
    #[instrument(skip(self), fields(server_id = %self.config.id))]
    pub async fn connect(&mut self) -> Result<()> {
        self.status = ServerStatus::Starting;

        match &self.config.transport {
            McpTransport::Stdio { command, args, env } => {
                use tokio::io::AsyncBufReadExt;
                use tokio::process::Command;

                let mut cmd = Command::new(command);
                cmd.args(args)
                    .envs(env)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);

                let mut child = cmd.spawn().map_err(McpError::Io)?;

                let stdin = child.stdin.take().ok_or(McpError::SpawnFailed)?;
                let stdout = child.stdout.take().ok_or(McpError::SpawnFailed)?;

                let reader = tokio::io::BufReader::new(stdout);
                let lines = Arc::new(Mutex::new(reader.lines()));

                self.transport = Some(Transport::Stdio(StdioTransport {
                    stdin,
                    stdout_lines: lines,
                }));
                self._child = Some(child);

                // MCP initialize handshake
                self.initialize().await?;
            }
            McpTransport::Http { url, headers } => {
                let mut builder = reqwest::Client::builder();
                let mut header_map = reqwest::header::HeaderMap::new();
                for (k, v) in headers {
                    let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                        .map_err(|e| McpError::Config(e.to_string()))?;
                    let val = reqwest::header::HeaderValue::from_str(v)
                        .map_err(|e| McpError::Config(e.to_string()))?;
                    header_map.insert(name, val);
                }
                builder = builder.default_headers(header_map);
                let client = builder.build().map_err(McpError::Http)?;

                self.transport = Some(Transport::Http(HttpTransport {
                    url: url.clone(),
                    client,
                }));

                self.initialize().await?;
            }
        }

        self.status = ServerStatus::Running;
        Ok(())
    }

    async fn initialize(&mut self) -> Result<()> {
        let req = JsonRpcRequest::new(
            "initialize",
            Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "caduceus",
                    "version": "0.1.0"
                }
            })),
        );
        let resp = self.rpc(req).await?;
        debug!(?resp, "MCP initialize response");

        // Send initialized notification
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        self.notify(notif).await?;
        Ok(())
    }

    async fn notify(&mut self, notif: serde_json::Value) -> Result<()> {
        match self.transport.as_mut() {
            Some(Transport::Stdio(t)) => {
                use tokio::io::AsyncWriteExt;
                let payload = serde_json::to_string(&notif)?;
                t.stdin
                    .write_all(format!("{}\n", payload).as_bytes())
                    .await
                    .map_err(McpError::Io)?;
                t.stdin.flush().await.map_err(McpError::Io)?;
            }
            Some(Transport::Http(_)) => {
                // HTTP servers handle initialization differently; notifications are no-ops
            }
            None => return Err(McpError::NotConnected),
        }
        Ok(())
    }

    async fn rpc(&mut self, req: JsonRpcRequest) -> Result<Value> {
        let transport = self.transport.as_mut().ok_or(McpError::NotConnected)?;
        let resp = transport.send(&req).await?;
        resp.into_result()
    }

    /// List all tools exposed by this server.
    pub async fn list_tools(&mut self) -> Result<Vec<McpToolDef>> {
        let req = JsonRpcRequest::new("tools/list", None);
        let result = self.rpc(req).await?;
        let tools: Vec<McpToolDef> = serde_json::from_value(result["tools"].clone())?;
        Ok(tools)
    }

    /// Call a tool on this server.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let req = JsonRpcRequest::new(
            "tools/call",
            Some(serde_json::json!({
                "name": name,
                "arguments": arguments,
            })),
        );
        self.rpc(req).await
    }

    /// List all resources exposed by this server.
    pub async fn list_resources(&mut self) -> Result<Vec<McpResource>> {
        let req = JsonRpcRequest::new("resources/list", None);
        let result = self.rpc(req).await?;
        let resources: Vec<McpResource> = serde_json::from_value(result["resources"].clone())?;
        Ok(resources)
    }

    /// Read a specific resource by URI.
    pub async fn read_resource(&mut self, uri: &str) -> Result<Value> {
        let req = JsonRpcRequest::new("resources/read", Some(serde_json::json!({ "uri": uri })));
        self.rpc(req).await
    }

    pub fn is_running(&self) -> bool {
        self.status == ServerStatus::Running
    }

    /// Gracefully shut down the server.
    pub async fn shutdown(&mut self) {
        if let Some(Transport::Stdio(t)) = self.transport.as_mut() {
            // Send shutdown notification (best-effort)
            use tokio::io::AsyncWriteExt;
            let _ = t
                .stdin
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"shutdown\"}\n")
                .await;
            let _ = t.stdin.flush().await;
        }
        self.status = ServerStatus::Stopped;
        self.transport = None;
        if let Some(mut child) = self._child.take() {
            let _ = child.kill().await;
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rpc_request_serializes() {
        let req = JsonRpcRequest::new("tools/list", None);
        let json = serde_json::to_string(&req).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["method"], "tools/list");
        assert!(v["id"].is_number());
    }

    #[test]
    fn json_rpc_request_with_params() {
        let req = JsonRpcRequest::new(
            "tools/call",
            Some(serde_json::json!({
                "name": "read_file",
                "arguments": { "path": "/tmp/test.txt" }
            })),
        );
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["params"]["name"], "read_file");
    }

    #[test]
    fn json_rpc_response_ok_extracts_result() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "tools": [] }
        }"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        let result = resp.into_result().unwrap();
        assert_eq!(result["tools"], serde_json::json!([]));
    }

    #[test]
    fn json_rpc_response_error_becomes_err() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32601, "message": "Method not found" }
        }"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        let err = resp.into_result().unwrap_err();
        assert!(matches!(err, McpError::JsonRpc { code: -32601, .. }));
    }

    #[test]
    fn json_rpc_ids_are_monotonically_increasing() {
        let a = JsonRpcRequest::new("a", None);
        let b = JsonRpcRequest::new("b", None);
        assert!(b.id > a.id);
    }

    #[test]
    fn client_starts_stopped() {
        use std::collections::HashMap;
        let cfg = McpServerConfig {
            id: "test".into(),
            name: "Test".into(),
            description: None,
            transport: McpTransport::Stdio {
                command: "echo".into(),
                args: vec![],
                env: HashMap::new(),
            },
            auto_start: false,
        };
        let client = McpClient::new(cfg);
        assert_eq!(client.status, ServerStatus::Stopped);
        assert!(!client.is_running());
    }
}
