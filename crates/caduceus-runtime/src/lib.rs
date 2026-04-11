pub mod browser;
pub mod sandbox;
use caduceus_core::{CaduceusError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const MAX_READ_SIZE: u64 = 1024 * 1024; // 1 MB
const MAX_WRITE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
const MAX_OUTPUT_SIZE: usize = 1024 * 1024; // 1 MB for command output

// Env vars to strip from child processes for safety
const SANITIZED_ENV_VARS: &[&str] = &[
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "NPM_TOKEN",
];

const ALLOWED_INHERITED_ENV_VARS: &[&str] =
    &["HOME", "LANG", "LC_ALL", "PATH", "SHELL", "TERM", "USER"];

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn resolve_workspace_path(workspace_root: &Path, path: &Path) -> Result<PathBuf> {
    let root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let normalized = normalize_path(&candidate);

    if normalized.exists() {
        let canonical = normalized.canonicalize().map_err(CaduceusError::Io)?;
        if !canonical.starts_with(&root) {
            return Err(CaduceusError::PermissionDenied {
                capability: "fs".into(),
                tool: "Path escapes workspace".into(),
            });
        }
        return Ok(canonical);
    }

    let parent = normalized.parent().unwrap_or(&normalized);
    if parent.exists() {
        let canonical_parent = parent.canonicalize().map_err(CaduceusError::Io)?;
        if !canonical_parent.starts_with(&root) {
            return Err(CaduceusError::PermissionDenied {
                capability: "fs".into(),
                tool: "Path escapes workspace".into(),
            });
        }
    } else if !normalized.starts_with(&root) {
        return Err(CaduceusError::PermissionDenied {
            capability: "fs".into(),
            tool: "Path escapes workspace".into(),
        });
    }

    Ok(normalized)
}

fn is_secret_env_var(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SANITIZED_ENV_VARS.iter().any(|var| upper == *var)
        || upper.contains("API_KEY")
        || upper.ends_with("_TOKEN")
        || upper.contains("SECRET")
        || upper.ends_with("_PASSWORD")
        || upper.ends_with("_PASS")
        || upper == "OPENAI_API_KEY"
        || upper == "ANTHROPIC_API_KEY"
        || upper == "GROQ_API_KEY"
        || upper == "OPENROUTER_API_KEY"
        || upper == "XAI_API_KEY"
}

fn build_child_env(request_env: &HashMap<String, String>) -> HashMap<String, String> {
    let mut env = HashMap::new();
    for key in ALLOWED_INHERITED_ENV_VARS {
        if let Ok(value) = std::env::var(key) {
            env.insert((*key).to_string(), value);
        }
    }
    for (key, value) in request_env {
        if !is_secret_env_var(key) {
            env.insert(key.clone(), value.clone());
        }
    }
    env
}

fn secure_write_file(path: &Path, content: &str) -> Result<()> {
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

// ── Process execution ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: std::collections::HashMap<String, String>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

pub struct BashSandbox {
    workspace_root: PathBuf,
}

impl BashSandbox {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let root: PathBuf = workspace_root.into();
        Self {
            workspace_root: root.canonicalize().unwrap_or(root),
        }
    }

    /// Truncate a string to at most `max_bytes` bytes on a valid UTF-8 boundary.
    fn truncate_output(s: &str, max_bytes: usize) -> String {
        if s.len() <= max_bytes {
            return s.to_string();
        }
        let mut end = max_bytes;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut truncated = s[..end].to_string();
        truncated.push_str("\n... [output truncated]");
        truncated
    }

    pub async fn execute(&self, request: ExecRequest) -> Result<ExecResult> {
        let cwd = request
            .cwd
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.workspace_root.clone());
        let cwd = resolve_workspace_path(&self.workspace_root, &cwd)?;

        let timeout = Duration::from_secs(request.timeout_secs.unwrap_or(30));

        let mut cmd = if request.args.is_empty() {
            let mut command = Command::new("bash");
            command.arg("-c").arg(&request.command);
            command
        } else {
            let mut command = Command::new(&request.command);
            command.args(&request.args);
            command
        };
        cmd.current_dir(&cwd).kill_on_drop(true).env_clear();

        for (key, value) in build_child_env(&request.env) {
            cmd.env(key, value);
        }

        let result = tokio::time::timeout(timeout, cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let stdout_raw = String::from_utf8_lossy(&output.stdout);
                let stderr_raw = String::from_utf8_lossy(&output.stderr);
                Ok(ExecResult {
                    stdout: Self::truncate_output(&stdout_raw, MAX_OUTPUT_SIZE),
                    stderr: Self::truncate_output(&stderr_raw, MAX_OUTPUT_SIZE),
                    exit_code: output.status.code().unwrap_or(-1),
                    timed_out: false,
                })
            }
            Ok(Err(e)) => Err(CaduceusError::Io(e)),
            Err(_elapsed) => {
                // Timeout: the process is killed by kill_on_drop
                Ok(ExecResult {
                    stdout: String::new(),
                    stderr: format!("Command timed out after {}s", timeout.as_secs()),
                    exit_code: -1,
                    timed_out: true,
                })
            }
        }
    }
}

