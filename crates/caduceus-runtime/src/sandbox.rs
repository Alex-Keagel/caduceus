use async_trait::async_trait;
use caduceus_core::{CaduceusError, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

static LOCAL_SANDBOX_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub template: String,
    pub timeout_secs: u64,
    pub env_vars: HashMap<String, String>,
    pub cwd: Option<String>,
    pub lifetime_timeout_secs: Option<u64>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            template: "base".to_string(),
            timeout_secs: 300,
            env_vars: HashMap::new(),
            cwd: None,
            lifetime_timeout_secs: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

#[async_trait]
pub trait SandboxProvider: Send + Sync {
    async fn create(&self, config: SandboxConfig) -> Result<Box<dyn Sandbox>>;
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn exec(&self, command: &str, timeout_secs: u64) -> Result<ExecResult>;
    async fn write_file(&self, path: &str, content: &str) -> Result<()>;
    async fn read_file(&self, path: &str) -> Result<String>;
    async fn list_files(&self, path: &str) -> Result<Vec<String>>;
    async fn destroy(&self) -> Result<()>;
    fn id(&self) -> &str;
}

#[derive(Default)]
pub struct LocalSandboxProvider;

#[async_trait]
impl SandboxProvider for LocalSandboxProvider {
    async fn create(&self, config: SandboxConfig) -> Result<Box<dyn Sandbox>> {
        Ok(Box::new(LocalSandbox::new(config).await?))
    }
}

pub struct LocalSandbox {
    id: String,
    workspace_root: PathBuf,
    env_vars: HashMap<String, String>,
    cwd: Option<String>,
    destroyed: AtomicBool,
}

impl LocalSandbox {
    pub async fn new(config: SandboxConfig) -> Result<Self> {
        let id = format!(
            "local-{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            LOCAL_SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let workspace_root = std::env::temp_dir().join(format!("caduceus-sandbox-{id}"));
        tokio::fs::create_dir_all(&workspace_root).await?;

        if let Some(cwd) = &config.cwd {
            let cwd_path = if Path::new(cwd).is_absolute() {
                workspace_root.join(cwd.trim_start_matches('/'))
            } else {
                workspace_root.join(cwd)
            };
            tokio::fs::create_dir_all(cwd_path).await?;
        }

        Ok(Self {
            id,
            workspace_root,
            env_vars: config.env_vars,
            cwd: config.cwd,
            destroyed: AtomicBool::new(false),
        })
    }

    pub fn workspace_path(&self) -> &Path {
        &self.workspace_root
    }

    fn ensure_active(&self) -> Result<()> {
        if self.destroyed.load(Ordering::SeqCst) {
            return Err(CaduceusError::Tool {
                tool: "sandbox".into(),
                message: "sandbox is destroyed".into(),
            });
        }
        Ok(())
    }

    fn working_directory(&self) -> PathBuf {
        match &self.cwd {
            Some(cwd) if Path::new(cwd).is_absolute() => {
                self.workspace_root.join(cwd.trim_start_matches('/'))
            }
            Some(cwd) => self.workspace_root.join(cwd),
            None => self.workspace_root.clone(),
        }
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let raw = Path::new(path);
        let relative = if raw.is_absolute() {
            PathBuf::from(path.trim_start_matches('/'))
        } else {
            PathBuf::from(path)
        };

        for component in relative.components() {
            if matches!(component, Component::ParentDir) {
                return Err(CaduceusError::PermissionDenied {
                    capability: "fs".into(),
                    tool: "path escapes sandbox workspace".into(),
                });
            }
        }

        Ok(self.workspace_root.join(relative))
    }
}

#[async_trait]
impl Sandbox for LocalSandbox {
    async fn exec(&self, command: &str, timeout_secs: u64) -> Result<ExecResult> {
        self.ensure_active()?;

        let mut cmd = Command::new("bash");
        cmd.arg("-lc")
            .arg(command)
            .current_dir(self.working_directory())
            .kill_on_drop(true)
            .envs(&self.env_vars);

        match timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
            Ok(Ok(output)) => Ok(ExecResult {
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code().unwrap_or(-1),
                timed_out: false,
            }),
            Ok(Err(e)) => Err(CaduceusError::Io(e)),
            Err(_) => Ok(ExecResult {
                stdout: String::new(),
                stderr: format!("Command timed out after {timeout_secs}s"),
                exit_code: -1,
                timed_out: true,
            }),
        }
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        self.ensure_active()?;
        let resolved = self.resolve_path(path)?;
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(resolved, content).await?;
        Ok(())
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        self.ensure_active()?;
        let resolved = self.resolve_path(path)?;
        Ok(tokio::fs::read_to_string(resolved).await?)
    }

    async fn list_files(&self, path: &str) -> Result<Vec<String>> {
        self.ensure_active()?;
        let resolved = self.resolve_path(path)?;
        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(resolved).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            if let Some(name) = entry.file_name().to_str() {
                entries.push(name.to_string());
            }
        }
        entries.sort();
        Ok(entries)
    }

    async fn destroy(&self) -> Result<()> {
        if self.destroyed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        match tokio::fs::remove_dir_all(&self.workspace_root).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CaduceusError::Io(e)),
        }
    }

    fn id(&self) -> &str {
        &self.id
    }
}

