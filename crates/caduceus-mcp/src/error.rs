use thiserror::Error;

/// Classification axis for [`McpError`] (gap G32). Lets callers decide
/// whether to retry, prompt for auth, or surface a fatal config issue
/// without `match`ing every variant.
///
/// Mapping:
/// - `Transient`  — try again later (network blips, server crashes mid-stream)
/// - `Permanent`  — won't get better by retrying (malformed JSON, contract bug)
/// - `Auth`       — credentials missing/expired; UI should prompt
/// - `Config`     — wrong server URL, missing binary, unparseable manifest
/// - `NotFound`   — addressed a server/tool that doesn't exist
/// - `Permission` — caller is authenticated but not authorised
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum McpErrorKind {
    Transient,
    Permanent,
    Auth,
    Config,
    NotFound,
    Permission,
}

impl McpErrorKind {
    /// Stable label used as the `kind` field on `caduceus.mcp.error`
    /// tracing events and on the `caduceus.mcp.error.<kind>` target so
    /// operators can filter by class without matching the message text.
    pub fn label(self) -> &'static str {
        match self {
            McpErrorKind::Transient => "transient",
            McpErrorKind::Permanent => "permanent",
            McpErrorKind::Auth => "auth",
            McpErrorKind::Config => "config",
            McpErrorKind::NotFound => "not_found",
            McpErrorKind::Permission => "permission",
        }
    }

    /// `true` iff the calling layer SHOULD apply its retry / backoff
    /// policy. Currently only `Transient` qualifies — `Auth`/`Config`
    /// require user action and `Permanent`/`NotFound`/`Permission`
    /// are by definition stable.
    pub fn is_retryable(self) -> bool {
        matches!(self, McpErrorKind::Transient)
    }
}

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

    #[error("MCP permission denied: {0}")]
    PermissionDenied(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(reqwest::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl McpError {
    /// Classify this error for retry / UX-routing decisions (gap G32).
    ///
    /// JSON-RPC code mapping follows the JSON-RPC 2.0 + MCP spec
    /// conventions:
    /// - `-32700` (parse error), `-32600` (invalid request),
    ///   `-32601` (method not found), `-32602` (invalid params)
    ///   → `Permanent` (caller bug — retrying won't help).
    /// - `-32603` (internal error), or any positive server-defined code
    ///   between `-32099..=-32000` → `Transient` (server-side hiccup).
    /// - `401`, `403` (HTTP-style codes some servers reuse) → `Auth`.
    /// - Any other code → `Permanent` (conservative; if a server has
    ///   exotic codes we don't recognise, we don't blindly retry).
    pub fn kind(&self) -> McpErrorKind {
        match self {
            McpError::NotConnected | McpError::ServerClosed => McpErrorKind::Transient,
            McpError::SpawnFailed | McpError::Config(_) => McpErrorKind::Config,
            McpError::ServerNotFound(_) | McpError::ToolNotFound(_) => McpErrorKind::NotFound,
            McpError::PermissionDenied(_) => McpErrorKind::Permission,
            McpError::EmptyResult | McpError::Serialization(_) => McpErrorKind::Permanent,
            McpError::JsonRpc { code, .. } => match *code {
                401 | 403 => McpErrorKind::Auth,
                -32603 => McpErrorKind::Transient,
                c if (-32099..=-32000).contains(&c) => McpErrorKind::Transient,
                _ => McpErrorKind::Permanent,
            },
            McpError::Io(e) => match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
                    McpErrorKind::Config
                }
                std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::Interrupted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::WouldBlock => McpErrorKind::Transient,
                _ => McpErrorKind::Permanent,
            },
            McpError::Http(e) => {
                if e.is_timeout() || e.is_connect() {
                    McpErrorKind::Transient
                } else if e
                    .status()
                    .map(|s| s.as_u16() == 401 || s.as_u16() == 403)
                    .unwrap_or(false)
                {
                    McpErrorKind::Auth
                } else if e.status().map(|s| s.is_server_error()).unwrap_or(false) {
                    McpErrorKind::Transient
                } else {
                    McpErrorKind::Permanent
                }
            }
        }
    }

    /// Convenience — true iff [`kind().is_retryable()`].
    pub fn is_retryable(&self) -> bool {
        self.kind().is_retryable()
    }

    /// Suffix label for the tracing target classifier — combine with
    /// the constant `"caduceus.mcp.error"` target plus a `kind` field
    /// when emitting events. Tracing requires a `&'static str` literal
    /// for `target:` so callers can't substitute the per-class target
    /// directly; instead they should use:
    ///
    /// ```ignore
    /// tracing::warn!(
    ///     target: "caduceus.mcp.error",
    ///     kind = err.kind().label(),
    ///     error = %err,
    ///     "..."
    /// );
    /// ```
    ///
    /// This still gives operators a single namespace to filter on
    /// (`RUST_LOG=caduceus.mcp.error=warn`) plus a structured `kind`
    /// field for downstream routing (e.g. ship `kind=auth` to a
    /// re-auth UI hook).
    pub fn tracing_target(&self) -> &'static str {
        match self.kind() {
            McpErrorKind::Transient => "caduceus.mcp.error.transient",
            McpErrorKind::Permanent => "caduceus.mcp.error.permanent",
            McpErrorKind::Auth => "caduceus.mcp.error.auth",
            McpErrorKind::Config => "caduceus.mcp.error.config",
            McpErrorKind::NotFound => "caduceus.mcp.error.not_found",
            McpErrorKind::Permission => "caduceus.mcp.error.permission",
        }
    }
}

