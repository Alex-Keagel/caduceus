//! Instruction management system for Caduceus.
//!
//! Reads and merges agent instructions from an 8-level priority hierarchy:
//! 1. `~/.caduceus/instructions.md` — user global
//! 2. `CADUCEUS.md` in workspace root — project-level
//! 3. `AGENTS.md` in workspace root — cross-tool agent config
//! 4. `.caduceus/instructions/*.md` — path-specific (YAML `applyTo:` glob)
//! 5. `.caduceus/agents/*.md` — custom agent definitions
//! 6. `.caduceus/skills/*.md` — skill definitions
//! 7. `.caduceus/mcp.json` — MCP server configurations
//! 8. `.caduceus/memory.md` — persistent memory

use caduceus_core::{CaduceusError, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── Data structures ────────────────────────────────────────────────────────────

/// The fully-merged instruction set for a workspace.
#[derive(Debug, Clone, Default)]
pub struct InstructionSet {
    /// Merged system prompt assembled from all layers.
    pub system_prompt: String,
    /// Raw project-level instructions from CADUCEUS.md / AGENTS.md.
    pub project_instructions: String,
    /// Per-path instruction overrides with glob patterns.
    pub path_instructions: Vec<PathInstruction>,
    /// Custom agent definitions loaded from `.caduceus/agents/`.
    pub active_agents: Vec<AgentDefinition>,
    /// Skill definitions loaded from `.caduceus/skills/`.
    pub available_skills: Vec<SkillDefinition>,
    /// MCP server configurations from `.caduceus/mcp.json`.
    pub mcp_servers: Vec<McpServerConfig>,
    /// Persistent memory entries from `.caduceus/memory.md`.
    pub memory_entries: Vec<String>,
}

/// A path-specific instruction override.
#[derive(Debug, Clone)]
pub struct PathInstruction {
    pub glob_pattern: String,
    pub instructions: String,
}

/// A custom agent definition parsed from YAML frontmatter + markdown body.
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
    pub trigger_phrases: Vec<String>,
}

/// A reusable skill definition parsed from YAML frontmatter + markdown body.
#[derive(Debug, Clone)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    pub steps: Vec<String>,
    pub trigger_phrases: Vec<String>,
}

/// An MCP server configuration entry.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

// ── YAML frontmatter helpers ───────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
struct PathInstructionFrontmatter {
    #[serde(default, alias = "applyTo")]
    apply_to: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct AgentFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    triggers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    triggers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct McpConfigFile {
    #[serde(default)]
    servers: Vec<McpServerEntry>,
}

#[derive(Debug, Deserialize)]
struct McpServerEntry {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

/// Split a markdown file into optional YAML frontmatter and body.
/// Frontmatter is delimited by `---` on its own lines at the start of the file.
fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let trimmed = content.trim_start();
    let after_open = if let Some(rest) = trimmed.strip_prefix("---\n") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("---\r\n") {
        rest
    } else {
        return (None, content);
    };

    let mut offset = 0usize;
    for line in after_open.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let yaml = after_open[..offset].trim_end_matches(['\r', '\n']);
            let body = after_open[offset + line.len()..].trim_start_matches(['\r', '\n']);
            return (Some(yaml), body);
        }
        offset += line.len();
    }

    if after_open.trim_end_matches(['\r', '\n']) == "---" {
        return (Some(""), "");
    }

    (None, content)
}

// ── InstructionLoader ──────────────────────────────────────────────────────────

/// Loads and merges instructions from the 8-level hierarchy.
pub struct InstructionLoader {
    workspace_root: PathBuf,
}

