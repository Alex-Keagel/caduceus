//! P11.4 — bridge between an MCP server's tool surface and the
//! orchestrator's [`caduceus_tools::Tool`] trait.
//!
//! Wraps a single MCP tool definition + an `Arc`-clonable invoker
//! callback into a `Tool` impl that:
//!
//! 1. Reports the MCP tool's `name` / `description` / `input_schema`
//!    as its `ToolSpec`.
//! 2. Implements [`caduceus_tools::Tool::resource_keys`] using
//!    [`crate::resource_keys::extract`] so the orchestrator's
//!    parallel locking layer (`execute_parallel_locked`) can serialise
//!    on the same path/uri/file the MCP call is going to touch
//!    instead of falling back to a global lock.
//! 3. Forwards `call(input)` to the invoker, which is typically a
//!    closure that owns an `Arc<McpServerManager>` and translates the
//!    MCP JSON-RPC reply into a `ToolResult`.
//!
//! The invoker is taken as a boxed async closure so the bridge stays
//! decoupled from any specific manager implementation — we don't pull
//! the whole manager into the Tool trait, which would force the
//! orchestrator to know about MCP transports.

use crate::resource_keys;
use crate::types::McpToolDef;
use caduceus_core::{Result, ToolKind, ToolResult, ToolSpec};
use caduceus_tools::Tool;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Type alias for the boxed async invoker closure. Accepts the input
/// JSON, returns the tool result. Owns whatever state it needs
/// (typically an `Arc<McpServerManager>`).
pub type McpInvoker = Arc<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'static>>
        + Send
        + Sync,
>;

/// `Tool` adapter for one MCP tool definition.
pub struct McpToolBridge {
    spec: ToolSpec,
    invoker: McpInvoker,
    /// Reported `kind` for downstream scheduler hints. Defaults to
    /// [`ToolKind::Destructive`] (most MCP tools touch external state;
    /// callers can override via [`with_kind`]).
    kind: ToolKind,
}

impl McpToolBridge {
    pub fn new(def: &McpToolDef, invoker: McpInvoker) -> Self {
        let spec = ToolSpec {
            name: def.name.clone(),
            description: def.description.clone(),
            input_schema: def.input_schema.clone(),
            required_capability: None,
        };
        Self {
            spec,
            invoker,
            kind: ToolKind::Destructive,
        }
    }

    pub fn with_kind(mut self, kind: ToolKind) -> Self {
        self.kind = kind;
        self
    }
}

#[async_trait::async_trait]
impl Tool for McpToolBridge {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn kind(&self) -> ToolKind {
        self.kind.clone()
    }

    fn resource_keys(&self, input: &Value) -> Vec<String> {
        resource_keys::extract(input)
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        (self.invoker)(input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_def() -> McpToolDef {
        McpToolDef {
            name: "echo".into(),
            description: "returns input.path as content".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }
    }

    fn echo_invoker() -> McpInvoker {
        Arc::new(|input: Value| {
            Box::pin(async move {
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(none)")
                    .to_string();
                Ok(ToolResult::success(format!("echo: {path}")))
            })
        })
    }

    #[test]
    fn p11_4_bridge_spec_mirrors_mcp_def() {
        let bridge = McpToolBridge::new(&echo_def(), echo_invoker());
        let spec = bridge.spec();
        assert_eq!(spec.name, "echo");
        assert_eq!(spec.description, "returns input.path as content");
        assert!(spec.input_schema.get("properties").is_some());
    }

    #[test]
    fn p11_4_bridge_resource_keys_extract_path_from_input() {
        let bridge = McpToolBridge::new(&echo_def(), echo_invoker());
        let input = serde_json::json!({ "path": "src/main.rs" });
        let keys = bridge.resource_keys(&input);
        assert_eq!(keys, vec!["path:src/main.rs".to_string()]);
    }

    #[test]
    fn p11_4_bridge_resource_keys_empty_when_no_recognized_field() {
        let bridge = McpToolBridge::new(&echo_def(), echo_invoker());
        let input = serde_json::json!({ "unrelated": 42 });
        assert!(bridge.resource_keys(&input).is_empty());
    }

    #[tokio::test]
    async fn p11_4_bridge_call_forwards_to_invoker() {
        let bridge = McpToolBridge::new(&echo_def(), echo_invoker());
        let res = bridge
            .call(serde_json::json!({"path": "/tmp/x"}))
            .await
            .expect("invoker ok");
        assert!(res.content.contains("/tmp/x"));
        assert!(!res.is_error);
    }

    #[test]
    fn p11_4_bridge_resource_keys_dedupe_and_prefix_collision_safe() {
        let bridge = McpToolBridge::new(&echo_def(), echo_invoker());
        let input = serde_json::json!({
            "path": "shared/key",
            "uri": "shared/key"
        });
        let keys = bridge.resource_keys(&input);
        // Distinct prefixes prevent path-vs-uri collisions even when
        // the values match — two separate keys, but each canonical.
        assert!(keys.contains(&"path:shared/key".to_string()));
        assert!(keys.contains(&"uri:shared/key".to_string()));
        assert_eq!(keys.len(), 2);
        // Sorted for deterministic acquire order.
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }
}