pub struct E2BSandboxProvider {
    client: Client,
    api_url: String,
    api_key: String,
}

impl E2BSandboxProvider {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("E2B_API_KEY").map_err(|_| {
            CaduceusError::Config("E2B_API_KEY environment variable is required".into())
        })?;

        let api_url =
            std::env::var("E2B_API_URL").unwrap_or_else(|_| "https://api.e2b.app".to_string());
        Ok(Self {
            client: Client::new(),
            api_url,
            api_key,
        })
    }
}

#[async_trait]
impl SandboxProvider for E2BSandboxProvider {
    async fn create(&self, config: SandboxConfig) -> Result<Box<dyn Sandbox>> {
        let response = self
            .client
            .post(format!("{}/sandboxes", self.api_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({
                "templateID": config.template,
                "timeout": config.timeout_secs,
                "envVars": config.env_vars,
                "cwd": config.cwd,
            }))
            .send()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(CaduceusError::Tool {
                tool: "sandbox".into(),
                message: format!("E2B create failed ({status}): {body}"),
            });
        }

        let payload = response
            .json::<Value>()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;

        let sandbox_id = payload
            .get("sandboxID")
            .or_else(|| payload.get("sandboxId"))
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| CaduceusError::Tool {
                tool: "sandbox".into(),
                message: "E2B create response missing sandbox id".into(),
            })?
            .to_string();

        Ok(Box::new(E2BSandbox {
            id: sandbox_id,
            client: self.client.clone(),
            api_url: self.api_url.clone(),
            api_key: self.api_key.clone(),
            started_at: std::time::Instant::now(),
            lifetime_timeout_secs: config.lifetime_timeout_secs,
        }))
    }
}

pub struct E2BSandbox {
    id: String,
    client: Client,
    api_url: String,
    api_key: String,
    started_at: std::time::Instant,
    lifetime_timeout_secs: Option<u64>,
}

impl E2BSandbox {
    fn req(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.header("X-API-Key", &self.api_key)
    }

    async fn ensure_success(response: reqwest::Response, op: &str) -> Result<reqwest::Response> {
        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(CaduceusError::Tool {
            tool: "sandbox".into(),
            message: format!("E2B {op} failed ({status}): {body}"),
        })
    }
}

