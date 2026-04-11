use async_trait::async_trait;
use caduceus_core::{CaduceusError, Result, ToolResult, ToolSpec};
use caduceus_runtime::{BashSandbox, ExecRequest, FileOps};
use glob::glob;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn call(&self, input: Value) -> Result<ToolResult>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name.clone(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn list_specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<_> = self.tools.values().map(|tool| tool.spec()).collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.list_specs()
    }

    pub async fn execute(&self, name: &str, input: Value) -> Result<ToolResult> {
        let Some(tool) = self.tools.get(name) else {
            return Err(CaduceusError::Tool {
                tool: name.to_string(),
                message: format!("Unknown tool: {name}"),
            });
        };

        tool.call(input).await
    }

    pub async fn call(&self, name: &str, input: Value) -> Result<ToolResult> {
        self.execute(name, input).await
    }

    pub async fn execute_parallel(&self, tools: Vec<(String, Value)>) -> Vec<Result<ToolResult>> {
        self.execute_parallel_with_limit(tools, 4).await
    }

    pub async fn execute_parallel_with_limit(
        &self,
        tools: Vec<(String, Value)>,
        concurrency_limit: usize,
    ) -> Vec<Result<ToolResult>> {
        let limit = concurrency_limit.max(1);
        let semaphore = Arc::new(Semaphore::new(limit));
        let mut join_set = JoinSet::new();

        for (idx, (name, input)) in tools.into_iter().enumerate() {
            let tool = self.get(&name);
            let semaphore = semaphore.clone();
            join_set.spawn(async move {
                let permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|err| CaduceusError::Tool {
                        tool: name.clone(),
                        message: format!("failed to acquire parallel execution permit: {err}"),
                    });

                let result = match permit {
                    Ok(_permit) => match tool {
                        Some(tool) => tool.call(input).await,
                        None => Err(CaduceusError::Tool {
                            tool: name.clone(),
                            message: format!("Unknown tool: {name}"),
                        }),
                    },
                    Err(err) => Err(err),
                };
                (idx, result)
            });
        }

        let total = join_set.len();
        let mut results: Vec<Option<Result<ToolResult>>> =
            std::iter::repeat_with(|| None).take(total).collect();
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok((idx, result)) => results[idx] = Some(result),
                Err(err) => {
                    let message = format!("parallel tool task failed: {err}");
                    if let Some(slot) = results.iter_mut().find(|slot| slot.is_none()) {
                        *slot = Some(Err(CaduceusError::Tool {
                            tool: "parallel".into(),
                            message,
                        }));
                    }
                }
            }
        }

        results
            .into_iter()
            .map(|result| {
                result.unwrap_or_else(|| {
                    Err(CaduceusError::Tool {
                        tool: "parallel".into(),
                        message: "parallel execution result missing".into(),
                    })
                })
            })
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn canonical_or_self(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

fn validate_input_object(input: Value) -> std::result::Result<Map<String, Value>, String> {
    match input {
        Value::Object(map) => Ok(map),
        _ => Err("input must be a JSON object".to_string()),
    }
}

fn get_required_string(map: &Map<String, Value>, key: &str) -> std::result::Result<String, String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing or invalid '{key}'"))
}

fn get_optional_string(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn json_result(value: Value) -> ToolResult {
    match serde_json::to_string_pretty(&value) {
        Ok(content) => ToolResult::success(content),
        Err(err) => ToolResult::error(format!("failed to serialize tool output: {err}")),
    }
}

fn tool_error(message: impl Into<String>) -> ToolResult {
    ToolResult::error(message.into())
}

fn resolve_workspace_path(workspace_root: &Path, path: &str) -> Result<PathBuf> {
    let root = canonical_or_self(workspace_root.to_path_buf());
    let raw = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };

    let normalized = normalize_path(&raw);
    if !normalized.starts_with(&root) {
        return Err(CaduceusError::PermissionDenied {
            capability: "fs".to_string(),
            tool: "path escapes workspace".to_string(),
        });
    }

    Ok(normalized)
}

#[derive(Debug, Clone)]
pub struct BashTool {
    workspace_root: PathBuf,
}