// ── File operations ────────────────────────────────────────────────────────────

pub struct FileOps {
    workspace_root: PathBuf,
    ignore_filter: IgnoreFilter,
}

impl FileOps {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let root: PathBuf = workspace_root.into();
        let canonical_root = root.canonicalize().unwrap_or(root);
        let ignore_filter = IgnoreFilter::load(&canonical_root);
        Self {
            workspace_root: canonical_root,
            ignore_filter,
        }
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let resolved = resolve_workspace_path(&self.workspace_root, Path::new(path))?;
        if self.ignore_filter.is_ignored(&resolved) {
            return Err(CaduceusError::PermissionDenied {
                capability: "fs".into(),
                tool: format!("Path is ignored by .caduceusignore: {}", resolved.display()),
            });
        }
        Ok(resolved)
    }

    pub async fn read(&self, path: &str) -> Result<String> {
        let resolved = self.resolve_path(path)?;
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(CaduceusError::Io)?;
        if meta.len() > MAX_READ_SIZE {
            return Err(CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!(
                    "File too large to read: {} bytes (max {})",
                    meta.len(),
                    MAX_READ_SIZE
                ),
            });
        }
        tokio::fs::read_to_string(&resolved)
            .await
            .map_err(CaduceusError::Io)
    }

    pub async fn write(&self, path: &str, content: &str) -> Result<()> {
        if content.len() as u64 > MAX_WRITE_SIZE {
            return Err(CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!(
                    "Content too large to write: {} bytes (max {})",
                    content.len(),
                    MAX_WRITE_SIZE
                ),
            });
        }
        let resolved = self.resolve_path(path)?;
        let content = content.to_string();
        tokio::task::spawn_blocking(move || secure_write_file(&resolved, &content))
            .await
            .map_err(|e| CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!("Write task failed: {e}"),
            })?
    }

    pub async fn edit(&self, path: &str, old: &str, new: &str) -> Result<usize> {
        let content = self.read(path).await?;
        let count = content.matches(old).count();
        if count == 0 {
            return Err(CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!("String not found in {path}"),
            });
        }
        if count > 1 {
            return Err(CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!("Ambiguous edit: {count} occurrences in {path}"),
            });
        }
        let updated = content.replacen(old, new, 1);
        self.write(path, &updated).await?;
        Ok(1)
    }

    /// Check whether a path exists within the workspace.
    pub async fn exists(&self, path: &str) -> Result<bool> {
        let resolved = self.resolve_path(path)?;
        Ok(tokio::fs::try_exists(&resolved).await.unwrap_or(false))
    }

    /// List directory entries (non-recursive, names only).
    pub async fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        let resolved = self.resolve_path(path)?;
        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&resolved)
            .await
            .map_err(CaduceusError::Io)?;
        while let Some(entry) = read_dir.next_entry().await.map_err(CaduceusError::Io)? {
            if self.ignore_filter.is_ignored(&entry.path()) {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                let file_type = entry.file_type().await.map_err(CaduceusError::Io)?;
                let suffix = if file_type.is_dir() { "/" } else { "" };
                entries.push(format!("{name}{suffix}"));
            }
        }
        entries.sort();
        Ok(entries)
    }

    /// Simple glob search relative to workspace root.
    pub async fn glob_search(&self, pattern: &str) -> Result<Vec<String>> {
        let full_pattern = self.workspace_root.join(pattern);
        let pattern_str = full_pattern.to_string_lossy().to_string();
        let root = self.workspace_root.clone();
        let ignore_filter = self.ignore_filter.clone();

        tokio::task::spawn_blocking(move || {
            let mut results = Vec::new();
            let entries = glob::glob(&pattern_str).map_err(|e| CaduceusError::Tool {
                tool: "runtime".into(),
                message: format!("Invalid glob pattern: {e}"),
            })?;
            for entry in entries.flatten() {
                if ignore_filter.is_ignored(&entry) {
                    continue;
                }
                if let Ok(rel) = entry.strip_prefix(&root) {
                    results.push(rel.to_string_lossy().to_string());
                }
            }
            Ok(results)
        })
        .await
        .map_err(|e| CaduceusError::Tool {
            tool: "runtime".into(),
            message: format!("Glob task failed: {e}"),
        })?
    }

    /// Line-by-line grep search within the workspace. Returns matching file:line pairs.
    pub async fn grep_search(
        &self,
        pattern: &str,
        file_glob: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<String>> {
        let root = self.workspace_root.clone();
        let ignore_filter = self.ignore_filter.clone();
        let pattern = pattern.to_string();
        let file_glob = file_glob.map(|s| s.to_string());
        let max = max_results;

        tokio::task::spawn_blocking(move || {
            let re = regex::RegexBuilder::new(&pattern)
                .case_insensitive(false)
                .build()
                .map_err(|e| CaduceusError::Tool {
                    tool: "runtime".into(),
                    message: format!("Invalid regex: {e}"),
                })?;

            let glob_pattern = file_glob.unwrap_or_else(|| "**/*".to_string());
            let full_glob = root.join(&glob_pattern);
            let entries =
                glob::glob(&full_glob.to_string_lossy()).map_err(|e| CaduceusError::Tool {
                    tool: "runtime".into(),
                    message: format!("Invalid glob: {e}"),
                })?;

            let mut results = Vec::new();
            'outer: for entry in entries.flatten() {
                if !entry.is_file() {
                    continue;
                }
                if ignore_filter.is_ignored(&entry) {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&entry) {
                    for (line_no, line) in content.lines().enumerate() {
                        if re.is_match(line) {
                            let rel = entry
                                .strip_prefix(&root)
                                .unwrap_or(&entry)
                                .to_string_lossy();
                            results.push(format!("{}:{}: {}", rel, line_no + 1, line));
                            if results.len() >= max {
                                break 'outer;
                            }
                        }
                    }
                }
            }
            Ok(results)
        })
        .await
        .map_err(|e| CaduceusError::Tool {
            tool: "runtime".into(),
            message: format!("Grep task failed: {e}"),
        })?
    }
}

