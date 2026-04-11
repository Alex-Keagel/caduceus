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

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

// ── #207: Skill Evolver ───────────────────────────────────────────────────

pub struct SessionSummary {
    pub session_id: String,
    pub patterns: Vec<String>,
    pub tools_used: Vec<String>,
    pub success: bool,
}

pub struct EvolverConfig {
    pub min_sessions_before_evolve: usize,
    pub evolution_interval_secs: u64,
    pub quality_threshold: f64,
}

pub struct EvolvedSkill {
    pub name: String,
    pub content: String,
    pub version: u32,
    pub source_sessions: Vec<String>,
    pub quality_score: f64,
    pub created_at: u64,
}

pub struct SkillEvolver {
    config: EvolverConfig,
    skill_registry: Vec<EvolvedSkill>,
}

impl SkillEvolver {
    pub fn new(config: EvolverConfig) -> Self {
        Self {
            config,
            skill_registry: Vec::new(),
        }
    }

    pub fn evolve_from_summaries(&mut self, summaries: &[SessionSummary]) -> Vec<EvolvedSkill> {
        let successful_count = summaries.iter().filter(|s| s.success).count();
        if !self.should_evolve(successful_count) {
            return Vec::new();
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
        for s in summaries.iter().filter(|s| s.success) {
            if let Some(pattern) = s.patterns.first() {
                grouped
                    .entry(pattern.clone())
                    .or_default()
                    .push(s.session_id.clone());
            }
        }
        grouped
            .into_iter()
            .filter_map(|(pattern, session_ids)| {
                let quality = session_ids.len() as f64 / successful_count as f64;
                if quality >= self.config.quality_threshold {
                    Some(EvolvedSkill {
                        name: pattern.to_lowercase().replace(' ', "-"),
                        content: format!(
                            "# Skill: {pattern}\n\nAuto-evolved from {} sessions.",
                            session_ids.len()
                        ),
                        version: 1,
                        source_sessions: session_ids,
                        quality_score: quality,
                        created_at: now,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn should_evolve(&self, session_count: usize) -> bool {
        session_count >= self.config.min_sessions_before_evolve
    }

    pub fn register_evolved(&mut self, skill: EvolvedSkill) {
        self.skill_registry.push(skill);
    }

    pub fn list_evolved(&self) -> &[EvolvedSkill] {
        &self.skill_registry
    }
}

// ── #208: Pattern Aggregator ──────────────────────────────────────────────

pub struct PatternEntry {
    pub pattern: String,
    pub occurrences: usize,
    pub sessions: Vec<String>,
    pub confidence: f64,
}

pub struct PatternAggregator {
    patterns: HashMap<String, PatternEntry>,
    min_occurrences: usize,
    total_sessions: usize,
}

impl PatternAggregator {
    pub fn new(min_occurrences: usize) -> Self {
        Self {
            patterns: HashMap::new(),
            min_occurrences,
            total_sessions: 0,
        }
    }

    pub fn ingest_session(&mut self, session_id: &str, patterns: &[String]) {
        self.total_sessions += 1;
        for pattern in patterns {
            let entry = self
                .patterns
                .entry(pattern.clone())
                .or_insert_with(|| PatternEntry {
                    pattern: pattern.clone(),
                    occurrences: 0,
                    sessions: Vec::new(),
                    confidence: 0.0,
                });
            if !entry.sessions.contains(&session_id.to_string()) {
                entry.occurrences += 1;
                entry.sessions.push(session_id.to_string());
            }
        }
        let total = self.total_sessions;
        for entry in self.patterns.values_mut() {
            entry.confidence = entry.occurrences as f64 / total as f64;
        }
    }

    pub fn aggregate(&self) -> Vec<&PatternEntry> {
        let mut result: Vec<&PatternEntry> = self
            .patterns
            .values()
            .filter(|e| e.occurrences >= self.min_occurrences)
            .collect();
        result.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        result
    }

    pub fn top_patterns(&self, n: usize) -> Vec<&PatternEntry> {
        self.aggregate().into_iter().take(n).collect()
    }

    pub fn clear(&mut self) {
        self.patterns.clear();
        self.total_sessions = 0;
    }
}

// ── #209: Skill Auto-Generator ────────────────────────────────────────────

pub struct SkillAutoGenerator;

impl SkillAutoGenerator {
    pub fn generate_skill_md(
        name: &str,
        description: &str,
        patterns: &[String],
        tools: &[String],
    ) -> String {
        let patterns_str = patterns
            .iter()
            .map(|p| format!("- {p}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tools_str = if tools.is_empty() {
            "none".to_string()
        } else {
            tools.join(", ")
        };
        format!(
            "# Skill: {name}\n\n## Description\n{description}\n\n## Patterns\n{patterns_str}\n\n## Tools\n{tools_str}\n"
        )
    }

    pub fn generate_from_pattern(pattern: &PatternEntry) -> String {
        let name = Self::suggest_skill_name(std::slice::from_ref(&pattern.pattern));
        Self::generate_skill_md(
            &name,
            &format!("Auto-generated skill from pattern: {}", pattern.pattern),
            std::slice::from_ref(&pattern.pattern),
            &[],
        )
    }

    pub fn suggest_skill_name(patterns: &[String]) -> String {
        if patterns.is_empty() {
            return "unnamed-skill".to_string();
        }
        patterns[0]
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
    }
}

// ── #210: Skill Quality Scorer ────────────────────────────────────────────

pub struct QualityWeights {
    pub success_rate_weight: f64,
    pub usage_frequency_weight: f64,
    pub user_feedback_weight: f64,
    pub recency_weight: f64,
}

impl Default for QualityWeights {
    fn default() -> Self {
        Self {
            success_rate_weight: 0.4,
            usage_frequency_weight: 0.2,
            user_feedback_weight: 0.2,
            recency_weight: 0.2,
        }
    }
}

pub struct SkillScoreInput {
    pub total_uses: u32,
    pub successful_uses: u32,
    pub user_ratings: Vec<f64>,
    pub last_used_epoch: u64,
    pub days_since_creation: u32,
}

pub struct SkillQualityScorer {
    weights: QualityWeights,
}

impl Default for SkillQualityScorer {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillQualityScorer {
    pub fn new() -> Self {
        Self {
            weights: QualityWeights::default(),
        }
    }

    pub fn with_weights(weights: QualityWeights) -> Self {
        Self { weights }
    }

    pub fn score(&self, input: &SkillScoreInput) -> f64 {
        let success_rate = if input.total_uses > 0 {
            input.successful_uses as f64 / input.total_uses as f64
        } else {
            0.0
        };
        let usage_freq = if input.days_since_creation > 0 {
            (input.total_uses as f64 / input.days_since_creation as f64).min(1.0)
        } else if input.total_uses > 0 {
            1.0
        } else {
            0.0
        };
        let avg_rating = if input.user_ratings.is_empty() {
            0.5
        } else {
            let sum: f64 = input.user_ratings.iter().sum();
            (sum / input.user_ratings.len() as f64).clamp(0.0, 1.0)
        };
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let days_since_use = if now_secs > input.last_used_epoch {
            (now_secs - input.last_used_epoch) as f64 / 86400.0
        } else {
            0.0
        };
        let recency = (-days_since_use / 30.0_f64).exp();
        let w = &self.weights;
        let total_weight = w.success_rate_weight
            + w.usage_frequency_weight
            + w.user_feedback_weight
            + w.recency_weight;
        if total_weight == 0.0 {
            return 0.0;
        }
        ((success_rate * w.success_rate_weight
            + usage_freq * w.usage_frequency_weight
            + avg_rating * w.user_feedback_weight
            + recency * w.recency_weight)
            / total_weight)
            .clamp(0.0, 1.0)
    }

    pub fn should_promote(&self, score: f64) -> bool {
        score >= 0.7
    }

    pub fn should_deprecate(&self, score: f64) -> bool {
        score < 0.3
    }
}

// ── #211: Collective Skill Sync ───────────────────────────────────────────

pub struct SyncableSkill {
    pub name: String,
    pub version: u32,
    pub hash: String,
    pub content: String,
}

#[derive(Debug, PartialEq)]
pub enum SyncAction {
    Push(String),
    Pull(String),
    Conflict(String, String),
    UpToDate,
}

pub struct SkillSyncManager {
    local_skills: Vec<SyncableSkill>,
    remote_skills: Vec<SyncableSkill>,
}

impl Default for SkillSyncManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillSyncManager {
    pub fn new() -> Self {
        Self {
            local_skills: Vec::new(),
            remote_skills: Vec::new(),
        }
    }

    pub fn add_local(&mut self, skill: SyncableSkill) {
        self.local_skills.push(skill);
    }

    pub fn add_remote(&mut self, skill: SyncableSkill) {
        self.remote_skills.push(skill);
    }

    pub fn diff(&self) -> Vec<SyncAction> {
        let mut actions = Vec::new();
        let local_map: HashMap<&str, &SyncableSkill> = self
            .local_skills
            .iter()
            .map(|s| (s.name.as_str(), s))
            .collect();
        let remote_map: HashMap<&str, &SyncableSkill> = self
            .remote_skills
            .iter()
            .map(|s| (s.name.as_str(), s))
            .collect();
        for (name, local) in &local_map {
            match remote_map.get(name) {
                None => actions.push(SyncAction::Push(name.to_string())),
                Some(remote) => {
                    if local.hash == remote.hash {
                        actions.push(SyncAction::UpToDate);
                    } else {
                        actions.push(SyncAction::Conflict(
                            name.to_string(),
                            format!("local v{} vs remote v{}", local.version, remote.version),
                        ));
                    }
                }
            }
        }
        for name in remote_map.keys() {
            if !local_map.contains_key(name) {
                actions.push(SyncAction::Pull(name.to_string()));
            }
        }
        actions
    }

    pub fn resolve_conflicts(&self, prefer: &str) -> Vec<SyncAction> {
        self.diff()
            .into_iter()
            .map(|action| match action {
                SyncAction::Conflict(name, _) => {
                    if prefer == "local" {
                        SyncAction::Push(name)
                    } else {
                        SyncAction::Pull(name)
                    }
                }
                other => other,
            })
            .collect()
    }
}

// ── #212: Skill Versioning & Rollback ────────────────────────────────────

pub struct SkillVersion {
    pub version: u32,
    pub content: String,
    pub timestamp: u64,
    pub change_summary: String,
}

pub struct SkillVersionManager {
    versions: HashMap<String, Vec<SkillVersion>>,
}

impl Default for SkillVersionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillVersionManager {
    pub fn new() -> Self {
        Self {
            versions: HashMap::new(),
        }
    }

    pub fn record_version(&mut self, name: &str, content: &str, summary: &str) {
        let versions = self.versions.entry(name.to_string()).or_default();
        let version = versions.len() as u32 + 1;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        versions.push(SkillVersion {
            version,
            content: content.to_string(),
            timestamp,
            change_summary: summary.to_string(),
        });
    }

    pub fn get_version(&self, name: &str, version: u32) -> Option<&SkillVersion> {
        self.versions
            .get(name)?
            .iter()
            .find(|v| v.version == version)
    }

    pub fn latest_version(&self, name: &str) -> Option<&SkillVersion> {
        self.versions.get(name)?.last()
    }

    pub fn rollback(&self, name: &str, version: u32) -> Option<String> {
        Some(self.get_version(name, version)?.content.clone())
    }

    pub fn history(&self, name: &str) -> Option<&[SkillVersion]> {
        Some(self.versions.get(name)?.as_slice())
    }

    pub fn diff_versions(&self, name: &str, v1: u32, v2: u32) -> Option<String> {
        let c1 = &self.get_version(name, v1)?.content;
        let c2 = &self.get_version(name, v2)?.content;
        if c1 == c2 {
            Some("No differences".to_string())
        } else {
            Some(format!(
                "v{v1} → v{v2}: content differs ({} chars vs {} chars)",
                c1.len(),
                c2.len()
            ))
        }
    }
}

// ── #215: Memory Garbage Collector ───────────────────────────────────────

pub struct GarbageCandidate {
    pub id: String,
    pub last_accessed_days_ago: u32,
    pub access_count: u32,
    pub size_bytes: usize,
}

pub struct MemoryGarbageCollector {
    threshold_days: u32,
    max_items: usize,
}

impl MemoryGarbageCollector {
    pub fn new(threshold_days: u32, max_items: usize) -> Self {
        Self {
            threshold_days,
            max_items,
        }
    }

    pub fn identify_garbage(&self, items: &[GarbageCandidate]) -> Vec<String> {
        let mut garbage: Vec<String> = items
            .iter()
            .filter(|i| i.last_accessed_days_ago > self.threshold_days)
            .map(|i| i.id.clone())
            .collect();
        let mut remaining: Vec<&GarbageCandidate> =
            items.iter().filter(|i| !garbage.contains(&i.id)).collect();
        if remaining.len() > self.max_items {
            remaining.sort_by_key(|i| i.access_count);
            let excess = remaining.len() - self.max_items;
            for item in remaining.iter().take(excess) {
                garbage.push(item.id.clone());
            }
        }
        garbage
    }

    pub fn collect(&self, items: &mut Vec<GarbageCandidate>) -> usize {
        let garbage = self.identify_garbage(items);
        let before = items.len();
        items.retain(|i| !garbage.contains(&i.id));
        before - items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;
    use std::time::SystemTime;

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

    // ── Additional marketplace tests ─────────────────────────────────────

    #[test]
    fn test_recommend_rust_project() {
        let ctx = ProjectContext {
            languages: vec!["rust".to_string()],
            frameworks: vec!["tokio".to_string(), "serde".to_string()],
        };
        let recs = recommend(&ctx, None, 10);
        assert!(
            !recs.skills.is_empty(),
            "Rust project should get skill recommendations"
        );
        assert!(
            !recs.agents.is_empty(),
            "Rust project should get agent recommendations"
        );

        // Verify scores are in descending order
        for window in recs.skills.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "skills should be sorted by score descending"
            );
        }
        for window in recs.agents.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "agents should be sorted by score descending"
            );
        }
    }

    #[test]
    fn test_recommend_with_prompt() {
        let ctx = ProjectContext::default();
        let recs = recommend(&ctx, Some("add authentication and authorization"), 10);
        assert!(
            !recs.skills.is_empty(),
            "prompt should trigger recommendations"
        );

        let skill_names: Vec<_> = recs.skills.iter().map(|r| r.skill.name).collect();
        // "authentication and authorization" should surface security-related skills
        assert!(
            skill_names
                .iter()
                .any(|n| n.contains("security") || n.contains("auth")),
            "expected security/auth-related skill in recommendations, got: {:?}",
            skill_names
        );
    }

    #[test]
    fn test_skill_search_fuzzy() {
        let mut registry = MarketplaceRegistry::new();
        registry.register_skill(SkillEntry {
            name: "authentication-setup".to_string(),
            version: "1.0".to_string(),
            description: "Set up authentication flows including OAuth and JWT".to_string(),
            categories: vec![Category::Security],
            triggers: vec!["auth setup".to_string(), "add authentication".to_string()],
            tools: vec![],
        });
        registry.register_skill(SkillEntry {
            name: "database-migration".to_string(),
            version: "1.0".to_string(),
            description: "Manage database schema migrations".to_string(),
            categories: vec![],
            triggers: vec!["migrate db".to_string()],
            tools: vec![],
        });

        // Partial match should find the skill
        let results = registry.search_skills("auth");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "authentication-setup");

        // Search by description keyword
        let results = registry.search_skills("OAuth");
        assert_eq!(results.len(), 1, "search should match description text");

        // No match
        let results = registry.search_skills("zzz_nonexistent");
        assert!(
            results.is_empty(),
            "non-matching search should return empty"
        );
    }

    // ── #207: Skill Evolver ─────────────────────────────────────────────

    #[test]
    fn test_skill_evolver_should_evolve() {
        let config = EvolverConfig {
            min_sessions_before_evolve: 3,
            evolution_interval_secs: 3600,
            quality_threshold: 0.5,
        };
        let evolver = SkillEvolver::new(config);
        assert!(!evolver.should_evolve(2));
        assert!(evolver.should_evolve(3));
        assert!(evolver.should_evolve(10));
    }

    #[test]
    fn test_skill_evolver_below_threshold() {
        let config = EvolverConfig {
            min_sessions_before_evolve: 5,
            evolution_interval_secs: 3600,
            quality_threshold: 0.5,
        };
        let mut evolver = SkillEvolver::new(config);
        let summaries = vec![
            SessionSummary {
                session_id: "s1".to_string(),
                patterns: vec!["refactor".to_string()],
                tools_used: vec![],
                success: true,
            },
            SessionSummary {
                session_id: "s2".to_string(),
                patterns: vec!["refactor".to_string()],
                tools_used: vec![],
                success: true,
            },
        ];
        let evolved = evolver.evolve_from_summaries(&summaries);
        assert!(evolved.is_empty(), "should not evolve below threshold");
    }

    #[test]
    fn test_skill_evolver_creates_skills() {
        let config = EvolverConfig {
            min_sessions_before_evolve: 2,
            evolution_interval_secs: 3600,
            quality_threshold: 0.0,
        };
        let mut evolver = SkillEvolver::new(config);
        let summaries = vec![
            SessionSummary {
                session_id: "s1".to_string(),
                patterns: vec!["code review".to_string()],
                tools_used: vec![],
                success: true,
            },
            SessionSummary {
                session_id: "s2".to_string(),
                patterns: vec!["code review".to_string()],
                tools_used: vec![],
                success: true,
            },
            SessionSummary {
                session_id: "s3".to_string(),
                patterns: vec!["refactor".to_string()],
                tools_used: vec![],
                success: false,
            },
        ];
        let evolved = evolver.evolve_from_summaries(&summaries);
        assert_eq!(evolved.len(), 1);
        assert_eq!(evolved[0].name, "code-review");
        assert_eq!(evolved[0].version, 1);
        assert!(evolved[0].source_sessions.contains(&"s1".to_string()));
    }

    #[test]
    fn test_skill_evolver_register_and_list() {
        let config = EvolverConfig {
            min_sessions_before_evolve: 1,
            evolution_interval_secs: 3600,
            quality_threshold: 0.0,
        };
        let mut evolver = SkillEvolver::new(config);
        evolver.register_evolved(EvolvedSkill {
            name: "my-skill".to_string(),
            content: "content".to_string(),
            version: 1,
            source_sessions: vec!["s1".to_string()],
            quality_score: 0.8,
            created_at: 0,
        });
        assert_eq!(evolver.list_evolved().len(), 1);
        assert_eq!(evolver.list_evolved()[0].name, "my-skill");
    }

    // ── #208: Pattern Aggregator ────────────────────────────────────────

    #[test]
    fn test_pattern_aggregator_ingest_and_aggregate() {
        let mut agg = PatternAggregator::new(2);
        agg.ingest_session("s1", &["refactor".to_string(), "test".to_string()]);
        agg.ingest_session("s2", &["refactor".to_string()]);
        agg.ingest_session("s3", &["refactor".to_string()]);
        let results = agg.aggregate();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].pattern, "refactor");
        assert_eq!(results[0].occurrences, 3);
    }

    #[test]
    fn test_pattern_aggregator_top_patterns() {
        let mut agg = PatternAggregator::new(1);
        agg.ingest_session("s1", &["a".to_string(), "b".to_string(), "c".to_string()]);
        agg.ingest_session("s2", &["a".to_string(), "b".to_string()]);
        agg.ingest_session("s3", &["a".to_string()]);
        let top = agg.top_patterns(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].pattern, "a");
    }

    #[test]
    fn test_pattern_aggregator_clear() {
        let mut agg = PatternAggregator::new(1);
        agg.ingest_session("s1", &["pattern".to_string()]);
        assert_eq!(agg.aggregate().len(), 1);
        agg.clear();
        assert_eq!(agg.aggregate().len(), 0);
    }

    #[test]
    fn test_pattern_aggregator_confidence_sorted() {
        let mut agg = PatternAggregator::new(1);
        agg.ingest_session("s1", &["common".to_string(), "rare".to_string()]);
        agg.ingest_session("s2", &["common".to_string()]);
        let results = agg.aggregate();
        assert!(results[0].confidence >= results[1].confidence);
    }

    // ── #209: Skill Auto-Generator ──────────────────────────────────────

    #[test]
    fn test_skill_auto_generator_generate_skill_md() {
        let md = SkillAutoGenerator::generate_skill_md(
            "my-skill",
            "Does something useful",
            &["pattern1".to_string(), "pattern2".to_string()],
            &["shell".to_string(), "read".to_string()],
        );
        assert!(md.contains("# Skill: my-skill"));
        assert!(md.contains("Does something useful"));
        assert!(md.contains("- pattern1"));
        assert!(md.contains("- pattern2"));
        assert!(md.contains("shell, read"));
    }

    #[test]
    fn test_skill_auto_generator_suggest_name() {
        let name = SkillAutoGenerator::suggest_skill_name(&["code review helper".to_string()]);
        assert_eq!(name, "code-review-helper");
        let empty = SkillAutoGenerator::suggest_skill_name(&[]);
        assert_eq!(empty, "unnamed-skill");
    }

    #[test]
    fn test_skill_auto_generator_from_pattern() {
        let entry = PatternEntry {
            pattern: "code review".to_string(),
            occurrences: 5,
            sessions: vec!["s1".to_string()],
            confidence: 0.8,
        };
        let md = SkillAutoGenerator::generate_from_pattern(&entry);
        assert!(md.contains("code-review"));
        assert!(md.contains("Auto-generated skill"));
    }

    // ── #210: Skill Quality Scorer ──────────────────────────────────────

    #[test]
    fn test_skill_quality_scorer_score_range() {
        let scorer = SkillQualityScorer::new();
        let now = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let input = SkillScoreInput {
            total_uses: 100,
            successful_uses: 90,
            user_ratings: vec![0.8, 0.9, 0.7],
            last_used_epoch: now - 86400,
            days_since_creation: 30,
        };
        let score = scorer.score(&input);
        assert!((0.0..=1.0).contains(&score), "score {score} out of range");
    }

    #[test]
    fn test_skill_quality_scorer_promote_and_deprecate() {
        let scorer = SkillQualityScorer::new();
        assert!(scorer.should_promote(0.7));
        assert!(scorer.should_promote(1.0));
        assert!(!scorer.should_promote(0.69));
        assert!(scorer.should_deprecate(0.29));
        assert!(scorer.should_deprecate(0.0));
        assert!(!scorer.should_deprecate(0.3));
    }

    #[test]
    fn test_skill_quality_scorer_zero_uses() {
        let scorer = SkillQualityScorer::new();
        let input = SkillScoreInput {
            total_uses: 0,
            successful_uses: 0,
            user_ratings: vec![],
            last_used_epoch: 0,
            days_since_creation: 0,
        };
        let score = scorer.score(&input);
        assert!((0.0..=1.0).contains(&score));
    }

    #[test]
    fn test_skill_quality_scorer_custom_weights() {
        let weights = QualityWeights {
            success_rate_weight: 1.0,
            usage_frequency_weight: 0.0,
            user_feedback_weight: 0.0,
            recency_weight: 0.0,
        };
        let scorer = SkillQualityScorer::with_weights(weights);
        let now = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let input = SkillScoreInput {
            total_uses: 10,
            successful_uses: 10,
            user_ratings: vec![],
            last_used_epoch: now,
            days_since_creation: 10,
        };
        let score = scorer.score(&input);
        assert!(
            (score - 1.0).abs() < 0.001,
            "100% success rate should give score ~1.0, got {score}"
        );
    }

    // ── #211: Collective Skill Sync ─────────────────────────────────────

    #[test]
    fn test_skill_sync_push_only_local() {
        let mut mgr = SkillSyncManager::new();
        mgr.add_local(SyncableSkill {
            name: "my-skill".to_string(),
            version: 1,
            hash: "abc123".to_string(),
            content: "content".to_string(),
        });
        let diff = mgr.diff();
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0], SyncAction::Push("my-skill".to_string()));
    }