impl InstructionLoader {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    /// Load and merge all instruction sources. Returns a fully assembled `InstructionSet`.
    pub fn load(&self) -> Result<InstructionSet> {
        let mut set = InstructionSet::default();
        let mut prompt_parts: Vec<String> = Vec::new();

        // 1. User global instructions (~/.caduceus/instructions.md)
        let user_global = dirs_home()?.join(".caduceus/instructions.md");
        if let Some(content) = read_optional(&user_global)? {
            prompt_parts.push(format!(
                "<user_instructions>\n{}\n</user_instructions>",
                content.trim()
            ));
        }

        // 2. CADUCEUS.md in workspace root
        let caduceus_md = self.workspace_root.join("CADUCEUS.md");
        if let Some(content) = read_optional(&caduceus_md)? {
            set.project_instructions.push_str(&content);
            prompt_parts.push(format!(
                "<project_instructions>\n{}\n</project_instructions>",
                content.trim()
            ));
        }

        // 3. AGENTS.md in workspace root
        let agents_md = self.workspace_root.join("AGENTS.md");
        if let Some(content) = read_optional(&agents_md)? {
            if !set.project_instructions.is_empty() {
                set.project_instructions.push_str("\n\n");
            }
            set.project_instructions.push_str(&content);
            prompt_parts.push(format!(
                "<agents_config>\n{}\n</agents_config>",
                content.trim()
            ));
        }

        // 4. Path-specific instructions (.caduceus/instructions/*.md)
        let instr_dir = self.workspace_root.join(".caduceus/instructions");
        if instr_dir.is_dir() {
            let mut entries = read_dir_md_files(&instr_dir)?;
            entries.sort();
            for path in entries {
                if let Some(pi) = self.load_path_instruction(&path)? {
                    set.path_instructions.push(pi);
                }
            }
        }

        // 5. Custom agent definitions (.caduceus/agents/*.md)
        let agents_dir = self.workspace_root.join(".caduceus/agents");
        if agents_dir.is_dir() {
            let mut entries = read_dir_md_files(&agents_dir)?;
            entries.sort();
            for path in entries {
                if let Some(agent) = self.load_agent_definition(&path)? {
                    set.active_agents.push(agent);
                }
            }
        }

        // 6. Skill definitions (.caduceus/skills/*.md)
        let skills_dir = self.workspace_root.join(".caduceus/skills");
        if skills_dir.is_dir() {
            let mut entries = read_dir_md_files(&skills_dir)?;
            entries.sort();
            for path in entries {
                if let Some(skill) = self.load_skill_definition(&path)? {
                    set.available_skills.push(skill);
                }
            }
        }

        // 7. MCP server config (.caduceus/mcp.json)
        let mcp_json = self.workspace_root.join(".caduceus/mcp.json");
        if let Some(content) = read_optional(&mcp_json)? {
            let config: McpConfigFile = serde_json::from_str(&content)
                .map_err(|e| CaduceusError::Config(format!("Invalid mcp.json: {e}")))?;
            for entry in config.servers {
                set.mcp_servers.push(McpServerConfig {
                    name: entry.name,
                    command: entry.command,
                    args: entry.args,
                    env: entry.env,
                });
            }
        }

        // 8. Persistent memory (.caduceus/memory.md)
        let memory_md = self.workspace_root.join(".caduceus/memory.md");
        if let Some(content) = read_optional(&memory_md)? {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    set.memory_entries.push(trimmed.to_string());
                }
            }
            if !set.memory_entries.is_empty() {
                prompt_parts.push(format!(
                    "<memory>\n{}\n</memory>",
                    set.memory_entries.join("\n")
                ));
            }
        }

        // Append agent/skill summaries so the LLM knows what's available
        if !set.active_agents.is_empty() {
            let mut agent_info = String::from("<available_agents>\n");
            for agent in &set.active_agents {
                agent_info.push_str(&format!(
                    "- {} — {} (triggers: {})\n",
                    agent.name,
                    agent.description,
                    agent.trigger_phrases.join(", ")
                ));
            }
            agent_info.push_str("</available_agents>");
            prompt_parts.push(agent_info);
        }

        if !set.available_skills.is_empty() {
            let mut skill_info = String::from("<available_skills>\n");
            for skill in &set.available_skills {
                skill_info.push_str(&format!(
                    "- {} — {} (triggers: {})\n",
                    skill.name,
                    skill.description,
                    skill.trigger_phrases.join(", ")
                ));
            }
            skill_info.push_str("</available_skills>");
            prompt_parts.push(skill_info);
        }

        set.system_prompt = prompt_parts.join("\n\n");
        Ok(set)
    }

    /// Return path-specific instructions whose glob matches the given file path.
    pub fn instructions_for_path(&self, set: &InstructionSet, file_path: &str) -> String {
        let mut matched = Vec::new();
        for pi in &set.path_instructions {
            if glob_matches(&pi.glob_pattern, file_path) {
                matched.push(pi.instructions.as_str());
            }
        }
        matched.join("\n\n")
    }

    fn load_path_instruction(&self, path: &Path) -> Result<Option<PathInstruction>> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            CaduceusError::Config(format!("Failed to read {}: {e}", path.display()))
        })?;

        let (yaml, body) = split_frontmatter(&content);
        let fm: PathInstructionFrontmatter = match yaml {
            Some(y) => serde_yaml_lite_parse(y).unwrap_or_default(),
            None => PathInstructionFrontmatter::default(),
        };

        let glob_pattern = match fm.apply_to {
            Some(g) => g,
            None => return Ok(None), // No applyTo → skip
        };

        Ok(Some(PathInstruction {
            glob_pattern,
            instructions: body.trim().to_string(),
        }))
    }

    fn load_agent_definition(&self, path: &Path) -> Result<Option<AgentDefinition>> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            CaduceusError::Config(format!("Failed to read {}: {e}", path.display()))
        })?;

        let (yaml, body) = split_frontmatter(&content);
        let fm: AgentFrontmatter = match yaml {
            Some(y) => serde_yaml_lite_parse(y).unwrap_or_default(),
            None => AgentFrontmatter::default(),
        };

        let name = fm.name.unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });

        Ok(Some(AgentDefinition {
            name,
            description: fm.description.unwrap_or_default(),
            system_prompt: body.trim().to_string(),
            tools: fm.tools.unwrap_or_default(),
            trigger_phrases: fm.triggers.unwrap_or_default(),
        }))
    }

    fn load_skill_definition(&self, path: &Path) -> Result<Option<SkillDefinition>> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            CaduceusError::Config(format!("Failed to read {}: {e}", path.display()))
        })?;

        let (yaml, body) = split_frontmatter(&content);
        let fm: SkillFrontmatter = match yaml {
            Some(y) => serde_yaml_lite_parse(y).unwrap_or_default(),
            None => SkillFrontmatter::default(),
        };

        let name = fm.name.unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });

        // Extract numbered steps from the body
        let steps: Vec<String> = body
            .lines()
            .filter(|l| {
                let t = l.trim();
                // Match lines starting with "N." or "N)" (numbered steps)
                t.chars()
                    .next()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false)
                    && (t.contains(". ") || t.contains(") "))
            })
            .map(|l| l.trim().to_string())
            .collect();

        Ok(Some(SkillDefinition {
            name,
            description: fm.description.unwrap_or_default(),
            steps,
            trigger_phrases: fm.triggers.unwrap_or_default(),
        }))
    }
}

