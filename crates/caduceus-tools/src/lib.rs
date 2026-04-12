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

// ── Security analysis features ─────────────────────────────────────────────────

use caduceus_permissions::VulnSeverity;

// ── #218: SAST Vulnerability Scanner ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VulnCategory {
    Secrets,
    Injection,
    XSS,
    SQLi,
    SSRF,
    SSTI,
    IDOR,
    WeakCrypto,
    InsecureDeserialize,
    PathTraversal,
    AuthBypass,
}

pub struct SastRule {
    pub id: String,
    pub category: VulnCategory,
    pub pattern: String,
    pub description: String,
    pub severity: VulnSeverity,
    pub remediation: String,
}

pub struct SastFinding {
    pub rule_id: String,
    pub category: VulnCategory,
    pub severity: VulnSeverity,
    pub file: String,
    pub line: usize,
    pub snippet: String,
    pub description: String,
    pub remediation: String,
}

pub struct SastScanner {
    rules: Vec<SastRule>,
}

impl Default for SastScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl SastScanner {
    pub fn new() -> Self {
        Self {
            rules: Self::default_rules(),
        }
    }

    pub fn add_rule(&mut self, rule: SastRule) {
        self.rules.push(rule);
    }

    pub fn scan_content(&self, file: &str, content: &str) -> Vec<SastFinding> {
        let mut findings = Vec::new();
        for (i, line) in content.lines().enumerate() {
            let lineno = i + 1;
            let lower = line.to_lowercase();
            for rule in &self.rules {
                let pat_lower = rule.pattern.to_lowercase();
                if lower.contains(&pat_lower) || line.contains(&rule.pattern) {
                    findings.push(SastFinding {
                        rule_id: rule.id.clone(),
                        category: rule.category.clone(),
                        severity: rule.severity.clone(),
                        file: file.to_string(),
                        line: lineno,
                        snippet: line.trim().to_string(),
                        description: rule.description.clone(),
                        remediation: rule.remediation.clone(),
                    });
                }
            }
        }
        findings
    }

    pub fn scan_diff(&self, diff: &str) -> Vec<SastFinding> {
        let mut findings = Vec::new();
        let mut current_file = String::new();
        let mut line_num: usize = 0;
        for line in diff.lines() {
            if let Some(stripped) = line.strip_prefix("+++ b/") {
                current_file = stripped.to_string();
            } else if line.starts_with("@@ ") {
                if let Some(plus_part) = line.split('+').nth(1) {
                    let num_str = plus_part.split([',', ' ']).next().unwrap_or("0");
                    line_num = num_str.parse::<usize>().unwrap_or(1).saturating_sub(1);
                }
            } else if line.starts_with('+') && !line.starts_with("+++") {
                line_num += 1;
                let content_line = &line[1..];
                let lower = content_line.to_lowercase();
                for rule in &self.rules {
                    let pat_lower = rule.pattern.to_lowercase();
                    if lower.contains(&pat_lower) || content_line.contains(&rule.pattern) {
                        findings.push(SastFinding {
                            rule_id: rule.id.clone(),
                            category: rule.category.clone(),
                            severity: rule.severity.clone(),
                            file: current_file.clone(),
                            line: line_num,
                            snippet: content_line.trim().to_string(),
                            description: rule.description.clone(),
                            remediation: rule.remediation.clone(),
                        });
                    }
                }
            } else if line.starts_with(' ') {
                line_num += 1;
            }
        }
        findings
    }

    pub fn default_rules() -> Vec<SastRule> {
        vec![
            SastRule {
                id: "SEC-001".into(),
                category: VulnCategory::Secrets,
                pattern: "API_KEY=".into(),
                description: "Hardcoded API key detected".into(),
                severity: VulnSeverity::Critical,
                remediation: "Store API keys in environment variables or a secrets manager".into(),
            },
            SastRule {
                id: "SEC-002".into(),
                category: VulnCategory::Secrets,
                pattern: "PASSWORD=".into(),
                description: "Hardcoded password detected".into(),
                severity: VulnSeverity::Critical,
                remediation: "Never hardcode passwords; use environment variables".into(),
            },
            SastRule {
                id: "SEC-003".into(),
                category: VulnCategory::Secrets,
                pattern: "-----BEGIN RSA".into(),
                description: "Private RSA key material detected in source".into(),
                severity: VulnSeverity::Critical,
                remediation: "Remove private keys from source; use a secrets manager".into(),
            },
            SastRule {
                id: "SEC-004".into(),
                category: VulnCategory::Injection,
                pattern: "eval(".into(),
                description: "Use of eval() is a code injection risk".into(),
                severity: VulnSeverity::High,
                remediation: "Avoid eval(); use safe alternatives".into(),
            },
            SastRule {
                id: "SEC-005".into(),
                category: VulnCategory::Injection,
                pattern: "exec(".into(),
                description: "Use of exec() can execute arbitrary code".into(),
                severity: VulnSeverity::High,
                remediation: "Validate and sanitize inputs before exec(); prefer safe APIs".into(),
            },
            SastRule {
                id: "SEC-006".into(),
                category: VulnCategory::Injection,
                pattern: "system(".into(),
                description: "Shell command injection risk via system()".into(),
                severity: VulnSeverity::High,
                remediation: "Avoid system(); use parameterized APIs; sanitize inputs".into(),
            },
            SastRule {
                id: "SEC-007".into(),
                category: VulnCategory::XSS,
                pattern: "innerHTML".into(),
                description: "Direct innerHTML assignment may cause XSS".into(),
                severity: VulnSeverity::High,
                remediation: "Use textContent or sanitize HTML before assignment".into(),
            },
            SastRule {
                id: "SEC-008".into(),
                category: VulnCategory::Injection,
                pattern: ".raw(".into(),
                description: "Raw SQL/template usage detected; injection risk".into(),
                severity: VulnSeverity::High,
                remediation: "Use parameterized queries instead of raw string interpolation".into(),
            },
            SastRule {
                id: "SEC-009".into(),
                category: VulnCategory::SQLi,
                pattern: "SELECT".into(),
                description: "Potential SQL injection: raw SELECT statement detected".into(),
                severity: VulnSeverity::High,
                remediation: "Use parameterized queries or an ORM".into(),
            },
            SastRule {
                id: "SEC-010".into(),
                category: VulnCategory::SSRF,
                pattern: "requests.get(user_input)".into(),
                description: "SSRF risk: user-controlled URL passed to HTTP request".into(),
                severity: VulnSeverity::High,
                remediation: "Validate and allowlist URLs before making requests".into(),
            },
            SastRule {
                id: "SEC-011".into(),
                category: VulnCategory::InsecureDeserialize,
                pattern: "pickle.loads".into(),
                description: "Insecure deserialization via pickle.loads".into(),
                severity: VulnSeverity::High,
                remediation:
                    "Avoid pickle for untrusted data; use JSON or authenticated serialization"
                        .into(),
            },
            SastRule {
                id: "SEC-012".into(),
                category: VulnCategory::Injection,
                pattern: "yaml.load(".into(),
                description: "Unsafe YAML deserialization; use yaml.safe_load instead".into(),
                severity: VulnSeverity::High,
                remediation: "Replace yaml.load with yaml.safe_load".into(),
            },
            SastRule {
                id: "SEC-013".into(),
                category: VulnCategory::SSRF,
                pattern: "curl".into(),
                description: "curl command with potentially user-controlled URL".into(),
                severity: VulnSeverity::Medium,
                remediation: "Validate URLs and use allowlists for curl-based requests".into(),
            },
        ]
    }
}

// ── #219: Audit Scope Tool ─────────────────────────────────────────────────────

pub struct AuditScopeTool;

pub struct DiffFile {
    pub path: String,
    pub added_lines: Vec<(usize, String)>,
    pub removed_lines: Vec<(usize, String)>,
}

impl AuditScopeTool {
    pub fn is_valid_git_ref(ref_str: &str) -> bool {
        if ref_str.contains("..") {
            return false;
        }
        !ref_str.is_empty()
            && ref_str
                .chars()
                .all(|c| c.is_alphanumeric() || "-_./".contains(c))
    }

    pub fn get_diff_args(
        base: Option<&str>,
        head: Option<&str>,
    ) -> std::result::Result<Vec<String>, String> {
        let mut args = vec!["diff".to_string()];
        if let Some(b) = base {
            if !Self::is_valid_git_ref(b) {
                return Err(format!("Invalid git ref: {b}"));
            }
            args.push(b.to_string());
        }
        if let Some(h) = head {
            if !Self::is_valid_git_ref(h) {
                return Err(format!("Invalid git ref: {h}"));
            }
            args.push(h.to_string());
        }
        Ok(args)
    }

    pub fn parse_diff_files(diff: &str) -> Vec<DiffFile> {
        let mut files: Vec<DiffFile> = Vec::new();
        let mut current: Option<DiffFile> = None;
        let mut add_line: usize = 0;
        let mut rem_line: usize = 0;
        for line in diff.lines() {
            if let Some(stripped) = line.strip_prefix("+++ b/") {
                if let Some(f) = current.take() {
                    files.push(f);
                }
                current = Some(DiffFile {
                    path: stripped.to_string(),
                    added_lines: Vec::new(),
                    removed_lines: Vec::new(),
                });
                add_line = 0;
                rem_line = 0;
            } else if line.starts_with("@@ ") {
                let parts: Vec<&str> = line.split(' ').collect();
                if parts.len() >= 3 {
                    let rem_part = parts[1].trim_start_matches('-');
                    let add_part = parts[2].trim_start_matches('+');
                    rem_line = rem_part
                        .split(',')
                        .next()
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(1)
                        .saturating_sub(1);
                    add_line = add_part
                        .split(',')
                        .next()
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(1)
                        .saturating_sub(1);
                }
            } else if line.starts_with('+') && !line.starts_with("+++") {
                add_line += 1;
                if let Some(ref mut f) = current {
                    f.added_lines.push((add_line, line[1..].to_string()));
                }
            } else if line.starts_with('-') && !line.starts_with("---") {
                rem_line += 1;
                if let Some(ref mut f) = current {
                    f.removed_lines.push((rem_line, line[1..].to_string()));
                }
            } else if line.starts_with(' ') {
                add_line += 1;
                rem_line += 1;
            }
        }
        if let Some(f) = current.take() {
            files.push(f);
        }
        files
    }
}

// ── #220: Line Number Finder ───────────────────────────────────────────────────

pub struct LineNumberFinder;

pub struct SnippetLocation {
    pub start_line: usize,
    pub end_line: usize,
    pub start_col: usize,
}

impl LineNumberFinder {
    pub fn find_snippet(content: &str, snippet: &str) -> Option<SnippetLocation> {
        Self::find_all_snippets(content, snippet).into_iter().next()
    }