impl BashTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct BashInput {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Execute a bash command in the workspace".into(),
            input_schema: json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "cwd": {"type": "string"},
                    "env": {"type": "object", "additionalProperties": {"type": "string"}},
                    "timeout_secs": {"type": "integer", "minimum": 1}
                },
                "additionalProperties": false
            }),
            required_capability: Some("process_exec".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: BashInput = match serde_json::from_value::<BashInput>(input) {
            Ok(v) if !v.command.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'command' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };

        let cwd = match parsed.cwd {
            Some(cwd) => match resolve_workspace_path(&self.workspace_root, &cwd) {
                Ok(path) => Some(path.to_string_lossy().to_string()),
                Err(err) => return Ok(tool_error(err.to_string())),
            },
            None => Some(self.workspace_root.to_string_lossy().to_string()),
        };

        let command = if parsed.args.is_empty() {
            parsed.command
        } else {
            format!("{} {}", parsed.command, parsed.args.join(" "))
        };

        let request = ExecRequest {
            command,
            args: parsed.args,
            cwd,
            env: parsed.env,
            timeout_secs: parsed.timeout_secs,
        };

        let sandbox = BashSandbox::new(&self.workspace_root);
        let exec = match sandbox.execute(request).await {
            Ok(result) => result,
            Err(err) => return Ok(tool_error(err.to_string())),
        };

        Ok(json_result(json!({
            "stdout": exec.stdout,
            "stderr": exec.stderr,
            "exit_code": exec.exit_code,
            "timed_out": exec.timed_out
        })))
    }
}

#[derive(Debug, Clone)]
pub struct ReadFileTool {
    workspace_root: PathBuf,
}

impl ReadFileTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReadFileInput {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a text file from the workspace".into(),
            input_schema: json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {"type": "string"},
                    "offset": {"type": "integer", "minimum": 0},
                    "limit": {"type": "integer", "minimum": 1}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_read".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: ReadFileInput = match serde_json::from_value::<ReadFileInput>(input) {
            Ok(v) if !v.path.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'path' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };

        let file_ops = FileOps::new(&self.workspace_root);
        let content = match file_ops.read(&parsed.path).await {
            Ok(content) => content,
            Err(err) => return Ok(tool_error(err.to_string())),
        };

        let all_lines: Vec<&str> = content.lines().collect();
        let offset = parsed.offset.unwrap_or(0);
        if offset > all_lines.len() {
            return Ok(tool_error(format!(
                "offset {} exceeds line count {}",
                offset,
                all_lines.len()
            )));
        }

        let end = parsed
            .limit
            .map(|limit| offset.saturating_add(limit).min(all_lines.len()))
            .unwrap_or(all_lines.len());
        let selected = all_lines[offset..end].join("\n");

        Ok(json_result(json!({
            "path": parsed.path,
            "content": selected,
            "start_line": offset + 1,
            "line_count": end.saturating_sub(offset),
            "total_lines": all_lines.len()
        })))
    }
}

#[derive(Debug, Clone)]
pub struct WriteFileTool {
    workspace_root: PathBuf,
}

impl WriteFileTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct WriteFileInput {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "Write content to a file, creating it if missing".into(),
            input_schema: json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_write".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: WriteFileInput = match serde_json::from_value::<WriteFileInput>(input) {
            Ok(v) if !v.path.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'path' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };

        let resolved = match resolve_workspace_path(&self.workspace_root, &parsed.path) {
            Ok(path) => path,
            Err(err) => return Ok(tool_error(err.to_string())),
        };
        let existed = resolved.exists();

        let file_ops = FileOps::new(&self.workspace_root);
        if let Err(err) = file_ops.write(&parsed.path, &parsed.content).await {
            return Ok(tool_error(err.to_string()));
        }

        Ok(json_result(json!({
            "path": parsed.path,
            "created": !existed,
            "bytes": parsed.content.len()
        })))
    }
}

#[derive(Debug, Clone)]
pub struct EditFileTool {
    workspace_root: PathBuf,
}

impl EditFileTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file".into(),
            description: "Replace exactly one occurrence of text in a file".into(),
            input_schema: json!({
                "type": "object",
                "required": ["path", "old_str", "new_str"],
                "properties": {
                    "path": {"type": "string"},
                    "old_str": {"type": "string"},
                    "new_str": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_write".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let map = match validate_input_object(input) {
            Ok(map) => map,
            Err(err) => return Ok(tool_error(err)),
        };

        let path = match get_required_string(&map, "path") {
            Ok(path) => path,
            Err(err) => return Ok(tool_error(err)),
        };

        let old_str = get_optional_string(&map, "old_str")
            .or_else(|| get_optional_string(&map, "old_string"));
        let new_str = get_optional_string(&map, "new_str")
            .or_else(|| get_optional_string(&map, "new_string"));

        let (Some(old_str), Some(new_str)) = (old_str, new_str) else {
            return Ok(tool_error(
                "missing required 'old_str'/'new_str' (or old_string/new_string)",
            ));
        };

        let file_ops = FileOps::new(&self.workspace_root);
        let replacements = match file_ops.edit(&path, &old_str, &new_str).await {
            Ok(count) => count,
            Err(err) => return Ok(tool_error(err.to_string())),
        };

        Ok(json_result(json!({
            "path": path,
            "replacements": replacements
        })))
    }
}

