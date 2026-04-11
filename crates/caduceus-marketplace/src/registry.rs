use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use walkdir::WalkDir;

use crate::error::MarketplaceError;
use crate::manifest::{Category, PluginManifest};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub categories: Vec<Category>,
    pub triggers: Vec<String>,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub specialty: String,
    pub tools: Vec<String>,
    pub triggers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEntry {
    pub manifest: PluginManifest,
    pub path: String,
}

#[derive(Debug, Default)]
pub struct MarketplaceRegistry {
    plugins: HashMap<String, PluginEntry>,
    skills: HashMap<String, SkillEntry>,
    agents: HashMap<String, AgentEntry>,
}

impl MarketplaceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Plugins ────────────────────────────────────────────────────────────────

    pub fn register_plugin(&mut self, entry: PluginEntry) {
        self.plugins.insert(entry.manifest.name.clone(), entry);
    }

    pub fn list_plugins(&self) -> Vec<&PluginEntry> {
        self.plugins.values().collect()
    }

    pub fn search_plugins(&self, query: &str) -> Vec<&PluginEntry> {
        let q = query.to_lowercase();
        self.plugins
            .values()
            .filter(|e| {
                e.manifest.name.to_lowercase().contains(&q)
                    || e.manifest.description.to_lowercase().contains(&q)
            })
            .collect()
    }

    // ── Skills ─────────────────────────────────────────────────────────────────

    pub fn register_skill(&mut self, entry: SkillEntry) {
        self.skills.insert(entry.name.clone(), entry);
    }

    pub fn list_skills(&self) -> Vec<&SkillEntry> {
        self.skills.values().collect()
    }

    pub fn search_skills(&self, query: &str) -> Vec<&SkillEntry> {
        let q = query.to_lowercase();
        self.skills
            .values()
            .filter(|e| {
                e.name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
                    || e.triggers.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .collect()
    }

    // ── Agents ─────────────────────────────────────────────────────────────────

    pub fn register_agent(&mut self, entry: AgentEntry) {
        self.agents.insert(entry.name.clone(), entry);
    }

    pub fn list_agents(&self) -> Vec<&AgentEntry> {
        self.agents.values().collect()
    }

    pub fn search_agents(&self, query: &str) -> Vec<&AgentEntry> {
        let q = query.to_lowercase();
        self.agents
            .values()
            .filter(|e| {
                e.name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
                    || e.specialty.to_lowercase().contains(&q)
                    || e.triggers.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .collect()
    }

    // ── Filesystem loaders ─────────────────────────────────────────────────────

    /// Load skills from a `.caduceus/skills/` directory by parsing YAML frontmatter.
    pub fn load_skills_from_dir(&mut self, skills_dir: &Path) -> Result<usize, MarketplaceError> {
        if !skills_dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        for entry in WalkDir::new(skills_dir).max_depth(2).min_depth(1) {
            let entry = entry.map_err(|e| MarketplaceError::Io(e.to_string()))?;
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "md") {
                if let Ok(skill) = parse_skill_frontmatter(path) {
                    self.register_skill(skill);
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Load agents from a `.caduceus/agents/` directory by parsing YAML frontmatter.
    pub fn load_agents_from_dir(&mut self, agents_dir: &Path) -> Result<usize, MarketplaceError> {
        if !agents_dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        for entry in WalkDir::new(agents_dir).max_depth(2).min_depth(1) {
            let entry = entry.map_err(|e| MarketplaceError::Io(e.to_string()))?;
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "md") {
                if let Ok(agent) = parse_agent_frontmatter(path) {
                    self.register_agent(agent);
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}

fn extract_frontmatter(content: &str) -> Option<&str> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let rest = &content[3..];
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

fn yaml_field<'a>(yaml: &'a str, key: &str) -> Option<&'a str> {
    for line in yaml.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(rest) = rest.strip_prefix(':') {
                return Some(rest.trim().trim_matches('"'));
            }
        }
    }
    None
}

fn yaml_list_field(yaml: &str, key: &str) -> Vec<String> {
    for line in yaml.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(rest) = rest.strip_prefix(':') {
                let rest = rest.trim();
                if rest.starts_with('[') && rest.ends_with(']') {
                    return rest[1..rest.len() - 1]
                        .split(',')
                        .map(|s| s.trim().trim_matches('"').to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
        }
    }
    Vec::new()
}

fn parse_skill_frontmatter(path: &Path) -> Result<SkillEntry, MarketplaceError> {
    let content = std::fs::read_to_string(path).map_err(|e| MarketplaceError::Io(e.to_string()))?;
    let yaml = extract_frontmatter(&content)
        .ok_or_else(|| MarketplaceError::ManifestParse("no frontmatter".into()))?;

    let name = yaml_field(yaml, "name")
        .ok_or_else(|| MarketplaceError::ManifestParse("missing name".into()))?
        .to_string();

    let description = yaml_field(yaml, "description").unwrap_or("").to_string();
    let version = yaml_field(yaml, "version").unwrap_or("1.0").to_string();

    let raw_cats = yaml_list_field(yaml, "categories");
    let categories = raw_cats.iter().filter_map(|c| Category::parse(c)).collect();

    let triggers = yaml_list_field(yaml, "triggers");
    let tools = yaml_list_field(yaml, "tools");

    Ok(SkillEntry {
        name,
        version,
        description,
        categories,
        triggers,
        tools,
    })
}

fn parse_agent_frontmatter(path: &Path) -> Result<AgentEntry, MarketplaceError> {
    let content = std::fs::read_to_string(path).map_err(|e| MarketplaceError::Io(e.to_string()))?;
    let yaml = extract_frontmatter(&content)
        .ok_or_else(|| MarketplaceError::ManifestParse("no frontmatter".into()))?;

    let name = yaml_field(yaml, "name")
        .ok_or_else(|| MarketplaceError::ManifestParse("missing name".into()))?
        .to_string();

    let description = yaml_field(yaml, "description").unwrap_or("").to_string();
    let version = yaml_field(yaml, "version").unwrap_or("1.0").to_string();
    let specialty = yaml_field(yaml, "specialty").unwrap_or(&name).to_string();

    let raw_triggers = yaml_list_field(yaml, "triggers");
    let tools = yaml_list_field(yaml, "tools");

    Ok(AgentEntry {
        name,
        version,
        description,
        specialty,
        tools,
        triggers: raw_triggers,
    })
}