// ── Utility functions ──────────────────────────────────────────────────────────

/// Get the user's home directory.
fn dirs_home() -> Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .map_err(|_| CaduceusError::Config("Cannot determine home directory".into()))
}

/// Read a file if it exists, returning None if not found.
fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CaduceusError::Config(format!(
            "Failed to read {}: {e}",
            path.display()
        ))),
    }
}

/// List all `.md` files in a directory.
fn read_dir_md_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| CaduceusError::Config(format!("Cannot read {}: {e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| CaduceusError::Config(format!("Cannot read dir entry: {e}")))?;
        let path = entry.path();
        if path.extension().map(|e| e == "md").unwrap_or(false) {
            files.push(path);
        }
    }
    Ok(files)
}

/// Minimal YAML-like frontmatter parser.
///
/// We intentionally avoid pulling in a full YAML crate (serde_yaml is unmaintained,
/// serde_yml is heavy). This handles the simple key: value and key: [list] format
/// used in agent/skill/instruction frontmatter.
fn serde_yaml_lite_parse<T: serde::de::DeserializeOwned>(yaml: &str) -> Option<T> {
    // Convert our simple YAML subset to JSON, then use serde_json.
    let mut obj = serde_json::Map::new();

    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Skip indented lines (list items handled in second pass)
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let (key, value) = match trimmed.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let key = key.trim().to_string();
        let value = value.trim();

        if value.starts_with('[') && value.ends_with(']') {
            // Parse as array: [item1, item2, ...]
            let inner = &value[1..value.len() - 1];
            let items: Vec<serde_json::Value> = inner
                .split(',')
                .map(|s| {
                    let s = s.trim().trim_matches('"').trim_matches('\'');
                    serde_json::Value::String(s.to_string())
                })
                .collect();
            obj.insert(key, serde_json::Value::Array(items));
        } else if value.starts_with('-') || value.is_empty() {
            // Multi-line list starting on next lines, or first item on same line
            let mut items: Vec<serde_json::Value> = Vec::new();
            if value.starts_with('-') {
                items.push(serde_json::Value::String(
                    value
                        .trim_start_matches('-')
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string(),
                ));
            }
            obj.insert(key, serde_json::Value::Array(items));
        } else {
            let value = value.trim_matches('"').trim_matches('\'');
            obj.insert(key, serde_json::Value::String(value.to_string()));
        }
    }

    // Second pass: collect multi-line list items (lines starting with `  -`)
    let mut current_key: Option<String> = None;
    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if !line.starts_with(' ') && !line.starts_with('\t') && trimmed.contains(':') {
            let (key, _) = trimmed.split_once(':').unwrap();
            current_key = Some(key.trim().to_string());
        } else if trimmed.starts_with('-') {
            if let Some(ref key) = current_key {
                let item = trimmed
                    .trim_start_matches('-')
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'');
                if let Some(serde_json::Value::Array(arr)) = obj.get_mut(key) {
                    let val = serde_json::Value::String(item.to_string());
                    if !arr.contains(&val) {
                        arr.push(val);
                    }
                }
            }
        }
    }

    let json_value = serde_json::Value::Object(obj);
    serde_json::from_value(json_value).ok()
}

