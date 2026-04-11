use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Transport ──────────────────────────────────────────────────────────────────

/// How to connect to an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpTransport {
    /// Spawn a subprocess and communicate over stdin/stdout.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// Connect to an HTTP MCP server (JSON-RPC over HTTP/SSE).
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

// ── Server Config ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique identifier for this server instance.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,
    /// Transport configuration.
    pub transport: McpTransport,
    /// Whether this server should be started automatically.
    #[serde(default = "default_true")]
    pub auto_start: bool,
}

fn default_true() -> bool {
    true
}

impl McpServerConfig {
    pub fn stdio(
        id: impl Into<String>,
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: None,
            transport: McpTransport::Stdio {
                command: command.into(),
                args,
                env: HashMap::new(),
            },
            auto_start: true,
        }
    }

    pub fn http(id: impl Into<String>, name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: None,
            transport: McpTransport::Http {
                url: url.into(),
                headers: HashMap::new(),
            },
            auto_start: true,
        }
    }
}

// ── Tool Definition ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

// ── Resource ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default)]
    pub mime_type: Option<String>,
}

// ── Server Status ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerStatus {
    Starting,
    Running,
    Degraded,
    Stopped,
    Error,
}

// ── Registry Entry ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRegistryEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub publisher: Option<McpPublisher>,
    #[serde(default)]
    pub packages: Vec<McpPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPublisher {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PackageRuntime {
    Npm,
    Python,
    Go,
    Docker,
    Binary,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPackage {
    pub runtime: PackageRuntime,
    pub name: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_stdio_round_trips() {
        let t = McpTransport::Stdio {
            command: "npx".into(),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-filesystem".into(),
            ],
            env: HashMap::new(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: McpTransport = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn transport_http_round_trips() {
        let t = McpTransport::Http {
            url: "http://localhost:3000".into(),
            headers: HashMap::new(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: McpTransport = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn server_config_stdio_constructor() {
        let cfg = McpServerConfig::stdio(
            "fs",
            "Filesystem",
            "npx",
            vec![
                "-y".into(),
                "@modelcontextprotocol/server-filesystem".into(),
            ],
        );
        assert_eq!(cfg.id, "fs");
        assert!(cfg.auto_start);
        assert!(matches!(cfg.transport, McpTransport::Stdio { .. }));
    }

    #[test]
    fn server_config_http_constructor() {
        let cfg = McpServerConfig::http("remote", "Remote", "http://example.com/mcp");
        assert!(matches!(cfg.transport, McpTransport::Http { .. }));
    }

    #[test]
    fn tool_def_parses_schema() {
        let json = r#"{
            "name": "read_file",
            "description": "Read a file from the filesystem",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }
        }"#;
        let tool: McpToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.input_schema["type"], "object");
    }

    #[test]
    fn resource_parses_with_optional_fields() {
        let json = r#"{
            "uri": "file:///home/user/doc.txt",
            "name": "doc.txt"
        }"#;
        let res: McpResource = serde_json::from_str(json).unwrap();
        assert_eq!(res.uri, "file:///home/user/doc.txt");
        assert!(res.description.is_none());
        assert!(res.mime_type.is_none());
    }

    #[test]
    fn registry_entry_parses() {
        let json = r#"{
            "id": "abc123",
            "name": "filesystem",
            "description": "Filesystem MCP server",
            "version": "0.5.0",
            "packages": [
                {
                    "runtime": "npm",
                    "name": "@modelcontextprotocol/server-filesystem",
                    "args": ["/tmp"]
                }
            ]
        }"#;
        let entry: McpRegistryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, "abc123");
        assert_eq!(entry.packages.len(), 1);
        assert_eq!(entry.packages[0].runtime, PackageRuntime::Npm);
    }
}
