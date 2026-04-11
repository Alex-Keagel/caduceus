use async_trait::async_trait;
use caduceus_core::{CaduceusError, Result, ToolResult, ToolSpec};
use caduceus_runtime::{BashSandbox, ExecRequest, FileOps};
use glob::glob;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};
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

    if normalized.exists() {
        let canonical = std::fs::canonicalize(&normalized).map_err(CaduceusError::Io)?;
        if !canonical.starts_with(&root) {
            return Err(CaduceusError::PermissionDenied {
                capability: "fs".to_string(),
                tool: "path escapes workspace".to_string(),
            });
        }
        return Ok(canonical);
    }

    let parent = normalized.parent().unwrap_or(&normalized);
    if parent.exists() {
        let canonical_parent = std::fs::canonicalize(parent).map_err(CaduceusError::Io)?;
        if !canonical_parent.starts_with(&root) {
            return Err(CaduceusError::PermissionDenied {
                capability: "fs".to_string(),
                tool: "path escapes workspace".to_string(),
            });
        }
    } else if !normalized.starts_with(&root) {
        return Err(CaduceusError::PermissionDenied {
            capability: "fs".to_string(),
            tool: "path escapes workspace".to_string(),
        });
    }

    Ok(normalized)
}

fn secure_write_path(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(CaduceusError::Io)?;
    }

    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(CaduceusError::Io)?;
        file.write_all(content.as_bytes())
            .map_err(CaduceusError::Io)?;
        file.flush().map_err(CaduceusError::Io)?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, content).map_err(CaduceusError::Io)?;
        Ok(())
    }
}

fn is_metadata_hostname(host: &str) -> bool {
    matches!(
        host,
        "metadata.google.internal"
            | "metadata"
            | "metadata.azure.internal"
            | "instance-data"
            | "100.100.100.200"
    )
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || ip.is_multicast()
                || octets[0] == 0
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
                || octets == [100, 100, 100, 200]
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    }
}

async fn validate_web_fetch_url(url: &reqwest::Url) -> Result<()> {
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(CaduceusError::Tool {
                tool: "web_fetch".into(),
                message: format!("unsupported URL scheme `{other}`"),
            });
        }
    }

    let host = url.host_str().ok_or_else(|| CaduceusError::Tool {
        tool: "web_fetch".into(),
        message: "URL must include a host".into(),
    })?;
    let host_lower = host.to_ascii_lowercase();
    if host_lower == "localhost"
        || host_lower.ends_with(".localhost")
        || host_lower.ends_with(".local")
    {
        return Err(CaduceusError::Tool {
            tool: "web_fetch".into(),
            message: "requests to localhost or local domains are blocked".into(),
        });
    }
    if is_metadata_hostname(&host_lower) {
        return Err(CaduceusError::Tool {
            tool: "web_fetch".into(),
            message: "requests to metadata endpoints are blocked".into(),
        });
    }

    if let Ok(ip) = host_lower.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(CaduceusError::Tool {
                tool: "web_fetch".into(),
                message: "requests to private or local IP addresses are blocked".into(),
            });
        }
        return Ok(());
    }

    let port = url
        .port_or_known_default()
        .ok_or_else(|| CaduceusError::Tool {
            tool: "web_fetch".into(),
            message: "unable to determine target port".into(),
        })?;

    let resolved =
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|err| CaduceusError::Tool {
                tool: "web_fetch".into(),
                message: format!("failed to resolve host `{host}`: {err}"),
            })?;

    for addr in resolved {
        if is_blocked_ip(addr.ip()) {
            return Err(CaduceusError::Tool {
                tool: "web_fetch".into(),
                message: "requests to private or local network addresses are blocked".into(),
            });
        }
    }

    Ok(())
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

        let request = ExecRequest {
            command: parsed.command,
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
                secure_write_path(&write.path, content)
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
        let client = match reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(client) => client,
            Err(err) => return Ok(tool_error(format!("failed to create HTTP client: {err}"))),
        };
        let mut current_url = match reqwest::Url::parse(&parsed.url) {
            Ok(url) => url,
            Err(err) => return Ok(tool_error(format!("invalid URL: {err}"))),
        };
        if let Err(err) = validate_web_fetch_url(&current_url).await {
            return Ok(tool_error(err.to_string()));
        }

        let mut redirect_count = 0usize;
        let response = loop {
            let response = match client.get(current_url.clone()).send().await {
                Ok(response) => response,
                Err(err) => return Ok(tool_error(format!("request failed: {err}"))),
            };

            if response.status().is_redirection() {
                redirect_count += 1;
                if redirect_count > 5 {
                    return Ok(tool_error("too many redirects"));
                }

                let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                    return Ok(tool_error("redirect response missing Location header"));
                };
                let location = match location.to_str() {
                    Ok(value) => value,
                    Err(err) => return Ok(tool_error(format!("invalid redirect location: {err}"))),
                };
                let next_url = match current_url.join(location) {
                    Ok(url) => url,
                    Err(err) => return Ok(tool_error(format!("invalid redirect target: {err}"))),
                };
                if let Err(err) = validate_web_fetch_url(&next_url).await {
                    return Ok(tool_error(err.to_string()));
                }
                current_url = next_url;
                continue;
            }

            break response;
        };

        let status = response.status();
        let final_url = current_url.to_string();
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

// ── BrowserActionTool ──────────────────────────────────────────────────────────

pub struct BrowserActionTool {
    headless: bool,
}

impl BrowserActionTool {
    pub fn new(headless: bool) -> Self {
        Self { headless }
    }
}

#[async_trait]
impl Tool for BrowserActionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser_action".into(),
            description: "Control a browser via Playwright CLI. Supports navigate, click, type, screenshot, get_text, wait_for, scroll, get_console actions.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["action_type"],
                "properties": {
                    "action_type": {
                        "type": "string",
                        "enum": ["Navigate","Click","Type","Screenshot","GetText","WaitFor","Scroll","GetConsole"]
                    },
                    "url": {"type": "string"},
                    "selector": {"type": "string"},
                    "value": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: None,
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let action_type_str = match input.get("action_type").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return Ok(tool_error("'action_type' is required")),
        };

        let action_type = match action_type_str.as_str() {
            "Navigate" => caduceus_runtime::browser::BrowserActionType::Navigate,
            "Click" => caduceus_runtime::browser::BrowserActionType::Click,
            "Type" => caduceus_runtime::browser::BrowserActionType::Type,
            "Screenshot" => caduceus_runtime::browser::BrowserActionType::Screenshot,
            "GetText" => caduceus_runtime::browser::BrowserActionType::GetText,
            "WaitFor" => caduceus_runtime::browser::BrowserActionType::WaitFor,
            "Scroll" => caduceus_runtime::browser::BrowserActionType::Scroll,
            "GetConsole" => caduceus_runtime::browser::BrowserActionType::GetConsole,
            other => return Ok(tool_error(format!("unknown action_type: {other}"))),
        };

        let action = caduceus_runtime::browser::BrowserAction {
            action_type,
            url: input
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            selector: input
                .get("selector")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            value: input
                .get("value")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        };

        let svc = caduceus_runtime::browser::BrowserService::new(self.headless);
        let result = svc.execute(action).await;

        if result.success {
            let data = result.data.unwrap_or_default();
            Ok(ToolResult::success(if result.console_logs.is_empty() {
                data
            } else {
                format!("{}\nConsole:\n{}", data, result.console_logs.join("\n"))
            }))
        } else {
            Ok(tool_error(
                result
                    .error
                    .unwrap_or_else(|| "browser action failed".into()),
            ))
        }
    }
}

// ── Tool 1: WebSearchTool ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WebSearchTool {
    timeout: Duration,
}

impl WebSearchTool {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

#[derive(Debug, Deserialize)]
struct WebSearchInput {
    query: String,
    #[serde(default)]
    num_results: Option<usize>,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".into(),
            description: "Search the web using DuckDuckGo. Returns titles, snippets, and URLs."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {"type": "string", "description": "Search query"},
                    "num_results": {"type": "integer", "minimum": 1, "maximum": 20}
                },
                "additionalProperties": false
            }),
            required_capability: Some("network".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: WebSearchInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        if parsed.query.trim().is_empty() {
            return Ok(tool_error("'query' must not be empty"));
        }
        let num_results = parsed.num_results.unwrap_or(5).min(20);
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(&parsed.query)
        );
        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .user_agent("Mozilla/5.0 (compatible; Caduceus/0.1)")
            .build()
            .map_err(|err| CaduceusError::Tool {
                tool: "web_search".into(),
                message: format!("failed to build HTTP client: {err}"),
            })?;
        let resp = match client.get(&url).send().await {
            Ok(resp) => resp,
            Err(err) => return Ok(tool_error(format!("search request failed: {err}"))),
        };
        let body = match resp.text().await {
            Ok(body) => body,
            Err(err) => return Ok(tool_error(format!("failed to read response: {err}"))),
        };
        let mut results: Vec<Value> = Vec::new();
        let link_re =
            regex::Regex::new(r#"<a[^>]+class="result__a"[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#)
                .unwrap_or_else(|_| regex::Regex::new(".^").unwrap());
        let snippet_re = regex::Regex::new(r#"<a[^>]+class="result__snippet"[^>]*>(.*?)</a>"#)
            .unwrap_or_else(|_| regex::Regex::new(".^").unwrap());
        let tag_re =
            regex::Regex::new(r"<[^>]+>").unwrap_or_else(|_| regex::Regex::new(".^").unwrap());
        let links: Vec<_> = link_re.captures_iter(&body).collect();
        let snippets: Vec<_> = snippet_re.captures_iter(&body).collect();
        for (i, link) in links.iter().enumerate().take(links.len().min(num_results)) {
            let raw_url = link.get(1).map_or("", |m| m.as_str());
            let title = link.get(2).map_or("", |m| m.as_str());
            let title_clean = tag_re.replace_all(title, "").trim().to_string();
            let snippet = snippets
                .get(i)
                .and_then(|captures| captures.get(1))
                .map_or(String::new(), |m| {
                    tag_re.replace_all(m.as_str(), "").trim().to_string()
                });
            let actual_url = if let Some(pos) = raw_url.find("uddg=") {
                let after = &raw_url[pos + 5..];
                urlencoding::decode(after.split('&').next().unwrap_or(after))
                    .unwrap_or_else(|_| after.into())
                    .to_string()
            } else {
                raw_url.to_string()
            };
            results.push(json!({
                "title": title_clean,
                "snippet": snippet,
                "url": actual_url
            }));
        }
        Ok(json_result(json!({
            "query": parsed.query,
            "results": results,
            "count": results.len()
        })))
    }
}