/// Simple glob matching supporting `*`, `**`, and `?`.
fn glob_matches(pattern: &str, path: &str) -> bool {
    glob_match_recursive(pattern.as_bytes(), path.as_bytes())
}

fn glob_match_recursive(pattern: &[u8], path: &[u8]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            if pattern.get(1) == Some(&b'*') {
                // `**` matches zero or more path segments
                let rest_pattern = if pattern.get(2) == Some(&b'/') {
                    &pattern[3..]
                } else {
                    &pattern[2..]
                };
                // Try matching rest of pattern at every position in path
                for i in 0..=path.len() {
                    if glob_match_recursive(rest_pattern, &path[i..]) {
                        return true;
                    }
                }
                false
            } else {
                // Single `*` matches within a single path segment (no `/`)
                let rest_pattern = &pattern[1..];
                // Try matching rest at every position that doesn't cross `/`
                for i in 0..=path.len() {
                    if i > 0 && path[i - 1] == b'/' {
                        break;
                    }
                    if glob_match_recursive(rest_pattern, &path[i..]) {
                        return true;
                    }
                }
                false
            }
        }
        (Some(b'?'), Some(c)) if *c != b'/' => glob_match_recursive(&pattern[1..], &path[1..]),
        (Some(a), Some(b)) if a == b => glob_match_recursive(&pattern[1..], &path[1..]),
        _ => false,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a temp workspace with given file structure.
    fn setup_workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full, content).unwrap();
        }
        dir
    }

    /// 1. Load from empty directory returns sensible defaults.
    #[test]
    fn load_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let loader = InstructionLoader::new(dir.path());
        let set = loader.load().unwrap();

        assert!(set.system_prompt.is_empty());
        assert!(set.project_instructions.is_empty());
        assert!(set.path_instructions.is_empty());
        assert!(set.active_agents.is_empty());
        assert!(set.available_skills.is_empty());
        assert!(set.mcp_servers.is_empty());
        assert!(set.memory_entries.is_empty());
    }

    /// 2. Load CADUCEUS.md project instructions.
    #[test]
    fn load_caduceus_md() {
        let dir = setup_workspace(&[("CADUCEUS.md", "# Project\nUse Rust conventions.")]);
        let loader = InstructionLoader::new(dir.path());
        let set = loader.load().unwrap();

        assert!(set.project_instructions.contains("Use Rust conventions"));
        assert!(set.system_prompt.contains("project_instructions"));
        assert!(set.system_prompt.contains("Use Rust conventions"));
    }

    /// 3. Load path-specific instructions with glob matching.
    #[test]
    fn load_path_specific_instructions() {
        let dir = setup_workspace(&[(
            ".caduceus/instructions/rust.md",
            "---\napplyTo: \"**/*.rs\"\n---\nUse idiomatic Rust. Prefer iterators over loops.",
        )]);
        let loader = InstructionLoader::new(dir.path());
        let set = loader.load().unwrap();

        assert_eq!(set.path_instructions.len(), 1);
        assert_eq!(set.path_instructions[0].glob_pattern, "**/*.rs");
        assert!(set.path_instructions[0]
            .instructions
            .contains("idiomatic Rust"));

        // Test matching
        let matched = loader.instructions_for_path(&set, "src/main.rs");
        assert!(matched.contains("idiomatic Rust"));

        let not_matched = loader.instructions_for_path(&set, "src/index.ts");
        assert!(not_matched.is_empty());
    }

    /// 4. Load agent definitions from YAML frontmatter.
    #[test]
    fn load_agent_definitions() {
        let dir = setup_workspace(&[(
            ".caduceus/agents/code-reviewer.md",
            "---\nname: code-reviewer\ndescription: Reviews code\ntools: [read_file, grep_search]\ntriggers:\n  - \"review this code\"\n  - \"check for bugs\"\n---\nYou are a code reviewer.",
        )]);
        let loader = InstructionLoader::new(dir.path());
        let set = loader.load().unwrap();

        assert_eq!(set.active_agents.len(), 1);
        let agent = &set.active_agents[0];
        assert_eq!(agent.name, "code-reviewer");
        assert_eq!(agent.description, "Reviews code");
        assert!(agent.system_prompt.contains("code reviewer"));
        assert_eq!(agent.tools, vec!["read_file", "grep_search"]);
        assert_eq!(agent.trigger_phrases.len(), 2);
        assert!(agent
            .trigger_phrases
            .contains(&"review this code".to_string()));
    }

    /// 5. Load skill definitions.
    #[test]
    fn load_skill_definitions() {
        let dir = setup_workspace(&[(
            ".caduceus/skills/release.md",
            "---\nname: release\ndescription: Create a new release\ntriggers:\n  - \"create a release\"\n  - \"ship it\"\n---\n## Steps\n1. Run tests\n2. Update version\n3. Push tags",
        )]);
        let loader = InstructionLoader::new(dir.path());
        let set = loader.load().unwrap();

        assert_eq!(set.available_skills.len(), 1);
        let skill = &set.available_skills[0];
        assert_eq!(skill.name, "release");
        assert_eq!(skill.description, "Create a new release");
        assert_eq!(skill.trigger_phrases.len(), 2);
        assert!(skill.steps.len() >= 3);
    }

    /// 6. Load MCP server configuration.
    #[test]
    fn load_mcp_config() {
        let dir = setup_workspace(&[(
            ".caduceus/mcp.json",
            r#"{"servers":[{"name":"filesystem","command":"npx","args":["-y","@mcp/server-fs","."],"env":{}}]}"#,
        )]);
        let loader = InstructionLoader::new(dir.path());
        let set = loader.load().unwrap();

        assert_eq!(set.mcp_servers.len(), 1);
        assert_eq!(set.mcp_servers[0].name, "filesystem");
        assert_eq!(set.mcp_servers[0].command, "npx");
        assert_eq!(set.mcp_servers[0].args.len(), 3);
    }

    /// 7. Merge priority: user global > project > path.
    #[test]
    fn merge_priority_order() {
        let dir = setup_workspace(&[
            ("CADUCEUS.md", "Project instructions here."),
            ("AGENTS.md", "Agent configuration here."),
        ]);

        // Create a fake user-global file
        let user_dir = dir.path().join("fake_home/.caduceus");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("instructions.md"), "User global prefs.").unwrap();

        // We test the ordering by checking the system prompt sections appear
        // in the correct XML-tag order. Since we can't override HOME easily,
        // we verify project + agents ordering.
        let loader = InstructionLoader::new(dir.path());
        let set = loader.load().unwrap();

        let prompt = &set.system_prompt;
        let project_pos = prompt.find("project_instructions").unwrap();
        let agents_pos = prompt.find("agents_config").unwrap();
        assert!(
            project_pos < agents_pos,
            "project_instructions should appear before agents_config in the merged prompt"
        );

        // Project instructions includes both files
        assert!(set.project_instructions.contains("Project instructions"));
        assert!(set.project_instructions.contains("Agent configuration"));
    }

    /// 8. Memory entries are loaded and appear in system prompt.
    #[test]
    fn load_memory_entries() {
        let dir = setup_workspace(&[(
            ".caduceus/memory.md",
            "# Memory\nPrefer async/await over raw futures.\nUser likes concise responses.",
        )]);
        let loader = InstructionLoader::new(dir.path());
        let set = loader.load().unwrap();

        assert_eq!(set.memory_entries.len(), 2);
        assert!(set.memory_entries[0].contains("async/await"));
        assert!(set.system_prompt.contains("<memory>"));
    }

    /// 9. Agents and skills appear in system prompt discovery sections.
    #[test]
    fn agents_skills_in_system_prompt() {
        let dir = setup_workspace(&[
            (
                ".caduceus/agents/reviewer.md",
                "---\nname: reviewer\ndescription: Review code\ntools: [read_file]\ntriggers:\n  - \"review\"\n---\nReview body.",
            ),
            (
                ".caduceus/skills/deploy.md",
                "---\nname: deploy\ndescription: Deploy app\ntriggers:\n  - \"deploy\"\n---\n1. Build\n2. Deploy",
            ),
        ]);
        let loader = InstructionLoader::new(dir.path());
        let set = loader.load().unwrap();

        assert!(set.system_prompt.contains("<available_agents>"));
        assert!(set.system_prompt.contains("reviewer"));
        assert!(set.system_prompt.contains("<available_skills>"));
        assert!(set.system_prompt.contains("deploy"));
    }

    // ── Glob matching unit tests ───────────────────────────────────────────────

    #[test]
    fn glob_star_matches_single_segment() {
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(!glob_matches("*.rs", "src/main.rs"));
    }

    #[test]
    fn glob_doublestar_matches_across_segments() {
        assert!(glob_matches("**/*.rs", "src/main.rs"));
        assert!(glob_matches("**/*.rs", "crates/core/src/lib.rs"));
        assert!(!glob_matches("**/*.rs", "src/main.ts"));
    }

    #[test]
    fn glob_question_mark() {
        assert!(glob_matches("?.rs", "a.rs"));
        assert!(!glob_matches("?.rs", "ab.rs"));
    }

    // ── Frontmatter parsing tests ──────────────────────────────────────────────

    #[test]
    fn split_frontmatter_works() {
        let input = "---\nname: test\n---\nBody content here.";
        let (yaml, body) = split_frontmatter(input);
        assert_eq!(yaml, Some("name: test"));
        assert_eq!(body, "Body content here.");
    }

    #[test]
    fn split_frontmatter_no_yaml() {
        let input = "Just a plain markdown file.";
        let (yaml, body) = split_frontmatter(input);
        assert!(yaml.is_none());
        assert_eq!(body, input);
    }
}
