pub mod catalog;
pub mod error;
pub mod installer;
pub mod manifest;
pub mod recommender;
pub mod registry;

pub use catalog::{BuiltinCatalog, CatalogAgent, CatalogSkill};
pub use error::MarketplaceError;
pub use installer::{install_from_path, uninstall, verify_manifest};
pub use manifest::{Category, PluginManifest};
pub use recommender::{recommend, ProjectContext, Recommendations};
pub use registry::{AgentEntry, MarketplaceRegistry, PluginEntry, SkillEntry};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("test_artifacts")
            .join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ── manifest.rs ────────────────────────────────────────────────────────────

    #[test]
    fn test_manifest_parse_valid() {
        let dir = test_dir("manifest_valid");
        let plugin_json = dir.join("plugin.json");
        fs::write(
            &plugin_json,
            r#"{
                "name": "my-plugin",
                "version": "0.1.0",
                "description": "A test plugin",
                "author": "Alice",
                "categories": ["CodeReview", "Testing"],
                "commands": ["review"],
                "hooks": [],
                "skills": ["code-review"],
                "agents": []
            }"#,
        )
        .unwrap();

        let manifest = PluginManifest::from_file(&plugin_json).unwrap();
        assert_eq!(manifest.name, "my-plugin");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.author, "Alice");
        assert!(manifest.skills.contains(&"code-review".to_string()));
    }

    #[test]
    fn test_manifest_parse_invalid_json() {
        let dir = test_dir("manifest_invalid");
        let plugin_json = dir.join("plugin.json");
        fs::write(&plugin_json, "{ not valid json").unwrap();
        assert!(PluginManifest::from_file(&plugin_json).is_err());
    }

    #[test]
    fn test_manifest_missing_file() {
        let path = PathBuf::from("/nonexistent/path/plugin.json");
        assert!(PluginManifest::from_file(&path).is_err());
    }

    // ── registry.rs ────────────────────────────────────────────────────────────

    #[test]
    fn test_registry_search_skills() {
        let mut registry = MarketplaceRegistry::new();
        registry.register_skill(SkillEntry {
            name: "code-review".to_string(),
            version: "1.0".to_string(),
            description: "Review code for quality".to_string(),
            categories: vec![Category::CodeReview],
            triggers: vec!["review code".to_string()],
            tools: vec![],
        });
        registry.register_skill(SkillEntry {
            name: "security-audit".to_string(),
            version: "1.0".to_string(),
            description: "Audit security vulnerabilities".to_string(),
            categories: vec![Category::Security],
            triggers: vec!["security audit".to_string()],
            tools: vec![],
        });

        let results = registry.search_skills("security");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "security-audit");

        let all = registry.list_skills();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_registry_search_agents() {
        let mut registry = MarketplaceRegistry::new();
        registry.register_agent(AgentEntry {
            name: "debugger".to_string(),
            version: "1.0".to_string(),
            description: "Debug complex issues".to_string(),
            specialty: "debugging".to_string(),
            tools: vec![],
            triggers: vec!["debug this".to_string()],
        });
        registry.register_agent(AgentEntry {
            name: "tester".to_string(),
            version: "1.0".to_string(),
            description: "Run test suites".to_string(),
            specialty: "testing".to_string(),
            tools: vec![],
            triggers: vec!["run tests".to_string()],
        });

        let results = registry.search_agents("debug");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "debugger");
    }

    // ── catalog.rs ─────────────────────────────────────────────────────────────

    #[test]
    fn test_catalog_skill_count() {
        let skills = BuiltinCatalog::skills();
        // At least 50 skills in the catalog
        assert!(
            skills.len() >= 50,
            "expected ≥50 skills, got {}",
            skills.len()
        );

        // All names unique
        let names: HashSet<_> = skills.iter().map(|s| s.name).collect();
        assert_eq!(names.len(), skills.len(), "duplicate skill names found");
    }

    #[test]
    fn test_catalog_agent_count() {
        let agents = BuiltinCatalog::agents();
        assert!(
            agents.len() >= 20,
            "expected ≥20 agents, got {}",
            agents.len()
        );

        let names: HashSet<_> = agents.iter().map(|a| a.name).collect();
        assert_eq!(names.len(), agents.len(), "duplicate agent names found");
    }

    #[test]
    fn test_catalog_by_category() {
        let security_skills = BuiltinCatalog::skills_by_category(&Category::Security);
        assert!(
            !security_skills.is_empty(),
            "expected security skills in catalog"
        );
        for s in &security_skills {
            assert!(s.categories.contains(&Category::Security));
        }

        let security_agents = BuiltinCatalog::agents_by_category(&Category::Security);
        assert!(!security_agents.is_empty());
    }

    // ── recommender.rs ─────────────────────────────────────────────────────────

    #[test]
    fn test_recommender_rust_project() {
        let ctx = ProjectContext {
            languages: vec!["rust".to_string()],
            frameworks: vec!["tokio".to_string(), "axum".to_string()],
        };
        let recs = recommend(&ctx, None, 10);
        assert!(!recs.skills.is_empty());
        assert!(!recs.agents.is_empty());
        // type-safety and error-handling should score higher for Rust projects
        let top_skill_names: Vec<_> = recs.skills.iter().map(|r| r.skill.name).collect();
        assert!(
            top_skill_names.contains(&"type-safety") || top_skill_names.contains(&"error-handling"),
            "expected Rust-relevant skills in top recommendations"
        );
    }

    #[test]
    fn test_recommender_python_project() {
        let ctx = ProjectContext {
            languages: vec!["python".to_string()],
            frameworks: vec!["pandas".to_string(), "airflow".to_string()],
        };
        let recs = recommend(&ctx, None, 10);
        assert!(!recs.skills.is_empty());
        let top_names: Vec<_> = recs.skills.iter().map(|r| r.skill.name).collect();
        // etl-pipeline matches python + airflow/pandas
        assert!(
            top_names.contains(&"etl-pipeline") || top_names.contains(&"query-optimize"),
            "expected Python data skills in top recommendations, got {:?}",
            top_names
        );
    }

    #[test]
    fn test_recommender_with_prompt() {
        let ctx = ProjectContext::default();
        let recs = recommend(&ctx, Some("security audit for my api"), 5);
        assert!(!recs.skills.is_empty());
        // security-audit trigger "security audit" should match
        let top_names: Vec<_> = recs.skills.iter().map(|r| r.skill.name).collect();
        assert!(
            top_names.contains(&"security-audit"),
            "expected security-audit in prompt-driven results, got {:?}",
            top_names
        );
    }

    // ── installer.rs ───────────────────────────────────────────────────────────

    #[test]
    fn test_installer_verify_manifest() {
        let dir = test_dir("install_verify");
        let plugin_json = dir.join("plugin.json");
        fs::write(
            &plugin_json,
            r#"{"name":"p","version":"0.1","description":"d","author":"a","categories":[],"commands":[],"hooks":[],"skills":[],"agents":[]}"#,
        )
        .unwrap();

        let manifest = verify_manifest(&plugin_json).unwrap();
        assert_eq!(manifest.name, "p");
    }

    #[test]
    fn test_installer_verify_missing() {
        let path = PathBuf::from("/nonexistent/plugin.json");
        assert!(verify_manifest(&path).is_err());
    }

    #[test]
    fn test_installer_install_and_uninstall() {
        let base = test_dir("install_ops");
        let source = base.join("my-plugin");
        let target = base.join("installed");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("plugin.json"),
            r#"{"name":"my-plugin","version":"0.1","description":"d","author":"a","categories":[],"commands":[],"hooks":[],"skills":[],"agents":[]}"#,
        )
        .unwrap();
        fs::write(source.join("README.md"), "# Plugin").unwrap();

        install_from_path(&source, &target).unwrap();
        assert!(target.join("my-plugin").join("plugin.json").exists());
        assert!(target.join("my-plugin").join("README.md").exists());

        uninstall("my-plugin", &target).unwrap();
        assert!(!target.join("my-plugin").exists());
    }
}