    pub fn find_all_snippets(content: &str, snippet: &str) -> Vec<SnippetLocation> {
        let mut results = Vec::new();
        let snippet_lines: Vec<&str> = snippet.lines().collect();
        let content_lines: Vec<&str> = content.lines().collect();
        if snippet_lines.is_empty() || content_lines.is_empty() {
            return results;
        }
        let snippet_line_count = snippet_lines.len();
        'outer: for (i, content_line) in content_lines.iter().enumerate() {
            if let Some(col) = content_line.find(snippet_lines[0]) {
                if snippet_line_count > 1 {
                    if i + snippet_line_count > content_lines.len() {
                        continue;
                    }
                    for (j, snippet_line) in snippet_lines.iter().enumerate().skip(1) {
                        if !content_lines[i + j].contains(snippet_line) {
                            continue 'outer;
                        }
                    }
                }
                results.push(SnippetLocation {
                    start_line: i + 1,
                    end_line: i + snippet_line_count,
                    start_col: col,
                });
            }
        }
        results
    }
}

// ── #222: Dependency Vulnerability Scanner ────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockFileType {
    Npm,
    Yarn,
    Pip,
    Gemfile,
    GoMod,
    CargoLock,
    Composer,
    Gradle,
}

pub struct DepVulnerability {
    pub package: String,
    pub version: String,
    pub cve_id: Option<String>,
    pub severity: VulnSeverity,
    pub description: String,
    pub fix_version: Option<String>,
}

pub struct DepScanner;

impl DepScanner {
    pub fn detect_lock_files(dir: &str) -> Vec<(String, LockFileType)> {
        let candidates: &[(&str, LockFileType)] = &[
            ("package-lock.json", LockFileType::Npm),
            ("yarn.lock", LockFileType::Yarn),
            ("requirements.txt", LockFileType::Pip),
            ("Pipfile.lock", LockFileType::Pip),
            ("Gemfile.lock", LockFileType::Gemfile),
            ("go.sum", LockFileType::GoMod),
            ("Cargo.lock", LockFileType::CargoLock),
            ("composer.lock", LockFileType::Composer),
            ("gradle.lockfile", LockFileType::Gradle),
        ];
        let mut found = Vec::new();
        for (name, kind) in candidates {
            let path = format!("{dir}/{name}");
            if std::path::Path::new(&path).exists() {
                found.push((path, kind.clone()));
            }
        }
        found
    }