#[derive(Debug, Clone)]
pub struct ApplyPatchTool {
    workspace_root: PathBuf,
}

impl ApplyPatchTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ApplyPatchInput {
    patch: String,
}

#[derive(Debug, Clone)]
struct ParsedPatch {
    files: Vec<PatchFile>,
}

#[derive(Debug, Clone)]
struct PatchFile {
    old_path: Option<String>,
    new_path: Option<String>,
    hunks: Vec<PatchHunk>,
}

#[derive(Debug, Clone)]
struct PatchHunk {
    old_start: usize,
    lines: Vec<PatchLine>,
}

#[derive(Debug, Clone)]
enum PatchLine {
    Context(String),
    Add(String),
    Remove(String),
}

#[derive(Debug, Clone)]
struct PendingWrite {
    path: PathBuf,
    content: Option<String>,
}

fn parse_patch_path(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed == "/dev/null" {
        return None;
    }
    let trimmed = trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .unwrap_or(trimmed);
    Some(trimmed.to_string())
}

fn parse_hunk_header(line: &str) -> std::result::Result<(usize, usize), String> {
    let Some(rest) = line.strip_prefix("@@ -") else {
        return Err(format!("invalid hunk header: {line}"));
    };
    let Some((old_range, remainder)) = rest.split_once(" +") else {
        return Err(format!("invalid hunk header: {line}"));
    };
    let Some((new_range, _)) = remainder.split_once(" @@") else {
        return Err(format!("invalid hunk header: {line}"));
    };
    let old_start = old_range
        .split(',')
        .next()
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|err| format!("invalid old hunk start in `{line}`: {err}"))?;
    let _new_start = new_range
        .split(',')
        .next()
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|err| format!("invalid new hunk start in `{line}`: {err}"))?;
    Ok((old_start, _new_start))
}

fn parse_unified_diff(patch: &str) -> std::result::Result<ParsedPatch, String> {
    let mut files = Vec::new();
    let mut lines = patch.lines().peekable();

    while let Some(line) = lines.next() {
        if line.starts_with("diff --git ") || line.starts_with("index ") || line.is_empty() {
            continue;
        }
        if !line.starts_with("--- ") {
            return Err(format!("expected file header, found `{line}`"));
        }

        let old_path = parse_patch_path(line.trim_start_matches("--- ").trim());
        let new_line = lines
            .next()
            .ok_or_else(|| "patch ended before new file path".to_string())?;
        if !new_line.starts_with("+++ ") {
            return Err(format!("expected new file path, found `{new_line}`"));
        }
        let new_path = parse_patch_path(new_line.trim_start_matches("+++ ").trim());
        let mut hunks = Vec::new();

        while let Some(next) = lines.peek().copied() {
            if next.starts_with("--- ") {
                break;
            }
            if next.starts_with("diff --git ") || next.starts_with("index ") || next.is_empty() {
                lines.next();
                continue;
            }
            if !next.starts_with("@@ ") {
                return Err(format!("expected hunk header, found `{next}`"));
            }

            let header = lines.next().unwrap_or_default();
            let (old_start, _) = parse_hunk_header(header)?;
            let mut hunk_lines = Vec::new();

            while let Some(hunk_line) = lines.peek().copied() {
                if hunk_line.starts_with("@@ ")
                    || hunk_line.starts_with("--- ")
                    || hunk_line.starts_with("diff --git ")
                {
                    break;
                }
                let hunk_line = lines.next().unwrap_or_default();
                if hunk_line == r"\ No newline at end of file" {
                    continue;
                }
                let (prefix, body) = hunk_line.split_at(1);
                let parsed = match prefix {
                    " " => PatchLine::Context(body.to_string()),
                    "+" => PatchLine::Add(body.to_string()),
                    "-" => PatchLine::Remove(body.to_string()),
                    _ => return Err(format!("invalid hunk line `{hunk_line}`")),
                };
                hunk_lines.push(parsed);
            }

            hunks.push(PatchHunk {
                old_start,
                lines: hunk_lines,
            });
        }

        files.push(PatchFile {
            old_path,
            new_path,
            hunks,
        });
    }

    if files.is_empty() {
        return Err("patch did not contain any files".into());
    }

    Ok(ParsedPatch { files })
}