// ── Tool 2: TodoWriteTool ─────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
struct TodoItem {
    id: usize,
    text: String,
    completed: bool,
}

#[derive(Debug)]
pub struct TodoWriteTool {
    todos: Mutex<Vec<TodoItem>>,
    next_id: Mutex<usize>,
}

impl Default for TodoWriteTool {
    fn default() -> Self {
        Self {
            todos: Mutex::new(Vec::new()),
            next_id: Mutex::new(1),
        }
    }
}

impl TodoWriteTool {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Deserialize)]
struct TodoInput {
    action: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<usize>,
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "todo_write".into(),
            description: "Manage an in-memory todo list. Actions: add, list, complete, remove."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["action"],
                "properties": {
                    "action": {"type": "string", "enum": ["add", "list", "complete", "remove"]},
                    "text": {"type": "string"},
                    "id": {"type": "integer"}
                },
                "additionalProperties": false
            }),
            required_capability: None,
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: TodoInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        let mut todos = self.todos.lock().await;
        let mut next_id = self.next_id.lock().await;
        match parsed.action.as_str() {
            "add" => {
                let text = match parsed.text {
                    Some(t) if !t.trim().is_empty() => t,
                    _ => return Ok(tool_error("'text' is required for 'add' action")),
                };
                let item = TodoItem {
                    id: *next_id,
                    text,
                    completed: false,
                };
                *next_id += 1;
                todos.push(item);
            }
            "list" => {}
            "complete" => {
                let id = match parsed.id {
                    Some(id) => id,
                    None => return Ok(tool_error("'id' is required for 'complete' action")),
                };
                match todos.iter_mut().find(|t| t.id == id) {
                    Some(item) => item.completed = true,
                    None => return Ok(tool_error(format!("todo with id {id} not found"))),
                }
            }
            "remove" => {
                let id = match parsed.id {
                    Some(id) => id,
                    None => return Ok(tool_error("'id' is required for 'remove' action")),
                };
                let before = todos.len();
                todos.retain(|t| t.id != id);
                if todos.len() == before {
                    return Ok(tool_error(format!("todo with id {id} not found")));
                }
            }
            other => return Ok(tool_error(format!("unknown action: {other}"))),
        }
        Ok(json_result(json!({
            "action": parsed.action,
            "todos": *todos,
            "count": todos.len()
        })))
    }
}

// ── Tool 3: ReplTool ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ReplTool;

impl Default for ReplTool {
    fn default() -> Self {
        Self
    }
}

impl ReplTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct ReplInput {
    code: String,
    #[serde(default)]
    language: Option<String>,
}

#[async_trait]
impl Tool for ReplTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "repl".into(),
            description: "Execute code in a REPL. Supports python3 and node.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["code"],
                "properties": {
                    "code": {"type": "string"},
                    "language": {"type": "string", "enum": ["python", "node"]}
                },
                "additionalProperties": false
            }),
            required_capability: Some("process_exec".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: ReplInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        if parsed.code.trim().is_empty() {
            return Ok(tool_error("'code' must not be empty"));
        }
        let lang = parsed.language.unwrap_or_else(|| "python".into());
        let (cmd, args) = match lang.as_str() {
            "python" => ("python3", vec!["-c".to_string(), parsed.code.clone()]),
            "node" => ("node", vec!["-e".to_string(), parsed.code.clone()]),
            other => return Ok(tool_error(format!("unsupported language: {other}"))),
        };
        let output = match tokio::process::Command::new(cmd).args(&args).output().await {
            Ok(output) => output,
            Err(err) => return Ok(tool_error(format!("failed to execute {cmd}: {err}"))),
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(json_result(json!({
            "language": lang,
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": output.status.code().unwrap_or(-1)
        })))
    }
}

// ── Tool 4: PowerShellTool ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PowerShellTool;

impl Default for PowerShellTool {
    fn default() -> Self {
        Self
    }
}

impl PowerShellTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct PowerShellInput {
    command: String,
}

#[async_trait]
impl Tool for PowerShellTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "powershell".into(),
            description: "Execute PowerShell commands via pwsh.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("process_exec".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: PowerShellInput = match serde_json::from_value::<PowerShellInput>(input) {
            Ok(v) if !v.command.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'command' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        let output = match tokio::process::Command::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-Command", &parsed.command])
            .output()
            .await
        {
            Ok(output) => output,
            Err(err) => {
                return Ok(tool_error(format!(
                    "pwsh not available or failed to execute: {err}"
                )));
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(json_result(json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": output.status.code().unwrap_or(-1)
        })))
    }
}

// ── Tool 5: SleepTool ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SleepTool;

impl Default for SleepTool {
    fn default() -> Self {
        Self
    }
}

impl SleepTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct SleepInput {
    seconds: f64,
}

#[async_trait]
impl Tool for SleepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "sleep".into(),
            description: "Wait for a specified number of seconds. Useful for rate limiting.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["seconds"],
                "properties": {
                    "seconds": {"type": "number", "minimum": 0, "maximum": 300}
                },
                "additionalProperties": false
            }),
            required_capability: None,
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: SleepInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        if parsed.seconds < 0.0 || parsed.seconds > 300.0 {
            return Ok(tool_error("seconds must be between 0 and 300"));
        }
        tokio::time::sleep(Duration::from_secs_f64(parsed.seconds)).await;
        Ok(ToolResult::success(format!(
            "Slept for {} seconds",
            parsed.seconds
        )))
    }
}

// ── Tool 6: StructuredOutputTool ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StructuredOutputTool;

impl Default for StructuredOutputTool {
    fn default() -> Self {
        Self
    }
}

impl StructuredOutputTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for StructuredOutputTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "structured_output".into(),
            description: "Validate a JSON value against a JSON Schema.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["json_value", "schema"],
                "properties": {
                    "json_value": {"description": "The JSON value to validate"},
                    "schema": {"type": "object", "description": "JSON Schema to validate against"}
                },
                "additionalProperties": false
            }),
            required_capability: None,
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let map = match validate_input_object(input) {
            Ok(map) => map,
            Err(err) => return Ok(tool_error(err)),
        };
        let json_val = match map.get("json_value") {
            Some(v) => {
                if let Some(s) = v.as_str() {
                    match serde_json::from_str::<Value>(s) {
                        Ok(parsed) => parsed,
                        Err(err) => {
                            return Ok(json_result(json!({
                                "valid": false,
                                "errors": [format!("invalid JSON string: {err}")]
                            })));
                        }
                    }
                } else {
                    v.clone()
                }
            }
            None => return Ok(tool_error("'json_value' is required")),
        };
        let schema = match map.get("schema") {
            Some(Value::Object(s)) => Value::Object(s.clone()),
            _ => return Ok(tool_error("'schema' must be a JSON object")),
        };
        let mut errors: Vec<String> = Vec::new();
        validate_json_against_schema(&json_val, &schema, "", &mut errors);
        Ok(json_result(json!({
            "valid": errors.is_empty(),
            "errors": errors
        })))
    }
}