    pub fn parse_osv_output(json: &str) -> Vec<DepVulnerability> {
        let mut vulns = Vec::new();
        let Ok(v) = serde_json::from_str::<Value>(json) else {
            return vulns;
        };
        let Some(results) = v.get("results").and_then(|r| r.as_array()) else {
            return vulns;
        };
        for result in results {
            let Some(pkgs) = result.get("packages").and_then(|p| p.as_array()) else {
                continue;
            };
            for pkg in pkgs {
                let package = pkg
                    .get("package")
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let version = pkg
                    .get("package")
                    .and_then(|p| p.get("version"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let Some(vuln_list) = pkg.get("vulnerabilities").and_then(|v| v.as_array()) else {
                    continue;
                };
                for vuln in vuln_list {
                    let id = vuln
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    let summary = vuln
                        .get("summary")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    let severity_str = vuln
                        .get("database_specific")
                        .and_then(|d| d.get("severity"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("MEDIUM");
                    let severity = match severity_str.to_uppercase().as_str() {
                        "CRITICAL" => VulnSeverity::Critical,
                        "HIGH" => VulnSeverity::High,
                        "LOW" => VulnSeverity::Low,
                        _ => VulnSeverity::Medium,
                    };
                    let fix_version = vuln
                        .get("affected")
                        .and_then(|a| a.as_array())
                        .and_then(|a| a.first())
                        .and_then(|a| a.get("ranges"))
                        .and_then(|r| r.as_array())
                        .and_then(|r| r.first())
                        .and_then(|r| r.get("events"))
                        .and_then(|e| e.as_array())
                        .and_then(|e| e.iter().find(|ev| ev.get("fixed").is_some()))
                        .and_then(|ev| ev.get("fixed"))
                        .and_then(|f| f.as_str())
                        .map(|s| s.to_string());
                    vulns.push(DepVulnerability {
                        package: package.clone(),
                        version: version.clone(),
                        cve_id: if id.starts_with("CVE") {
                            Some(id)
                        } else {
                            None
                        },
                        severity,
                        description: summary,
                        fix_version,
                    });
                }
            }
        }
        vulns
    }

    pub fn build_osv_command(lock_file: &str) -> Vec<String> {
        vec![
            "osv-scanner".to_string(),
            "--lockfile".to_string(),
            lock_file.to_string(),
        ]
    }
}

// ── #224: PII Flow Tracer ──────────────────────────────────────────────────────

pub struct PiiPattern {
    pub name: String,
    pub category: String,
    pub pattern: String,
}

pub struct PiiFlow {
    pub source: String,
    pub sink: String,
    pub pii_type: String,
    pub file: String,
    pub line: usize,
}

pub struct PiiTracer {
    pii_patterns: Vec<PiiPattern>,
}

impl Default for PiiTracer {
    fn default() -> Self {
        Self::new()
    }
}

impl PiiTracer {
    pub fn new() -> Self {
        Self {
            pii_patterns: vec![
                PiiPattern {
                    name: "email".into(),
                    category: "contact".into(),
                    pattern: "email".into(),
                },
                PiiPattern {
                    name: "ssn".into(),
                    category: "identity".into(),
                    pattern: "ssn".into(),
                },
                PiiPattern {
                    name: "phone".into(),
                    category: "contact".into(),
                    pattern: "phone".into(),
                },
                PiiPattern {
                    name: "creditCard".into(),
                    category: "financial".into(),
                    pattern: "credit_card".into(),
                },
                PiiPattern {
                    name: "apiKey".into(),
                    category: "credential".into(),
                    pattern: "api_key".into(),
                },
                PiiPattern {
                    name: "password".into(),
                    category: "credential".into(),
                    pattern: "password".into(),
                },
            ],
        }
    }

    pub fn trace_content(&self, file: &str, content: &str) -> Vec<PiiFlow> {
        let mut flows = Vec::new();
        for (i, line) in content.lines().enumerate() {
            let lineno = i + 1;
            if let Some(pii_type) = self.is_pii_source(line) {
                if Self::is_pii_sink(line) {
                    flows.push(PiiFlow {
                        source: line.trim().to_string(),
                        sink: line.trim().to_string(),
                        pii_type,
                        file: file.to_string(),
                        line: lineno,
                    });
                }
            }
        }
        flows
    }

    pub fn is_pii_source(&self, text: &str) -> Option<String> {
        let lower = text.to_lowercase();
        for pattern in &self.pii_patterns {
            if lower.contains(&pattern.pattern) {
                return Some(pattern.name.clone());
            }
        }
        None
    }

    pub fn is_pii_sink(text: &str) -> bool {
        let lower = text.to_lowercase();
        let sink_patterns = [
            "console.log",
            "logger.",
            "log.",
            "analytics.",
            "send(",
            "post(",
            "fetch(",
            "axios.",
            "http.",
        ];
        sink_patterns.iter().any(|p| lower.contains(p))
    }
}

// ── #226: Security Report Generator ───────────────────────────────────────────

pub struct SecurityReport {
    pub title: String,
    pub timestamp: u64,
    pub total_findings: usize,
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub findings: Vec<ReportFinding>,
}

pub struct ReportFinding {
    pub severity: String,
    pub category: String,
    pub location: String,
    pub description: String,
    pub evidence: String,
    pub remediation: String,
}

pub struct SecurityReportGenerator;

impl SecurityReportGenerator {
    pub fn generate_markdown(report: &SecurityReport) -> String {
        let mut md = format!("# {}\n\n", report.title);
        md.push_str(&format!("**Generated:** {}\n\n", report.timestamp));
        md.push_str(&format!("## Summary\n\n{}\n\n", Self::summary_line(report)));
        md.push_str(&format!(
            "**Total Findings:** {}\n\n",
            report.total_findings
        ));
        md.push_str("## Findings\n\n");
        for finding in &report.findings {
            md.push_str(&format!(
                "### [{severity}] {category} — {location}\n\n",
                severity = finding.severity,
                category = finding.category,
                location = finding.location,
            ));
            md.push_str(&format!("**Description:** {}\n\n", finding.description));
            md.push_str(&format!(
                "**Evidence:**\n```\n{}\n```\n\n",
                finding.evidence
            ));
            md.push_str(&format!("**Remediation:** {}\n\n", finding.remediation));
            md.push_str("---\n\n");
        }
        md
    }

    pub fn generate_json(report: &SecurityReport) -> String {
        serde_json::json!({
            "title": report.title,
            "timestamp": report.timestamp,
            "summary": {
                "total": report.total_findings,
                "critical": report.critical,
                "high": report.high,
                "medium": report.medium,
                "low": report.low,
            },
            "findings": report.findings.iter().map(|f| serde_json::json!({
                "severity": f.severity,
                "category": f.category,
                "location": f.location,
                "description": f.description,
                "evidence": f.evidence,
                "remediation": f.remediation,
            })).collect::<Vec<_>>(),
        })
        .to_string()
    }

    pub fn summary_line(report: &SecurityReport) -> String {
        format!(
            "{} Critical, {} High, {} Medium, {} Low",
            report.critical, report.high, report.medium, report.low
        )
    }
}

// ── #227: Crypto Weakness Detector ────────────────────────────────────────────

pub struct CryptoPattern {
    pub name: String,
    pub pattern: String,
    pub severity: VulnSeverity,
    pub alternative: String,
}

pub struct CryptoFinding {
    pub pattern_name: String,
    pub file: String,
    pub line: usize,
    pub severity: VulnSeverity,
    pub alternative: String,
}

pub struct CryptoWeaknessDetector {
    weak_patterns: Vec<CryptoPattern>,
}

impl Default for CryptoWeaknessDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl CryptoWeaknessDetector {
    pub fn new() -> Self {
        Self {
            weak_patterns: vec![
                CryptoPattern {
                    name: "DES".into(),
                    pattern: "DES".into(),
                    severity: VulnSeverity::Critical,
                    alternative: "AES-256-GCM".into(),
                },
                CryptoPattern {
                    name: "TripleDES".into(),
                    pattern: "TripleDES".into(),
                    severity: VulnSeverity::High,
                    alternative: "AES-256-GCM".into(),
                },
                CryptoPattern {
                    name: "3DES".into(),
                    pattern: "3DES".into(),
                    severity: VulnSeverity::High,
                    alternative: "AES-256-GCM".into(),
                },
                CryptoPattern {
                    name: "RC4".into(),
                    pattern: "RC4".into(),
                    severity: VulnSeverity::Critical,
                    alternative: "ChaCha20-Poly1305 or AES-256-GCM".into(),
                },
                CryptoPattern {
                    name: "ECB".into(),
                    pattern: "ECB".into(),
                    severity: VulnSeverity::High,
                    alternative: "AES-256-GCM (authenticated)".into(),
                },
                CryptoPattern {
                    name: "MD5".into(),
                    pattern: "MD5".into(),
                    severity: VulnSeverity::High,
                    alternative: "SHA-256 or SHA-3".into(),
                },
                CryptoPattern {
                    name: "SHA1".into(),
                    pattern: "SHA1".into(),
                    severity: VulnSeverity::Medium,
                    alternative: "SHA-256 or SHA-3".into(),
                },
                CryptoPattern {
                    name: "AES-128".into(),
                    pattern: "AES-128".into(),
                    severity: VulnSeverity::Low,
                    alternative: "AES-256".into(),
                },
            ],
        }
    }

    pub fn scan_content(&self, file: &str, content: &str) -> Vec<CryptoFinding> {
        let mut findings = Vec::new();
        for (i, line) in content.lines().enumerate() {
            let lineno = i + 1;
            for pattern in &self.weak_patterns {
                if line.contains(&pattern.pattern) {
                    findings.push(CryptoFinding {
                        pattern_name: pattern.name.clone(),
                        file: file.to_string(),
                        line: lineno,
                        severity: pattern.severity.clone(),
                        alternative: pattern.alternative.clone(),
                    });
                }
            }
        }
        findings
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

    // ── #218: SastScanner tests ────────────────────────────────────────────────

    #[test]
    fn sast_scanner_detects_hardcoded_api_key() {
        let scanner = SastScanner::new();
        let findings = scanner.scan_content("test.py", "API_KEY=supersecret123");
        assert!(!findings.is_empty());
        assert_eq!(findings[0].rule_id, "SEC-001");
        assert_eq!(findings[0].severity, VulnSeverity::Critical);
    }

    #[test]
    fn sast_scanner_detects_eval() {
        let scanner = SastScanner::new();
        let findings = scanner.scan_content("test.js", "eval(userInput)");
        assert!(findings.iter().any(|f| f.rule_id == "SEC-004"));
    }

    #[test]
    fn sast_scanner_detects_pickle_loads() {
        let scanner = SastScanner::new();
        let findings = scanner.scan_content("test.py", "data = pickle.loads(raw)");
        assert!(findings.iter().any(|f| f.rule_id == "SEC-011"));
    }

    #[test]
    fn sast_scanner_clean_content_returns_no_findings() {
        let scanner = SastScanner::new();
        let findings = scanner.scan_content("test.py", "def safe_fn():\n    return 42");
        assert!(findings.is_empty());
    }

    #[test]
    fn sast_scanner_scan_diff_only_added_lines() {
        let scanner = SastScanner::new();
        let diff = "--- a/app.py\n+++ b/app.py\n@@ -1,2 +1,3 @@\n context line\n+eval(user_input)\n-old_line\n";
        let findings = scanner.scan_diff(diff);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].file, "app.py");
    }

    #[test]
    fn sast_scanner_add_custom_rule() {
        let mut scanner = SastScanner::new();
        scanner.add_rule(SastRule {
            id: "CUSTOM-001".into(),
            category: VulnCategory::AuthBypass,
            pattern: "bypass_auth(".into(),
            description: "Auth bypass detected".into(),
            severity: VulnSeverity::Critical,
            remediation: "Fix authentication".into(),
        });
        let findings = scanner.scan_content("test.py", "bypass_auth(user)");
        assert!(findings.iter().any(|f| f.rule_id == "CUSTOM-001"));
    }

    #[test]
    fn sast_scanner_default_rules_not_empty() {
        let rules = SastScanner::default_rules();
        assert!(rules.len() >= 13);
    }

    // ── #219: AuditScopeTool tests ─────────────────────────────────────────────

    #[test]
    fn audit_scope_valid_git_refs() {
        assert!(AuditScopeTool::is_valid_git_ref("main"));
        assert!(AuditScopeTool::is_valid_git_ref("feature/my-branch"));
        assert!(AuditScopeTool::is_valid_git_ref("v1.2.3"));
        assert!(AuditScopeTool::is_valid_git_ref("abc123def456"));
    }

    #[test]
    fn audit_scope_invalid_git_refs() {
        assert!(!AuditScopeTool::is_valid_git_ref("main..HEAD"));
        assert!(!AuditScopeTool::is_valid_git_ref("ref with spaces"));
        assert!(!AuditScopeTool::is_valid_git_ref(""));
    }

    #[test]
    fn audit_scope_get_diff_args_both() {
        let args = AuditScopeTool::get_diff_args(Some("main"), Some("HEAD")).unwrap();
        assert_eq!(args, vec!["diff", "main", "HEAD"]);
    }

    #[test]
    fn audit_scope_get_diff_args_invalid_ref_errors() {
        let result = AuditScopeTool::get_diff_args(Some("main..evil"), None);
        assert!(result.is_err());
    }

    #[test]
    fn audit_scope_parse_diff_files() {
        let diff = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,2 +1,3 @@\n fn main() {}\n+let x = 1;\n-let y = 2;\n";
        let files = AuditScopeTool::parse_diff_files(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.rs");
        assert!(!files[0].added_lines.is_empty());
    }

    // ── #220: LineNumberFinder tests ───────────────────────────────────────────

    #[test]
    fn line_number_finder_finds_single_line_snippet() {
        let content = "line one\nline two\nline three";
        let loc = LineNumberFinder::find_snippet(content, "line two").unwrap();
        assert_eq!(loc.start_line, 2);
        assert_eq!(loc.end_line, 2);
    }

    #[test]
    fn line_number_finder_finds_all_occurrences() {
        let content = "foo\nbar\nfoo\nbaz";
        let locs = LineNumberFinder::find_all_snippets(content, "foo");
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[0].start_line, 1);
        assert_eq!(locs[1].start_line, 3);
    }

    #[test]
    fn line_number_finder_returns_none_for_missing() {
        let content = "hello world";
        assert!(LineNumberFinder::find_snippet(content, "not found").is_none());
    }

    #[test]
    fn line_number_finder_start_col_correct() {
        let content = "  let x = 1;";
        let loc = LineNumberFinder::find_snippet(content, "let x").unwrap();
        assert_eq!(loc.start_col, 2);
    }

    // ── #222: DepScanner tests ─────────────────────────────────────────────────

    #[test]
    fn dep_scanner_build_osv_command() {
        let cmd = DepScanner::build_osv_command("Cargo.lock");
        assert_eq!(cmd[0], "osv-scanner");
        assert!(cmd.contains(&"Cargo.lock".to_string()));
    }

    #[test]
    fn dep_scanner_parse_osv_output_empty_json() {
        let vulns = DepScanner::parse_osv_output("{}");
        assert!(vulns.is_empty());
    }

    #[test]
    fn dep_scanner_parse_osv_output_with_vuln() {
        let json = r#"{"results":[{"packages":[{"package":{"name":"lodash","version":"4.17.20"},"vulnerabilities":[{"id":"CVE-2021-23337","summary":"Prototype pollution","database_specific":{"severity":"HIGH"}}]}]}]}"#;
        let vulns = DepScanner::parse_osv_output(json);
        assert_eq!(vulns.len(), 1);
        assert_eq!(vulns[0].package, "lodash");
        assert_eq!(vulns[0].severity, VulnSeverity::High);
        assert_eq!(vulns[0].cve_id, Some("CVE-2021-23337".to_string()));
    }

    #[test]
    fn dep_scanner_detect_lock_files_nonexistent_dir() {
        let found = DepScanner::detect_lock_files("/nonexistent/path/xyz");
        assert!(found.is_empty());
    }

    // ── #224: PiiTracer tests ──────────────────────────────────────────────────

    #[test]
    fn pii_tracer_detects_email_source() {
        let tracer = PiiTracer::new();
        assert_eq!(
            tracer.is_pii_source("user.email = input"),
            Some("email".to_string())
        );
    }

    #[test]
    fn pii_tracer_detects_sink() {
        assert!(PiiTracer::is_pii_sink("console.log(userData)"));
        assert!(PiiTracer::is_pii_sink("analytics.track(event)"));
        assert!(!PiiTracer::is_pii_sink("let x = compute()"));
    }

    #[test]
    fn pii_tracer_trace_content_finds_flow() {
        let tracer = PiiTracer::new();
        let code = "let emailData = user.email;\nconsole.log(emailData);";
        let flows = tracer.trace_content("app.js", code);
        assert!(!flows.is_empty());
        assert_eq!(flows[0].pii_type, "email");
    }

    #[test]
    fn pii_tracer_no_flow_for_clean_code() {
        let tracer = PiiTracer::new();
        let code = "let x = compute_value();\nreturn x;";
        let flows = tracer.trace_content("app.js", code);
        assert!(flows.is_empty());
    }

    // ── #226: SecurityReportGenerator tests ───────────────────────────────────

    #[test]
    fn security_report_summary_line() {
        let report = SecurityReport {
            title: "Test Report".into(),
            timestamp: 0,
            total_findings: 11,
            critical: 3,
            high: 5,
            medium: 2,
            low: 1,
            findings: vec![],
        };
        assert_eq!(
            SecurityReportGenerator::summary_line(&report),
            "3 Critical, 5 High, 2 Medium, 1 Low"
        );
    }

    #[test]
    fn security_report_generate_markdown_contains_title() {
        let report = SecurityReport {
            title: "My Security Audit".into(),
            timestamp: 1234567890,
            total_findings: 1,
            critical: 1,
            high: 0,
            medium: 0,
            low: 0,
            findings: vec![ReportFinding {
                severity: "CRITICAL".into(),
                category: "Secrets".into(),
                location: "src/config.py:5".into(),
                description: "API key found".into(),
                evidence: "API_KEY=abc123".into(),
                remediation: "Use env vars".into(),
            }],
        };
        let md = SecurityReportGenerator::generate_markdown(&report);
        assert!(md.contains("# My Security Audit"));
        assert!(md.contains("CRITICAL"));
        assert!(md.contains("API_KEY=abc123"));
    }

    #[test]
    fn security_report_generate_json_is_valid() {
        let report = SecurityReport {
            title: "Test".into(),
            timestamp: 0,
            total_findings: 0,
            critical: 0,
            high: 0,
            medium: 0,
            low: 0,
            findings: vec![],
        };
        let json_str = SecurityReportGenerator::generate_json(&report);
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["title"], "Test");
        assert_eq!(parsed["summary"]["total"], 0);
    }

    // ── #227: CryptoWeaknessDetector tests ────────────────────────────────────

    #[test]
    fn crypto_detector_finds_des() {
        let detector = CryptoWeaknessDetector::new();
        let findings = detector.scan_content("crypto.py", "cipher = DES.new(key)");
        assert!(!findings.is_empty());
        assert_eq!(findings[0].severity, VulnSeverity::Critical);
        assert!(findings[0].alternative.contains("AES-256"));
    }

    #[test]
    fn crypto_detector_finds_md5() {
        let detector = CryptoWeaknessDetector::new();
        let findings = detector.scan_content("hash.py", "hashlib.MD5(data)");
        assert!(findings.iter().any(|f| f.pattern_name == "MD5"));
    }

    #[test]
    fn crypto_detector_finds_rc4() {
        let detector = CryptoWeaknessDetector::new();
        let findings = detector.scan_content("enc.py", "RC4.encrypt(data)");
        assert!(findings.iter().any(|f| f.pattern_name == "RC4"));
        assert_eq!(
            findings
                .iter()
                .find(|f| f.pattern_name == "RC4")
                .unwrap()
                .severity,
            VulnSeverity::Critical
        );
    }

    #[test]
    fn crypto_detector_finds_sha1() {
        let detector = CryptoWeaknessDetector::new();
        let findings = detector.scan_content("hash.py", "SHA1.digest(data)");
        assert!(findings.iter().any(|f| f.pattern_name == "SHA1"));
        assert_eq!(
            findings
                .iter()
                .find(|f| f.pattern_name == "SHA1")
                .unwrap()
                .severity,
            VulnSeverity::Medium
        );
    }

    #[test]
    fn crypto_detector_clean_content_no_findings() {
        let detector = CryptoWeaknessDetector::new();
        let findings = detector.scan_content("enc.py", "cipher = AES256_GCM.new(key, nonce)");
        assert!(findings.is_empty());
    }
}

// ── #238: Research-Guided Tasks ───────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ResearchQuery {
    pub query: String,
    pub category: String,
    pub priority: u8,
}

#[derive(Debug, Clone)]
pub struct ResearchResult {
    pub query: String,
    pub findings: Vec<String>,
    pub cached: bool,
}

pub struct ResearchGuide;

impl ResearchGuide {
    /// Generate research queries from a task description and optional context.
    pub fn generate_queries(task_description: &str, context: &str) -> Vec<ResearchQuery> {
        let stop_words = [
            "the", "a", "an", "is", "are", "and", "or", "to", "in", "of", "for", "with", "be",
            "at", "by", "on", "as",
        ];
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queries: Vec<ResearchQuery> = Vec::new();

        // Primary: full task description
        if !task_description.is_empty() && seen.insert(task_description.to_string()) {
            queries.push(ResearchQuery {
                query: task_description.to_string(),
                category: "primary".to_string(),
                priority: 9,
            });
        }

        // Secondary: bigrams from non-stop words
        let words: Vec<&str> = task_description
            .split_whitespace()
            .filter(|w| {
                let lower = w.to_lowercase();
                let stripped = lower.trim_matches(|c: char| !c.is_alphabetic());
                !stop_words.contains(&stripped)
            })
            .collect();

        for window in words.windows(2) {
            let q = window.join(" ");
            if q.len() > 5 && seen.insert(q.clone()) {
                queries.push(ResearchQuery {
                    query: q,
                    category: "secondary".to_string(),
                    priority: 5,
                });
            }
        }

        // Contextual: description + context
        if !context.is_empty() {
            let ctx_q = format!("{task_description} {context}");
            if seen.insert(ctx_q.clone()) {
                queries.push(ResearchQuery {
                    query: ctx_q,
                    category: "contextual".to_string(),
                    priority: 7,
                });
            }
        }

        queries
    }