fn apply_patch_to_content(
    original: &str,
    hunks: &[PatchHunk],
) -> std::result::Result<String, String> {
    let original_lines: Vec<String> = if original.is_empty() {
        Vec::new()
    } else {
        original.lines().map(ToString::to_string).collect()
    };
    let mut output = Vec::new();
    let mut cursor = 0usize;

    for hunk in hunks {
        let target = hunk.old_start.saturating_sub(1);
        if target < cursor || target > original_lines.len() {
            return Err("hunk start is out of bounds".into());
        }

        while cursor < target {
            output.push(original_lines[cursor].clone());
            cursor += 1;
        }

        for line in &hunk.lines {
            match line {
                PatchLine::Context(expected) => {
                    let actual = original_lines
                        .get(cursor)
                        .ok_or_else(|| format!("missing context line `{expected}`"))?;
                    if actual != expected {
                        return Err(format!(
                            "context mismatch: expected `{expected}`, found `{actual}`"
                        ));
                    }
                    output.push(actual.clone());
                    cursor += 1;
                }
                PatchLine::Remove(expected) => {
                    let actual = original_lines
                        .get(cursor)
                        .ok_or_else(|| format!("missing removal line `{expected}`"))?;
                    if actual != expected {
                        return Err(format!(
                            "removal mismatch: expected `{expected}`, found `{actual}`"
                        ));
                    }
                    cursor += 1;
                }
                PatchLine::Add(value) => output.push(value.clone()),
            }
        }
    }

    while cursor < original_lines.len() {
        output.push(original_lines[cursor].clone());
        cursor += 1;
    }

    Ok(output.join("\n"))
}

