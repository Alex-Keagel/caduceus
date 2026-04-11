use caduceus_core::Result;
use std::path::{Path, PathBuf};

// ── Mention types ──────────────────────────────────────────────────────────────

/// Represents a parsed @ mention in user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mention {
    /// `@file:path/to/file.rs` — read file content
    File(PathBuf),
    /// `@folder:src/` — list directory tree
    Folder(PathBuf),
    /// `@url:https://...` — fetch URL content
    Url(String),
    /// `@git:diff` — current git diff
    GitDiff,
    /// `@git:status` — current git status
    GitStatus,
    /// `@git:log:N` — last N commits
    GitLog(usize),
}

impl Mention {
    /// Parse a single mention token like `@file:src/lib.rs`.
    pub fn parse(token: &str) -> Option<Self> {
        let stripped = token.strip_prefix('@')?;

        if let Some(path) = stripped.strip_prefix("file:") {
            if !path.is_empty() {
                return Some(Self::File(PathBuf::from(path)));
            }
        }

        if let Some(path) = stripped.strip_prefix("folder:") {
            if !path.is_empty() {
                return Some(Self::Folder(PathBuf::from(path)));
            }
        }

        if let Some(url) = stripped.strip_prefix("url:") {
            if !url.is_empty() {
                return Some(Self::Url(url.to_string()));
            }
        }

        if stripped == "git:diff" {
            return Some(Self::GitDiff);
        }

        if stripped == "git:status" {
            return Some(Self::GitStatus);
        }

        if let Some(n_str) = stripped.strip_prefix("git:log:") {
            if let Ok(n) = n_str.parse::<usize>() {
                return Some(Self::GitLog(n));
            }
        }

        None
    }
}

// ── Mention resolver ───────────────────────────────────────────────────────────

/// Extracts @ mentions from user input and resolves them to context text.
pub struct MentionResolver {
    workspace_root: PathBuf,
    max_file_size: usize,
    max_folder_depth: usize,
}