fn validate_json_against_schema(
    value: &Value,
    schema: &Value,
    path: &str,
    errors: &mut Vec<String>,
) {
    if let Some(type_str) = schema.get("type").and_then(Value::as_str) {
        let type_ok = match type_str {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" | "integer" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => true,
        };
        if !type_ok {
            errors.push(format!(
                "at '{path}': expected type '{type_str}', got {}",
                value_type_name(value)
            ));
            return;
        }
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        if let Some(obj) = value.as_object() {
            for req in required {
                if let Some(key) = req.as_str() {
                    if !obj.contains_key(key) {
                        let field_path = if path.is_empty() {
                            key.to_string()
                        } else {
                            format!("{path}.{key}")
                        };
                        errors.push(format!("at '{field_path}': required field missing"));
                    }
                }
            }
        }
    }
    if let (Some(props), Some(obj)) = (
        schema.get("properties").and_then(Value::as_object),
        value.as_object(),
    ) {
        for (key, prop_schema) in props {
            if let Some(val) = obj.get(key) {
                let field_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                validate_json_against_schema(val, prop_schema, &field_path, errors);
            }
        }
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ── Tool 7: AgentSpawnTool ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AgentSpawnTool;

impl Default for AgentSpawnTool {
    fn default() -> Self {
        Self
    }
}

impl AgentSpawnTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct AgentSpawnInput {
    task: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_turns: Option<usize>,
}

#[async_trait]
impl Tool for AgentSpawnTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "agent_spawn".into(),
            description: "Spawn a sub-agent to handle a subtask (placeholder).".into(),
            input_schema: json!({
                "type": "object",
                "required": ["task"],
                "properties": {
                    "task": {"type": "string"},
                    "model": {"type": "string"},
                    "max_turns": {"type": "integer", "minimum": 1, "maximum": 100}
                },
                "additionalProperties": false
            }),
            required_capability: Some("agent_spawn".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: AgentSpawnInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        if parsed.task.trim().is_empty() {
            return Ok(tool_error("'task' must not be empty"));
        }
        let model = parsed.model.unwrap_or_else(|| "default".into());
        let max_turns = parsed.max_turns.unwrap_or(10);
        Ok(json_result(json!({
            "status": "spawned",
            "task": parsed.task,
            "model": model,
            "max_turns": max_turns,
            "note": "Sub-agent placeholder -- full implementation requires orchestrator wiring."
        })))
    }
}

// ── Tool 8: PdfExtractTool ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PdfExtractTool {
    workspace_root: PathBuf,
}

impl PdfExtractTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PdfExtractInput {
    path: String,
}

#[async_trait]
impl Tool for PdfExtractTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "pdf_extract".into(),
            description: "Extract text from a PDF file using pdftotext.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_read".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: PdfExtractInput = match serde_json::from_value::<PdfExtractInput>(input) {
            Ok(v) if !v.path.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'path' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        let resolved = match resolve_workspace_path(&self.workspace_root, &parsed.path) {
            Ok(path) => path,
            Err(err) => return Ok(tool_error(err.to_string())),
        };
        if !resolved.exists() {
            return Ok(tool_error(format!("file not found: {}", parsed.path)));
        }
        let output = match tokio::process::Command::new("pdftotext")
            .args([resolved.to_string_lossy().as_ref(), "-"])
            .output()
            .await
        {
            Ok(output) => output,
            Err(_) => {
                return Ok(tool_error(
                    "pdftotext not found. Install poppler-utils: brew install poppler (macOS) or apt install poppler-utils (Linux)",
                ));
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Ok(tool_error(format!("pdftotext failed: {stderr}")));
        }
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(json_result(json!({
            "path": parsed.path,
            "text": text,
            "length": text.len()
        })))
    }
}

// ── Tool 9: NotebookEditTool ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NotebookEditTool {
    workspace_root: PathBuf,
}

impl NotebookEditTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct NotebookEditInput {
    path: String,
    cell_index: usize,
    new_source: String,
}

#[async_trait]
impl Tool for NotebookEditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "notebook_edit".into(),
            description: "Edit a Jupyter notebook cell by index.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["path", "cell_index", "new_source"],
                "properties": {
                    "path": {"type": "string"},
                    "cell_index": {"type": "integer", "minimum": 0},
                    "new_source": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_write".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: NotebookEditInput = match serde_json::from_value::<NotebookEditInput>(input) {
            Ok(v) if !v.path.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'path' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        let resolved = match resolve_workspace_path(&self.workspace_root, &parsed.path) {
            Ok(path) => path,
            Err(err) => return Ok(tool_error(err.to_string())),
        };
        let content = match std::fs::read_to_string(&resolved) {
            Ok(c) => c,
            Err(err) => return Ok(tool_error(format!("failed to read notebook: {err}"))),
        };
        let mut notebook: Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("failed to parse notebook JSON: {err}"))),
        };
        let cells = match notebook.get_mut("cells").and_then(Value::as_array_mut) {
            Some(cells) => cells,
            None => return Ok(tool_error("notebook has no 'cells' array")),
        };
        if parsed.cell_index >= cells.len() {
            return Ok(tool_error(format!(
                "cell_index {} out of range (notebook has {} cells)",
                parsed.cell_index,
                cells.len()
            )));
        }
        let source_lines: Vec<String> = parsed.new_source.lines().map(String::from).collect();
        let source_json: Vec<Value> = source_lines
            .iter()
            .enumerate()
            .map(|(i, line): (usize, &String)| {
                if i < source_lines.len() - 1 {
                    Value::String(format!("{line}\n"))
                } else {
                    Value::String(line.to_string())
                }
            })
            .collect();
        cells[parsed.cell_index]["source"] = Value::Array(source_json);
        let serialized = match serde_json::to_string_pretty(&notebook) {
            Ok(s) => s,
            Err(err) => return Ok(tool_error(format!("failed to serialize notebook: {err}"))),
        };
        if let Err(err) = secure_write_path(&resolved, &serialized) {
            return Ok(tool_error(format!("failed to write notebook: {err}")));
        }
        Ok(json_result(json!({
            "path": parsed.path,
            "cell_index": parsed.cell_index,
            "status": "updated"
        })))
    }
}

// ── Tool 10: ToolSearchTool ───────────────────────────────────────────────────

#[derive(Debug)]
pub struct ToolSearchTool {
    registry_specs: Arc<Mutex<Vec<ToolSpec>>>,
}

impl Default for ToolSearchTool {
    fn default() -> Self {
        Self {
            registry_specs: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl ToolSearchTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn update_specs(&self, specs: Vec<ToolSpec>) {
        let mut locked = self.registry_specs.lock().await;
        *locked = specs;
    }
}

#[derive(Debug, Deserialize)]
struct ToolSearchInput {
    query: String,
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "tool_search".into(),
            description: "Search available tools by keyword in names and descriptions.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: None,
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: ToolSearchInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        if parsed.query.trim().is_empty() {
            return Ok(tool_error("'query' must not be empty"));
        }
        let query_lower = parsed.query.to_lowercase();
        let specs = self.registry_specs.lock().await;
        let matches: Vec<Value> = specs
            .iter()
            .filter(|s| {
                s.name.to_lowercase().contains(&query_lower)
                    || s.description.to_lowercase().contains(&query_lower)
            })
            .map(|s| {
                json!({
                    "name": s.name,
                    "description": s.description,
                    "input_schema": s.input_schema
                })
            })
            .collect();
        Ok(json_result(json!({
            "query": parsed.query,
            "matches": matches,
            "count": matches.len()
        })))
    }
}

// ── Tool 11: InsertCodeTool ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct InsertCodeTool {
    workspace_root: PathBuf,
}

impl InsertCodeTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct InsertCodeInput {
    path: String,
    line: usize,
    code: String,
}

#[async_trait]
impl Tool for InsertCodeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "insert_code".into(),
            description: "Insert code at a specific line number in a file.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["path", "line", "code"],
                "properties": {
                    "path": {"type": "string"},
                    "line": {"type": "integer", "minimum": 1},
                    "code": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_write".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: InsertCodeInput = match serde_json::from_value::<InsertCodeInput>(input) {
            Ok(v) if !v.path.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'path' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        if parsed.line == 0 {
            return Ok(tool_error("'line' must be >= 1"));
        }
        let resolved = match resolve_workspace_path(&self.workspace_root, &parsed.path) {
            Ok(path) => path,
            Err(err) => return Ok(tool_error(err.to_string())),
        };
        let content = match std::fs::read_to_string(&resolved) {
            Ok(c) => c,
            Err(err) => return Ok(tool_error(format!("failed to read file: {err}"))),
        };
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        let insert_idx = (parsed.line - 1).min(lines.len());
        let new_lines: Vec<String> = parsed.code.lines().map(String::from).collect();
        let inserted_count = new_lines.len();
        for (i, new_line) in new_lines.into_iter().enumerate() {
            lines.insert(insert_idx + i, new_line);
        }
        let new_content = lines.join("\n");
        let final_content = if content.ends_with('\n') {
            format!("{new_content}\n")
        } else {
            new_content
        };
        if let Err(err) = secure_write_path(&resolved, &final_content) {
            return Ok(tool_error(format!("failed to write file: {err}")));
        }
        let context_start = insert_idx.saturating_sub(2);
        let context_end = (insert_idx + inserted_count + 2).min(lines.len());
        let context: Vec<String> = lines[context_start..context_end]
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{}: {l}", context_start + i + 1))
            .collect();
        Ok(json_result(json!({
            "path": parsed.path,
            "inserted_at_line": parsed.line,
            "lines_inserted": inserted_count,
            "context": context.join("\n")
        })))
    }
}

// ── Tool 12: MultiEditTool ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MultiEditTool {
    workspace_root: PathBuf,
}

impl MultiEditTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct MultiEditInput {
    path: String,
    edits: Vec<EditPair>,
}

#[derive(Debug, Deserialize)]
struct EditPair {
    old: String,
    new: String,
}