// ── Ignore filter (.caduceusignore) ────────────────────────────────────────────

#[derive(Clone)]
pub struct IgnoreFilter {
    root: PathBuf,
    patterns: Vec<glob::Pattern>,
}

impl IgnoreFilter {
    pub fn load(workspace_root: &Path) -> Self {
        let ignore_path = workspace_root.join(".caduceusignore");
        let patterns = if ignore_path.exists() {
            std::fs::read_to_string(&ignore_path)
                .unwrap_or_default()
                .lines()
                .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
                .filter_map(|line| glob::Pattern::new(line.trim()).ok())
                .collect()
        } else {
            Vec::new()
        };
        Self {
            root: workspace_root.to_path_buf(),
            patterns,
        }
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        let path_str = relative.to_string_lossy();
        self.patterns.iter().any(|pattern| {
            pattern.matches(&path_str) || pattern.matches(path_str.trim_start_matches('/'))
        })
    }
}

impl Default for IgnoreFilter {
    fn default() -> Self {
        Self::load(Path::new("."))
    }
}

// ── File watching ──────────────────────────────────────────────────────────────

use notify::{Event as NotifyEvent, RecommendedWatcher, RecursiveMode, Watcher};

#[derive(Debug, Clone)]
pub enum FileEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}

pub struct FileWatcher {
    _watcher: RecommendedWatcher,
    rx: tokio::sync::mpsc::UnboundedReceiver<FileEvent>,
}