#[async_trait]
impl Sandbox for E2BSandbox {
    async fn exec(&self, command: &str, timeout_secs: u64) -> Result<ExecResult> {
        // Enforce lifetime timeout
        if let Some(limit) = self.lifetime_timeout_secs {
            if self.started_at.elapsed().as_secs() > limit {
                return Err(CaduceusError::Tool {
                    tool: "sandbox".into(),
                    message: format!("Sandbox lifetime exceeded: {}s limit expired", limit),
                });
            }
        }

        let response = self
            .req(
                self.client
                    .post(format!("{}/sandboxes/{}/processes", self.api_url, self.id)),
            )
            .json(&serde_json::json!({
                "command": command,
                "timeout": timeout_secs,
            }))
            .send()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;

        let response = Self::ensure_success(response, "exec").await?;
        let payload = response
            .json::<Value>()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;

        Ok(ExecResult {
            stdout: payload
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stderr: payload
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            exit_code: payload
                .get("exitCode")
                .or_else(|| payload.get("exit_code"))
                .and_then(Value::as_i64)
                .unwrap_or(-1) as i32,
            timed_out: payload
                .get("timedOut")
                .or_else(|| payload.get("timed_out"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        let response = self
            .req(
                self.client
                    .put(format!("{}/sandboxes/{}/filesystem", self.api_url, self.id)),
            )
            .query(&[("path", path)])
            .body(content.to_string())
            .send()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;

        Self::ensure_success(response, "write file").await?;
        Ok(())
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        let response = self
            .req(
                self.client
                    .get(format!("{}/sandboxes/{}/filesystem", self.api_url, self.id)),
            )
            .query(&[("path", path)])
            .send()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;

        let response = Self::ensure_success(response, "read file").await?;
        response
            .text()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))
    }

    async fn list_files(&self, path: &str) -> Result<Vec<String>> {
        let response = self
            .req(
                self.client
                    .get(format!("{}/sandboxes/{}/filesystem", self.api_url, self.id)),
            )
            .query(&[("path", path), ("list", "true")])
            .send()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;

        let response = Self::ensure_success(response, "list files").await?;
        let payload = response
            .json::<Value>()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;

        let mut files = Vec::new();
        if let Some(items) = payload.as_array() {
            for item in items {
                if let Some(name) = item.as_str() {
                    files.push(name.to_string());
                } else if let Some(name) = item.get("name").and_then(Value::as_str) {
                    files.push(name.to_string());
                } else if let Some(name) = item.get("path").and_then(Value::as_str) {
                    files.push(name.to_string());
                }
            }
        } else if let Some(items) = payload.get("files").and_then(Value::as_array) {
            for item in items {
                if let Some(name) = item.as_str() {
                    files.push(name.to_string());
                } else if let Some(name) = item.get("name").and_then(Value::as_str) {
                    files.push(name.to_string());
                } else if let Some(name) = item.get("path").and_then(Value::as_str) {
                    files.push(name.to_string());
                }
            }
        }

        Ok(files)
    }

    async fn destroy(&self) -> Result<()> {
        let response = self
            .req(
                self.client
                    .delete(format!("{}/sandboxes/{}", self.api_url, self.id)),
            )
            .send()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;

        Self::ensure_success(response, "destroy").await?;
        Ok(())
    }

    fn id(&self) -> &str {
        &self.id
    }
}

// ── Sandbox lifecycle (pause/resume/snapshot/status) ──────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SandboxStatus {
    Running,
    Paused,
    Stopped,
    Error(String),
}

#[async_trait]
pub trait SandboxLifecycle: Send + Sync {
    async fn pause(&self) -> Result<()>;
    async fn resume(&self) -> Result<()>;
    async fn snapshot(&self) -> Result<String>;
    async fn status(&self) -> Result<SandboxStatus>;
}

#[async_trait]
impl SandboxLifecycle for E2BSandbox {
    async fn pause(&self) -> Result<()> {
        let response = self
            .req(
                self.client
                    .post(format!("{}/sandboxes/{}/pause", self.api_url, self.id)),
            )
            .send()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;
        Self::ensure_success(response, "pause").await?;
        Ok(())
    }

    async fn resume(&self) -> Result<()> {
        let response = self
            .req(
                self.client
                    .post(format!("{}/sandboxes/{}/resume", self.api_url, self.id)),
            )
            .send()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;
        Self::ensure_success(response, "resume").await?;
        Ok(())
    }

    async fn snapshot(&self) -> Result<String> {
        let response = self
            .req(
                self.client
                    .post(format!("{}/sandboxes/{}/snapshot", self.api_url, self.id)),
            )
            .send()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;
        let response = Self::ensure_success(response, "snapshot").await?;
        let payload = response
            .json::<Value>()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;
        let snapshot_id = payload
            .get("snapshotId")
            .or_else(|| payload.get("snapshot_id"))
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(snapshot_id)
    }