    pub fn cache_result(
        results: &mut std::collections::HashMap<String, ResearchResult>,
        query: &str,
        findings: Vec<String>,
    ) {
        results.insert(
            query.to_string(),
            ResearchResult {
                query: query.to_string(),
                findings,
                cached: true,
            },
        );
    }

    pub fn get_cached<'a>(
        results: &'a std::collections::HashMap<String, ResearchResult>,
        query: &str,
    ) -> Option<&'a ResearchResult> {
        results.get(query)
    }
}

// ── #243: Cloud Resource Management ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CloudResource {
    pub id: String,
    pub name: String,
    pub resource_type: String,
    pub region: String,
    pub status: String,
    pub tags: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum ResourceAction {
    List(String),
    Create {
        resource_type: String,
        name: String,
        region: String,
    },
    Delete(String),
    Describe(String),
    Scale {
        resource: String,
        target: String,
    },
}

pub struct CloudResourceManager;

impl CloudResourceManager {
    pub fn supported_actions() -> Vec<&'static str> {
        vec!["list", "create", "delete", "describe", "scale"]
    }

    /// Parse natural-language cloud resource commands.
    pub fn parse_resource_request(natural_language: &str) -> Option<ResourceAction> {
        let lower = natural_language.to_lowercase();
        let words: Vec<&str> = natural_language.split_whitespace().collect();
        let lwords: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
        let first = lwords.first().map(String::as_str).unwrap_or("");

        if first == "list" || lower.starts_with("show all") || lower.starts_with("get all") {
            let rest = words.get(1..).map(|s| s.join(" ")).unwrap_or_default();
            let rt = if rest.is_empty() {
                "all".to_string()
            } else {
                rest
            };
            return Some(ResourceAction::List(rt));
        }

        if matches!(first, "create" | "provision" | "deploy") {
            let resource_type = words.get(1).copied().unwrap_or("resource").to_string();
            let named_pos = lwords.iter().position(|w| w == "named" || w == "called");
            let name = named_pos
                .and_then(|i| words.get(i + 1))
                .copied()
                .unwrap_or("default")
                .to_string();
            let in_pos = lwords.iter().position(|w| w == "in");
            let region = in_pos
                .and_then(|i| words.get(i + 1))
                .copied()
                .unwrap_or("us-east-1")
                .to_string();
            return Some(ResourceAction::Create {
                resource_type,
                name,
                region,
            });
        }

        if matches!(first, "delete" | "remove" | "destroy") {
            let id = words.get(1).copied().unwrap_or("unknown").to_string();
            return Some(ResourceAction::Delete(id));
        }

        if matches!(first, "describe" | "inspect") {
            let id = words.get(1).copied().unwrap_or("unknown").to_string();
            return Some(ResourceAction::Describe(id));
        }

        if lower.contains("scale") {
            let pos = lwords.iter().position(|w| w == "scale").unwrap_or(0);
            let resource = words.get(pos + 1).copied().unwrap_or("unknown").to_string();
            let to_pos = lwords.iter().position(|w| w == "to");
            let target = to_pos
                .and_then(|i| words.get(i + 1..))
                .map(|s| s.join(" "))
                .or_else(|| words.last().map(|w| (*w).to_string()))
                .unwrap_or_else(|| "1".to_string());
            return Some(ResourceAction::Scale { resource, target });
        }

        None
    }
}

// ── Tests for #238, #243 ──────────────────────────────────────────────────────

#[cfg(test)]
mod feature_tests_238_243 {
    use super::*;

    // ── #238 ResearchGuide ────────────────────────────────────────────────────

    #[test]
    fn research_generate_queries_primary() {
        let qs = ResearchGuide::generate_queries("implement OAuth2 login", "");
        assert!(!qs.is_empty());
        assert_eq!(qs[0].category, "primary");
        assert_eq!(qs[0].query, "implement OAuth2 login");
        assert_eq!(qs[0].priority, 9);
    }

    #[test]
    fn research_generate_queries_bigrams() {
        let qs = ResearchGuide::generate_queries("implement OAuth2 login flow", "");
        let categories: Vec<&str> = qs.iter().map(|q| q.category.as_str()).collect();
        assert!(categories.contains(&"secondary"));
    }

    #[test]
    fn research_generate_queries_contextual() {
        let qs = ResearchGuide::generate_queries("OAuth2 login", "TypeScript backend");
        let ctx = qs.iter().find(|q| q.category == "contextual");
        assert!(ctx.is_some());
        assert!(ctx.unwrap().query.contains("TypeScript"));
    }

    #[test]
    fn research_cache_and_retrieve() {
        let mut cache = std::collections::HashMap::new();
        ResearchGuide::cache_result(&mut cache, "OAuth2", vec!["RFC 6749".to_string()]);
        let result = ResearchGuide::get_cached(&cache, "OAuth2");
        assert!(result.is_some());
        assert!(result.unwrap().cached);
        assert_eq!(result.unwrap().findings[0], "RFC 6749");
    }

    #[test]
    fn research_get_cached_miss() {
        let cache = std::collections::HashMap::new();
        assert!(ResearchGuide::get_cached(&cache, "missing").is_none());
    }

    // ── #243 CloudResourceManager ─────────────────────────────────────────────

    #[test]
    fn cloud_parse_list() {
        let action = CloudResourceManager::parse_resource_request("list s3 buckets");
        assert!(matches!(action, Some(ResourceAction::List(rt)) if rt == "s3 buckets"));
    }

    #[test]
    fn cloud_parse_create() {
        let action =
            CloudResourceManager::parse_resource_request("create vm named myvm in us-west-2");
        match action {
            Some(ResourceAction::Create {
                resource_type,
                name,
                region,
            }) => {
                assert_eq!(resource_type, "vm");
                assert_eq!(name, "myvm");
                assert_eq!(region, "us-west-2");
            }
            _ => panic!("expected Create action"),
        }
    }

    #[test]
    fn cloud_parse_delete() {
        let action = CloudResourceManager::parse_resource_request("delete my-bucket");
        assert!(matches!(action, Some(ResourceAction::Delete(id)) if id == "my-bucket"));
    }

    #[test]
    fn cloud_parse_describe() {
        let action = CloudResourceManager::parse_resource_request("describe my-instance");
        assert!(matches!(action, Some(ResourceAction::Describe(id)) if id == "my-instance"));
    }

    #[test]
    fn cloud_parse_scale() {
        let action = CloudResourceManager::parse_resource_request("scale my-app to 5 replicas");
        match action {
            Some(ResourceAction::Scale { resource, target }) => {
                assert_eq!(resource, "my-app");
                assert!(target.contains("5"));
            }
            _ => panic!("expected Scale action"),
        }
    }

    #[test]
    fn cloud_parse_unknown_returns_none() {
        let action = CloudResourceManager::parse_resource_request("do something weird");
        assert!(action.is_none());
    }

    #[test]
    fn cloud_supported_actions() {
        let actions = CloudResourceManager::supported_actions();
        assert!(actions.contains(&"list"));
        assert!(actions.contains(&"create"));
        assert!(actions.contains(&"delete"));
        assert!(actions.contains(&"describe"));
        assert!(actions.contains(&"scale"));
    }
}

// ── #262: PlaybookScaffolder ──────────────────────────────────────────────────

