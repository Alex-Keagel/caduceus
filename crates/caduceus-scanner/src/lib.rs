use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedLanguage {
    pub name: String,
    pub file_count: usize,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedFramework {
    pub name: String,
    pub confidence: f32,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub root: PathBuf,
    pub languages: Vec<DetectedLanguage>,
    pub frameworks: Vec<DetectedFramework>,
    pub total_files: usize,
    pub total_size_bytes: u64,
    pub entry_points: Vec<PathBuf>,
    pub token_estimate: u32,
}

impl ProjectContext {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            languages: Vec::new(),
            frameworks: Vec::new(),
            total_files: 0,
            total_size_bytes: 0,
            entry_points: Vec::new(),
            token_estimate: 0,
        }
    }
}

pub struct ProjectScanner {
    root: PathBuf,
    token_budget: u32,
}

impl ProjectScanner {
    pub fn new(root: impl Into<PathBuf>, token_budget: u32) -> Self {
        Self {
            root: root.into(),
            token_budget,
        }
    }

    pub fn scan(&self) -> caduceus_core::Result<ProjectContext> {
        let mut ctx = ProjectContext::new(self.root.clone());
        let mut ext_counts: HashMap<String, usize> = HashMap::new();

        for entry in walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            ctx.total_files += 1;
            if let Ok(meta) = entry.metadata() {
                ctx.total_size_bytes += meta.len();
            }

            if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                *ext_counts.entry(ext.to_lowercase()).or_insert(0) += 1;
            }
        }

        ctx.languages = Self::detect_languages(&ext_counts);
        ctx.frameworks = self.detect_frameworks();
        ctx.token_estimate = (ctx.total_size_bytes / 4).min(self.token_budget as u64) as u32;

        Ok(ctx)
    }

    fn detect_languages(ext_counts: &HashMap<String, usize>) -> Vec<DetectedLanguage> {
        let ext_map: &[(&str, &[&str])] = &[
            ("Rust", &["rs"]),
            ("TypeScript", &["ts", "tsx"]),
            ("JavaScript", &["js", "jsx", "mjs", "cjs"]),
            ("Python", &["py", "pyi"]),
            ("Go", &["go"]),
            ("C++", &["cpp", "cc", "cxx", "hpp", "h"]),
            ("Java", &["java"]),
            ("Ruby", &["rb"]),
            ("Swift", &["swift"]),
            ("Kotlin", &["kt", "kts"]),
        ];

        let mut langs = Vec::new();
        for (name, exts) in ext_map {
            let file_count: usize = exts.iter().filter_map(|e| ext_counts.get(*e)).sum();
            if file_count > 0 {
                langs.push(DetectedLanguage {
                    name: name.to_string(),
                    file_count,
                    extensions: exts.iter().map(|e| e.to_string()).collect(),
                });
            }
        }
        langs.sort_by(|a, b| b.file_count.cmp(&a.file_count));
        langs
    }

    fn detect_frameworks(&self) -> Vec<DetectedFramework> {
        let mut frameworks = Vec::new();

        let checks: &[(&str, &str, &[&str])] = &[
            ("React", "package.json", &["react", "react-dom"]),
            ("Next.js", "next.config.js", &[]),
            ("Vue", "package.json", &["vue"]),
            ("Tauri", "tauri.conf.json", &[]),
            ("Cargo Workspace", "Cargo.toml", &["workspace"]),
        ];

        for (name, file, keywords) in checks {
            let path = self.root.join(file);
            if path.exists() {
                let evidence = vec![file.to_string()];
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                let matches = keywords.iter().all(|kw| content.contains(kw));
                if keywords.is_empty() || matches {
                    frameworks.push(DetectedFramework {
                        name: name.to_string(),
                        confidence: if matches { 0.9 } else { 0.5 },
                        evidence,
                    });
                }
            }
        }

        frameworks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let ctx = ProjectContext::new("/tmp");
        assert_eq!(ctx.total_files, 0);
    }
}