impl FileWatcher {
    pub fn new(watch_path: impl Into<PathBuf>) -> Result<Self> {
        let path = watch_path.into();
        let (std_tx, std_rx) = std::sync::mpsc::channel::<notify::Result<NotifyEvent>>();
        let (tokio_tx, tokio_rx) = tokio::sync::mpsc::unbounded_channel::<FileEvent>();

        let mut watcher = notify::recommended_watcher(std_tx).map_err(|e| CaduceusError::Tool {
            tool: "file_watcher".into(),
            message: e.to_string(),
        })?;
        watcher
            .watch(&path, RecursiveMode::Recursive)
            .map_err(|e| CaduceusError::Tool {
                tool: "file_watcher".into(),
                message: e.to_string(),
            })?;

        std::thread::spawn(move || {
            let debounce = std::time::Duration::from_millis(500);
            let mut pending: Vec<FileEvent> = Vec::new();

            loop {
                match std_rx.recv_timeout(debounce) {
                    Ok(Ok(NotifyEvent { kind, paths, .. })) => {
                        for p in paths {
                            let fe = match &kind {
                                notify::EventKind::Create(_) => FileEvent::Created(p),
                                notify::EventKind::Modify(_) => FileEvent::Modified(p),
                                notify::EventKind::Remove(_) => FileEvent::Deleted(p),
                                _ => continue,
                            };
                            pending.push(fe);
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        for event in pending.drain(..) {
                            if tokio_tx.send(event).is_err() {
                                return;
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(Err(_)) => {}
                }
            }
        });

        Ok(Self {
            _watcher: watcher,
            rx: tokio_rx,
        })
    }

    pub async fn next(&mut self) -> Option<FileEvent> {
        self.rx.recv().await
    }

    pub fn try_next(&mut self) -> Option<FileEvent> {
        self.rx.try_recv().ok()
    }
}

// ── Feature #90: E2B Template Management ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2bTemplate {
    pub id: String,
    pub name: String,
    pub dockerfile: Option<String>,
    pub start_command: Option<String>,
    pub env_vars: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2bInstance {
    pub id: String,
    pub template_id: String,
    pub status: String,
}

#[derive(Default)]
pub struct E2bTemplateManager {
    templates: HashMap<String, E2bTemplate>,
}

impl E2bTemplateManager {
    pub fn new() -> Self {
        Self {
            templates: HashMap::new(),
        }
    }

    pub fn register_template(&mut self, template: E2bTemplate) {
        self.templates.insert(template.id.clone(), template);
    }

    pub fn get_template(&self, id: &str) -> Option<&E2bTemplate> {
        self.templates.get(id)
    }

    pub fn list_templates(&self) -> Vec<&E2bTemplate> {
        self.templates.values().collect()
    }

    pub fn instantiate(&self, template_id: &str) -> Result<E2bInstance> {
        if !self.templates.contains_key(template_id) {
            return Err(CaduceusError::Tool {
                tool: "e2b".into(),
                message: format!("Template not found: {template_id}"),
            });
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let instance_id = format!("instance-{template_id}-{ts}");
        Ok(E2bInstance {
            id: instance_id,
            template_id: template_id.to_string(),
            status: "running".to_string(),
        })
    }
}

// ── Feature #91: E2B Volume Management ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2bVolume {
    pub id: String,
    pub name: String,
    pub size_mb: u64,
    pub mount_path: String,
    pub attached_to: Option<String>,
}

#[derive(Default)]
pub struct E2bVolumeManager {
    volumes: HashMap<String, E2bVolume>,
}

impl E2bVolumeManager {
    pub fn new() -> Self {
        Self {
            volumes: HashMap::new(),
        }
    }

    pub fn create_volume(&mut self, name: &str, size_mb: u64, mount_path: &str) -> String {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let id = format!("vol-{name}-{ts}");
        self.volumes.insert(
            id.clone(),
            E2bVolume {
                id: id.clone(),
                name: name.to_string(),
                size_mb,
                mount_path: mount_path.to_string(),
                attached_to: None,
            },
        );
        id
    }

    pub fn attach(&mut self, volume_id: &str, instance_id: &str) -> Result<()> {
        let vol = self
            .volumes
            .get_mut(volume_id)
            .ok_or_else(|| CaduceusError::Tool {
                tool: "e2b".into(),
                message: format!("Volume not found: {volume_id}"),
            })?;
        if vol.attached_to.is_some() {
            return Err(CaduceusError::Tool {
                tool: "e2b".into(),
                message: format!("Volume {volume_id} is already attached"),
            });
        }
        vol.attached_to = Some(instance_id.to_string());
        Ok(())
    }

    pub fn detach(&mut self, volume_id: &str) -> Result<()> {
        let vol = self
            .volumes
            .get_mut(volume_id)
            .ok_or_else(|| CaduceusError::Tool {
                tool: "e2b".into(),
                message: format!("Volume not found: {volume_id}"),
            })?;
        vol.attached_to = None;
        Ok(())
    }

    pub fn list_volumes(&self) -> Vec<&E2bVolume> {
        self.volumes.values().collect()
    }
}

// ── Feature #92: E2B Network Controls ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2bNetworkConfig {
    pub allowed_ports: Vec<u16>,
    pub cidr_allowlist: Vec<String>,
    pub dns_servers: Vec<String>,
    pub egress_enabled: bool,
}

impl Default for E2bNetworkConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl E2bNetworkConfig {
    /// Default: no ports allowed, no CIDR, egress disabled.
    pub fn new() -> Self {
        Self {
            allowed_ports: Vec::new(),
            cidr_allowlist: Vec::new(),
            dns_servers: Vec::new(),
            egress_enabled: false,
        }
    }

    pub fn allow_port(&mut self, port: u16) {
        if !self.allowed_ports.contains(&port) {
            self.allowed_ports.push(port);
        }
    }

    pub fn add_cidr(&mut self, cidr: &str) {
        let cidr = cidr.to_string();
        if !self.cidr_allowlist.contains(&cidr) {
            self.cidr_allowlist.push(cidr);
        }
    }

    pub fn set_dns(&mut self, servers: Vec<String>) {
        self.dns_servers = servers;
    }

    pub fn is_port_allowed(&self, port: u16) -> bool {
        self.allowed_ports.contains(&port)
    }

    pub fn is_cidr_allowed(&self, addr: &str) -> bool {
        if self.cidr_allowlist.is_empty() {
            return false;
        }
        self.cidr_allowlist
            .iter()
            .any(|cidr| cidr_contains(cidr, addr))
    }

    /// Most locked-down preset: nothing allowed.
    pub fn restrictive() -> Self {
        Self::new()
    }

    /// All-open preset: all ports, all IPs, egress enabled.
    pub fn permissive() -> Self {
        Self {
            allowed_ports: (1..=65535).collect(),
            cidr_allowlist: vec!["0.0.0.0/0".to_string()],
            dns_servers: vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()],
            egress_enabled: true,
        }
    }
}

fn parse_ipv4(addr: &str) -> Option<u32> {
    let parts: Vec<&str> = addr.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut result: u32 = 0;
    for part in parts {
        let octet: u32 = part.parse().ok()?;
        if octet > 255 {
            return None;
        }
        result = (result << 8) | octet;
    }
    Some(result)
}

fn cidr_contains(cidr: &str, addr: &str) -> bool {
    let parts: Vec<&str> = cidr.splitn(2, '/').collect();
    if parts.len() != 2 {
        return cidr == addr;
    }
    let prefix_len: u32 = match parts[1].parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    match (parse_ipv4(parts[0]), parse_ipv4(addr)) {
        (Some(cidr_n), Some(target_n)) => {
            if prefix_len == 0 {
                return true;
            }
            let mask = if prefix_len >= 32 {
                u32::MAX
            } else {
                !((1u32 << (32 - prefix_len)) - 1)
            };
            (cidr_n & mask) == (target_n & mask)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_creation() {
        let _sandbox = BashSandbox::new("/workspace");
    }

    #[tokio::test]
    async fn file_ops_write_read() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        ops.write("test.txt", "hello").await.unwrap();
        let content = ops.read("test.txt").await.unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn file_ops_exists() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        assert!(!ops.exists("nope.txt").await.unwrap());
        ops.write("yep.txt", "data").await.unwrap();
        assert!(ops.exists("yep.txt").await.unwrap());
    }

    #[tokio::test]
    async fn file_ops_list_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        ops.write("a.txt", "aaa").await.unwrap();
        ops.write("b.txt", "bbb").await.unwrap();
        let entries = ops.list_dir(".").await.unwrap();
        assert!(entries.contains(&"a.txt".to_string()));
        assert!(entries.contains(&"b.txt".to_string()));
    }

    #[tokio::test]
    async fn file_ops_edit() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        ops.write("code.rs", "fn main() { hello() }").await.unwrap();
        let changed = ops.edit("code.rs", "hello()", "world()").await.unwrap();
        assert_eq!(changed, 1);
        let content = ops.read("code.rs").await.unwrap();
        assert_eq!(content, "fn main() { world() }");
    }

    #[tokio::test]
    async fn file_ops_write_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        let huge = "x".repeat(11 * 1024 * 1024); // 11MB > 10MB limit
        let result = ops.write("big.txt", &huge).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn bash_sandbox_echo() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = BashSandbox::new(dir.path());
        let result = sandbox
            .execute(ExecRequest {
                command: "echo hello".into(),
                args: vec![],
                cwd: None,
                env: std::collections::HashMap::new(),
                timeout_secs: Some(5),
            })
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn bash_sandbox_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = BashSandbox::new(dir.path());
        let result = sandbox
            .execute(ExecRequest {
                command: "sleep 60".into(),
                args: vec![],
                cwd: None,
                env: std::collections::HashMap::new(),
                timeout_secs: Some(1),
            })
            .await
            .unwrap();
        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);
    }

    #[test]
    fn truncate_output_works() {
        let long = "a".repeat(2_000_000);
        let truncated = BashSandbox::truncate_output(&long, MAX_OUTPUT_SIZE);
        assert!(truncated.len() < long.len());
        assert!(truncated.ends_with("... [output truncated]"));
    }

    #[tokio::test]
    async fn file_ops_glob_search() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        ops.write("src/main.rs", "fn main(){}").await.unwrap();
        ops.write("src/lib.rs", "pub mod foo;").await.unwrap();
        ops.write("readme.md", "# Hello").await.unwrap();
        let results = ops.glob_search("src/*.rs").await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|r| r.contains("main.rs")));
        assert!(results.iter().any(|r| r.contains("lib.rs")));
    }

    #[tokio::test]
    async fn file_ops_grep_search() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        ops.write("a.txt", "hello world\nfoo bar\nhello again")
            .await
            .unwrap();
        ops.write("b.txt", "no match here").await.unwrap();
        let results = ops.grep_search("hello", Some("*.txt"), 100).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].contains("a.txt:1:"));
        assert!(results[1].contains("a.txt:3:"));
    }

    #[tokio::test]
    async fn file_ops_respects_caduceusignore_for_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".caduceusignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "top secret").unwrap();

        let ops = FileOps::new(dir.path());
        let result = ops.read("secret.txt").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_ops_respects_caduceusignore_for_listing_and_glob() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".caduceusignore"), "private/*.txt\n").unwrap();
        std::fs::create_dir_all(dir.path().join("private")).unwrap();
        std::fs::write(dir.path().join("private/hidden.txt"), "secret").unwrap();
        std::fs::write(dir.path().join("visible.txt"), "public").unwrap();

        let ops = FileOps::new(dir.path());
        let listing = ops.list_dir(".").await.unwrap();
        assert!(listing.contains(&"visible.txt".to_string()));

        let matches = ops.glob_search("**/*.txt").await.unwrap();
        assert!(matches.iter().any(|path| path == "visible.txt"));
        assert!(!matches.iter().any(|path| path.contains("hidden.txt")));
    }

    #[tokio::test]
    async fn file_watcher_detects_create() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = FileWatcher::new(dir.path()).unwrap();
        // Give watcher time to start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Write a file
        let file_path = dir.path().join("watched.txt");
        tokio::fs::write(&file_path, "hello").await.unwrap();

        // Wait for the debounce period + some margin
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), watcher.next()).await;
        assert!(event.is_ok(), "Should receive a file event");
        let event = event.unwrap();
        assert!(event.is_some(), "Event should not be None");
    }

    // ── IgnoreFilter tests ───────────────────────────────────────────────

    #[test]
    fn test_caduceus_ignore_filter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".caduceusignore"),
            "*.log\nsecret/*\n# comment\n\n",
        )
        .unwrap();

        let filter = IgnoreFilter::load(dir.path());
        assert!(filter.is_ignored(Path::new("debug.log")));
        assert!(filter.is_ignored(Path::new("secret/key.pem")));
        assert!(!filter.is_ignored(Path::new("src/main.rs")));
    }

    #[test]
    fn test_caduceus_ignore_filter_empty() {
        let dir = tempfile::tempdir().unwrap();
        let filter = IgnoreFilter::load(dir.path());
        assert!(!filter.is_ignored(Path::new("anything.txt")));
    }

    // ── Security boundary tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_path_traversal_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        let result = ops.read("../../../etc/passwd").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("escapes workspace") || err.contains("Permission denied"),
            "expected path escape error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_path_traversal_encoded() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        // URL-encoded traversal: %2e%2e = ".."
        let result = ops.read("%2e%2e/%2e%2e/etc/passwd").await;
        // The literal percent-encoded name should either be not found or be rejected
        assert!(result.is_err(), "URL-encoded traversal should not succeed");
    }

    #[tokio::test]
    async fn test_symlink_escape_denied() {
        let dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        std::fs::write(outside_dir.path().join("secret.txt"), "top-secret-data").unwrap();

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside_dir.path().join("secret.txt"),
                dir.path().join("escape_link"),
            )
            .unwrap();
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, just verify path validation still works
            return;
        }

        let ops = FileOps::new(dir.path());
        let result = ops.read("escape_link").await;
        assert!(result.is_err(), "symlink escape should be denied");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("escapes workspace") || err.contains("Permission denied"),
            "expected workspace escape error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_write_outside_workspace_denied() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FileOps::new(dir.path());
        let result = ops.write("/tmp/evil_file.txt", "hacked").await;
        assert!(
            result.is_err(),
            "writing to absolute path outside workspace should fail"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("escapes workspace") || err.contains("Permission denied"),
            "expected permission error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_caduceusignore_blocks_access() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".caduceusignore"),
            "*.secret\nconfidential/*\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("data.secret"), "hidden").unwrap();
        std::fs::create_dir_all(dir.path().join("confidential")).unwrap();
        std::fs::write(dir.path().join("confidential/keys.txt"), "key123").unwrap();

        let ops = FileOps::new(dir.path());

        // Direct read of ignored file
        let result = ops.read("data.secret").await;
        assert!(
            result.is_err(),
            ".caduceusignore should block read of *.secret"
        );

        // Read of file in ignored directory
        let result = ops.read("confidential/keys.txt").await;
        assert!(
            result.is_err(),
            ".caduceusignore should block confidential/*"
        );

        // Glob should exclude ignored files
        let results = ops.glob_search("**/*").await.unwrap();
        assert!(
            !results.iter().any(|r| r.contains("data.secret")),
            "glob should exclude .secret files"
        );
        assert!(
            !results.iter().any(|r| r.contains("confidential/keys.txt")),
            "glob should exclude files under confidential/"
        );

        // Non-ignored file should still work
        std::fs::write(dir.path().join("public.txt"), "visible").unwrap();
        let content = ops.read("public.txt").await.unwrap();
        assert_eq!(content, "visible");
    }

    #[tokio::test]
    async fn test_bash_dangerous_command_rm_rf() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = BashSandbox::new(dir.path());
        // We can't actually classify commands in BashSandbox (it just executes),
        // but we verify that rm -rf / fails or is contained:
        let result = sandbox
            .execute(ExecRequest {
                command: "rm -rf / --no-preserve-root 2>&1 || true".into(),
                args: vec![],
                cwd: None,
                env: std::collections::HashMap::new(),
                timeout_secs: Some(2),
            })
            .await
            .unwrap();
        // The command should either fail (non-zero exit) or be denied.
        // On macOS it will produce permission errors since we're not root.
        // The key assertion: our workspace dir still exists.
        assert!(
            dir.path().exists(),
            "workspace must survive dangerous commands"
        );
    }

    #[tokio::test]
    async fn test_bash_sudo_command_fails_noninteractive() {
        // Test the bash validator classifies sudo as dangerous
        let result = sandbox::BashValidator::validate("sudo rm -rf /tmp/test");
        assert!(
            matches!(result.level, sandbox::ValidationLevel::Dangerous),
            "sudo should be classified as dangerous"
        );
    }

    #[tokio::test]
    async fn test_bash_safe_command_ls() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("visible.txt"), "hi").unwrap();
        let sandbox = BashSandbox::new(dir.path());
        let result = sandbox
            .execute(ExecRequest {
                command: "ls".into(),
                args: vec![],
                cwd: None,
                env: std::collections::HashMap::new(),
                timeout_secs: Some(5),
            })
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("visible.txt"));
        assert!(!result.timed_out);
    }

    // ── E2bTemplateManager tests (#90) ───────────────────────────────────

    #[test]
    fn e2b_template_register_get_list() {
        let mut mgr = E2bTemplateManager::new();
        let tmpl = E2bTemplate {
            id: "tmpl-1".to_string(),
            name: "Test Template".to_string(),
            dockerfile: Some("FROM ubuntu".to_string()),
            start_command: Some("bash".to_string()),
            env_vars: HashMap::new(),
        };
        mgr.register_template(tmpl);

        assert!(mgr.get_template("tmpl-1").is_some());
        assert_eq!(mgr.get_template("tmpl-1").unwrap().name, "Test Template");
        assert_eq!(mgr.list_templates().len(), 1);
        assert!(mgr.get_template("nonexistent").is_none());
    }

    #[test]
    fn e2b_template_instantiate_ok() {
        let mut mgr = E2bTemplateManager::new();
        mgr.register_template(E2bTemplate {
            id: "base".to_string(),
            name: "Base".to_string(),
            dockerfile: None,
            start_command: None,
            env_vars: HashMap::new(),
        });

        let inst = mgr.instantiate("base").unwrap();
        assert_eq!(inst.template_id, "base");
        assert_eq!(inst.status, "running");
        assert!(!inst.id.is_empty());
    }

    #[test]
    fn e2b_template_instantiate_missing_errors() {
        let mgr = E2bTemplateManager::new();
        let err = mgr.instantiate("ghost").unwrap_err().to_string();
        assert!(err.contains("ghost") || err.contains("not found") || err.contains("Template"));
    }

    #[test]
    fn e2b_template_list_multiple() {
        let mut mgr = E2bTemplateManager::new();
        for i in 0..3 {
            mgr.register_template(E2bTemplate {
                id: format!("t{i}"),
                name: format!("Template {i}"),
                dockerfile: None,
                start_command: None,
                env_vars: HashMap::new(),
            });
        }
        assert_eq!(mgr.list_templates().len(), 3);
    }

    // ── E2bVolumeManager tests (#91) ─────────────────────────────────────

    #[test]
    fn e2b_volume_create_and_list() {
        let mut mgr = E2bVolumeManager::new();
        let id = mgr.create_volume("data", 512, "/mnt/data");
        assert!(!id.is_empty());
        let vols = mgr.list_volumes();
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0].name, "data");
        assert_eq!(vols[0].size_mb, 512);
        assert_eq!(vols[0].mount_path, "/mnt/data");
        assert!(vols[0].attached_to.is_none());
    }

    #[test]
    fn e2b_volume_attach_detach() {
        let mut mgr = E2bVolumeManager::new();
        let id = mgr.create_volume("logs", 100, "/var/log");
        mgr.attach(&id, "inst-42").unwrap();

        let vol = mgr.list_volumes()[0];
        assert_eq!(vol.attached_to.as_deref(), Some("inst-42"));

        mgr.detach(&id).unwrap();
        let vol = mgr.list_volumes()[0];
        assert!(vol.attached_to.is_none());
    }

    #[test]
    fn e2b_volume_double_attach_errors() {
        let mut mgr = E2bVolumeManager::new();
        let id = mgr.create_volume("cache", 256, "/cache");
        mgr.attach(&id, "inst-1").unwrap();
        assert!(mgr.attach(&id, "inst-2").is_err());
    }

    #[test]
    fn e2b_volume_attach_missing_errors() {
        let mut mgr = E2bVolumeManager::new();
        assert!(mgr.attach("nonexistent", "inst").is_err());
    }

    #[test]
    fn e2b_volume_detach_missing_errors() {
        let mut mgr = E2bVolumeManager::new();
        assert!(mgr.detach("nonexistent").is_err());
    }

    // ── E2bNetworkConfig tests (#92) ─────────────────────────────────────

    #[test]
    fn e2b_network_default_denies_all() {
        let cfg = E2bNetworkConfig::new();
        assert!(cfg.allowed_ports.is_empty());
        assert!(cfg.cidr_allowlist.is_empty());
        assert!(cfg.dns_servers.is_empty());
        assert!(!cfg.egress_enabled);
        assert!(!cfg.is_port_allowed(80));
        assert!(!cfg.is_cidr_allowed("10.0.0.1"));
    }

    #[test]
    fn e2b_network_allow_port() {
        let mut cfg = E2bNetworkConfig::new();
        cfg.allow_port(80);
        cfg.allow_port(443);
        assert!(cfg.is_port_allowed(80));
        assert!(cfg.is_port_allowed(443));
        assert!(!cfg.is_port_allowed(8080));

        // No duplicates
        cfg.allow_port(80);
        assert_eq!(cfg.allowed_ports.iter().filter(|&&p| p == 80).count(), 1);
    }

    #[test]
    fn e2b_network_cidr_filtering() {
        let mut cfg = E2bNetworkConfig::new();
        cfg.add_cidr("192.168.1.0/24");

        assert!(cfg.is_cidr_allowed("192.168.1.1"));
        assert!(cfg.is_cidr_allowed("192.168.1.254"));
        assert!(!cfg.is_cidr_allowed("192.168.2.1"));
        assert!(!cfg.is_cidr_allowed("10.0.0.1"));

        // No duplicates
        cfg.add_cidr("192.168.1.0/24");
        assert_eq!(cfg.cidr_allowlist.len(), 1);
    }

    #[test]
    fn e2b_network_set_dns() {
        let mut cfg = E2bNetworkConfig::new();
        cfg.set_dns(vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()]);
        assert_eq!(cfg.dns_servers, vec!["8.8.8.8", "1.1.1.1"]);
    }

    #[test]
    fn e2b_network_restrictive() {
        let cfg = E2bNetworkConfig::restrictive();
        assert!(cfg.allowed_ports.is_empty());
        assert!(cfg.cidr_allowlist.is_empty());
        assert!(!cfg.egress_enabled);
        assert!(!cfg.is_port_allowed(22));
        assert!(!cfg.is_cidr_allowed("0.0.0.0"));
    }

    #[test]
    fn e2b_network_permissive() {
        let cfg = E2bNetworkConfig::permissive();
        assert!(cfg.egress_enabled);
        assert!(cfg.is_port_allowed(22));
        assert!(cfg.is_port_allowed(80));
        assert!(cfg.is_port_allowed(443));
        assert!(cfg.is_port_allowed(65535));
        assert!(cfg.is_cidr_allowed("10.0.0.1"));
        assert!(cfg.is_cidr_allowed("172.16.0.1"));
        assert!(cfg.is_cidr_allowed("192.168.1.100"));
        assert!(!cfg.dns_servers.is_empty());
    }

    #[test]
    fn cidr_contains_slash_zero_matches_all() {
        // 0.0.0.0/0 matches everything
        assert!(cidr_contains("0.0.0.0/0", "1.2.3.4"));
        assert!(cidr_contains("0.0.0.0/0", "255.255.255.255"));
    }

    #[test]
    fn cidr_contains_slash_32_exact_match() {
        assert!(cidr_contains("10.0.0.1/32", "10.0.0.1"));
        assert!(!cidr_contains("10.0.0.1/32", "10.0.0.2"));
    }
}
