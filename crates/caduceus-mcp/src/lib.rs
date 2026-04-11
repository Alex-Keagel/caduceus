//! `caduceus-mcp` — MCP (Model Context Protocol) client runtime.
//!
//! Provides:
//! - [`types`] — core type definitions (configs, tool defs, resources, registry entries)
//! - [`error`] — unified error type
//! - [`client`] — per-server JSON-RPC 2.0 client (stdio + HTTP transports)
//! - [`manager`] — multi-server pool with tool aggregation and routing
//! - [`registry`] — MCP public registry client for discovery and install

pub mod client;
pub mod error;
pub mod manager;
pub mod registry;
pub mod types;

pub use client::McpClient;
pub use error::{McpError, Result};
pub use manager::McpServerManager;
pub use registry::McpRegistryClient;
pub use types::{
    McpPackage, McpRegistryEntry, McpResource, McpServerConfig, McpToolDef, McpTransport,
    PackageRuntime, ServerStatus,
};