#[async_trait]
impl Tool for MultiEditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "multi_edit".into(),
            description: "Apply multiple find-and-replace edits to a file atomically.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["path", "edits"],
                "properties": {
                    "path": {"type": "string"},
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["old", "new"],
                            "properties": {
                                "old": {"type": "string"},
                                "new": {"type": "string"}
                            }
                        }
                    }
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_write".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: MultiEditInput = match serde_json::from_value::<MultiEditInput>(input) {
            Ok(v) if !v.path.trim().is_empty() => v,
            Ok(_) => return Ok(tool_error("'path' must not be empty")),
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        if parsed.edits.is_empty() {
            return Ok(tool_error("'edits' must not be empty"));
        }
        let resolved = match resolve_workspace_path(&self.workspace_root, &parsed.path) {
            Ok(path) => path,
            Err(err) => return Ok(tool_error(err.to_string())),
        };
        let mut content = match std::fs::read_to_string(&resolved) {
            Ok(c) => c,
            Err(err) => return Ok(tool_error(format!("failed to read file: {err}"))),
        };
        let mut applied = 0;
        let mut failed: Vec<String> = Vec::new();
        for (i, edit) in parsed.edits.iter().enumerate() {
            if content.contains(&edit.old) {
                content = content.replacen(&edit.old, &edit.new, 1);
                applied += 1;
            } else {
                failed.push(format!("edit[{i}]: old text not found"));
            }
        }
        if applied > 0 {
            if let Err(err) = secure_write_path(&resolved, &content) {
                return Ok(tool_error(format!("failed to write file: {err}")));
            }
        }
        Ok(json_result(json!({
            "path": parsed.path,
            "applied": applied,
            "failed": failed,
            "total_edits": parsed.edits.len()
        })))
    }
}

// ── Tool 13: TreeTool ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TreeTool {
    workspace_root: PathBuf,
}

impl TreeTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TreeInput {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    depth: Option<usize>,
    #[serde(default)]
    show_hidden: Option<bool>,
}

#[allow(clippy::too_many_arguments)]
struct TreeConfig {
    max_depth: usize,
    show_hidden: bool,
}

fn build_tree(
    dir: &Path,
    prefix: &str,
    depth: usize,
    config: &TreeConfig,
    output: &mut String,
    counts: &mut (usize, usize),
) {
    if depth >= config.max_depth {
        return;
    }
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| e.file_name());
    if !config.show_hidden {
        entries.retain(|e| !e.file_name().to_string_lossy().starts_with('.'));
    }
    let count = entries.len();
    for (i, entry) in entries.iter().enumerate() {
        let is_last = i == count - 1;
        let connector = if is_last {
            "\u{2514}\u{2500}\u{2500} "
        } else {
            "\u{251c}\u{2500}\u{2500} "
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        output.push_str(&format!("{prefix}{connector}{name}"));
        if is_dir {
            output.push('/');
            counts.0 += 1;
        } else {
            counts.1 += 1;
        }
        output.push('\n');
        if is_dir {
            let child_prefix = if is_last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}\u{2502}   ")
            };
            build_tree(
                &entry.path(),
                &child_prefix,
                depth + 1,
                config,
                output,
                counts,
            );
        }
    }
}

#[async_trait]
impl Tool for TreeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "tree".into(),
            description: "Show directory tree structure with configurable depth.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "depth": {"type": "integer", "minimum": 1, "maximum": 10},
                    "show_hidden": {"type": "boolean"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("fs_read".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: TreeInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        let dir_path = match parsed.path {
            Some(ref p) if !p.is_empty() => match resolve_workspace_path(&self.workspace_root, p) {
                Ok(path) => path,
                Err(err) => return Ok(tool_error(err.to_string())),
            },
            _ => self.workspace_root.clone(),
        };
        if !dir_path.is_dir() {
            return Ok(tool_error(format!(
                "not a directory: {}",
                dir_path.display()
            )));
        }
        let max_depth = parsed.depth.unwrap_or(3).min(10);
        let show_hidden = parsed.show_hidden.unwrap_or(false);
        let dir_name = dir_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        let mut output = format!("{dir_name}/\n");
        let mut counts = (0usize, 0usize);
        build_tree(
            &dir_path,
            "",
            0,
            &TreeConfig {
                max_depth,
                show_hidden,
            },
            &mut output,
            &mut counts,
        );
        let (dir_count, file_count) = counts;
        output.push_str(&format!("\n{dir_count} directories, {file_count} files"));
        Ok(json_result(json!({
            "tree": output,
            "directories": dir_count,
            "files": file_count
        })))
    }
}

// ── Tool 14: DiagnosticsTool ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DiagnosticsTool {
    workspace_root: PathBuf,
}

impl DiagnosticsTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: canonical_or_self(workspace_root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct DiagnosticsInput {
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for DiagnosticsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "diagnostics".into(),
            description: "Get project diagnostics. Detects Rust or TypeScript.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "additionalProperties": false
            }),
            required_capability: Some("process_exec".into()),
        }
    }

    async fn call(&self, input: Value) -> Result<ToolResult> {
        let parsed: DiagnosticsInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(err) => return Ok(tool_error(format!("invalid input: {err}"))),
        };
        let project_path = match parsed.path {
            Some(ref p) if !p.is_empty() => match resolve_workspace_path(&self.workspace_root, p) {
                Ok(path) => path,
                Err(err) => return Ok(tool_error(err.to_string())),
            },
            _ => self.workspace_root.clone(),
        };
        let is_rust = project_path.join("Cargo.toml").exists();
        let is_ts = project_path.join("tsconfig.json").exists();
        let (tool_name, output) = if is_rust {
            match tokio::process::Command::new("cargo")
                .args(["check", "--message-format=json"])
                .current_dir(&project_path)
                .output()
                .await
            {
                Ok(o) => ("cargo check", o),
                Err(err) => return Ok(tool_error(format!("failed to run cargo check: {err}"))),
            }
        } else if is_ts {
            match tokio::process::Command::new("npx")
                .args(["tsc", "--noEmit"])
                .current_dir(&project_path)
                .output()
                .await
            {
                Ok(o) => ("tsc --noEmit", o),
                Err(err) => return Ok(tool_error(format!("failed to run tsc: {err}"))),
            }
        } else {
            return Ok(tool_error(
                "could not detect project type (no Cargo.toml or tsconfig.json found)",
            ));
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let success = output.status.success();
        let mut errors: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        if is_rust {
            for line in stdout.lines() {
                if let Ok(msg) = serde_json::from_str::<Value>(line) {
                    if let Some(message) = msg.get("message") {
                        let level = message.get("level").and_then(Value::as_str).unwrap_or("");
                        let text = message
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        match level {
                            "error" => errors.push(text),
                            "warning" => warnings.push(text),
                            _ => {}
                        }
                    }
                }
            }
        } else {
            for line in stdout.lines().chain(stderr.lines()) {
                let trimmed = line.trim();
                if trimmed.contains("error TS") {
                    errors.push(trimmed.to_string());
                } else if !trimmed.is_empty() {
                    warnings.push(trimmed.to_string());
                }
            }
        }
        Ok(json_result(json!({
            "tool": tool_name,
            "success": success,
            "errors": errors,
            "warnings": warnings,
            "error_count": errors.len(),
            "warning_count": warnings.len()
        })))
    }
}

// ── Tool 15: ContextTool ──────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ContextTool {
    total_tokens: usize,
    used_tokens: Mutex<usize>,
}

impl ContextTool {
    pub fn new(total_tokens: usize) -> Self {
        Self {
            total_tokens,
            used_tokens: Mutex::new(0),
        }
    }

    pub async fn set_used_tokens(&self, used: usize) {
        let mut locked = self.used_tokens.lock().await;
        *locked = used;
    }
}