// Manual impl for reqwest::Error since it doesn't impl std::error::Error in all configs
impl From<reqwest::Error> for McpError {
    fn from(e: reqwest::Error) -> Self {
        McpError::Http(e)
    }
}

pub type Result<T> = std::result::Result<T, McpError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_transient_on_disconnect() {
        assert_eq!(McpError::NotConnected.kind(), McpErrorKind::Transient);
        assert_eq!(McpError::ServerClosed.kind(), McpErrorKind::Transient);
        assert!(McpError::ServerClosed.is_retryable());
    }

    #[test]
    fn classifies_jsonrpc_codes_correctly() {
        let parse = McpError::JsonRpc {
            code: -32700,
            message: "x".into(),
        };
        assert_eq!(parse.kind(), McpErrorKind::Permanent);
        assert!(!parse.is_retryable());

        let internal = McpError::JsonRpc {
            code: -32603,
            message: "x".into(),
        };
        assert_eq!(internal.kind(), McpErrorKind::Transient);

        let server_defined = McpError::JsonRpc {
            code: -32050,
            message: "x".into(),
        };
        assert_eq!(server_defined.kind(), McpErrorKind::Transient);

        let auth = McpError::JsonRpc {
            code: 401,
            message: "x".into(),
        };
        assert_eq!(auth.kind(), McpErrorKind::Auth);
    }

    #[test]
    fn classifies_io_errors() {
        let timed_out = McpError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert_eq!(timed_out.kind(), McpErrorKind::Transient);
        let not_found = McpError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(not_found.kind(), McpErrorKind::Config);
    }

    #[test]
    fn classifies_lookup_misses_as_not_found() {
        assert_eq!(
            McpError::ServerNotFound("s".into()).kind(),
            McpErrorKind::NotFound
        );
        assert_eq!(
            McpError::ToolNotFound("t".into()).kind(),
            McpErrorKind::NotFound
        );
    }

    #[test]
    fn permission_distinct_from_auth() {
        assert_eq!(
            McpError::PermissionDenied("x".into()).kind(),
            McpErrorKind::Permission
        );
    }

    #[test]
    fn tracing_target_uses_caduceus_mcp_namespace() {
        let err = McpError::ServerClosed;
        assert!(err.tracing_target().starts_with("caduceus.mcp.error."));
        // Sanity — every variant has a distinct label.
        let labels: std::collections::HashSet<_> = [
            McpErrorKind::Transient,
            McpErrorKind::Permanent,
            McpErrorKind::Auth,
            McpErrorKind::Config,
            McpErrorKind::NotFound,
            McpErrorKind::Permission,
        ]
        .iter()
        .map(|k| k.label())
        .collect();
        assert_eq!(labels.len(), 6);
    }
}