    async fn status(&self) -> Result<SandboxStatus> {
        let response = self
            .req(
                self.client
                    .get(format!("{}/sandboxes/{}", self.api_url, self.id)),
            )
            .send()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;
        let response = Self::ensure_success(response, "status").await?;
        let payload = response
            .json::<Value>()
            .await
            .map_err(|e| CaduceusError::Other(e.into()))?;
        let status_str = payload
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("running");
        let status = match status_str {
            "paused" => SandboxStatus::Paused,
            "stopped" | "destroyed" => SandboxStatus::Stopped,
            s if s.starts_with("error") => SandboxStatus::Error(s.to_string()),
            _ => SandboxStatus::Running,
        };
        Ok(status)
    }
}

pub struct SandboxManager {
    provider: Arc<dyn SandboxProvider>,
    sandboxes: Mutex<HashMap<String, Arc<dyn Sandbox>>>,
}

impl SandboxManager {
    pub fn new() -> Self {
        let provider: Arc<dyn SandboxProvider> = match std::env::var("E2B_API_KEY") {
            Ok(key) if !key.trim().is_empty() => match E2BSandboxProvider::from_env() {
                Ok(provider) => Arc::new(provider),
                Err(_) => Arc::new(LocalSandboxProvider),
            },
            _ => Arc::new(LocalSandboxProvider),
        };

        Self {
            provider,
            sandboxes: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_provider(provider: Arc<dyn SandboxProvider>) -> Self {
        Self {
            provider,
            sandboxes: Mutex::new(HashMap::new()),
        }
    }

    pub async fn get_or_create(&self, session_id: &str) -> Result<Arc<dyn Sandbox>> {
        self.get_or_create_with_config(session_id, SandboxConfig::default())
            .await
    }

    pub async fn get_or_create_with_config(
        &self,
        session_id: &str,
        config: SandboxConfig,
    ) -> Result<Arc<dyn Sandbox>> {
        if let Some(existing) = self.sandboxes.lock().await.get(session_id).cloned() {
            return Ok(existing);
        }

        let created = Arc::<dyn Sandbox>::from(self.provider.create(config).await?);
        let mut sandboxes = self.sandboxes.lock().await;
        if let Some(existing) = sandboxes.get(session_id).cloned() {
            return Ok(existing);
        }

        sandboxes.insert(session_id.to_string(), created.clone());
        Ok(created)
    }

    pub async fn destroy_all(&self) -> Result<()> {
        let sandboxes = {
            let mut guard = self.sandboxes.lock().await;
            std::mem::take(&mut *guard)
        };

        for sandbox in sandboxes.into_values() {
            sandbox.destroy().await?;
        }

        Ok(())
    }
}

impl Default for SandboxManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Bash validation pipeline ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationLevel {
    Safe,
    Caution,
    Dangerous,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub level: ValidationLevel,
    pub warnings: Vec<String>,
}

pub struct BashValidator;

impl BashValidator {
    pub fn validate(command: &str) -> ValidationResult {
        let mut warnings = Vec::new();
        let mut level = ValidationLevel::Safe;

        let dangerous_patterns: &[(&str, &str)] = &[
            ("rm -rf", "Recursive force remove detected"),
            ("rm -r", "Recursive remove detected"),
            ("dd if=", "Disk dump (dd) command detected"),
            ("mkfs", "Filesystem format (mkfs) command detected"),
            (
                "chmod 777",
                "World-writable permissions (chmod 777) detected",
            ),
        ];

        for (pattern, warning) in dangerous_patterns {
            if command.contains(pattern) {
                level = ValidationLevel::Dangerous;
                warnings.push(warning.to_string());
            }
        }

        // Check sudo/su separately to handle word boundaries
        if command.starts_with("sudo ")
            || command.contains(" sudo ")
            || command.starts_with("sudo\t")
        {
            level = ValidationLevel::Dangerous;
            warnings.push("Privilege escalation (sudo) detected".to_string());
        }
        if command.starts_with("su ") || command.contains(" su ") || command.starts_with("su\t") {
            level = ValidationLevel::Dangerous;
            warnings.push("Privilege escalation (su) detected".to_string());
        }

        // curl/wget exfiltration
        if (command.contains("curl -X POST") || command.contains("curl --data"))
            && !command.contains("localhost")
            && !command.contains("127.0.0.1")
        {
            level = ValidationLevel::Dangerous;
            warnings.push("Potential data exfiltration via curl POST detected".to_string());
        }
        if command.contains("wget ") && command.contains("--post") {
            level = ValidationLevel::Dangerous;
            warnings.push("Potential data exfiltration via wget POST detected".to_string());
        }

        if level == ValidationLevel::Dangerous {
            return ValidationResult { level, warnings };
        }

        // Caution patterns
        if command.contains("rm ") && !command.contains("rm -rf") && !command.contains("rm -r") {
            level = ValidationLevel::Caution;
            warnings.push("File removal (rm) detected".to_string());
        }
        if command.contains("chmod") && !command.contains("chmod 777") {
            level = ValidationLevel::Caution;
            warnings.push("Permission change (chmod) detected".to_string());
        }
        if command.contains("chown") {
            level = ValidationLevel::Caution;
            warnings.push("Ownership change (chown) detected".to_string());
        }
        if command.contains('>') {
            level = ValidationLevel::Caution;
            warnings.push("Output redirection detected".to_string());
        }
        if command.contains("curl") && level != ValidationLevel::Dangerous {
            level = ValidationLevel::Caution;
            warnings.push("Network access (curl) detected".to_string());
        }
        if command.contains("wget") && level != ValidationLevel::Dangerous {
            level = ValidationLevel::Caution;
            warnings.push("Network access (wget) detected".to_string());
        }

        ValidationResult { level, warnings }
    }
}

// ── Container-first sandbox ──────────────────────────────────────────────────

pub struct ContainerSandboxProvider {
    image: String,
}

impl ContainerSandboxProvider {
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
        }
    }
}

#[async_trait]
impl SandboxProvider for ContainerSandboxProvider {
    async fn create(&self, config: SandboxConfig) -> Result<Box<dyn Sandbox>> {
        Ok(Box::new(
            ContainerSandbox::new(self.image.clone(), config).await?,
        ))
    }
}

pub struct ContainerSandbox {
    id: String,
    image: String,
    workspace_root: PathBuf,
    destroyed: AtomicBool,
}

impl ContainerSandbox {
    pub async fn new(image: String, _config: SandboxConfig) -> Result<Self> {
        let id = format!(
            "container-{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            LOCAL_SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let workspace_root = std::env::temp_dir().join(format!("caduceus-container-{id}"));
        tokio::fs::create_dir_all(&workspace_root).await?;

        Ok(Self {
            id,
            image,
            workspace_root,
            destroyed: AtomicBool::new(false),
        })
    }

    fn ensure_active(&self) -> Result<()> {
        if self.destroyed.load(Ordering::SeqCst) {
            return Err(CaduceusError::Tool {
                tool: "sandbox".into(),
                message: "container sandbox is destroyed".into(),
            });
        }
        Ok(())
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let raw = Path::new(path);
        let relative = if raw.is_absolute() {
            PathBuf::from(path.trim_start_matches('/'))
        } else {
            PathBuf::from(path)
        };

        for component in relative.components() {
            if matches!(component, Component::ParentDir) {
                return Err(CaduceusError::PermissionDenied {
                    capability: "fs".into(),
                    tool: "path escapes container workspace".into(),
                });
            }
        }

        Ok(self.workspace_root.join(relative))
    }
}

#[async_trait]
impl Sandbox for ContainerSandbox {
    async fn exec(&self, command: &str, timeout_secs: u64) -> Result<ExecResult> {
        self.ensure_active()?;

        let workspace = self.workspace_root.to_string_lossy().to_string();
        let mut cmd = Command::new("docker");
        cmd.args([
            "run",
            "--rm",
            "-v",
            &format!("{workspace}:/workspace"),
            "-w",
            "/workspace",
            &self.image,
            "bash",
            "-c",
            command,
        ])
        .kill_on_drop(true);

        match timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
            Ok(Ok(output)) => Ok(ExecResult {
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code().unwrap_or(-1),
                timed_out: false,
            }),
            Ok(Err(e)) => Err(CaduceusError::Io(e)),
            Err(_) => Ok(ExecResult {
                stdout: String::new(),
                stderr: format!("Command timed out after {timeout_secs}s"),
                exit_code: -1,
                timed_out: true,
            }),
        }
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        self.ensure_active()?;
        let resolved = self.resolve_path(path)?;
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(resolved, content).await?;
        Ok(())
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        self.ensure_active()?;
        let resolved = self.resolve_path(path)?;
        Ok(tokio::fs::read_to_string(resolved).await?)
    }