#[async_trait]
impl Tool for ContextTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "context".into(),
            description: "Show current context window usage: token counts, fill percentage, and warning level.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_capability: None,
        }
    }

    async fn call(&self, _input: Value) -> Result<ToolResult> {
        let used = *self.used_tokens.lock().await;
        let total = self.total_tokens;
        let fill_pct = if total > 0 {
            (used as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        let warning_level = if fill_pct >= 90.0 {
            "critical"
        } else if fill_pct >= 75.0 {
            "high"
        } else if fill_pct >= 50.0 {
            "medium"
        } else {
            "low"
        };
        Ok(json_result(json!({
            "total_tokens": total,
            "used_tokens": used,
            "available_tokens": total.saturating_sub(used),
            "fill_percentage": format!("{fill_pct:.1}%"),
            "warning_level": warning_level
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
    registry.register(Arc::new(BrowserActionTool::new(true)));
    // New tools
    registry.register(Arc::new(WebSearchTool::new(Duration::from_secs(15))));
    registry.register(Arc::new(TodoWriteTool::new()));
    registry.register(Arc::new(ReplTool::new()));
    registry.register(Arc::new(PowerShellTool::new()));
    registry.register(Arc::new(SleepTool::new()));
    registry.register(Arc::new(StructuredOutputTool::new()));
    registry.register(Arc::new(AgentSpawnTool::new()));
    registry.register(Arc::new(PdfExtractTool::new(&workspace_root)));
    registry.register(Arc::new(NotebookEditTool::new(&workspace_root)));
    registry.register(Arc::new(ToolSearchTool::new()));
    registry.register(Arc::new(InsertCodeTool::new(&workspace_root)));
    registry.register(Arc::new(MultiEditTool::new(&workspace_root)));
    registry.register(Arc::new(TreeTool::new(&workspace_root)));
    registry.register(Arc::new(DiagnosticsTool::new(&workspace_root)));
    registry.register(Arc::new(ContextTool::new(128_000)));
    registry
}

pub fn default_registry() -> ToolRegistry {
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    default_registry_with_root(workspace_root)
}

// ── Feature #74: Tool Preset Reduction ─────────────────────────────────────

/// Named tool subsets for constrained agents.
pub struct ToolPresets;

impl ToolPresets {
    pub fn read_only() -> Vec<String> {
        ["Read", "Glob", "Grep", "Tree", "Diagnostics"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    pub fn minimal() -> Vec<String> {
        let mut tools = Self::read_only();
        tools.extend(["Write", "Edit"].iter().map(|s| s.to_string()));
        tools
    }

    pub fn standard() -> Vec<String> {
        let mut tools = Self::minimal();
        tools.extend(
            ["Bash", "WebSearch", "TodoWrite"]
                .iter()
                .map(|s| s.to_string()),
        );
        tools
    }

    pub fn full() -> Vec<String> {
        default_registry()
            .list_specs()
            .iter()
            .map(|t| t.name.clone())
            .collect()
    }

    pub fn get_preset(name: &str) -> Option<Vec<String>> {
        match name {
            "read_only" => Some(Self::read_only()),
            "minimal" => Some(Self::minimal()),
            "standard" => Some(Self::standard()),
            "full" => Some(Self::full()),
            _ => None,
        }
    }

    pub fn list_presets() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "read_only",
                "Read-only tools: Read, Glob, Grep, Tree, Diagnostics",
            ),
            ("minimal", "read_only + Write, Edit"),
            ("standard", "minimal + Bash, WebSearch, TodoWrite"),
            ("full", "All registered tools"),
        ]
    }
}

/// Filters tool invocations based on an allowed set.
#[derive(Debug)]
pub struct ToolFilter {
    allowed_tools: HashSet<String>,
}

impl ToolFilter {
    pub fn from_preset(preset: &str) -> std::result::Result<Self, String> {
        ToolPresets::get_preset(preset)
            .map(Self::from_list)
            .ok_or_else(|| format!("Unknown preset: {preset}"))
    }

    pub fn from_list(tools: Vec<String>) -> Self {
        Self {
            allowed_tools: tools.into_iter().collect(),
        }
    }

    pub fn is_allowed(&self, tool_name: &str) -> bool {
        self.allowed_tools.contains(tool_name)
    }

    pub fn allowed_count(&self) -> usize {
        self.allowed_tools.len()
    }
}

// ── Feature #157: Self-Verification (Agent QA) ─────────────────────────────

#[derive(Debug, Clone)]
pub enum ArtifactType {
    TestOutput,
    Log,
    Screenshot,
    Coverage,
    Diff,
}

#[derive(Debug, Clone)]
pub struct VerificationArtifact {
    pub name: String,
    pub artifact_type: ArtifactType,
    pub content: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub passed: bool,
    pub total: usize,
    pub failures: Vec<String>,
    pub output: String,
}

pub struct SelfVerifier {
    pub workspace: PathBuf,
    pub artifacts: Vec<VerificationArtifact>,
}

impl SelfVerifier {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            artifacts: Vec::new(),
        }
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    pub fn run_tests(&mut self, command: &str) -> Result<VerificationResult> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return Err(CaduceusError::Tool {
                tool: "SelfVerifier".to_string(),
                message: "Empty command".to_string(),
            });
        }
        let output = std::process::Command::new(parts[0])
            .args(&parts[1..])
            .current_dir(&self.workspace)
            .output()
            .map_err(|e| CaduceusError::Tool {
                tool: "SelfVerifier".to_string(),
                message: e.to_string(),
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{stdout}{stderr}");
        let passed = output.status.success();

        let failures: Vec<String> = combined
            .lines()
            .filter(|l| l.contains("FAILED") || l.contains("test FAILED"))
            .map(String::from)
            .collect();

        let total = combined
            .lines()
            .filter(|l| {
                l.contains("test ")
                    && (l.contains(" ok") || l.contains("FAILED") || l.contains("ignored"))
            })
            .count();

        let result = VerificationResult {
            passed,
            total: total.max(1),
            failures,
            output: combined.clone(),
        };

        self.artifacts.push(VerificationArtifact {
            name: format!("test-{}", Self::now_secs()),
            artifact_type: ArtifactType::TestOutput,
            content: combined,
            timestamp: Self::now_secs(),
        });

        Ok(result)
    }

    pub fn capture_log(&mut self, name: &str, content: &str) {
        self.artifacts.push(VerificationArtifact {
            name: name.to_string(),
            artifact_type: ArtifactType::Log,
            content: content.to_string(),
            timestamp: Self::now_secs(),
        });
    }

    pub fn capture_diff(&mut self, before: &str, after: &str) -> String {
        let mut diff = String::from("--- before\n+++ after\n");
        for line in before.lines() {
            diff.push_str(&format!("-{line}\n"));
        }
        for line in after.lines() {
            diff.push_str(&format!("+{line}\n"));
        }
        self.artifacts.push(VerificationArtifact {
            name: format!("diff-{}", Self::now_secs()),
            artifact_type: ArtifactType::Diff,
            content: diff.clone(),
            timestamp: Self::now_secs(),
        });
        diff
    }

    pub fn generate_report(&self) -> String {
        let mut report = String::from("# Verification Report\n\n");
        report.push_str(&format!("Total artifacts: {}\n\n", self.artifacts.len()));
        for artifact in &self.artifacts {
            let type_name = match artifact.artifact_type {
                ArtifactType::TestOutput => "TestOutput",
                ArtifactType::Log => "Log",
                ArtifactType::Screenshot => "Screenshot",
                ArtifactType::Coverage => "Coverage",
                ArtifactType::Diff => "Diff",
            };
            report.push_str(&format!("## {} ({})\n", artifact.name, type_name));
            report.push_str(&format!("Timestamp: {}\n", artifact.timestamp));
            report.push_str(&format!("```\n{}\n```\n\n", artifact.content));
        }
        report
    }

    pub fn all_passed(&self) -> bool {
        self.artifacts.iter().all(|a| {
            if matches!(a.artifact_type, ArtifactType::TestOutput) {
                !a.content.contains("FAILED")
            } else {
                true
            }
        })
    }
}

// ── Feature #71: LSP Bridge Tool ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspCapability {
    GotoDefinition,
    FindReferences,
    Diagnostics,
    Hover,
    Completion,
}

#[derive(Debug, Clone)]
pub struct LspBridgeTool {
    pub server_command: String,
    pub root_path: String,
    pub capabilities: Vec<LspCapability>,
}

#[derive(Debug, Clone)]
pub struct LspRequest {
    pub method: String,
    pub file: String,
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone)]
pub struct LspLocation {
    pub file: String,
    pub line: u32,
    pub character: u32,
    pub preview: String,
}

#[derive(Debug, Clone)]
pub struct LspResponse {
    pub result_type: String,
    pub content: String,
    pub locations: Vec<LspLocation>,
}

impl LspBridgeTool {
    pub fn new(command: &str, root: &str) -> Self {
        Self {
            server_command: command.to_string(),
            root_path: root.to_string(),
            capabilities: Vec::new(),
        }
    }

    pub fn format_request(&self, req: &LspRequest) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": req.method,
            "params": {
                "textDocument": { "uri": format!("file://{}", req.file) },
                "position": { "line": req.line, "character": req.character },
            }
        })
        .to_string()
    }

    pub fn parse_response(&self, json: &str) -> std::result::Result<LspResponse, String> {
        let val: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let result_type = val
            .get("result")
            .and_then(|r| r.get("type"))
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_string();
        let content = val
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        Ok(LspResponse {
            result_type,
            content,
            locations: Vec::new(),
        })
    }

    pub fn supports(&self, cap: &LspCapability) -> bool {
        self.capabilities.contains(cap)
    }
}

// ── Feature #75: Notebook Cell Tool ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellType {
    Code,
    Markdown,
    Raw,
}

#[derive(Debug, Clone)]
pub struct NotebookCell {
    pub cell_type: CellType,
    pub source: String,
    pub outputs: Vec<String>,
    pub execution_count: Option<u32>,
}

pub struct NotebookCellTool;

