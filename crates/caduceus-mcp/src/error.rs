use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("Not connected to MCP server")]
    NotConnected,

    #[error("MCP server process failed to spawn")]
    SpawnFailed,

    #[error("MCP server closed the connection")]
    ServerClosed,

    #[error("MCP server not found: {0}")]
    ServerNotFound(String),

    #[error("MCP tool not found: {0}")]
    ToolNotFound(String),

    #[error("JSON-RPC error {code}: {message}")]
    JsonRpc { code: i64, message: String },

    #[error("Empty result from MCP server")]
    EmptyResult,

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(reqwest::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

// Manual impl for reqwest::Error since it doesn't impl std::error::Error in all configs
impl From<reqwest::Error> for McpError {
    fn from(e: reqwest::Error) -> Self {
        McpError::Http(e)
    }
}

pub type Result<T> = std::result::Result<T, McpError>;
