use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::MarketplaceError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
    CodeReview,
    Testing,
    Security,
    Documentation,
    Frontend,
    Backend,
    DevOps,
    Database,
    AI,
    Productivity,
    Git,
    Deployment,
}

impl Category {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "code_review" | "codereview" | "code-review" | "review" => Some(Self::CodeReview),
            "testing" | "test" => Some(Self::Testing),
            "security" | "sec" => Some(Self::Security),
            "documentation" | "docs" | "doc" => Some(Self::Documentation),
            "frontend" | "front-end" | "ui" => Some(Self::Frontend),
            "backend" | "back-end" | "server" => Some(Self::Backend),
            "devops" | "dev-ops" | "ci" | "cd" => Some(Self::DevOps),
            "database" | "db" | "data" => Some(Self::Database),
            "ai" | "ml" | "machine-learning" => Some(Self::AI),
            "productivity" | "workflow" => Some(Self::Productivity),
            "git" | "vcs" | "version-control" => Some(Self::Git),
            "deployment" | "deploy" | "infra" | "infrastructure" => Some(Self::Deployment),
            _ => None,
        }
    }
}

impl std::str::FromStr for Category {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or(())
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::CodeReview => "code_review",
            Self::Testing => "testing",
            Self::Security => "security",
            Self::Documentation => "documentation",
            Self::Frontend => "frontend",
            Self::Backend => "backend",
            Self::DevOps => "devops",
            Self::Database => "database",
            Self::AI => "ai",
            Self::Productivity => "productivity",
            Self::Git => "git",
            Self::Deployment => "deployment",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default)]
    pub categories: Vec<Category>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
}

impl PluginManifest {
    pub fn from_file(path: &Path) -> Result<Self, MarketplaceError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| MarketplaceError::Io(e.to_string()))?;
        let manifest: Self = serde_json::from_str(&content)
            .map_err(|e| MarketplaceError::ManifestParse(e.to_string()))?;
        Ok(manifest)
    }
}
