//! Agent Execution Tree Visualizer (#234).
//!
//! Produces Mermaid / React-Flow JSON views of the agent's execution tree.
//! Extracted from `lib.rs` — see ST-B1 Wave 0c.

use serde::{Deserialize, Serialize};

// ── #234: Agent Execution Tree Visualizer ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VizTreeNode {
    pub id: String,
    pub label: String,
    /// One of: "active", "succeeded", "failed", "pruned"
    pub status: String,
    pub parent: Option<String>,
    pub error: Option<String>,
    pub depth: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionTreeViz {
    pub nodes: Vec<VizTreeNode>,
}

impl ExecutionTreeViz {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, node: VizTreeNode) {
        self.nodes.push(node);
    }

    pub fn node_color(status: &str) -> &'static str {
        match status {
            "active" => "#f59e0b",    // amber / yellow
            "succeeded" => "#10b981", // green
            "failed" => "#ef4444",    // red
            "pruned" => "#6b7280",    // gray
            _ => "#6b7280",
        }
    }

    /// Emit React Flow nodes + edges JSON.
    pub fn to_react_flow_json(&self) -> serde_json::Value {
        let rf_nodes: Vec<serde_json::Value> = self
            .nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "type": "default",
                    "data": {
                        "label": n.label,
                        "status": n.status,
                        "error": n.error,
                    },
                    "style": {
                        "background": Self::node_color(&n.status),
                        "color": "#fff",
                        "borderRadius": "8px",
                    },
                    "position": {
                        "x": (n.depth as f64) * 200.0,
                        "y": 0.0,  // caller is responsible for layout
                    }
                })
            })
            .collect();

        let rf_edges: Vec<serde_json::Value> = self
            .nodes
            .iter()
            .filter_map(|n| {
                n.parent.as_ref().map(|p| {
                    serde_json::json!({
                        "id": format!("{}->{}", p, n.id),
                        "source": p,
                        "target": n.id,
                        "type": "smoothstep",
                    })
                })
            })
            .collect();

        serde_json::json!({ "nodes": rf_nodes, "edges": rf_edges })
    }

    /// Emit Mermaid `graph TD` flowchart syntax.
    pub fn to_mermaid(&self) -> String {
        let mut out = String::from("graph TD\n");
        for node in &self.nodes {
            let safe_label = node.label.replace('"', "'");
            out.push_str(&format!("    {}[\"{}\"]\n", node.id, safe_label));
            let color = match node.status.as_str() {
                "succeeded" => "fill:#10b981,color:#fff",
                "failed" => "fill:#ef4444,color:#fff",
                "active" => "fill:#f59e0b,color:#fff",
                _ => "fill:#6b7280,color:#fff",
            };
            out.push_str(&format!("    style {} {}\n", node.id, color));
            if let Some(parent) = &node.parent {
                out.push_str(&format!("    {} --> {}\n", parent, node.id));
            }
        }
        out
    }
}