fn apply_unified_diff(workspace_root: &Path, patch: &str) -> std::result::Result<Value, String> {
    let parsed = parse_unified_diff(patch)?;
    let mut writes = Vec::new();
    let mut files_created = 0usize;
    let mut files_deleted = 0usize;
    let mut files_updated = 0usize;
    let mut hunk_count = 0usize;

    for file in parsed.files {
        hunk_count += file.hunks.len();
        match (&file.old_path, &file.new_path) {
            (None, Some(new_path)) => {
                let path = resolve_workspace_path(workspace_root, new_path)
                    .map_err(|err| err.to_string())?;
                let content = apply_patch_to_content("", &file.hunks)?;
                writes.push(PendingWrite {
                    path,
                    content: Some(content),
                });
                files_created += 1;
            }
            (Some(old_path), None) => {
                let path = resolve_workspace_path(workspace_root, old_path)
                    .map_err(|err| err.to_string())?;
                let original = std::fs::read_to_string(&path)
                    .map_err(|err| format!("failed to read `{old_path}`: {err}"))?;
                let _ = apply_patch_to_content(&original, &file.hunks)?;
                writes.push(PendingWrite {
                    path,
                    content: None,
                });
                files_deleted += 1;
            }
            (Some(old_path), Some(new_path)) => {
                let old_resolved = resolve_workspace_path(workspace_root, old_path)
                    .map_err(|err| err.to_string())?;
                let new_resolved = resolve_workspace_path(workspace_root, new_path)
                    .map_err(|err| err.to_string())?;
                if old_resolved != new_resolved {
                    return Err("renames are not supported by apply_patch".into());
                }
                let original = std::fs::read_to_string(&old_resolved)
                    .map_err(|err| format!("failed to read `{old_path}`: {err}"))?;
                let content = apply_patch_to_content(&original, &file.hunks)?;
                writes.push(PendingWrite {
                    path: old_resolved,
                    content: Some(content),
                });
                files_updated += 1;
            }
            (None, None) => return Err("patch file must have at least one path".into()),
        }
    }

    for write in &writes {
        if let Some(parent) = write.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create parent directories: {err}"))?;
        }
        match &write.content {
            Some(content) => {
                std::fs::write(&write.path, content)
                    .map_err(|err| format!("failed to write `{}`: {err}", write.path.display()))?;
            }
            None => {
                if write.path.exists() {
                    std::fs::remove_file(&write.path).map_err(|err| {
                        format!("failed to delete `{}`: {err}", write.path.display())
                    })?;
                }
            }
        }
    }

    Ok(json!({
        "files_created": files_created,
        "files_updated": files_updated,
        "files_deleted": files_deleted,
        "hunks_applied": hunk_count,
    }))
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "apply_patch".into(),
            description: "Apply a unified diff patch to workspace files".into(),
            input_schema: json!({
                "type": "object",
                "required": ["patch"],
                "properties": {
                    "patch": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_write".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: ApplyPatchInput = match serde_json::from_value::<ApplyPatchInput>(input) {
            Ok(value) if !value.patch.trim().is_empty() => value,
            Ok(_) => return Ok(tool_error("'patch' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };

        match apply_unified_diff(&self.workspace_root, &parsed.patch) {
            Ok(summary) => Ok(json_result(summary)),
            Err(err) => Ok(tool_error(err)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GlobSearchTool {
    workspace_root: PathBuf,
}

impl GlobSearchTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct GlobSearchInput {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    base_dir: Option<String>,
}

#[async_trait]
impl Tool for GlobSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob_search".into(),
            description: "Find files matching a glob pattern".into(),
            input_schema: json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string"},
                    "base_dir": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_read".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: GlobSearchInput = match serde_json::from_value::<GlobSearchInput>(input) {
            Ok(v) if !v.pattern.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'pattern' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };

        let base = parsed
            .path
            .or(parsed.base_dir)
            .unwrap_or_else(|| ".".to_string());
        let base_path = match resolve_workspace_path(&self.workspace_root, &base) {
            Ok(path) => path,
            Err(err) => return Ok(tool_error(err.to_string())),
        };

        let search_pattern = if Path::new(&parsed.pattern).is_absolute() {
            parsed.pattern.clone()
        } else {
            base_path
                .join(&parsed.pattern)
                .to_string_lossy()
                .to_string()
        };

        let mut matches = Vec::new();
        let entries = match glob(&search_pattern) {
            Ok(paths) => paths,
            Err(err) => return Ok(tool_error(format!("invalid glob pattern: {err}"))),
        };

        for entry in entries {
            match entry {
                Ok(path) => {
                    if path.starts_with(&self.workspace_root) {
                        let display = path
                            .strip_prefix(&self.workspace_root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .to_string();
                        matches.push(display);
                    }
                }
                Err(err) => return Ok(tool_error(format!("glob iteration failed: {err}"))),
            }
        }

        matches.sort();
        Ok(json_result(json!({
            "pattern": parsed.pattern,
            "matches": matches,
            "count": matches.len()
        })))
    }
}

#[derive(Debug, Clone)]
pub struct GrepSearchTool {
    workspace_root: PathBuf,
}

impl GrepSearchTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct GrepSearchInput {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    file_glob: Option<String>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
}

#[async_trait]
impl Tool for GrepSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep_search".into(),
            description: "Search file contents with regex".into(),
            input_schema: json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string"},
                    "file_glob": {"type": "string"},
                    "glob": {"type": "string"},
                    "case_insensitive": {"type": "boolean"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_read".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: GrepSearchInput = match serde_json::from_value::<GrepSearchInput>(input) {
            Ok(v) if !v.pattern.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'pattern' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };

        let base = parsed.path.unwrap_or_else(|| ".".to_string());
        let base_path = match resolve_workspace_path(&self.workspace_root, &base) {
            Ok(path) => path,
            Err(err) => return Ok(tool_error(err.to_string())),
        };

        let file_glob = parsed
            .file_glob
            .or(parsed.glob)
            .unwrap_or_else(|| "**/*".to_string());
        let walk_pattern = base_path.join(file_glob).to_string_lossy().to_string();

        let regex = match RegexBuilder::new(&parsed.pattern)
            .case_insensitive(parsed.case_insensitive)
            .build()
        {
            Ok(regex) => regex,
            Err(err) => return Ok(tool_error(format!("invalid regex: {err}"))),
        };

        let mut results = Vec::new();
        let mut total_matches = 0usize;

        let entries = match glob(&walk_pattern) {
            Ok(paths) => paths,
            Err(err) => return Ok(tool_error(format!("invalid glob: {err}"))),
        };

        for entry in entries {
            let path = match entry {
                Ok(path) => path,
                Err(err) => return Ok(tool_error(format!("glob iteration failed: {err}"))),
            };

            if !path.is_file() {
                continue;
            }
            if !path.starts_with(&self.workspace_root) {
                continue;
            }

            let content = match tokio::fs::read_to_string(&path).await {
                Ok(content) => content,
                Err(_) => continue,
            };

            for (idx, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    total_matches += 1;
                    results.push(json!({
                        "file": path.strip_prefix(&self.workspace_root).unwrap_or(&path).to_string_lossy().to_string(),
                        "line": idx + 1,
                        "text": line
                    }));
                }
            }
        }

        Ok(json_result(json!({
            "pattern": parsed.pattern,
            "matches": results,
            "count": total_matches
        })))
    }
}

#[derive(Debug, Clone)]
pub struct ListFilesTool {
    workspace_root: PathBuf,
}

impl ListFilesTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ListFilesInput {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    recursive: bool,
}

#[async_trait]
impl Tool for ListFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_files".into(),
            description: "List files in a directory".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "recursive": {"type": "boolean"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_read".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: ListFilesInput = match serde_json::from_value::<ListFilesInput>(input) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };

        let base = parsed.path.unwrap_or_else(|| ".".to_string());
        let root = match resolve_workspace_path(&self.workspace_root, &base) {
            Ok(path) => path,
            Err(err) => return Ok(tool_error(err.to_string())),
        };

        if !root.exists() {
            return Ok(tool_error("directory does not exist"));
        }
        if !root.is_dir() {
            return Ok(tool_error("path is not a directory"));
        }

        let mut files = Vec::new();
        if parsed.recursive {
            let pattern = root.join("**/*").to_string_lossy().to_string();
            let entries = match glob(&pattern) {
                Ok(paths) => paths,
                Err(err) => return Ok(tool_error(format!("invalid traversal pattern: {err}"))),
            };

            for entry in entries {
                let path = match entry {
                    Ok(path) => path,
                    Err(err) => return Ok(tool_error(format!("glob iteration failed: {err}"))),
                };
                if path.starts_with(&self.workspace_root) {
                    files.push(
                        path.strip_prefix(&self.workspace_root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .to_string(),
                    );
                }
            }
        } else {
            let mut dir = match tokio::fs::read_dir(&root).await {
                Ok(dir) => dir,
                Err(err) => return Ok(tool_error(format!("failed to list directory: {err}"))),
            };

            loop {
                match dir.next_entry().await {
                    Ok(Some(entry)) => {
                        let path = entry.path();
                        if path.starts_with(&self.workspace_root) {
                            files.push(
                                path.strip_prefix(&self.workspace_root)
                                    .unwrap_or(&path)
                                    .to_string_lossy()
                                    .to_string(),
                            );
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        return Ok(tool_error(format!("failed to read directory entry: {err}")))
                    }
                }
            }
        }

        files.sort();
        Ok(json_result(json!({
            "path": base,
            "recursive": parsed.recursive,
            "entries": files,
            "count": files.len()
        })))
    }
}

#[derive(Debug, Clone)]
pub struct GitStatusTool {
    workspace_root: PathBuf,
}

impl GitStatusTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[async_trait]
impl Tool for GitStatusTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_status".into(),
            description: "Get git status porcelain output".into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_capability: Some("process_exec".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        if !input.is_object() {
            return Ok(tool_error("input must be a JSON object"));
        }

        let sandbox = BashSandbox::new(&self.workspace_root);
        let request = ExecRequest {
            command: "git --no-pager status --porcelain".to_string(),
            args: vec![],
            cwd: Some(self.workspace_root.to_string_lossy().to_string()),
            env: HashMap::new(),
            timeout_secs: Some(30),
        };

        match sandbox.execute(request).await {
            Ok(result) => Ok(json_result(json!({
                "stdout": result.stdout,
                "stderr": result.stderr,
                "exit_code": result.exit_code
            }))),
            Err(err) => Ok(tool_error(err.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitDiffTool {
    workspace_root: PathBuf,
}

impl GitDiffTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct GitDiffInput {
    #[serde(default)]
    staged: bool,
}

#[async_trait]
impl Tool for GitDiffTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_diff".into(),
            description: "Get git diff output".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "staged": {"type": "boolean"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("process_exec".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: GitDiffInput = match serde_json::from_value::<GitDiffInput>(input) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };

        let cmd = if parsed.staged {
            "git --no-pager diff --staged"
        } else {
            "git --no-pager diff"
        };

        let sandbox = BashSandbox::new(&self.workspace_root);
        let request = ExecRequest {
            command: cmd.to_string(),
            args: vec![],
            cwd: Some(self.workspace_root.to_string_lossy().to_string()),
            env: HashMap::new(),
            timeout_secs: Some(30),
        };

        match sandbox.execute(request).await {
            Ok(result) => Ok(json_result(json!({
                "stdout": result.stdout,
                "stderr": result.stderr,
                "exit_code": result.exit_code,
                "staged": parsed.staged
            }))),
            Err(err) => Ok(tool_error(err.to_string())),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WebFetchTool {
    timeout: Duration,
}

impl WebFetchTool {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

#[derive(Debug, Deserialize)]
struct WebFetchInput {
    url: String,
    #[serde(default)]
    max_length: Option<usize>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch URL contents using HTTP GET".into(),
            input_schema: json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": {"type": "string"},
                    "max_length": {"type": "integer", "minimum": 1},
                    "timeout_secs": {"type": "integer", "minimum": 1}
                },
                "additionalProperties": false
            }),
            required_capability: Some("network_http".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: WebFetchInput = match serde_json::from_value::<WebFetchInput>(input) {
            Ok(v) if !v.url.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'url' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };

        let timeout =
            Duration::from_secs(parsed.timeout_secs.unwrap_or(self.timeout.as_secs().max(1)));
        let client = match reqwest::Client::builder().timeout(timeout).build() {
            Ok(client) => client,
            Err(err) => return Ok(tool_error(format!("failed to create HTTP client: {err}"))),
        };

        let response = match client.get(&parsed.url).send().await {
            Ok(response) => response,
            Err(err) => return Ok(tool_error(format!("request failed: {err}"))),
        };

        let status = response.status();
        let final_url = response.url().to_string();
        let text = match response.text().await {
            Ok(text) => text,
            Err(err) => return Ok(tool_error(format!("failed reading response body: {err}"))),
        };

        let body = if let Some(max) = parsed.max_length {
            text.chars().take(max).collect::<String>()
        } else {
            text
        };

        Ok(json_result(json!({
            "url": parsed.url,
            "final_url": final_url,
            "status": status.as_u16(),
            "body": body
        })))
    }
}

pub fn default_registry_with_root(workspace_root: impl Into<PathBuf>) -> ToolRegistry {
    let workspace_root = workspace_root.into();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(BashTool::new(&workspace_root)));
    registry.register(Arc::new(ReadFileTool::new(&workspace_root)));
    registry.register(Arc::new(WriteFileTool::new(&workspace_root)));
    registry.register(Arc::new(EditFileTool::new(&workspace_root)));
    registry.register(Arc::new(ApplyPatchTool::new(&workspace_root)));
    registry.register(Arc::new(GlobSearchTool::new(&workspace_root)));
    registry.register(Arc::new(GrepSearchTool::new(&workspace_root)));
    registry.register(Arc::new(ListFilesTool::new(&workspace_root)));
    registry.register(Arc::new(GitStatusTool::new(&workspace_root)));
    registry.register(Arc::new(GitDiffTool::new(&workspace_root)));
    registry.register(Arc::new(WebFetchTool::new(Duration::from_secs(15))));
    registry
}

pub fn default_registry() -> ToolRegistry {
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    default_registry_with_root(workspace_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_workspace(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("target")
            .join("caduceus-tools-tests")
            .join(format!("{name}-{nanos}"));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn registry_lookup_and_specs() {
        let registry = default_registry_with_root(std::env::current_dir().unwrap());
        assert!(registry.get("bash").is_some());
        assert_eq!(registry.list_specs().len(), 11);
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let registry = default_registry_with_root(std::env::current_dir().unwrap());
        let err = registry.execute("missing", json!({})).await.err().unwrap();
        assert!(err.to_string().contains("Unknown tool"));
    }

    #[tokio::test]
    async fn write_and_read_file_execute() {
        let root = test_workspace("write-read");
        let registry = default_registry_with_root(&root);

        let write = registry
            .execute(
                "write_file",
                json!({"path": "nested/test.txt", "content": "line1\nline2\nline3"}),
            )
            .await
            .unwrap();
        assert!(!write.is_error);

        let read = registry
            .execute(
                "read_file",
                json!({"path": "nested/test.txt", "offset": 1, "limit": 1}),
            )
            .await
            .unwrap();
        assert!(!read.is_error);
        assert!(read.content.contains("line2"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn edit_file_error_case() {
        let root = test_workspace("edit-error");
        let registry = default_registry_with_root(&root);

        registry
            .execute("write_file", json!({"path": "a.txt", "content": "hello"}))
            .await
            .unwrap();

        let edited = registry
            .execute(
                "edit_file",
                json!({"path": "a.txt", "old_str": "missing", "new_str": "x"}),
            )
            .await
            .unwrap();
        assert!(edited.is_error);

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn glob_and_grep_search_work() {
        let root = test_workspace("glob-grep");
        let registry = default_registry_with_root(&root);

        registry
            .execute(
                "write_file",
                json!({"path": "src/main.rs", "content": "fn main() { println!(\"hello\"); }"}),
            )
            .await
            .unwrap();

        let glob_result = registry
            .execute("glob_search", json!({"pattern": "src/**/*.rs"}))
            .await
            .unwrap();
        assert!(!glob_result.is_error);
        assert!(glob_result.content.contains("src/main.rs"));

        let grep_result = registry
            .execute(
                "grep_search",
                json!({"pattern": "println", "glob": "src/**/*.rs"}),
            )
            .await
            .unwrap();
        assert!(!grep_result.is_error);
        assert!(grep_result.content.contains("println"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn list_files_and_bash_work() {
        let root = test_workspace("list-bash");
        let registry = default_registry_with_root(&root);

        registry
            .execute("write_file", json!({"path": "a/b.txt", "content": "x"}))
            .await
            .unwrap();

        let listed = registry
            .execute("list_files", json!({"path": "a", "recursive": true}))
            .await
            .unwrap();
        assert!(!listed.is_error);
        assert!(listed.content.contains("a/b.txt"));

        let bash = registry
            .execute("bash", json!({"command": "echo ok"}))
            .await
            .unwrap();
        assert!(!bash.is_error);
        assert!(bash.content.contains("ok"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn web_fetch_input_validation_error() {
        let tool = WebFetchTool::new(Duration::from_secs(1));
        let result = tool.call(json!({"url": "not-a-valid-url"})).await.unwrap();
        assert!(result.is_error);
    }

    #[derive(Debug)]
    struct SlowTool {
        active: Arc<std::sync::atomic::AtomicUsize>,
        peak: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl Tool for SlowTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "slow".into(),
                description: "slow test tool".into(),
                input_schema: json!({"type": "object"}),
                required_capability: None,
            }
        }

        async fn call(&self, _input: Value) -> Result<ToolResult> {
            let current = self
                .active
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.peak
                .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            self.active
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolResult::success("ok"))
        }
    }

    #[tokio::test]
    async fn execute_parallel_respects_concurrency_limit() {
        let mut registry = ToolRegistry::new();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        registry.register(Arc::new(SlowTool {
            active: active.clone(),
            peak: peak.clone(),
        }));

        let results = registry
            .execute_parallel_with_limit(
                vec![
                    ("slow".to_string(), json!({})),
                    ("slow".to_string(), json!({})),
                    ("slow".to_string(), json!({})),
                    ("slow".to_string(), json!({})),
                ],
                2,
            )
            .await;

        assert_eq!(results.len(), 4);
        assert!(results
            .iter()
            .all(|result| result.as_ref().is_ok_and(|tool| !tool.is_error)));
        assert!(peak.load(std::sync::atomic::Ordering::SeqCst) <= 2);
    }

    #[tokio::test]
    async fn apply_patch_tool_updates_file() {
        let root = test_workspace("apply-patch");
        std::fs::write(root.join("demo.txt"), "hello\nworld\n").unwrap();
        let tool = ApplyPatchTool::new(&root);
        let patch = concat!(
            "--- a/demo.txt\n",
            "+++ b/demo.txt\n",
            "@@ -1,2 +1,2 @@\n",
            " hello\n",
            "-world\n",
            "+caduceus\n"
        );

        let result = tool.call(json!({"patch": patch})).await.unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(
            std::fs::read_to_string(root.join("demo.txt")).unwrap(),
            "hello\ncaduceus"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn apply_patch_rejects_workspace_escape() {
        let root = test_workspace("apply-patch-escape");
        let tool = ApplyPatchTool::new(&root);
        let patch = concat!(
            "--- a/../../evil.txt\n",
            "+++ b/../../evil.txt\n",
            "@@ -0,0 +1 @@\n",
            "+bad\n"
        );

        let result = tool.call(json!({"patch": patch})).await.unwrap();
        assert!(result.is_error);

        let _ = std::fs::remove_dir_all(root);
    }
}