impl MentionResolver {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            max_file_size: 1_048_576, // 1 MB
            max_folder_depth: 3,
        }
    }

    pub fn with_max_file_size(mut self, size: usize) -> Self {
        self.max_file_size = size;
        self
    }

    /// Extract all mentions from user input text.
    pub fn extract_mentions(input: &str) -> Vec<Mention> {
        input
            .split_whitespace()
            .filter_map(Mention::parse)
            .collect()
    }

    /// Resolve all mentions in user input and return the expanded context string.
    /// The original mention tokens are left in the user message; resolved content
    /// is returned separately for injection as ephemeral context.
    pub fn resolve(&self, input: &str) -> Result<Option<String>> {
        let mentions = Self::extract_mentions(input);
        if mentions.is_empty() {
            return Ok(None);
        }

        let mut context_parts = Vec::new();
        for mention in &mentions {
            match self.resolve_one(mention) {
                Ok(content) => context_parts.push(content),
                Err(e) => {
                    context_parts.push(format!("[Error resolving mention: {}]", e));
                }
            }
        }

        if context_parts.is_empty() {
            return Ok(None);
        }

        Ok(Some(context_parts.join("\n\n")))
    }

    fn resolve_one(&self, mention: &Mention) -> Result<String> {
        match mention {
            Mention::File(path) => self.resolve_file(path),
            Mention::Folder(path) => self.resolve_folder(path),
            Mention::Url(url) => Ok(format!(
                "<url_mention src=\"{}\">\n[URL fetch requires async — content placeholder]\n</url_mention>",
                url
            )),
            Mention::GitDiff => self.resolve_git_diff(),
            Mention::GitStatus => self.resolve_git_status(),
            Mention::GitLog(n) => self.resolve_git_log(*n),
        }
    }

    fn resolve_file(&self, path: &Path) -> Result<String> {
        let full_path = self.workspace_root.join(path);
        let canonical =
            full_path
                .canonicalize()
                .map_err(|e| caduceus_core::CaduceusError::Tool {
                    tool: "mention".into(),
                    message: format!("Cannot resolve file path {}: {}", path.display(), e),
                })?;

        // Validate within workspace
        if !canonical.starts_with(
            self.workspace_root
                .canonicalize()
                .unwrap_or_else(|_| self.workspace_root.clone()),
        ) {
            return Err(caduceus_core::CaduceusError::PermissionDenied {
                capability: "fs".into(),
                tool: format!("File {} is outside workspace", path.display()),
            });
        }

        let metadata =
            std::fs::metadata(&canonical).map_err(|e| caduceus_core::CaduceusError::Tool {
                tool: "mention".into(),
                message: format!("Cannot stat {}: {}", path.display(), e),
            })?;

        if metadata.len() as usize > self.max_file_size {
            return Ok(format!(
                "<file_mention path=\"{}\">\n[File too large: {} bytes, max {} bytes]\n</file_mention>",
                path.display(),
                metadata.len(),
                self.max_file_size
            ));
        }

        let content = std::fs::read_to_string(&canonical).map_err(|e| {
            caduceus_core::CaduceusError::Tool {
                tool: "mention".into(),
                message: format!("Cannot read {}: {}", path.display(), e),
            }
        })?;

        Ok(format!(
            "<file_mention path=\"{}\">\n{}\n</file_mention>",
            path.display(),
            content
        ))
    }

    fn resolve_folder(&self, path: &Path) -> Result<String> {
        let full_path = self.workspace_root.join(path);
        if !full_path.is_dir() {
            return Err(caduceus_core::CaduceusError::Tool {
                tool: "mention".into(),
                message: format!("{} is not a directory", path.display()),
            });
        }

        let mut tree = String::new();
        self.build_tree(&full_path, path, 0, &mut tree);

        Ok(format!(
            "<folder_mention path=\"{}\">\n{}</folder_mention>",
            path.display(),
            tree
        ))
    }

    fn build_tree(&self, abs_path: &Path, rel_path: &Path, depth: usize, out: &mut String) {
        if depth > self.max_folder_depth {
            return;
        }

        let entries = match std::fs::read_dir(abs_path) {
            Ok(entries) => entries,
            Err(_) => return,
        };

        let indent = "  ".repeat(depth);
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') {
                continue;
            }

            let entry_rel = rel_path.join(&name);
            if entry.path().is_dir() {
                out.push_str(&format!("{}{}/\n", indent, name_str));
                self.build_tree(&entry.path(), &entry_rel, depth + 1, out);
            } else {
                out.push_str(&format!("{}{}\n", indent, name_str));
            }
        }
    }

    fn resolve_git_diff(&self) -> Result<String> {
        let output = std::process::Command::new("git")
            .args(["diff"])
            .current_dir(&self.workspace_root)
            .output()
            .map_err(|e| caduceus_core::CaduceusError::Tool {
                tool: "mention".into(),
                message: format!("Failed to run git diff: {}", e),
            })?;

        let diff = String::from_utf8_lossy(&output.stdout);
        if diff.is_empty() {
            return Ok("<git_diff>\nNo changes (working tree clean)\n</git_diff>".into());
        }

        Ok(format!("<git_diff>\n{}\n</git_diff>", diff))
    }

    fn resolve_git_status(&self) -> Result<String> {
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.workspace_root)
            .output()
            .map_err(|e| caduceus_core::CaduceusError::Tool {
                tool: "mention".into(),
                message: format!("Failed to run git status: {}", e),
            })?;

        let status = String::from_utf8_lossy(&output.stdout);
        if status.is_empty() {
            return Ok("<git_status>\nNo changes (working tree clean)\n</git_status>".into());
        }

        Ok(format!("<git_status>\n{}\n</git_status>", status))
    }

    fn resolve_git_log(&self, n: usize) -> Result<String> {
        let n_str = format!("-{}", n.min(100));
        let output = std::process::Command::new("git")
            .args(["log", "--oneline", &n_str])
            .current_dir(&self.workspace_root)
            .output()
            .map_err(|e| caduceus_core::CaduceusError::Tool {
                tool: "mention".into(),
                message: format!("Failed to run git log: {}", e),
            })?;

        let log = String::from_utf8_lossy(&output.stdout);
        Ok(format!("<git_log count=\"{}\">\n{}\n</git_log>", n, log))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file_mention() {
        assert_eq!(
            Mention::parse("@file:src/lib.rs"),
            Some(Mention::File(PathBuf::from("src/lib.rs")))
        );
    }

    #[test]
    fn parse_folder_mention() {
        assert_eq!(
            Mention::parse("@folder:src/"),
            Some(Mention::Folder(PathBuf::from("src/")))
        );
    }

    #[test]
    fn parse_url_mention() {
        assert_eq!(
            Mention::parse("@url:https://example.com"),
            Some(Mention::Url("https://example.com".into()))
        );
    }

    #[test]
    fn parse_git_mentions() {
        assert_eq!(Mention::parse("@git:diff"), Some(Mention::GitDiff));
        assert_eq!(Mention::parse("@git:status"), Some(Mention::GitStatus));
        assert_eq!(Mention::parse("@git:log:5"), Some(Mention::GitLog(5)));
        assert_eq!(Mention::parse("@git:log:10"), Some(Mention::GitLog(10)));
    }

    #[test]
    fn parse_invalid_mentions() {
        assert_eq!(Mention::parse("hello"), None);
        assert_eq!(Mention::parse("@"), None);
        assert_eq!(Mention::parse("@file:"), None);
        assert_eq!(Mention::parse("@unknown:thing"), None);
        assert_eq!(Mention::parse("@git:log:abc"), None);
    }

    #[test]
    fn extract_mentions_from_input() {
        let input = "Fix @file:src/main.rs and check @git:status please";
        let mentions = MentionResolver::extract_mentions(input);
        assert_eq!(mentions.len(), 2);
        assert_eq!(mentions[0], Mention::File(PathBuf::from("src/main.rs")));
        assert_eq!(mentions[1], Mention::GitStatus);
    }

    #[test]
    fn extract_no_mentions() {
        let mentions = MentionResolver::extract_mentions("just a normal message");
        assert!(mentions.is_empty());
    }

    #[test]
    fn resolve_file_in_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello world").unwrap();

        let resolver = MentionResolver::new(dir.path());
        let result = resolver.resolve("Check @file:test.txt").unwrap();
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(ctx.contains("hello world"));
        assert!(ctx.contains("file_mention"));
    }

    #[test]
    fn resolve_folder_in_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub mod foo;").unwrap();

        let resolver = MentionResolver::new(dir.path());
        let result = resolver.resolve("List @folder:src").unwrap();
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(ctx.contains("folder_mention"));
        assert!(ctx.contains("main.rs"));
        assert!(ctx.contains("lib.rs"));
    }

    #[test]
    fn resolve_git_diff_in_repo() {
        // This test runs in a real git repo
        let resolver = MentionResolver::new(".");
        let result = resolver.resolve("Show @git:diff").unwrap();
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(ctx.contains("git_diff"));
    }

    #[test]
    fn resolve_git_status_in_repo() {
        let resolver = MentionResolver::new(".");
        let result = resolver.resolve("Check @git:status").unwrap();
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(ctx.contains("git_status"));
    }

    #[test]
    fn resolve_git_log_in_repo() {
        let resolver = MentionResolver::new(".");
        let result = resolver.resolve("Show @git:log:3").unwrap();
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(ctx.contains("git_log"));
    }

    #[test]
    fn resolve_no_mentions_returns_none() {
        let resolver = MentionResolver::new(".");
        let result = resolver.resolve("No mentions here").unwrap();
        assert!(result.is_none());
    }
}