fn to_title_case(value: &str) -> String {
    value
        .split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            if word
                .chars()
                .filter(|ch| ch.is_alphabetic())
                .all(|ch| ch.is_uppercase())
                && word.chars().any(|ch| ch.is_alphabetic())
            {
                word.to_string()
            } else {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn yaml_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

fn indent_block(value: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    value
        .lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn pretty_json(value: &Value) -> String {
    match serde_json::to_string_pretty(value) {
        Ok(content) => content,
        Err(_) => value.to_string(),
    }
}

fn workflow_job_id(name: &str) -> String {
    let mut id = String::new();

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | ' ') && !id.ends_with('_') {
            id.push('_');
        }
    }

    let trimmed = id.trim_matches('_');
    if trimmed.is_empty() {
        "workflow".to_string()
    } else {
        trimmed.to_string()
    }
}

fn registry_skill_scaffold(name: &str, description: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {}\n---\n\n# {}\n\n## When to Use\n- {}\n\n## Steps\n1. Gather the required context.\n2. Execute the requested task.\n3. Verify the result before returning.\n",
        yaml_quote(description),
        to_title_case(name),
        description,
    )
}

fn registry_agent_scaffold(name: &str, description: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {}\ntools: ['shell', 'read', 'edit', 'search']\n---\n\n# {}\n\nYou are a senior specialist for {}.\n\n## When Invoked\n1. Read the relevant context first.\n2. Plan before making changes.\n3. Validate the final result.\n",
        yaml_quote(description),
        to_title_case(name),
        description,
    )
}

fn registry_instructions_scaffold(name: &str, description: &str) -> String {
    format!(
        "# {} Instructions\n\n## Overview\n- Project: {name}\n- Focus: {description}\n\n## Commands\n- Build: `make build`\n- Test: `make test`\n- Lint: `make lint`\n\n## Rules\n- Keep changes focused on the requested scope.\n- Run validation commands before finishing.\n",
        to_title_case(name),
    )
}