impl NotebookCellTool {
    pub fn parse_notebook(json: &str) -> std::result::Result<Vec<NotebookCell>, String> {
        let val: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let cells_arr = val
            .get("cells")
            .and_then(|c| c.as_array())
            .ok_or_else(|| "missing 'cells' array".to_string())?;
        let mut cells = Vec::new();
        for cell in cells_arr {
            let cell_type_str = cell
                .get("cell_type")
                .and_then(|t| t.as_str())
                .unwrap_or("code");
            let cell_type = match cell_type_str {
                "markdown" => CellType::Markdown,
                "raw" => CellType::Raw,
                _ => CellType::Code,
            };
            let source = cell
                .get("source")
                .map(|s| {
                    if s.is_array() {
                        s.as_array()
                            .unwrap()
                            .iter()
                            .filter_map(|l| l.as_str())
                            .collect::<Vec<_>>()
                            .join("")
                    } else {
                        s.as_str().unwrap_or("").to_string()
                    }
                })
                .unwrap_or_default();
            let outputs = cell
                .get("outputs")
                .and_then(|o| o.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|o| o.get("text").and_then(|t| t.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let execution_count = cell
                .get("execution_count")
                .and_then(|e| e.as_u64())
                .map(|n| n as u32);
            cells.push(NotebookCell {
                cell_type,
                source,
                outputs,
                execution_count,
            });
        }
        Ok(cells)
    }

    pub fn get_cell(cells: &[NotebookCell], index: usize) -> Option<&NotebookCell> {
        cells.get(index)
    }

    pub fn edit_cell(
        cells: &mut [NotebookCell],
        index: usize,
        new_source: &str,
    ) -> std::result::Result<(), String> {
        cells
            .get_mut(index)
            .map(|c| c.source = new_source.to_string())
            .ok_or_else(|| format!("index {} out of bounds", index))
    }

    pub fn insert_cell(
        cells: &mut Vec<NotebookCell>,
        index: usize,
        cell: NotebookCell,
    ) -> std::result::Result<(), String> {
        if index > cells.len() {
            return Err(format!("index {} out of bounds", index));
        }
        cells.insert(index, cell);
        Ok(())
    }

    pub fn delete_cell(
        cells: &mut Vec<NotebookCell>,
        index: usize,
    ) -> std::result::Result<(), String> {
        if index >= cells.len() {
            return Err(format!("index {} out of bounds", index));
        }
        cells.remove(index);
        Ok(())
    }

    pub fn to_notebook_json(cells: &[NotebookCell]) -> String {
        let cells_json: Vec<serde_json::Value> = cells
            .iter()
            .map(|c| {
                let type_str = match c.cell_type {
                    CellType::Markdown => "markdown",
                    CellType::Raw => "raw",
                    CellType::Code => "code",
                };
                serde_json::json!({
                    "cell_type": type_str,
                    "source": c.source,
                    "outputs": c.outputs,
                    "execution_count": c.execution_count,
                })
            })
            .collect();
        serde_json::json!({ "cells": cells_json }).to_string()
    }
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
        assert_eq!(registry.list_specs().len(), 27);
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
    // ── Tests for 15 new tools (2 each = 30 tests) ───────────────────────────

    // 1. WebSearchTool
    #[tokio::test]
    async fn web_search_empty_query_error() {
        let tool = WebSearchTool::new(Duration::from_secs(5));
        let result = tool.call(json!({"query": ""})).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("must not be empty"));
    }

    #[tokio::test]
    async fn web_search_invalid_input_error() {
        let tool = WebSearchTool::new(Duration::from_secs(5));
        let result = tool.call(json!({"not_query": 42})).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("invalid input"));
    }

    // 2. TodoWriteTool
    #[tokio::test]
    async fn todo_add_and_list() {
        let tool = TodoWriteTool::new();
        let add_result = tool
            .call(json!({"action": "add", "text": "Buy milk"}))
            .await
            .unwrap();
        assert!(!add_result.is_error);
        assert!(add_result.content.contains("Buy milk"));

        let list_result = tool.call(json!({"action": "list"})).await.unwrap();
        assert!(!list_result.is_error);
        assert!(list_result.content.contains("Buy milk"));
    }

    #[tokio::test]
    async fn todo_complete_and_remove() {
        let tool = TodoWriteTool::new();
        tool.call(json!({"action": "add", "text": "Task A"}))
            .await
            .unwrap();
        tool.call(json!({"action": "add", "text": "Task B"}))
            .await
            .unwrap();

        let complete = tool
            .call(json!({"action": "complete", "id": 1}))
            .await
            .unwrap();
        assert!(!complete.is_error);
        assert!(complete.content.contains("true"));

        let remove = tool
            .call(json!({"action": "remove", "id": 2}))
            .await
            .unwrap();
        assert!(!remove.is_error);

        let remove_bad = tool
            .call(json!({"action": "remove", "id": 999}))
            .await
            .unwrap();
        assert!(remove_bad.is_error);
    }