    #[test]
    fn test_skill_sync_pull_only_remote() {
        let mut mgr = SkillSyncManager::new();
        mgr.add_remote(SyncableSkill {
            name: "remote-skill".to_string(),
            version: 1,
            hash: "def456".to_string(),
            content: "content".to_string(),
        });
        let diff = mgr.diff();
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0], SyncAction::Pull("remote-skill".to_string()));
    }

    #[test]
    fn test_skill_sync_up_to_date() {
        let mut mgr = SkillSyncManager::new();
        mgr.add_local(SyncableSkill {
            name: "skill".to_string(),
            version: 1,
            hash: "same".to_string(),
            content: "c".to_string(),
        });
        mgr.add_remote(SyncableSkill {
            name: "skill".to_string(),
            version: 1,
            hash: "same".to_string(),
            content: "c".to_string(),
        });
        let diff = mgr.diff();
        assert_eq!(diff, vec![SyncAction::UpToDate]);
    }

    #[test]
    fn test_skill_sync_conflict() {
        let mut mgr = SkillSyncManager::new();
        mgr.add_local(SyncableSkill {
            name: "skill".to_string(),
            version: 1,
            hash: "hash1".to_string(),
            content: "v1".to_string(),
        });
        mgr.add_remote(SyncableSkill {
            name: "skill".to_string(),
            version: 2,
            hash: "hash2".to_string(),
            content: "v2".to_string(),
        });
        let diff = mgr.diff();
        assert_eq!(diff.len(), 1);
        assert!(matches!(&diff[0], SyncAction::Conflict(name, _) if name == "skill"));
    }

    #[test]
    fn test_skill_sync_resolve_prefer_local() {
        let mut mgr = SkillSyncManager::new();
        mgr.add_local(SyncableSkill {
            name: "skill".to_string(),
            version: 1,
            hash: "h1".to_string(),
            content: "".to_string(),
        });
        mgr.add_remote(SyncableSkill {
            name: "skill".to_string(),
            version: 2,
            hash: "h2".to_string(),
            content: "".to_string(),
        });
        let resolved = mgr.resolve_conflicts("local");
        assert_eq!(resolved, vec![SyncAction::Push("skill".to_string())]);
    }

    #[test]
    fn test_skill_sync_resolve_prefer_remote() {
        let mut mgr = SkillSyncManager::new();
        mgr.add_local(SyncableSkill {
            name: "skill".to_string(),
            version: 1,
            hash: "h1".to_string(),
            content: "".to_string(),
        });
        mgr.add_remote(SyncableSkill {
            name: "skill".to_string(),
            version: 2,
            hash: "h2".to_string(),
            content: "".to_string(),
        });
        let resolved = mgr.resolve_conflicts("remote");
        assert_eq!(resolved, vec![SyncAction::Pull("skill".to_string())]);
    }

    // ── #212: Skill Versioning & Rollback ───────────────────────────────

    #[test]
    fn test_skill_version_record_and_retrieve() {
        let mut mgr = SkillVersionManager::new();
        mgr.record_version("my-skill", "content v1", "initial");
        mgr.record_version("my-skill", "content v2", "updated");
        let v1 = mgr.get_version("my-skill", 1).unwrap();
        assert_eq!(v1.content, "content v1");
        assert_eq!(v1.change_summary, "initial");
        let v2 = mgr.get_version("my-skill", 2).unwrap();
        assert_eq!(v2.content, "content v2");
        assert_eq!(v2.change_summary, "updated");
    }

    #[test]
    fn test_skill_version_latest() {
        let mut mgr = SkillVersionManager::new();
        mgr.record_version("skill", "v1", "first");
        mgr.record_version("skill", "v2", "second");
        mgr.record_version("skill", "v3", "third");
        let latest = mgr.latest_version("skill").unwrap();
        assert_eq!(latest.version, 3);
        assert_eq!(latest.content, "v3");
    }

    #[test]
    fn test_skill_version_rollback() {
        let mut mgr = SkillVersionManager::new();
        mgr.record_version("skill", "version one content", "v1");
        mgr.record_version("skill", "version two content", "v2");
        let content = mgr.rollback("skill", 1).unwrap();
        assert_eq!(content, "version one content");
    }

    #[test]
    fn test_skill_version_history() {
        let mut mgr = SkillVersionManager::new();
        mgr.record_version("skill", "a", "first");
        mgr.record_version("skill", "b", "second");
        let hist = mgr.history("skill").unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].version, 1);
        assert_eq!(hist[1].version, 2);
    }

    #[test]
    fn test_skill_version_diff() {
        let mut mgr = SkillVersionManager::new();
        mgr.record_version("skill", "short", "v1");
        mgr.record_version("skill", "much longer content here", "v2");
        let diff = mgr.diff_versions("skill", 1, 2).unwrap();
        assert!(diff.contains("differs") || diff.contains('→'));
    }

    #[test]
    fn test_skill_version_same_content_diff() {
        let mut mgr = SkillVersionManager::new();
        mgr.record_version("skill", "same", "v1");
        mgr.record_version("skill", "same", "v2");
        let diff = mgr.diff_versions("skill", 1, 2).unwrap();
        assert_eq!(diff, "No differences");
    }

    #[test]
    fn test_skill_version_missing() {
        let mgr = SkillVersionManager::new();
        assert!(mgr.get_version("nonexistent", 1).is_none());
        assert!(mgr.latest_version("nonexistent").is_none());
        assert!(mgr.history("nonexistent").is_none());
        assert!(mgr.rollback("nonexistent", 1).is_none());
    }

    // ── #215: Memory Garbage Collector ──────────────────────────────────

    #[test]
    fn test_garbage_collector_identify_old() {
        let gc = MemoryGarbageCollector::new(30, 1000);
        let items = vec![
            GarbageCandidate {
                id: "a".to_string(),
                last_accessed_days_ago: 10,
                access_count: 5,
                size_bytes: 100,
            },
            GarbageCandidate {
                id: "b".to_string(),
                last_accessed_days_ago: 60,
                access_count: 1,
                size_bytes: 200,
            },
            GarbageCandidate {
                id: "c".to_string(),
                last_accessed_days_ago: 31,
                access_count: 3,
                size_bytes: 150,
            },
        ];
        let garbage = gc.identify_garbage(&items);
        assert!(!garbage.contains(&"a".to_string()));
        assert!(garbage.contains(&"b".to_string()));
        assert!(garbage.contains(&"c".to_string()));
    }

    #[test]
    fn test_garbage_collector_enforce_max_items() {
        let gc = MemoryGarbageCollector::new(365, 2);
        let items = vec![
            GarbageCandidate {
                id: "a".to_string(),
                last_accessed_days_ago: 1,
                access_count: 10,
                size_bytes: 100,
            },
            GarbageCandidate {
                id: "b".to_string(),
                last_accessed_days_ago: 1,
                access_count: 5,
                size_bytes: 100,
            },
            GarbageCandidate {
                id: "c".to_string(),
                last_accessed_days_ago: 1,
                access_count: 1,
                size_bytes: 100,
            },
        ];
        let garbage = gc.identify_garbage(&items);
        assert_eq!(garbage.len(), 1);
        assert!(garbage.contains(&"c".to_string()));
    }

    #[test]
    fn test_garbage_collector_collect() {
        let gc = MemoryGarbageCollector::new(30, 1000);
        let mut items = vec![
            GarbageCandidate {
                id: "keep".to_string(),
                last_accessed_days_ago: 5,
                access_count: 10,
                size_bytes: 100,
            },
            GarbageCandidate {
                id: "remove".to_string(),
                last_accessed_days_ago: 90,
                access_count: 1,
                size_bytes: 200,
            },
        ];
        let removed = gc.collect(&mut items);
        assert_eq!(removed, 1);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "keep");
    }

    #[test]
    fn test_garbage_collector_nothing_to_collect() {
        let gc = MemoryGarbageCollector::new(100, 1000);
        let mut items = vec![GarbageCandidate {
            id: "fresh".to_string(),
            last_accessed_days_ago: 1,
            access_count: 10,
            size_bytes: 50,
        }];
        let removed = gc.collect(&mut items);
        assert_eq!(removed, 0);
        assert_eq!(items.len(), 1);
    }
}