    async fn list_files(&self, path: &str) -> Result<Vec<String>> {
        self.ensure_active()?;
        let resolved = self.resolve_path(path)?;
        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(resolved).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            if let Some(name) = entry.file_name().to_str() {
                entries.push(name.to_string());
            }
        }
        entries.sort();
        Ok(entries)
    }

    async fn destroy(&self) -> Result<()> {
        if self.destroyed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        match tokio::fs::remove_dir_all(&self.workspace_root).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CaduceusError::Io(e)),
        }
    }

    fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_local_sandbox_and_exec_command() {
        let sandbox = LocalSandbox::new(SandboxConfig::default()).await.unwrap();
        let result = sandbox.exec("echo hello", 5).await.unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
        sandbox.destroy().await.unwrap();
    }

    #[tokio::test]
    async fn write_and_read_file_in_sandbox() {
        let sandbox = LocalSandbox::new(SandboxConfig::default()).await.unwrap();
        sandbox
            .write_file("notes/test.txt", "hello world")
            .await
            .unwrap();
        let content = sandbox.read_file("notes/test.txt").await.unwrap();
        assert_eq!(content, "hello world");
        sandbox.destroy().await.unwrap();
    }

    #[tokio::test]
    async fn list_files_in_sandbox() {
        let sandbox = LocalSandbox::new(SandboxConfig::default()).await.unwrap();
        sandbox.write_file("a.txt", "a").await.unwrap();
        sandbox.write_file("b.txt", "b").await.unwrap();
        let files = sandbox.list_files(".").await.unwrap();
        assert_eq!(files, vec!["a.txt".to_string(), "b.txt".to_string()]);
        sandbox.destroy().await.unwrap();
    }

    #[tokio::test]
    async fn timeout_enforcement() {
        let sandbox = LocalSandbox::new(SandboxConfig::default()).await.unwrap();
        let result = sandbox.exec("sleep 2", 1).await.unwrap();
        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);
        sandbox.destroy().await.unwrap();
    }

    #[tokio::test]
    async fn destroy_cleans_up_tempdir() {
        let sandbox = LocalSandbox::new(SandboxConfig::default()).await.unwrap();
        let workspace = sandbox.workspace_path().to_path_buf();
        assert!(tokio::fs::try_exists(&workspace).await.unwrap());
        sandbox.destroy().await.unwrap();
        assert!(!tokio::fs::try_exists(&workspace).await.unwrap());
    }

    #[tokio::test]
    async fn sandbox_manager_reuses_existing_sandbox() {
        let manager = SandboxManager::with_provider(Arc::new(LocalSandboxProvider));
        let first = manager.get_or_create("session-1").await.unwrap();
        let second = manager.get_or_create("session-1").await.unwrap();

        assert_eq!(first.id(), second.id());
        manager.destroy_all().await.unwrap();
    }

    // ── BashValidator tests ──────────────────────────────────────────────

    #[test]
    fn test_bash_validator_safe() {
        let result = BashValidator::validate("echo hello");
        assert_eq!(result.level, ValidationLevel::Safe);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_bash_validator_dangerous_rm_rf() {
        let result = BashValidator::validate("rm -rf /");
        assert_eq!(result.level, ValidationLevel::Dangerous);
        assert!(!result.warnings.is_empty());
    }

    #[test]
    fn test_bash_validator_dangerous_sudo() {
        let result = BashValidator::validate("sudo apt-get install something");
        assert_eq!(result.level, ValidationLevel::Dangerous);
        assert!(result.warnings.iter().any(|w| w.contains("sudo")));
    }

    #[test]
    fn test_bash_validator_caution_curl() {
        let result = BashValidator::validate("curl https://example.com/file");
        assert_eq!(result.level, ValidationLevel::Caution);
        assert!(result.warnings.iter().any(|w| w.contains("curl")));
    }

    // ── ContainerSandbox tests ───────────────────────────────────────────

    #[test]
    fn test_container_sandbox_provider_construction() {
        let provider = ContainerSandboxProvider::new("ubuntu:22.04");
        assert_eq!(provider.image, "ubuntu:22.04");
    }
}
