use crate::error::{McpError, Result};
use crate::types::{McpPackage, McpRegistryEntry, McpServerConfig, McpTransport, PackageRuntime};
use tracing::{debug, instrument};

const REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io/v0";

// ── Registry API response shapes ───────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct RegistrySearchResponse {
    servers: Vec<McpRegistryEntry>,
}

// ── Registry Client ────────────────────────────────────────────────────────────

/// Client for the MCP public server registry.
pub struct McpRegistryClient {
    http: reqwest::Client,
    base_url: String,
}

impl McpRegistryClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build reqwest client"),
            base_url: REGISTRY_BASE.to_string(),
        }
    }

    /// Override the registry base URL (useful for testing).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Search for MCP servers matching a query string.
    #[instrument(skip(self), fields(query = %query))]
    pub async fn search(&self, query: &str) -> Result<Vec<McpRegistryEntry>> {
        let url = format!("{}/servers", self.base_url);
        debug!(url = %url, "Searching MCP registry");

        let resp = self
            .http
            .get(&url)
            .query(&[("q", query)])
            .send()
            .await
            .map_err(McpError::Http)?
            .error_for_status()
            .map_err(McpError::Http)?
            .json::<RegistrySearchResponse>()
            .await
            .map_err(McpError::Http)?;

        Ok(resp.servers)
    }

    /// Fetch details for a specific server by its registry ID or qualified name.
    #[instrument(skip(self), fields(id = %id))]
    pub async fn get_server(&self, id: &str) -> Result<McpRegistryEntry> {
        let url = format!("{}/servers/{}", self.base_url, id);
        debug!(url = %url, "Fetching MCP server from registry");

        let entry = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(McpError::Http)?
            .error_for_status()
            .map_err(McpError::Http)?
            .json::<McpRegistryEntry>()
            .await
            .map_err(McpError::Http)?;

        Ok(entry)
    }

    /// Generate an `McpServerConfig` from a registry entry's first package.
    ///
    /// Chooses the most appropriate package (npm → npx, python → uvx, etc.)
    /// and produces a stdio transport config ready to use with [`McpClient`].
    pub fn install(
        &self,
        entry: &McpRegistryEntry,
        extra_args: Vec<String>,
    ) -> Result<McpServerConfig> {
        let pkg = entry
            .packages
            .first()
            .ok_or_else(|| McpError::Config(format!("No packages found for '{}'", entry.id)))?;

        let config = build_config_for_package(&entry.id, &entry.name, pkg, extra_args)?;
        Ok(config)
    }
}

impl Default for McpRegistryClient {
    fn default() -> Self {
        Self::new()
    }
}

fn build_config_for_package(
    id: &str,
    name: &str,
    pkg: &McpPackage,
    extra_args: Vec<String>,
) -> Result<McpServerConfig> {
    let (command, mut args) = match pkg.runtime {
        PackageRuntime::Npm => {
            let pkg_name = pkg
                .name
                .as_deref()
                .ok_or_else(|| McpError::Config("npm package has no name".into()))?;
            (
                "npx".to_string(),
                vec!["-y".to_string(), pkg_name.to_string()],
            )
        }
        PackageRuntime::Python => {
            let pkg_name = pkg
                .name
                .as_deref()
                .ok_or_else(|| McpError::Config("python package has no name".into()))?;
            ("uvx".to_string(), vec![pkg_name.to_string()])
        }
        PackageRuntime::Go => {
            let pkg_name = pkg
                .name
                .as_deref()
                .ok_or_else(|| McpError::Config("go package has no name".into()))?;
            (
                "go".to_string(),
                vec!["run".to_string(), pkg_name.to_string()],
            )
        }
        PackageRuntime::Docker => {
            let img = pkg
                .name
                .as_deref()
                .ok_or_else(|| McpError::Config("docker image has no name".into()))?;
            (
                "docker".to_string(),
                vec![
                    "run".to_string(),
                    "--rm".to_string(),
                    "-i".to_string(),
                    img.to_string(),
                ],
            )
        }
        PackageRuntime::Binary => {
            let bin = pkg
                .name
                .as_deref()
                .ok_or_else(|| McpError::Config("binary package has no name".into()))?;
            (bin.to_string(), vec![])
        }
        PackageRuntime::Unknown => {
            return Err(McpError::Config(format!(
                "Unknown runtime for package in server '{}'",
                id
            )));
        }
    };

    args.extend(pkg.args.clone());
    args.extend(extra_args);

    Ok(McpServerConfig {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        transport: McpTransport::Stdio {
            command,
            args,
            env: pkg.env.clone(),
        },
        auto_start: true,
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{McpPackage, McpRegistryEntry, PackageRuntime};
    use std::collections::HashMap;

    fn npm_entry() -> McpRegistryEntry {
        McpRegistryEntry {
            id: "fs".to_string(),
            name: "Filesystem".to_string(),
            description: Some("Access local filesystem".to_string()),
            version: Some("0.5.0".to_string()),
            publisher: None,
            packages: vec![McpPackage {
                runtime: PackageRuntime::Npm,
                name: Some("@modelcontextprotocol/server-filesystem".to_string()),
                args: vec!["/home/user/projects".to_string()],
                env: HashMap::new(),
            }],
        }
    }

    fn python_entry() -> McpRegistryEntry {
        McpRegistryEntry {
            id: "brave-search".to_string(),
            name: "Brave Search".to_string(),
            description: None,
            version: None,
            publisher: None,
            packages: vec![McpPackage {
                runtime: PackageRuntime::Python,
                name: Some("mcp-server-brave-search".to_string()),
                args: vec![],
                env: {
                    let mut m = HashMap::new();
                    m.insert("BRAVE_API_KEY".to_string(), "".to_string());
                    m
                },
            }],
        }
    }

    #[test]
    fn install_npm_generates_npx_command() {
        let client = McpRegistryClient::new();
        let entry = npm_entry();
        let cfg = client.install(&entry, vec![]).unwrap();
        match &cfg.transport {
            McpTransport::Stdio { command, args, .. } => {
                assert_eq!(command, "npx");
                assert!(args.contains(&"-y".to_string()));
                assert!(args.iter().any(|a| a.contains("server-filesystem")));
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn install_python_generates_uvx_command() {
        let client = McpRegistryClient::new();
        let entry = python_entry();
        let cfg = client.install(&entry, vec![]).unwrap();
        match &cfg.transport {
            McpTransport::Stdio { command, args, env } => {
                assert_eq!(command, "uvx");
                assert!(args.iter().any(|a| a.contains("brave")));
                assert!(env.contains_key("BRAVE_API_KEY"));
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn install_extra_args_are_appended() {
        let client = McpRegistryClient::new();
        let entry = npm_entry();
        let cfg = client
            .install(&entry, vec!["/extra/path".to_string()])
            .unwrap();
        match &cfg.transport {
            McpTransport::Stdio { args, .. } => {
                assert!(args.contains(&"/extra/path".to_string()));
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn install_no_packages_errors() {
        let client = McpRegistryClient::new();
        let entry = McpRegistryEntry {
            id: "empty".into(),
            name: "Empty".into(),
            description: None,
            version: None,
            publisher: None,
            packages: vec![],
        };
        assert!(matches!(
            client.install(&entry, vec![]).unwrap_err(),
            McpError::Config(_)
        ));
    }

    #[test]
    fn registry_entry_full_round_trip() {
        let entry = npm_entry();
        let json = serde_json::to_string(&entry).unwrap();
        let back: McpRegistryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, entry.id);
        assert_eq!(back.packages[0].runtime, PackageRuntime::Npm);
    }
}