fn registry_instructions_template(template: &str) -> String {
    match template {
        "rust" => "# Rust Instructions\n\n## Commands\n- Build: `cargo build`\n- Test: `cargo test`\n- Lint: `cargo clippy --all-targets --all-features -- -D warnings`\n\n## Rules\n- Format code with `cargo fmt`.\n- Prefer idiomatic ownership and error handling.\n".to_string(),
        "python" => "# Python Instructions\n\n## Commands\n- Build: `python -m build`\n- Test: `pytest`\n- Lint: `ruff check . && mypy .`\n\n## Rules\n- Keep functions small and typed where practical.\n- Add tests for new behavior.\n".to_string(),
        "typescript" => "# TypeScript Instructions\n\n## Commands\n- Build: `npm run build`\n- Test: `npm test`\n- Lint: `npm run lint`\n\n## Rules\n- Prefer strict typing.\n- Keep components and services focused.\n".to_string(),
        "react" => "# React Instructions\n\n## Commands\n- Build: `npm run build`\n- Test: `npm test`\n- Lint: `npm run lint`\n\n## Rules\n- Prefer functional components and hooks.\n- Cover user flows with Testing Library.\n".to_string(),
        "fullstack" => "# Fullstack Instructions\n\n## Commands\n- Backend: `cargo test`\n- Frontend: `npm test`\n- API Checks: `openapi lint api/openapi.yaml`\n\n## Rules\n- Keep contracts in sync across layers.\n- Verify migrations and API compatibility.\n".to_string(),
        _ => registry_instructions_scaffold(template, "Project-specific instructions."),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybookConfig {
    pub name: String,
    pub description: String,
    pub trigger: String,
    pub steps: Vec<PlaybookStep>,
    pub rollback_steps: Vec<String>,
    pub success_criteria: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybookStep {
    pub title: String,
    pub command: Option<String>,
    pub check: Option<String>,
    pub on_failure: String,
}

pub struct PlaybookScaffolder;

impl PlaybookScaffolder {
    pub fn generate(config: &PlaybookConfig) -> String {
        let mut output = format!(
            "---\nname: {}\ntrigger: {}\n---\n# {} Playbook\n\n{}\n\n## Pre-checks\n- [ ] Confirm prerequisites and approvals\n- [ ] Verify access, owners, and communication plan\n\n## Steps\n",
            config.name,
            yaml_quote(&config.trigger),
            to_title_case(&config.name),
            config.description,
        );

        if config.steps.is_empty() {
            output.push_str("_No steps defined._\n\n");
        } else {
            for (index, step) in config.steps.iter().enumerate() {
                output.push_str(&format!("### {}. {}\n", index + 1, step.title));
                if let Some(command) = &step.command {
                    output.push_str(&format!("```bash\n{command}\n```\n"));
                }
                if let Some(check) = &step.check {
                    output.push_str(&format!("**Check:** {check}\n"));
                }
                output.push_str(&format!("**On failure:** {}\n\n", step.on_failure));
            }
        }

        let rollback_steps = if config.rollback_steps.is_empty() {
            vec![
                "Stop at the last known good checkpoint".to_string(),
                "Notify the responsible team and document the rollback".to_string(),
            ]
        } else {
            config.rollback_steps.clone()
        };
        output.push_str("## Rollback\n");
        for (index, step) in rollback_steps.iter().enumerate() {
            output.push_str(&format!("{}. {}\n", index + 1, step));
        }
        output.push('\n');

        let success_criteria = if config.success_criteria.is_empty() {
            vec!["All steps completed without unresolved errors".to_string()]
        } else {
            config.success_criteria.clone()
        };
        output.push_str("## Success Criteria\n");
        for criterion in success_criteria {
            output.push_str(&format!("- {criterion}\n"));
        }

        output
    }

    pub fn quick_generate(name: &str, description: &str, steps: &[&str]) -> String {
        let config = PlaybookConfig {
            name: name.to_string(),
            description: description.to_string(),
            trigger: "on demand".to_string(),
            steps: steps
                .iter()
                .map(|step| PlaybookStep {
                    title: (*step).to_string(),
                    command: None,
                    check: None,
                    on_failure: "abort".to_string(),
                })
                .collect(),
            rollback_steps: vec![
                "Undo the last safe change".to_string(),
                "Escalate to the owning team".to_string(),
            ],
            success_criteria: vec!["All listed steps completed successfully".to_string()],
        };
        Self::generate(&config)
    }

    pub fn incident_response_template() -> String {
        Self::generate(&PlaybookConfig {
            name: "incident-response".to_string(),
            description: "Coordinate containment, diagnosis, and recovery during an incident."
                .to_string(),
            trigger: "on incident".to_string(),
            steps: vec![
                PlaybookStep {
                    title: "Acknowledge the incident and open a shared channel".to_string(),
                    command: Some("echo 'Open incident channel and page responders'".to_string()),
                    check: Some("Incident commander assigned".to_string()),
                    on_failure: "retry".to_string(),
                },
                PlaybookStep {
                    title: "Capture recent telemetry and error samples".to_string(),
                    command: Some("./scripts/collect-incident-context.sh".to_string()),
                    check: Some("Logs and metrics attached to the incident record".to_string()),
                    on_failure: "skip".to_string(),
                },
                PlaybookStep {
                    title: "Apply containment or rollback".to_string(),
                    command: Some("./scripts/rollback-last-release.sh".to_string()),
                    check: Some("Customer impact stops increasing".to_string()),
                    on_failure: "rollback".to_string(),
                },
            ],
            rollback_steps: vec![
                "Revert the last known bad change".to_string(),
                "Communicate status to stakeholders".to_string(),
            ],
            success_criteria: vec![
                "Service health is stable".to_string(),
                "Incident timeline captured for follow-up".to_string(),
            ],
        })
    }

    pub fn deployment_template() -> String {
        Self::generate(&PlaybookConfig {
            name: "deploy-production".to_string(),
            description: "Ship a production release with validation and rollback guidance."
                .to_string(),
            trigger: "on deploy to production".to_string(),
            steps: vec![
                PlaybookStep {
                    title: "Build release".to_string(),
                    command: Some("cargo build --release".to_string()),
                    check: Some("Build artifacts created successfully".to_string()),
                    on_failure: "abort".to_string(),
                },
                PlaybookStep {
                    title: "Run smoke tests".to_string(),
                    command: Some("./scripts/smoke-test.sh".to_string()),
                    check: Some("Smoke tests pass in production".to_string()),
                    on_failure: "rollback".to_string(),
                },
            ],
            rollback_steps: vec![
                "Revert to the previous deployment".to_string(),
                "Notify the team and update the deployment log".to_string(),
            ],
            success_criteria: vec![
                "All health checks are green".to_string(),
                "No error spike appears in logs".to_string(),
            ],
        })
    }

    pub fn onboarding_template() -> String {
        Self::generate(&PlaybookConfig {
            name: "team-onboarding".to_string(),
            description: "Guide a new teammate through environment setup and project orientation."
                .to_string(),
            trigger: "on onboarding".to_string(),
            steps: vec![
                PlaybookStep {
                    title: "Provision accounts and repository access".to_string(),
                    command: Some("echo 'Grant repo, CI, and cloud access'".to_string()),
                    check: Some("Access confirmed by the new teammate".to_string()),
                    on_failure: "retry".to_string(),
                },
                PlaybookStep {
                    title: "Set up the local development environment".to_string(),
                    command: Some("cargo test -p caduceus-tools".to_string()),
                    check: Some("Local build and tests succeed".to_string()),
                    on_failure: "abort".to_string(),
                },
                PlaybookStep {
                    title: "Review architecture, coding standards, and support process".to_string(),
                    command: None,
                    check: Some("New teammate can explain the main workflow".to_string()),
                    on_failure: "skip".to_string(),
                },
            ],
            rollback_steps: vec![
                "Pause onboarding and capture blockers".to_string(),
                "Schedule follow-up with the owning mentor".to_string(),
            ],
            success_criteria: vec![
                "Environment is ready for day-one contributions".to_string(),
                "Owner and escalation paths are understood".to_string(),
            ],
        })
    }
}

// ── #263: WorkflowScaffolder ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowConfig {
    pub name: String,
    pub trigger: WorkflowTrigger,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowTrigger {
    OnPush,
    OnPR,
    OnSchedule(String),
    OnManual,
    OnWebhook,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStep {
    pub name: String,
    pub run: String,
    pub condition: Option<String>,
}

pub struct WorkflowScaffolder;

impl WorkflowScaffolder {
    fn trigger_block(trigger: &WorkflowTrigger) -> String {
        match trigger {
            WorkflowTrigger::OnPush => "  push:\n".to_string(),
            WorkflowTrigger::OnPR => "  pull_request:\n".to_string(),
            WorkflowTrigger::OnSchedule(cron) => {
                format!("  schedule:\n    - cron: {}\n", yaml_quote(cron))
            }
            WorkflowTrigger::OnManual => "  workflow_dispatch:\n".to_string(),
            WorkflowTrigger::OnWebhook => "  repository_dispatch:\n".to_string(),
        }
    }

    pub fn generate(config: &WorkflowConfig) -> String {
        let mut output = format!(
            "name: {}\non:\n{}jobs:\n  {}:\n    runs-on: ubuntu-latest\n    steps:\n",
            config.name,
            Self::trigger_block(&config.trigger),
            workflow_job_id(&config.name),
        );

        let steps = if config.steps.is_empty() {
            vec![WorkflowStep {
                name: "Placeholder step".to_string(),
                run: "echo 'Add workflow steps here'".to_string(),
                condition: None,
            }]
        } else {
            config.steps.clone()
        };

        for step in steps {
            output.push_str(&format!("      - name: {}\n", step.name));
            if let Some(condition) = step.condition {
                output.push_str(&format!("        if: {condition}\n"));
            }
            if step.run.contains('\n') {
                output.push_str("        run: |\n");
                output.push_str(&indent_block(&step.run, 10));
                output.push('\n');
            } else {
                output.push_str(&format!("        run: {}\n", yaml_quote(&step.run)));
            }
        }

        output
    }

    pub fn ci_template(language: &str) -> String {
        let language = language.to_ascii_lowercase();
        let config = match language.as_str() {
            "python" => WorkflowConfig {
                name: "python-ci".to_string(),
                trigger: WorkflowTrigger::OnPR,
                steps: vec![
                    WorkflowStep {
                        name: "Install dependencies".to_string(),
                        run: "pip install -r requirements.txt".to_string(),
                        condition: None,
                    },
                    WorkflowStep {
                        name: "Run tests".to_string(),
                        run: "pytest".to_string(),
                        condition: None,
                    },
                ],
            },
            "typescript" => WorkflowConfig {
                name: "typescript-ci".to_string(),
                trigger: WorkflowTrigger::OnPR,
                steps: vec![
                    WorkflowStep {
                        name: "Install packages".to_string(),
                        run: "npm ci".to_string(),
                        condition: None,
                    },
                    WorkflowStep {
                        name: "Lint and test".to_string(),
                        run: "npm run lint\nnpm test".to_string(),
                        condition: None,
                    },
                ],
            },
            _ => WorkflowConfig {
                name: "rust-ci".to_string(),
                trigger: WorkflowTrigger::OnPR,
                steps: vec![
                    WorkflowStep {
                        name: "Format check".to_string(),
                        run: "cargo fmt --all --check".to_string(),
                        condition: None,
                    },
                    WorkflowStep {
                        name: "Lint".to_string(),
                        run: "cargo clippy --all-targets --all-features -- -D warnings".to_string(),
                        condition: None,
                    },
                    WorkflowStep {
                        name: "Test".to_string(),
                        run: "cargo test --all".to_string(),
                        condition: None,
                    },
                ],
            },
        };
        Self::generate(&config)
    }

    pub fn deploy_template(target: &str) -> String {
        let target = target.to_ascii_lowercase();
        let config = match target.as_str() {
            "k8s" => WorkflowConfig {
                name: "deploy-k8s".to_string(),
                trigger: WorkflowTrigger::OnManual,
                steps: vec![
                    WorkflowStep {
                        name: "Build image".to_string(),
                        run: "docker build -t app:latest .".to_string(),
                        condition: None,
                    },
                    WorkflowStep {
                        name: "Apply manifests".to_string(),
                        run: "kubectl apply -f k8s/".to_string(),
                        condition: Some("github.ref == 'refs/heads/main'".to_string()),
                    },
                ],
            },
            "vercel" => WorkflowConfig {
                name: "deploy-vercel".to_string(),
                trigger: WorkflowTrigger::OnManual,
                steps: vec![WorkflowStep {
                    name: "Deploy to Vercel".to_string(),
                    run: "vercel deploy --prod".to_string(),
                    condition: None,
                }],
            },
            _ => WorkflowConfig {
                name: "deploy-docker".to_string(),
                trigger: WorkflowTrigger::OnManual,
                steps: vec![
                    WorkflowStep {
                        name: "Build container".to_string(),
                        run: "docker build -t app:latest .".to_string(),
                        condition: None,
                    },
                    WorkflowStep {
                        name: "Push image".to_string(),
                        run: "docker push app:latest".to_string(),
                        condition: None,
                    },
                ],
            },
        };
        Self::generate(&config)
    }

    pub fn release_template() -> String {
        Self::generate(&WorkflowConfig {
            name: "release".to_string(),
            trigger: WorkflowTrigger::OnManual,
            steps: vec![
                WorkflowStep {
                    name: "Generate changelog".to_string(),
                    run: "git cliff -o CHANGELOG.md".to_string(),
                    condition: None,
                },
                WorkflowStep {
                    name: "Create tag".to_string(),
                    run: "git tag $VERSION && git push origin $VERSION".to_string(),
                    condition: Some("github.ref == 'refs/heads/main'".to_string()),
                },
            ],
        })
    }
}

// ── #264: PromptScaffolder ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptConfig {
    pub name: String,
    pub description: String,
    pub system_context: String,
    pub user_template: String,
    pub variables: Vec<(String, String)>,
    pub examples: Vec<(String, String)>,
}

pub struct PromptScaffolder;

impl PromptScaffolder {
    pub fn generate(config: &PromptConfig) -> String {
        let variables_yaml = if config.variables.is_empty() {
            "variables: []\n".to_string()
        } else {
            let entries = config
                .variables
                .iter()
                .map(|(name, description)| {
                    format!(
                        "  - name: {name}\n    description: {}",
                        yaml_quote(description)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("variables:\n{entries}\n")
        };

        let mut output = format!(
            "---\nname: {}\ndescription: {}\n{}---\n\n# {}\n\n{}\n\n## System Context\n{}\n",
            config.name,
            yaml_quote(&config.description),
            variables_yaml,
            to_title_case(&config.name),
            config.user_template.trim(),
            config.system_context.trim(),
        );

        if !config.examples.is_empty() {
            output.push_str("\n## Examples\n");
            for (index, (input, output_text)) in config.examples.iter().enumerate() {
                output.push_str(&format!(
                    "### Example {}\n**Input:** {}\n**Output:** {}\n",
                    index + 1,
                    input,
                    output_text,
                ));
            }
        }

        output
    }

    pub fn code_review_template() -> String {
        Self::generate(&PromptConfig {
            name: "code-review".to_string(),
            description: "Review code for quality, security, and best practices".to_string(),
            system_context: "You are a senior reviewer focused on correctness, security, and maintainability.".to_string(),
            user_template: "Review {{file_path}} with focus on {{focus_area}}.\n\n## Checklist\n- [ ] No security vulnerabilities\n- [ ] Error handling is complete\n- [ ] Tests cover edge cases".to_string(),
            variables: vec![
                ("file_path".to_string(), "Path to file to review".to_string()),
                (
                    "focus_area".to_string(),
                    "What to focus on (security, performance, readability)".to_string(),
                ),
            ],
            examples: vec![(
                "src/lib.rs / security".to_string(),
                "Highlights security issues, missing checks, and risky patterns.".to_string(),
            )],
        })
    }

    pub fn refactor_template() -> String {
        Self::generate(&PromptConfig {
            name: "refactor".to_string(),
            description: "Plan and execute safe refactors with minimal regressions".to_string(),
            system_context: "You are a senior engineer improving code structure without changing behavior.".to_string(),
            user_template: "Refactor {{target}} to improve {{goal}}.\n\n## Requirements\n- Preserve existing behavior\n- Keep public APIs stable unless explicitly requested\n- Call out follow-up cleanup opportunities".to_string(),
            variables: vec![
                ("target".to_string(), "Code path or component to refactor".to_string()),
                ("goal".to_string(), "Desired improvement such as readability or performance".to_string()),
            ],
            examples: vec![(
                "src/api.rs / readability".to_string(),
                "Breaks large functions into smaller helpers while preserving behavior.".to_string(),
            )],
        })
    }

    pub fn test_generation_template() -> String {
        Self::generate(&PromptConfig {
            name: "test-generation".to_string(),
            description: "Generate focused tests for new or existing behavior".to_string(),
            system_context: "You are a test engineer who prioritizes coverage for edge cases and regressions.".to_string(),
            user_template: "Write tests for {{function_name}} covering {{scenario}}.\n\n## Expectations\n- Include happy-path coverage\n- Add edge cases and failure modes\n- Prefer deterministic fixtures".to_string(),
            variables: vec![
                (
                    "function_name".to_string(),
                    "Function, module, or behavior to test".to_string(),
                ),
                (
                    "scenario".to_string(),
                    "Important scenario or edge case to emphasize".to_string(),
                ),
            ],
            examples: vec![(
                "parse_config / invalid env values".to_string(),
                "Adds tests for invalid inputs, defaults, and error messages.".to_string(),
            )],
        })
    }

    pub fn documentation_template() -> String {
        Self::generate(&PromptConfig {
            name: "documentation".to_string(),
            description: "Produce clear documentation for code, workflows, or systems".to_string(),
            system_context: "You are a technical writer who explains systems concisely and accurately.".to_string(),
            user_template: "Document {{symbol_name}} for {{audience}}.\n\n## Deliverable\n- Start with a concise summary\n- Include usage notes or examples\n- Mention important constraints or failure modes".to_string(),
            variables: vec![
                (
                    "symbol_name".to_string(),
                    "Function, module, service, or workflow to document".to_string(),
                ),
                (
                    "audience".to_string(),
                    "Primary audience such as maintainers, users, or operators".to_string(),
                ),
            ],
            examples: vec![(
                "ToolRegistry / maintainers".to_string(),
                "Explains responsibilities, extension points, and common pitfalls.".to_string(),
            )],
        })
    }

    pub fn bug_fix_template() -> String {
        Self::generate(&PromptConfig {
            name: "bug-fix".to_string(),
            description: "Investigate a defect, identify root cause, and propose a safe fix".to_string(),
            system_context: "You are a debugger who prioritizes root-cause analysis, validation, and rollback safety.".to_string(),
            user_template: "Investigate {{error_message}} in {{component}}.\n\n## Deliverable\n- Explain the likely root cause\n- Propose the smallest safe fix\n- Identify validation steps and regression risks".to_string(),
            variables: vec![
                (
                    "error_message".to_string(),
                    "Observed failure, log message, or symptom".to_string(),
                ),
                (
                    "component".to_string(),
                    "File, service, or subsystem involved".to_string(),
                ),
            ],
            examples: vec![(
                "timeout waiting for DB / background worker".to_string(),
                "Explains likely connection leak and adds targeted verification steps.".to_string(),
            )],
        })
    }
}

// ── #265: HookScaffolder ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookConfig {
    pub name: String,
    pub phase: String,
    pub command: String,
    pub can_deny: bool,
}

pub struct HookScaffolder;

impl HookScaffolder {
    pub fn generate(config: &HookConfig) -> String {
        pretty_json(&json!({
            "name": config.name,
            "phase": config.phase,
            "command": config.command,
            "can_deny": config.can_deny,
        }))
    }

    pub fn pre_commit_template() -> String {
        Self::generate(&HookConfig {
            name: "pre-commit-guard".to_string(),
            phase: "pre_commit".to_string(),
            command: "cargo fmt --all && cargo test".to_string(),
            can_deny: true,
        })
    }

    pub fn post_tool_template() -> String {
        Self::generate(&HookConfig {
            name: "post-tool-notify".to_string(),
            phase: "post_tool".to_string(),
            command: "echo 'Tool execution finished'".to_string(),
            can_deny: false,
        })
    }

    pub fn lint_on_save_template() -> String {
        Self::generate(&HookConfig {
            name: "lint-on-save".to_string(),
            phase: "post_tool".to_string(),
            command: "npm run lint".to_string(),
            can_deny: false,
        })
    }
}

// ── #266: McpConfigScaffolder ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub server_type: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

pub struct McpConfigScaffolder;

impl McpConfigScaffolder {
    fn transport_type(server_type: &str) -> &'static str {
        let server_type = server_type.to_ascii_lowercase();
        match server_type.as_str() {
            "remote" => "http",
            _ => "stdio",
        }
    }

    pub fn generate(config: &McpServerConfig) -> String {
        let env = config
            .env
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect::<Map<String, Value>>();
        let args = config
            .args
            .iter()
            .cloned()
            .map(Value::String)
            .collect::<Vec<_>>();

        let mut server = Map::new();
        server.insert("command".to_string(), Value::String(config.command.clone()));
        server.insert("args".to_string(), Value::Array(args));
        server.insert("env".to_string(), Value::Object(env));
        server.insert(
            "type".to_string(),
            Value::String(Self::transport_type(&config.server_type).to_string()),
        );
        server.insert(
            "server_type".to_string(),
            Value::String(config.server_type.clone()),
        );

        let mut servers = Map::new();
        servers.insert(config.name.clone(), Value::Object(server));

        let mut root = Map::new();
        root.insert("mcpServers".to_string(), Value::Object(servers));
        pretty_json(&Value::Object(root))
    }

    pub fn filesystem_template() -> String {
        Self::generate(&McpServerConfig {
            name: "filesystem".to_string(),
            server_type: "local".to_string(),
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                ".".to_string(),
            ],
            env: vec![],
        })
    }

    pub fn database_template() -> String {
        Self::generate(&McpServerConfig {
            name: "database".to_string(),
            server_type: "docker".to_string(),
            command: "docker".to_string(),
            args: vec![
                "run".to_string(),
                "--rm".to_string(),
                "-i".to_string(),
                "mcp/database:latest".to_string(),
            ],
            env: vec![("DATABASE_URL".to_string(), "${DATABASE_URL}".to_string())],
        })
    }

    pub fn api_template() -> String {
        Self::generate(&McpServerConfig {
            name: "api".to_string(),
            server_type: "remote".to_string(),
            command: "mcp-remote-proxy".to_string(),
            args: vec!["https://api.example.com/mcp".to_string()],
            env: vec![("API_TOKEN".to_string(), "${API_TOKEN}".to_string())],
        })
    }
}

// ── #267: ScaffoldRegistry ────────────────────────────────────────────────────

pub struct ScaffoldRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldType {
    Skill,
    Agent,
    Instructions,
    Playbook,
    Workflow,
    Prompt,
    Hook,
    McpServer,
}

impl ScaffoldRegistry {
    pub fn available_types() -> Vec<(ScaffoldType, &'static str, &'static str)> {
        vec![
            (
                ScaffoldType::Skill,
                "skill",
                "Reusable SKILL.md capability scaffold",
            ),
            (
                ScaffoldType::Agent,
                "agent",
                "Reusable .agent.md specialist scaffold",
            ),
            (
                ScaffoldType::Instructions,
                "instructions",
                "Project-wide CADUCEUS.md instruction scaffold",
            ),
            (
                ScaffoldType::Playbook,
                "playbook",
                "Operational runbook and playbook scaffold",
            ),
            (
                ScaffoldType::Workflow,
                "workflow",
                "Automation workflow scaffold for CI/CD",
            ),
            (
                ScaffoldType::Prompt,
                "prompt",
                "Reusable prompt template scaffold",
            ),
            (ScaffoldType::Hook, "hook", "Lifecycle hook JSON scaffold"),
            (
                ScaffoldType::McpServer,
                "mcp-server",
                "MCP server configuration scaffold",
            ),
        ]
    }

    pub fn generate_quick(scaffold_type: ScaffoldType, name: &str, description: &str) -> String {
        match scaffold_type {
            ScaffoldType::Skill => registry_skill_scaffold(name, description),
            ScaffoldType::Agent => registry_agent_scaffold(name, description),
            ScaffoldType::Instructions => registry_instructions_scaffold(name, description),
            ScaffoldType::Playbook => PlaybookScaffolder::quick_generate(
                name,
                description,
                &[
                    "Review prerequisites",
                    "Execute the procedure",
                    "Validate the outcome",
                ],
            ),
            ScaffoldType::Workflow => WorkflowScaffolder::generate(&WorkflowConfig {
                name: name.to_string(),
                trigger: WorkflowTrigger::OnManual,
                steps: vec![WorkflowStep {
                    name: "Run task".to_string(),
                    run: format!("echo '{}'", description.replace('\'', "\\'")),
                    condition: None,
                }],
            }),
            ScaffoldType::Prompt => PromptScaffolder::generate(&PromptConfig {
                name: name.to_string(),
                description: description.to_string(),
                system_context: "You are a helpful specialist completing a reusable task."
                    .to_string(),
                user_template: format!("{description}\n\nInput: {{{{input}}}}"),
                variables: vec![(
                    "input".to_string(),
                    "Primary input for this prompt".to_string(),
                )],
                examples: vec![],
            }),
            ScaffoldType::Hook => HookScaffolder::generate(&HookConfig {
                name: name.to_string(),
                phase: "pre_tool".to_string(),
                command: format!("echo '{}'", description.replace('\'', "\\'")),
                can_deny: false,
            }),
            ScaffoldType::McpServer => McpConfigScaffolder::generate(&McpServerConfig {
                name: name.to_string(),
                server_type: "local".to_string(),
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-filesystem".to_string(),
                    ".".to_string(),
                ],
                env: vec![("DESCRIPTION".to_string(), description.to_string())],
            }),
        }
    }

    pub fn list_templates(scaffold_type: ScaffoldType) -> Vec<&'static str> {
        match scaffold_type {
            ScaffoldType::Skill => vec!["default"],
            ScaffoldType::Agent => vec!["default"],
            ScaffoldType::Instructions => {
                vec!["rust", "python", "typescript", "react", "fullstack"]
            }
            ScaffoldType::Playbook => vec!["incident-response", "deployment", "onboarding"],
            ScaffoldType::Workflow => vec![
                "ci-rust",
                "ci-python",
                "ci-typescript",
                "deploy-docker",
                "deploy-k8s",
                "deploy-vercel",
                "release",
            ],
            ScaffoldType::Prompt => {
                vec![
                    "code-review",
                    "refactor",
                    "test-generation",
                    "documentation",
                    "bug-fix",
                ]
            }
            ScaffoldType::Hook => vec!["pre-commit", "post-tool", "lint-on-save"],
            ScaffoldType::McpServer => vec!["filesystem", "database", "api"],
        }
    }

    pub fn generate_from_template(scaffold_type: ScaffoldType, template: &str) -> String {
        let template = template.to_ascii_lowercase();
        match scaffold_type {
            ScaffoldType::Skill => {
                registry_skill_scaffold("custom-skill", "Reusable skill scaffold.")
            }
            ScaffoldType::Agent => {
                registry_agent_scaffold("custom-agent", "Reusable agent scaffold.")
            }
            ScaffoldType::Instructions => registry_instructions_template(&template),
            ScaffoldType::Playbook => match template.as_str() {
                "incident-response" => PlaybookScaffolder::incident_response_template(),
                "deployment" => PlaybookScaffolder::deployment_template(),
                "onboarding" => PlaybookScaffolder::onboarding_template(),
                _ => PlaybookScaffolder::quick_generate(
                    &template,
                    "Custom operational playbook.",
                    &["Review context", "Execute steps", "Verify completion"],
                ),
            },
            ScaffoldType::Workflow => match template.as_str() {
                "ci-python" => WorkflowScaffolder::ci_template("python"),
                "ci-typescript" => WorkflowScaffolder::ci_template("typescript"),
                "deploy-docker" => WorkflowScaffolder::deploy_template("docker"),
                "deploy-k8s" => WorkflowScaffolder::deploy_template("k8s"),
                "deploy-vercel" => WorkflowScaffolder::deploy_template("vercel"),
                "release" => WorkflowScaffolder::release_template(),
                _ => WorkflowScaffolder::ci_template("rust"),
            },
            ScaffoldType::Prompt => match template.as_str() {
                "refactor" => PromptScaffolder::refactor_template(),
                "test-generation" => PromptScaffolder::test_generation_template(),
                "documentation" => PromptScaffolder::documentation_template(),
                "bug-fix" => PromptScaffolder::bug_fix_template(),
                _ => PromptScaffolder::code_review_template(),
            },
            ScaffoldType::Hook => match template.as_str() {
                "post-tool" => HookScaffolder::post_tool_template(),
                "lint-on-save" => HookScaffolder::lint_on_save_template(),
                _ => HookScaffolder::pre_commit_template(),
            },
            ScaffoldType::McpServer => match template.as_str() {
                "database" => McpConfigScaffolder::database_template(),
                "api" => McpConfigScaffolder::api_template(),
                _ => McpConfigScaffolder::filesystem_template(),
            },
        }
    }

    pub fn suggested_path(scaffold_type: ScaffoldType, name: &str) -> String {
        match scaffold_type {
            ScaffoldType::Skill => format!(".caduceus/skills/{name}/SKILL.md"),
            ScaffoldType::Agent => format!(".caduceus/agents/{name}.agent.md"),
            ScaffoldType::Instructions => "CADUCEUS.md".to_string(),
            ScaffoldType::Playbook => format!(".caduceus/playbooks/{name}.md"),
            ScaffoldType::Workflow => format!(".github/workflows/{name}.yml"),
            ScaffoldType::Prompt => format!(".caduceus/prompts/{name}.prompt.md"),
            ScaffoldType::Hook => format!(".caduceus/hooks/{name}.json"),
            ScaffoldType::McpServer => format!(".caduceus/mcp/{name}.json"),
        }
    }
}

// ── Tests for #262–#267 ───────────────────────────────────────────────────────

#[cfg(test)]
mod feature_tests_262_267 {
    use super::*;

    #[test]
    fn playbook_generate_contains_sections_and_steps() {
        let config = PlaybookConfig {
            name: "deploy-production".to_string(),
            description: "Deploy a production release safely.".to_string(),
            trigger: "on deploy to production".to_string(),
            steps: vec![
                PlaybookStep {
                    title: "Build release".to_string(),
                    command: Some("cargo build --release".to_string()),
                    check: Some("Artifacts built successfully".to_string()),
                    on_failure: "abort".to_string(),
                },
                PlaybookStep {
                    title: "Run smoke tests".to_string(),
                    command: Some("./scripts/smoke-test.sh".to_string()),
                    check: Some("Smoke tests pass".to_string()),
                    on_failure: "rollback".to_string(),
                },
            ],
            rollback_steps: vec![
                "Revert to previous version".to_string(),
                "Notify team".to_string(),
            ],
            success_criteria: vec![
                "All health checks green".to_string(),
                "No error spike in logs".to_string(),
            ],
        };

        let output = PlaybookScaffolder::generate(&config);
        assert!(output.contains("name: deploy-production"));
        assert!(output.contains("trigger: \"on deploy to production\""));
        assert!(output.contains("# Deploy Production Playbook"));
        assert!(output.contains("## Pre-checks"));
        assert!(output.contains("```bash\ncargo build --release\n```"));
        assert!(output.contains("**Check:** Smoke tests pass"));
        assert!(output.contains("**On failure:** rollback"));
        assert!(output.contains("## Rollback"));
        assert!(output.contains("## Success Criteria"));
    }

    #[test]
    fn playbook_quick_generate_and_templates_work() {
        let quick = PlaybookScaffolder::quick_generate(
            "incident-triage",
            "Handle a new production alert.",
            &["Assess impact", "Stabilize service"],
        );
        assert!(quick.contains("# Incident Triage Playbook"));
        assert!(quick.contains("### 1. Assess impact"));

        let incident = PlaybookScaffolder::incident_response_template();
        let deployment = PlaybookScaffolder::deployment_template();
        let onboarding = PlaybookScaffolder::onboarding_template();
        assert!(incident.contains("on incident"));
        assert!(deployment.contains("cargo build --release"));
        assert!(onboarding.contains("on onboarding"));
    }

    #[test]
    fn workflow_generate_renders_trigger_conditions_and_multiline_run() {
        let config = WorkflowConfig {
            name: "nightly-checks".to_string(),
            trigger: WorkflowTrigger::OnSchedule("0 0 * * *".to_string()),
            steps: vec![WorkflowStep {
                name: "Run nightly checks".to_string(),
                run: "cargo fmt --all --check\ncargo test --all".to_string(),
                condition: Some("github.ref == 'refs/heads/main'".to_string()),
            }],
        };

        let output = WorkflowScaffolder::generate(&config);
        assert!(output.contains("name: nightly-checks"));
        assert!(output.contains("schedule:"));
        assert!(output.contains("cron: \"0 0 * * *\""));
        assert!(output.contains("if: github.ref == 'refs/heads/main'"));
        assert!(output.contains("run: |"));
        assert!(output.contains("cargo test --all"));
    }

    #[test]
    fn workflow_templates_cover_ci_deploy_and_release() {
        let rust_ci = WorkflowScaffolder::ci_template("rust");
        let python_ci = WorkflowScaffolder::ci_template("python");
        let ts_ci = WorkflowScaffolder::ci_template("typescript");
        let docker = WorkflowScaffolder::deploy_template("docker");
        let k8s = WorkflowScaffolder::deploy_template("k8s");
        let vercel = WorkflowScaffolder::deploy_template("vercel");
        let release = WorkflowScaffolder::release_template();

        assert!(rust_ci.contains("cargo clippy"));
        assert!(python_ci.contains("pytest"));
        assert!(ts_ci.contains("npm ci"));
        assert!(docker.contains("docker push app:latest"));
        assert!(k8s.contains("kubectl apply -f k8s/"));
        assert!(vercel.contains("vercel deploy --prod"));
        assert!(release.contains("git cliff -o CHANGELOG.md"));
    }

    #[test]
    fn prompt_generate_contains_frontmatter_variables_examples_and_context() {
        let config = PromptConfig {
            name: "code-review".to_string(),
            description: "Review code for issues".to_string(),
            system_context: "You are a reviewer.".to_string(),
            user_template: "Review {{file_path}} carefully.".to_string(),
            variables: vec![(
                "file_path".to_string(),
                "Path to the file under review".to_string(),
            )],
            examples: vec![(
                "src/lib.rs".to_string(),
                "Highlights bugs and risks.".to_string(),
            )],
        };

        let output = PromptScaffolder::generate(&config);
        assert!(output.contains("name: code-review"));
        assert!(output.contains("description: \"Review code for issues\""));
        assert!(output.contains("- name: file_path"));
        assert!(output.contains("Review {{file_path}} carefully."));
        assert!(output.contains("## System Context"));
        assert!(output.contains("## Examples"));
    }

    #[test]
    fn prompt_templates_cover_common_tasks() {
        let review = PromptScaffolder::code_review_template();
        let refactor = PromptScaffolder::refactor_template();
        let tests = PromptScaffolder::test_generation_template();
        let docs = PromptScaffolder::documentation_template();
        let bug_fix = PromptScaffolder::bug_fix_template();

        assert!(review.contains("{{file_path}}"));
        assert!(review.contains("No security vulnerabilities"));
        assert!(refactor.contains("{{target}}"));
        assert!(tests.contains("{{function_name}}"));
        assert!(docs.contains("{{symbol_name}}"));
        assert!(bug_fix.contains("{{error_message}}"));
    }

    #[test]
    fn hook_generate_json_is_valid() {
        let config = HookConfig {
            name: "pre-commit-guard".to_string(),
            phase: "pre_commit".to_string(),
            command: "cargo test".to_string(),
            can_deny: true,
        };

        let output = HookScaffolder::generate(&config);
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["name"], "pre-commit-guard");
        assert_eq!(parsed["phase"], "pre_commit");
        assert_eq!(parsed["command"], "cargo test");
        assert_eq!(parsed["can_deny"], true);
    }

    #[test]
    fn hook_templates_cover_expected_phases() {
        let pre_commit: Value =
            serde_json::from_str(&HookScaffolder::pre_commit_template()).unwrap();
        let post_tool: Value = serde_json::from_str(&HookScaffolder::post_tool_template()).unwrap();
        let lint_on_save: Value =
            serde_json::from_str(&HookScaffolder::lint_on_save_template()).unwrap();

        assert_eq!(pre_commit["phase"], "pre_commit");
        assert_eq!(pre_commit["can_deny"], true);
        assert_eq!(post_tool["phase"], "post_tool");
        assert_eq!(lint_on_save["phase"], "post_tool");
    }

    #[test]
    fn mcp_generate_json_uses_standard_shape() {
        let config = McpServerConfig {
            name: "filesystem".to_string(),
            server_type: "local".to_string(),
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
            ],
            env: vec![("ROOT".to_string(), ".".to_string())],
        };

        let output = McpConfigScaffolder::generate(&config);
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["mcpServers"]["filesystem"]["command"], "npx");
        assert_eq!(parsed["mcpServers"]["filesystem"]["type"], "stdio");
        assert_eq!(parsed["mcpServers"]["filesystem"]["server_type"], "local");
        assert_eq!(parsed["mcpServers"]["filesystem"]["env"]["ROOT"], ".");
    }

    #[test]
    fn mcp_templates_cover_filesystem_database_and_api() {
        let filesystem: Value =
            serde_json::from_str(&McpConfigScaffolder::filesystem_template()).unwrap();
        let database: Value =
            serde_json::from_str(&McpConfigScaffolder::database_template()).unwrap();
        let api: Value = serde_json::from_str(&McpConfigScaffolder::api_template()).unwrap();

        assert_eq!(filesystem["mcpServers"]["filesystem"]["type"], "stdio");
        assert_eq!(database["mcpServers"]["database"]["server_type"], "docker");
        assert_eq!(api["mcpServers"]["api"]["type"], "http");
    }

    #[test]
    fn scaffold_registry_available_types_and_paths_match_requested_layout() {
        let available = ScaffoldRegistry::available_types();
        assert_eq!(available.len(), 8);
        assert!(available.iter().any(|(_, name, _)| *name == "playbook"));
        assert!(available.iter().any(|(_, name, _)| *name == "mcp-server"));

        assert_eq!(
            ScaffoldRegistry::suggested_path(ScaffoldType::Skill, "demo"),
            ".caduceus/skills/demo/SKILL.md"
        );
        assert_eq!(
            ScaffoldRegistry::suggested_path(ScaffoldType::Agent, "demo"),
            ".caduceus/agents/demo.agent.md"
        );
        assert_eq!(
            ScaffoldRegistry::suggested_path(ScaffoldType::Instructions, "ignored"),
            "CADUCEUS.md"
        );
        assert_eq!(
            ScaffoldRegistry::suggested_path(ScaffoldType::Playbook, "demo"),
            ".caduceus/playbooks/demo.md"
        );
        assert_eq!(
            ScaffoldRegistry::suggested_path(ScaffoldType::Workflow, "demo"),
            ".github/workflows/demo.yml"
        );
        assert_eq!(
            ScaffoldRegistry::suggested_path(ScaffoldType::Prompt, "demo"),
            ".caduceus/prompts/demo.prompt.md"
        );
        assert_eq!(
            ScaffoldRegistry::suggested_path(ScaffoldType::Hook, "demo"),
            ".caduceus/hooks/demo.json"
        );
        assert_eq!(
            ScaffoldRegistry::suggested_path(ScaffoldType::McpServer, "demo"),
            ".caduceus/mcp/demo.json"
        );
    }

    #[test]
    fn scaffold_registry_quick_generate_and_templates_cover_all_types() {
        for (scaffold_type, _, _) in ScaffoldRegistry::available_types() {
            let quick = ScaffoldRegistry::generate_quick(
                scaffold_type,
                "demo-scaffold",
                "Create a reusable scaffold",
            );
            assert!(!quick.is_empty());
            assert!(quick.contains("demo-scaffold") || quick.contains("reusable scaffold"));

            let templates = ScaffoldRegistry::list_templates(scaffold_type);
            assert!(!templates.is_empty());
            for template in templates {
                let generated = ScaffoldRegistry::generate_from_template(scaffold_type, template);
                assert!(
                    !generated.is_empty(),
                    "template {template} should generate content"
                );
            }
        }
    }
}
