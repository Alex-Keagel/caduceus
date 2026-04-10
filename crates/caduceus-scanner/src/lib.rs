use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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
    pub context_summary: String,
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
            context_summary: String::new(),
        }
    }

    /// Convert to the core `ProjectContext` type used by the orchestration layer.
    pub fn to_core_context(&self) -> caduceus_core::ProjectContext {
        caduceus_core::ProjectContext {
            root: self.root.clone(),
            languages: self.languages.iter().map(|l| l.name.clone()).collect(),
            frameworks: self.frameworks.iter().map(|f| f.name.clone()).collect(),
            file_count: self.total_files,
            token_estimate: self.token_estimate,
            context_summary: self.context_summary.clone(),
        }
    }
}

/// Directory names to skip during traversal.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".svn",
    ".hg",
    "dist",
    ".next",
    "build",
];

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

        let walker = ignore::WalkBuilder::new(&self.root)
            .hidden(false)
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            .filter_entry(|e| {
                if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    let name = e.file_name().to_string_lossy();
                    return !SKIP_DIRS.contains(&name.as_ref());
                }
                true
            })
            .build();

        for entry in walker.filter_map(|e| e.ok()).filter(|e| {
            e.file_type().map(|ft| ft.is_file()).unwrap_or(false)
        }) {
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
        // Spec: rough estimate is file_count * 200 avg tokens per file
        ctx.token_estimate = ((ctx.total_files as u32).saturating_mul(200)).min(self.token_budget);
        ctx.entry_points = self.detect_entry_points();
        ctx.context_summary = self.generate_summary(&ctx);

        Ok(ctx)
    }

    fn detect_languages(ext_counts: &HashMap<String, usize>) -> Vec<DetectedLanguage> {
        let ext_map: &[(&str, &[&str])] = &[
            ("Rust", &["rs"]),
            ("TypeScript", &["ts", "tsx"]),
            ("JavaScript", &["js", "jsx", "mjs", "cjs"]),
            ("Python", &["py", "pyi"]),
            ("Go", &["go"]),
            ("Java", &["java"]),
            ("Ruby", &["rb"]),
            ("C++", &["cpp", "cc", "cxx", "hpp"]),
            ("C", &["c", "h"]),
            ("C#", &["cs"]),
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

        // Node.js / package.json
        let pkg_path = self.root.join("package.json");
        if pkg_path.exists() {
            let content = std::fs::read_to_string(&pkg_path).unwrap_or_default();
            let lower = content.to_lowercase();

            let node_frameworks: &[(&str, &[&str])] = &[
                ("React", &["\"react\"", "\"react-dom\""]),
                ("Vue", &["\"vue\""]),
                ("Angular", &["\"@angular/core\""]),
                ("Next.js", &["\"next\""]),
                ("Express", &["\"express\""]),
            ];

            let mut matched = false;
            for (name, keywords) in node_frameworks {
                if keywords.iter().any(|kw| lower.contains(kw)) {
                    frameworks.push(DetectedFramework {
                        name: name.to_string(),
                        confidence: 0.9,
                        evidence: vec!["package.json".to_string()],
                    });
                    matched = true;
                }
            }
            if !matched {
                frameworks.push(DetectedFramework {
                    name: "Node.js".to_string(),
                    confidence: 0.7,
                    evidence: vec!["package.json".to_string()],
                });
            }
        }

        // Rust / Cargo.toml
        let cargo_path = self.root.join("Cargo.toml");
        if cargo_path.exists() {
            let content = std::fs::read_to_string(&cargo_path).unwrap_or_default();
            let lower = content.to_lowercase();

            let rust_frameworks: &[(&str, &str)] = &[
                ("Tauri", "tauri"),
                ("Actix", "actix"),
                ("Axum", "axum"),
                ("Tokio", "tokio"),
                ("Rocket", "rocket"),
            ];

            let mut matched = false;
            for (name, keyword) in rust_frameworks {
                if lower.contains(keyword) {
                    frameworks.push(DetectedFramework {
                        name: name.to_string(),
                        confidence: 0.9,
                        evidence: vec!["Cargo.toml".to_string()],
                    });
                    matched = true;
                }
            }
            if !matched {
                frameworks.push(DetectedFramework {
                    name: "Rust".to_string(),
                    confidence: 0.8,
                    evidence: vec!["Cargo.toml".to_string()],
                });
            }
        }

        // Python: requirements.txt or pyproject.toml
        let py_files = ["requirements.txt", "pyproject.toml"];
        for py_file in &py_files {
            let path = self.root.join(py_file);
            if path.exists() {
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                let lower = content.to_lowercase();

                let py_frameworks: &[(&str, &str)] = &[
                    ("Django", "django"),
                    ("Flask", "flask"),
                    ("FastAPI", "fastapi"),
                ];

                let mut matched = false;
                for (name, keyword) in py_frameworks {
                    if lower.contains(keyword) {
                        frameworks.push(DetectedFramework {
                            name: name.to_string(),
                            confidence: 0.9,
                            evidence: vec![py_file.to_string()],
                        });
                        matched = true;
                    }
                }
                if !matched {
                    frameworks.push(DetectedFramework {
                        name: "Python".to_string(),
                        confidence: 0.7,
                        evidence: vec![py_file.to_string()],
                    });
                }
                break; // Only process first Python config found
            }
        }

        // Go
        if self.root.join("go.mod").exists() {
            frameworks.push(DetectedFramework {
                name: "Go".to_string(),
                confidence: 0.9,
                evidence: vec!["go.mod".to_string()],
            });
        }

        frameworks
    }

    fn detect_entry_points(&self) -> Vec<PathBuf> {
        let candidates = [
            "src/main.rs",
            "main.rs",
            "src/main.ts",
            "src/index.ts",
            "index.ts",
            "src/main.js",
            "src/index.js",
            "index.js",
            "main.go",
            "cmd/main.go",
            "main.py",
            "app.py",
            "run.py",
            "Main.java",
            "src/Main.java",
            "main.swift",
            "Sources/main.swift",
        ];
        candidates
            .iter()
            .map(|c| self.root.join(c))
            .filter(|p| p.exists())
            .collect()
    }

    fn generate_summary(&self, ctx: &ProjectContext) -> String {
        let root_name = self
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project");

        let lang_str = match ctx.languages.as_slice() {
            [] => "unknown language".to_string(),
            [single] => single.name.clone(),
            [first, rest @ ..] => format!(
                "{} and {}",
                first.name,
                rest.iter().map(|l| l.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        };

        let fw_part = if ctx.frameworks.is_empty() {
            String::new()
        } else {
            format!(
                " using {}",
                ctx.frameworks
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        format!(
            "Project '{}' contains {} files written primarily in {}{}, \
             with an estimated {} tokens of context.",
            root_name, ctx.total_files, lang_str, fw_part, ctx.token_estimate,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_context_new_is_empty() {
        let ctx = ProjectContext::new("/some/path");
        assert_eq!(ctx.total_files, 0);
        assert_eq!(ctx.token_estimate, 0);
        assert!(ctx.languages.is_empty());
        assert!(ctx.frameworks.is_empty());
    }

    #[test]
    fn detect_languages_rust_only() {
        let mut counts = HashMap::new();
        counts.insert("rs".to_string(), 42usize);
        let langs = ProjectScanner::detect_languages(&counts);
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].name, "Rust");
        assert_eq!(langs[0].file_count, 42);
    }

    #[test]
    fn detect_languages_multiple_sorted_by_count() {
        let mut counts = HashMap::new();
        counts.insert("py".to_string(), 5);
        counts.insert("rs".to_string(), 20);
        counts.insert("ts".to_string(), 10);
        let langs = ProjectScanner::detect_languages(&counts);
        // Should be sorted descending by file_count
        assert_eq!(langs[0].name, "Rust");
        assert_eq!(langs[1].name, "TypeScript");
        assert_eq!(langs[2].name, "Python");
    }

    #[test]
    fn detect_languages_all_supported() {
        let mut counts = HashMap::new();
        for ext in ["rs", "ts", "js", "py", "go", "java", "rb", "cpp", "c", "cs", "swift", "kt"] {
            counts.insert(ext.to_string(), 1);
        }
        let langs = ProjectScanner::detect_languages(&counts);
        let names: Vec<&str> = langs.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"Rust"));
        assert!(names.contains(&"TypeScript"));
        assert!(names.contains(&"Python"));
        assert!(names.contains(&"Go"));
        assert!(names.contains(&"Java"));
        assert!(names.contains(&"Ruby"));
        assert!(names.contains(&"C++"));
        assert!(names.contains(&"C"));
        assert!(names.contains(&"C#"));
        assert!(names.contains(&"Swift"));
        assert!(names.contains(&"Kotlin"));
    }

    #[test]
    fn token_estimate_is_file_count_times_200() {
        // Scan a real path we know exists (the caduceus repo)
        let scanner = ProjectScanner::new(
            "/Users/alexkeagel/Dev/caduceus/crates/caduceus-core",
            u32::MAX,
        );
        let ctx = scanner.scan().expect("scan should succeed");
        assert!(ctx.total_files > 0, "should find files");
        assert_eq!(ctx.token_estimate, (ctx.total_files as u32) * 200);
    }

    #[test]
    fn scan_caduceus_detects_rust_and_tokio() {
        let scanner = ProjectScanner::new("/Users/alexkeagel/Dev/caduceus", u32::MAX);
        let ctx = scanner.scan().expect("scan should succeed");

        let lang_names: Vec<&str> = ctx.languages.iter().map(|l| l.name.as_str()).collect();
        assert!(lang_names.contains(&"Rust"), "should detect Rust language");

        let fw_names: Vec<&str> = ctx.frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(fw_names.contains(&"Tokio"), "should detect Tokio framework");
    }

    #[test]
    fn scan_produces_non_empty_summary() {
        let scanner = ProjectScanner::new(
            "/Users/alexkeagel/Dev/caduceus/crates/caduceus-core",
            u32::MAX,
        );
        let ctx = scanner.scan().expect("scan should succeed");
        assert!(!ctx.context_summary.is_empty());
        assert!(ctx.context_summary.contains("caduceus-core"));
    }

    #[test]
    fn to_core_context_maps_fields() {
        let mut ctx = ProjectContext::new("/foo/bar");
        ctx.languages.push(DetectedLanguage {
            name: "Rust".into(),
            file_count: 3,
            extensions: vec!["rs".into()],
        });
        ctx.frameworks.push(DetectedFramework {
            name: "Axum".into(),
            confidence: 0.9,
            evidence: vec!["Cargo.toml".into()],
        });
        ctx.total_files = 3;
        ctx.token_estimate = 600;
        ctx.context_summary = "summary".into();

        let core = ctx.to_core_context();
        assert_eq!(core.languages, vec!["Rust"]);
        assert_eq!(core.frameworks, vec!["Axum"]);
        assert_eq!(core.file_count, 3);
        assert_eq!(core.token_estimate, 600);
    }
}
