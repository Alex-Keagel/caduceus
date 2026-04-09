use async_trait::async_trait;
use caduceus_core::{CaduceusError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

// ── Tool spec ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value, // JSON Schema object
}

// ── Tool result ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_name: String,
    pub success: bool,
    pub output: Value,
    pub error: Option<String>,
}

impl ToolResult {
    pub fn ok(tool_name: impl Into<String>, output: Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            success: true,
            output,
            error: None,
        }
    }

    pub fn err(tool_name: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            success: false,
            output: Value::Null,
            error: Some(error.into()),
        }
    }
}

// ── Tool trait ─────────────────────────────────────────────────────────────────

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn call(&self, input: Value) -> Result<ToolResult>;
}

// ── Tool registry ──────────────────────────────────────────────────────────────

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name.clone(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub async fn call(&self, name: &str, input: Value) -> Result<ToolResult> {
        match self.tools.get(name) {
            Some(tool) => tool.call(input).await,
            None => Err(CaduceusError::Tool(format!("Unknown tool: {name}"))),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Built-in tool stubs ────────────────────────────────────────────────────────

macro_rules! simple_tool {
    ($name:ident, $tool_name:expr, $desc:expr, $schema:expr) => {
        pub struct $name;

        #[async_trait]
        impl Tool for $name {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: $tool_name.into(),
                    description: $desc.into(),
                    input_schema: serde_json::json!($schema),
                }
            }

            async fn call(&self, input: Value) -> Result<ToolResult> {
                todo!(concat!($tool_name, " not implemented"))
            }
        }
    };
}

simple_tool!(
    BashTool,
    "bash",
    "Execute a bash command in the workspace",
    {
        "type": "object",
        "required": ["command"],
        "properties": {
            "command": { "type": "string", "description": "Bash command to execute" },
            "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 30)" }
        }
    }
);

simple_tool!(
    ReadFileTool,
    "read_file",
    "Read the contents of a file",
    {
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": { "type": "string", "description": "File path relative to workspace root" }
        }
    }
);

simple_tool!(
    WriteFileTool,
    "write_file",
    "Write content to a file, creating it if it doesn't exist",
    {
        "type": "object",
        "required": ["path", "content"],
        "properties": {
            "path": { "type": "string" },
            "content": { "type": "string" }
        }
    }
);

simple_tool!(
    EditFileTool,
    "edit_file",
    "Replace exactly one occurrence of old_str with new_str in a file",
    {
        "type": "object",
        "required": ["path", "old_str", "new_str"],
        "properties": {
            "path": { "type": "string" },
            "old_str": { "type": "string" },
            "new_str": { "type": "string" }
        }
    }
);

simple_tool!(
    GlobSearchTool,
    "glob_search",
    "Find files matching a glob pattern",
    {
        "type": "object",
        "required": ["pattern"],
        "properties": {
            "pattern": { "type": "string", "description": "Glob pattern e.g. **/*.rs" },
            "base_dir": { "type": "string" }
        }
    }
);

simple_tool!(
    GrepSearchTool,
    "grep_search",
    "Search for a regex pattern in files",
    {
        "type": "object",
        "required": ["pattern"],
        "properties": {
            "pattern": { "type": "string" },
            "path": { "type": "string" },
            "file_glob": { "type": "string" },
            "case_insensitive": { "type": "boolean" }
        }
    }
);

simple_tool!(
    GitStatusTool,
    "git_status",
    "Get the current git status of the workspace",
    {
        "type": "object",
        "properties": {}
    }
);

simple_tool!(
    GitDiffTool,
    "git_diff",
    "Get the current git diff",
    {
        "type": "object",
        "properties": {
            "staged": { "type": "boolean", "description": "Show staged diff instead of unstaged" }
        }
    }
);

simple_tool!(
    WebFetchTool,
    "web_fetch",
    "Fetch content from a URL",
    {
        "type": "object",
        "required": ["url"],
        "properties": {
            "url": { "type": "string" },
            "max_length": { "type": "integer" }
        }
    }
);

simple_tool!(
    ListFilesTool,
    "list_files",
    "List files in a directory",
    {
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Directory path (default: workspace root)" },
            "recursive": { "type": "boolean" }
        }
    }
);

pub fn default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(BashTool));
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(EditFileTool));
    registry.register(Arc::new(GlobSearchTool));
    registry.register(Arc::new(GrepSearchTool));
    registry.register(Arc::new(GitStatusTool));
    registry.register(Arc::new(GitDiffTool));
    registry.register(Arc::new(WebFetchTool));
    registry.register(Arc::new(ListFilesTool));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let registry = default_registry();
        assert_eq!(registry.specs().len(), 10);
    }

    #[test]
    fn all_tools_have_specs() {
        let registry = default_registry();
        for spec in registry.specs() {
            assert!(!spec.name.is_empty());
            assert!(!spec.description.is_empty());
        }
    }
}