    // 3. ReplTool
    #[tokio::test]
    async fn repl_empty_code_error() {
        let tool = ReplTool::new();
        let result = tool
            .call(json!({"code": "", "language": "python"}))
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("must not be empty"));
    }

    #[tokio::test]
    async fn repl_unsupported_language() {
        let tool = ReplTool::new();
        let result = tool
            .call(json!({"code": "puts 1", "language": "ruby"}))
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("unsupported language"));
    }

    // 4. PowerShellTool
    #[tokio::test]
    async fn powershell_empty_command_error() {
        let tool = PowerShellTool::new();
        let result = tool.call(json!({"command": ""})).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("must not be empty"));
    }

    #[tokio::test]
    async fn powershell_invalid_input() {
        let tool = PowerShellTool::new();
        let result = tool.call(json!({"not_command": 1})).await.unwrap();
        assert!(result.is_error);
    }

    // 5. SleepTool
    #[tokio::test]
    async fn sleep_tool_works() {
        let tool = SleepTool::new();
        let start = std::time::Instant::now();
        let result = tool.call(json!({"seconds": 0.1})).await.unwrap();
        let elapsed = start.elapsed();
        assert!(!result.is_error);
        assert!(result.content.contains("Slept for 0.1 seconds"));
        assert!(elapsed.as_millis() >= 90);
    }

    #[tokio::test]
    async fn sleep_tool_rejects_out_of_range() {
        let tool = SleepTool::new();
        let result = tool.call(json!({"seconds": -5})).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("between 0 and 300"));
    }

    // 6. StructuredOutputTool
    #[tokio::test]
    async fn structured_output_valid_json() {
        let tool = StructuredOutputTool::new();
        let result = tool
            .call(json!({
                "json_value": {"name": "Alice", "age": 30},
                "schema": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": {"type": "string"},
                        "age": {"type": "number"}
                    }
                }
            }))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("\"valid\": true"));
    }

    #[tokio::test]
    async fn structured_output_invalid_json() {
        let tool = StructuredOutputTool::new();
        let result = tool
            .call(json!({
                "json_value": {"name": 123},
                "schema": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"}
                    }
                }
            }))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("\"valid\": false"));
    }

    // 7. AgentSpawnTool
    #[tokio::test]
    async fn agent_spawn_returns_placeholder() {
        let tool = AgentSpawnTool::new();
        let result = tool
            .call(json!({"task": "Summarize this file", "model": "gpt-4"}))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("spawned"));
        assert!(result.content.contains("placeholder"));
    }

    #[tokio::test]
    async fn agent_spawn_empty_task_error() {
        let tool = AgentSpawnTool::new();
        let result = tool.call(json!({"task": ""})).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("must not be empty"));
    }

    // 8. PdfExtractTool
    #[tokio::test]
    async fn pdf_extract_missing_file_error() {
        let root = test_workspace("pdf-missing");
        let tool = PdfExtractTool::new(&root);
        let result = tool.call(json!({"path": "nonexistent.pdf"})).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn pdf_extract_empty_path_error() {
        let root = test_workspace("pdf-empty");
        let tool = PdfExtractTool::new(&root);
        let result = tool.call(json!({"path": ""})).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("must not be empty"));
        let _ = std::fs::remove_dir_all(root);
    }

    // 9. NotebookEditTool
    #[tokio::test]
    async fn notebook_edit_updates_cell() {
        let root = test_workspace("notebook-edit");
        let nb = json!({
            "cells": [
                {"cell_type": "code", "source": ["print('old')\n"], "metadata": {}, "outputs": []},
                {"cell_type": "code", "source": ["x = 1\n"], "metadata": {}, "outputs": []}
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        std::fs::write(
            root.join("test.ipynb"),
            serde_json::to_string_pretty(&nb).unwrap(),
        )
        .unwrap();

        let tool = NotebookEditTool::new(&root);
        let result = tool
            .call(json!({"path": "test.ipynb", "cell_index": 0, "new_source": "print('new')"}))
            .await
            .unwrap();
        assert!(!result.is_error);

        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("test.ipynb")).unwrap())
                .unwrap();
        let source = updated["cells"][0]["source"][0].as_str().unwrap();
        assert!(source.contains("print('new')"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn notebook_edit_out_of_range() {
        let root = test_workspace("notebook-oor");
        let nb = json!({
            "cells": [{"cell_type": "code", "source": ["x\n"], "metadata": {}, "outputs": []}],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        std::fs::write(
            root.join("nb.ipynb"),
            serde_json::to_string_pretty(&nb).unwrap(),
        )
        .unwrap();

        let tool = NotebookEditTool::new(&root);
        let result = tool
            .call(json!({"path": "nb.ipynb", "cell_index": 5, "new_source": "y"}))
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("out of range"));

        let _ = std::fs::remove_dir_all(root);
    }

    // 10. ToolSearchTool
    #[tokio::test]
    async fn tool_search_finds_match() {
        let tool = ToolSearchTool::new();
        tool.update_specs(vec![
            ToolSpec {
                name: "bash".into(),
                description: "Execute a bash command".into(),
                input_schema: json!({}),
                required_capability: None,
            },
            ToolSpec {
                name: "read_file".into(),
                description: "Read a file from workspace".into(),
                input_schema: json!({}),
                required_capability: None,
            },
        ])
        .await;

        let result = tool.call(json!({"query": "bash"})).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("bash"));
        assert!(result.content.contains("\"count\": 1"));
    }

    #[tokio::test]
    async fn tool_search_empty_query_error() {
        let tool = ToolSearchTool::new();
        let result = tool.call(json!({"query": ""})).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("must not be empty"));
    }

    // 11. InsertCodeTool
    #[tokio::test]
    async fn insert_code_at_line() {
        let root = test_workspace("insert-code");
        std::fs::write(root.join("file.txt"), "line1\nline2\nline3\n").unwrap();
        let tool = InsertCodeTool::new(&root);
        let result = tool
            .call(json!({"path": "file.txt", "line": 2, "code": "inserted"}))
            .await
            .unwrap();
        assert!(!result.is_error);
        let content = std::fs::read_to_string(root.join("file.txt")).unwrap();
        assert_eq!(content, "line1\ninserted\nline2\nline3\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn insert_code_empty_path_error() {
        let root = test_workspace("insert-code-err");
        let tool = InsertCodeTool::new(&root);
        let result = tool
            .call(json!({"path": "", "line": 1, "code": "x"}))
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("must not be empty"));
        let _ = std::fs::remove_dir_all(root);
    }

    // 12. MultiEditTool
    #[tokio::test]
    async fn multi_edit_applies_replacements() {
        let root = test_workspace("multi-edit");
        std::fs::write(root.join("code.rs"), "let x = 1;\nlet y = 2;\n").unwrap();
        let tool = MultiEditTool::new(&root);
        let result = tool
            .call(json!({
                "path": "code.rs",
                "edits": [
                    {"old": "let x = 1;", "new": "let x = 10;"},
                    {"old": "let y = 2;", "new": "let y = 20;"}
                ]
            }))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("\"applied\": 2"));
        let content = std::fs::read_to_string(root.join("code.rs")).unwrap();
        assert!(content.contains("let x = 10;"));
        assert!(content.contains("let y = 20;"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn multi_edit_empty_edits_error() {
        let root = test_workspace("multi-edit-empty");
        let tool = MultiEditTool::new(&root);
        let result = tool
            .call(json!({"path": "code.rs", "edits": []}))
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("must not be empty"));
        let _ = std::fs::remove_dir_all(root);
    }

    // 13. TreeTool
    #[tokio::test]
    async fn tree_tool_shows_structure() {
        let root = test_workspace("tree-tool");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("README.md"), "# hello").unwrap();

        let tool = TreeTool::new(&root);
        let result = tool.call(json!({"depth": 2})).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("src"));
        assert!(result.content.contains("main.rs"));
        assert!(result.content.contains("README.md"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn tree_tool_nonexistent_dir_error() {
        let root = test_workspace("tree-bad");
        let tool = TreeTool::new(&root);
        let result = tool.call(json!({"path": "does_not_exist"})).await.unwrap();
        assert!(result.is_error);
        let _ = std::fs::remove_dir_all(root);
    }

    // 14. DiagnosticsTool
    #[tokio::test]
    async fn diagnostics_no_project_error() {
        let root = test_workspace("diag-none");
        let tool = DiagnosticsTool::new(&root);
        let result = tool.call(json!({})).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("could not detect project type"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn diagnostics_invalid_input() {
        let root = test_workspace("diag-input");
        let tool = DiagnosticsTool::new(&root);
        let result = tool.call(json!({"path": 12345})).await.unwrap();
        assert!(result.is_error);
        let _ = std::fs::remove_dir_all(root);
    }

    // 15. ContextTool
    #[tokio::test]
    async fn context_tool_returns_usage() {
        let tool = ContextTool::new(128_000);
        tool.set_used_tokens(64_000).await;
        let result = tool.call(json!({})).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("50.0%"));
        assert!(result.content.contains("medium"));
    }

    #[tokio::test]
    async fn context_tool_empty_usage() {
        let tool = ContextTool::new(100_000);
        let result = tool.call(json!({})).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("0.0%"));
        assert!(result.content.contains("low"));
    }

    // ── Tool edge case tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_read_empty_file() {
        let root = test_workspace("read-empty");
        let registry = default_registry_with_root(&root);
        std::fs::write(root.join("empty.txt"), "").unwrap();

        let result = registry
            .execute("read_file", json!({"path": "empty.txt"}))
            .await
            .unwrap();
        assert!(!result.is_error, "reading empty file should succeed");
        // Content may contain line numbers or be empty — just shouldn't error
        // The actual content representation depends on line numbering logic
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn test_write_creates_parent_dirs() {
        let root = test_workspace("write-parents");
        let registry = default_registry_with_root(&root);

        let result = registry
            .execute(
                "write_file",
                json!({"path": "deep/nested/dir/file.txt", "content": "deep content"}),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "write should create parent dirs");

        let content = std::fs::read_to_string(root.join("deep/nested/dir/file.txt")).unwrap();
        assert_eq!(content, "deep content");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn test_edit_nonexistent_file_error() {
        let root = test_workspace("edit-nonexist");
        let registry = default_registry_with_root(&root);

        let result = registry
            .execute(
                "edit_file",
                json!({"path": "does_not_exist.txt", "old_str": "old", "new_str": "new"}),
            )
            .await
            .unwrap();
        assert!(
            result.is_error,
            "editing nonexistent file should return error"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn test_grep_no_matches() {
        let root = test_workspace("grep-nomatch");
        let registry = default_registry_with_root(&root);
        std::fs::write(root.join("sample.txt"), "hello world").unwrap();

        let result = registry
            .execute("grep_search", json!({"pattern": "zzz_nonexistent_pattern"}))
            .await
            .unwrap();
        assert!(
            !result.is_error,
            "grep with no matches should succeed, not error"
        );
        // Content should be empty or indicate no matches
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn test_glob_no_matches() {
        let root = test_workspace("glob-nomatch");
        let registry = default_registry_with_root(&root);
        std::fs::write(root.join("hello.txt"), "data").unwrap();

        let result = registry
            .execute("glob_search", json!({"pattern": "**/*.zzz"}))
            .await
            .unwrap();
        assert!(
            !result.is_error,
            "glob with no matches should succeed with empty results"
        );
        // The content should indicate no matches
        assert!(
            result.content.contains("No matches")
                || result.content.is_empty()
                || result.content.contains("0"),
            "expected empty/no-match result, got: {}",
            &result.content[..result.content.len().min(200)]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn test_bash_timeout_enforced() {
        let root = test_workspace("bash-timeout");
        let registry = default_registry_with_root(&root);

        let result = registry
            .execute("bash", json!({"command": "sleep 100", "timeout": 1}))
            .await
            .unwrap();
        // The bash tool should return a result indicating timeout
        assert!(
            result.content.contains("timed out")
                || result.content.contains("timeout")
                || result.is_error,
            "sleep 100 with 1s timeout should indicate timeout, got: {}",
            &result.content[..result.content.len().min(300)]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn test_tool_unknown_name_error() {
        let registry = default_registry_with_root(std::env::current_dir().unwrap());
        let result = registry
            .execute("nonexistent_tool_xyz", json!({"foo": "bar"}))
            .await;
        assert!(result.is_err(), "unknown tool should return error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Unknown tool"),
            "expected 'Unknown tool' in error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_web_search_invalid_query() {
        let tool = WebSearchTool::new(Duration::from_secs(5));
        let result = tool.call(json!({"query": ""})).await.unwrap();
        assert!(
            result.is_error,
            "empty query should be handled gracefully as an error"
        );
    }

    // ── Feature #74: Tool Preset Reduction tests ─────────────────────────────

    #[test]
    fn test_preset_read_only_contents() {
        let tools = ToolPresets::read_only();
        assert!(tools.contains(&"Read".to_string()));
        assert!(tools.contains(&"Glob".to_string()));
        assert!(tools.contains(&"Grep".to_string()));
        assert!(tools.contains(&"Tree".to_string()));
        assert!(tools.contains(&"Diagnostics".to_string()));
        assert_eq!(tools.len(), 5);
    }

    #[test]
    fn test_preset_minimal_contains_read_only() {
        let minimal = ToolPresets::minimal();
        let read_only = ToolPresets::read_only();
        for t in &read_only {
            assert!(
                minimal.contains(t),
                "minimal should include read_only tool {t}"
            );
        }
        assert!(minimal.contains(&"Write".to_string()));
        assert!(minimal.contains(&"Edit".to_string()));
    }

    #[test]
    fn test_preset_standard_contains_minimal() {
        let standard = ToolPresets::standard();
        let minimal = ToolPresets::minimal();
        for t in &minimal {
            assert!(
                standard.contains(t),
                "standard should include minimal tool {t}"
            );
        }
        assert!(standard.contains(&"Bash".to_string()));
        assert!(standard.contains(&"WebSearch".to_string()));
        assert!(standard.contains(&"TodoWrite".to_string()));
    }

    #[test]
    fn test_preset_full_nonempty() {
        let full = ToolPresets::full();
        assert!(
            !full.is_empty(),
            "full preset should have at least one tool"
        );
    }

    #[test]
    fn test_get_preset_known() {
        assert!(ToolPresets::get_preset("read_only").is_some());
        assert!(ToolPresets::get_preset("minimal").is_some());
        assert!(ToolPresets::get_preset("standard").is_some());
        assert!(ToolPresets::get_preset("full").is_some());
    }

    #[test]
    fn test_get_preset_unknown() {
        assert!(ToolPresets::get_preset("superadmin").is_none());
    }

    #[test]
    fn test_list_presets_nonempty() {
        let presets = ToolPresets::list_presets();
        assert!(!presets.is_empty());
        for (name, desc) in &presets {
            assert!(!name.is_empty());
            assert!(!desc.is_empty());
        }
    }

    #[test]
    fn test_filter_from_preset_allow_deny() {
        let filter = ToolFilter::from_preset("read_only").unwrap();
        assert!(filter.is_allowed("Read"));
        assert!(filter.is_allowed("Grep"));
        assert!(!filter.is_allowed("Bash"));
        assert!(!filter.is_allowed("Write"));
        assert_eq!(filter.allowed_count(), 5);
    }

    #[test]
    fn test_filter_from_list() {
        let filter = ToolFilter::from_list(vec!["Foo".to_string(), "Bar".to_string()]);
        assert!(filter.is_allowed("Foo"));
        assert!(filter.is_allowed("Bar"));
        assert!(!filter.is_allowed("Baz"));
        assert_eq!(filter.allowed_count(), 2);
    }

    #[test]
    fn test_filter_from_unknown_preset() {
        let result = ToolFilter::from_preset("nonexistent_preset");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("nonexistent_preset"));
    }

    // ── Feature #157: Self-Verification tests ───────────────────────────────

    #[test]
    fn test_verifier_capture_log() {
        let mut verifier = SelfVerifier::new(PathBuf::from("."));
        verifier.capture_log("my-log", "some log content");
        assert_eq!(verifier.artifacts.len(), 1);
        assert_eq!(verifier.artifacts[0].name, "my-log");
        assert_eq!(verifier.artifacts[0].content, "some log content");
        assert!(matches!(
            verifier.artifacts[0].artifact_type,
            ArtifactType::Log
        ));
    }

    #[test]
    fn test_verifier_capture_diff() {
        let mut verifier = SelfVerifier::new(PathBuf::from("."));
        let diff = verifier.capture_diff("old line", "new line");
        assert!(diff.contains("-old line"));
        assert!(diff.contains("+new line"));
        assert_eq!(verifier.artifacts.len(), 1);
        assert!(matches!(
            verifier.artifacts[0].artifact_type,
            ArtifactType::Diff
        ));
    }

    #[test]
    fn test_verifier_generate_report() {
        let mut verifier = SelfVerifier::new(PathBuf::from("."));
        verifier.capture_log("app.log", "started");
        let report = verifier.generate_report();
        assert!(report.contains("Verification Report"));
        assert!(report.contains("app.log"));
        assert!(report.contains("started"));
        assert!(report.contains("Total artifacts: 1"));
    }

    #[test]
    fn test_verifier_all_passed_no_failures() {
        let mut verifier = SelfVerifier::new(PathBuf::from("."));
        verifier.capture_log("ok-log", "everything is fine");
        assert!(verifier.all_passed());
    }

    #[test]
    fn test_verifier_all_passed_with_failure_artifact() {
        let mut verifier = SelfVerifier::new(PathBuf::from("."));
        verifier.artifacts.push(VerificationArtifact {
            name: "test-run".to_string(),
            artifact_type: ArtifactType::TestOutput,
            content: "test foo ... FAILED".to_string(),
            timestamp: 0,
        });
        assert!(!verifier.all_passed());
    }

    #[test]
    fn test_verifier_run_tests_echo() {
        let mut verifier = SelfVerifier::new(PathBuf::from("."));
        let result = verifier.run_tests("echo hello").unwrap();
        assert!(result.passed);
        assert!(result.output.contains("hello"));
        // artifact captured
        assert_eq!(verifier.artifacts.len(), 1);
        assert!(matches!(
            verifier.artifacts[0].artifact_type,
            ArtifactType::TestOutput
        ));
    }

    #[test]
    fn test_verifier_run_tests_failing_command() {
        let mut verifier = SelfVerifier::new(PathBuf::from("."));
        let result = verifier.run_tests("false").unwrap();
        assert!(!result.passed);
    }

    // ── Feature #71: LSP Bridge Tool tests ─────────────────────────────────────

    #[test]
    fn lsp_bridge_new() {
        let tool = LspBridgeTool::new("rust-analyzer", "/workspace");
        assert_eq!(tool.server_command, "rust-analyzer");
        assert_eq!(tool.root_path, "/workspace");
        assert!(tool.capabilities.is_empty());
    }

    #[test]
    fn lsp_bridge_supports_capability() {
        let mut tool = LspBridgeTool::new("pylsp", "/proj");
        tool.capabilities.push(LspCapability::Hover);
        tool.capabilities.push(LspCapability::Diagnostics);
        assert!(tool.supports(&LspCapability::Hover));
        assert!(tool.supports(&LspCapability::Diagnostics));
        assert!(!tool.supports(&LspCapability::GotoDefinition));
    }

    #[test]
    fn lsp_bridge_format_request_is_json_rpc() {
        let tool = LspBridgeTool::new("ls", "/");
        let req = LspRequest {
            method: "textDocument/hover".to_string(),
            file: "/src/main.rs".to_string(),
            line: 10,
            character: 5,
        };
        let formatted = tool.format_request(&req);
        let val: serde_json::Value = serde_json::from_str(&formatted).unwrap();
        assert_eq!(val["jsonrpc"], "2.0");
        assert_eq!(val["method"], "textDocument/hover");
        assert_eq!(val["params"]["position"]["line"], 10);
        assert_eq!(val["params"]["position"]["character"], 5);
    }

    #[test]
    fn lsp_bridge_parse_response_ok() {
        let tool = LspBridgeTool::new("ls", "/");
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"type":"hover","content":"fn main()"}}"#;
        let resp = tool.parse_response(json).unwrap();
        assert_eq!(resp.result_type, "hover");
        assert_eq!(resp.content, "fn main()");
    }

    #[test]
    fn lsp_bridge_parse_response_invalid_json_errors() {
        let tool = LspBridgeTool::new("ls", "/");
        assert!(tool.parse_response("not json").is_err());
    }

    // ── Feature #75: Notebook Cell Tool tests ──────────────────────────────────

    fn sample_notebook_json() -> &'static str {
        concat!(
            r#"{"cells": ["#,
            r#"{"cell_type": "code", "source": "x = 1", "outputs": [], "execution_count": 1},"#,
            r#"{"cell_type": "markdown", "source": "Heading", "outputs": [], "execution_count": null}"#,
            r#"]}"#
        )
    }

    #[test]
    fn notebook_parse_cells() {
        let cells = NotebookCellTool::parse_notebook(sample_notebook_json()).unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].cell_type, CellType::Code);
        assert_eq!(cells[0].source, "x = 1");
        assert_eq!(cells[1].cell_type, CellType::Markdown);
    }

    #[test]
    fn notebook_parse_invalid_json_errors() {
        assert!(NotebookCellTool::parse_notebook("bad json").is_err());
    }

    #[test]
    fn notebook_get_cell() {
        let cells = NotebookCellTool::parse_notebook(sample_notebook_json()).unwrap();
        assert!(NotebookCellTool::get_cell(&cells, 0).is_some());
        assert!(NotebookCellTool::get_cell(&cells, 99).is_none());
    }

    #[test]
    fn notebook_edit_cell() {
        let mut cells = NotebookCellTool::parse_notebook(sample_notebook_json()).unwrap();
        NotebookCellTool::edit_cell(&mut cells, 0, "x = 42").unwrap();
        assert_eq!(cells[0].source, "x = 42");
    }

    #[test]
    fn notebook_edit_cell_out_of_bounds() {
        let mut cells = NotebookCellTool::parse_notebook(sample_notebook_json()).unwrap();
        assert!(NotebookCellTool::edit_cell(&mut cells, 99, "x").is_err());
    }

    #[test]
    fn notebook_insert_cell() {
        let mut cells = NotebookCellTool::parse_notebook(sample_notebook_json()).unwrap();
        let new_cell = NotebookCell {
            cell_type: CellType::Raw,
            source: "raw content".to_string(),
            outputs: vec![],
            execution_count: None,
        };
        NotebookCellTool::insert_cell(&mut cells, 1, new_cell).unwrap();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[1].cell_type, CellType::Raw);
    }

    #[test]
    fn notebook_insert_cell_out_of_bounds() {
        let mut cells = NotebookCellTool::parse_notebook(sample_notebook_json()).unwrap();
        let cell = NotebookCell {
            cell_type: CellType::Code,
            source: "".to_string(),
            outputs: vec![],
            execution_count: None,
        };
        assert!(NotebookCellTool::insert_cell(&mut cells, 99, cell).is_err());
    }

    #[test]
    fn notebook_delete_cell() {
        let mut cells = NotebookCellTool::parse_notebook(sample_notebook_json()).unwrap();
        NotebookCellTool::delete_cell(&mut cells, 0).unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].cell_type, CellType::Markdown);
    }

    #[test]
    fn notebook_delete_cell_out_of_bounds() {
        let mut cells = NotebookCellTool::parse_notebook(sample_notebook_json()).unwrap();
        assert!(NotebookCellTool::delete_cell(&mut cells, 5).is_err());
    }

    #[test]
    fn notebook_to_json_roundtrip() {
        let cells = NotebookCellTool::parse_notebook(sample_notebook_json()).unwrap();
        let json = NotebookCellTool::to_notebook_json(&cells);
        let reparsed = NotebookCellTool::parse_notebook(&json).unwrap();
        assert_eq!(reparsed.len(), 2);
        assert_eq!(reparsed[0].source, "x = 1");
    }
}
