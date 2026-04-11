use crate::catalog::{BuiltinCatalog, CatalogAgent, CatalogSkill};

/// Context describing the current project, used for recommendations.
#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    /// Primary programming languages detected (e.g. ["rust", "typescript"]).
    pub languages: Vec<String>,
    /// Frameworks and tools detected (e.g. ["react", "tokio", "sqlx"]).
    pub frameworks: Vec<String>,
}

/// A single recommendation with its score.
#[derive(Debug, Clone)]
pub struct SkillRecommendation {
    pub skill: CatalogSkill,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct AgentRecommendation {
    pub agent: CatalogAgent,
    pub score: f64,
}

/// Combined recommendation output.
#[derive(Debug, Clone)]
pub struct Recommendations {
    pub skills: Vec<SkillRecommendation>,
    pub agents: Vec<AgentRecommendation>,
}

/// Score weights.
const W_LANGUAGE: f64 = 0.30;
const W_FRAMEWORK: f64 = 0.25;
const W_PROMPT: f64 = 0.30;
const W_POPULARITY: f64 = 0.15;

/// Popularity scores (0.0–1.0) for well-known skills.
fn skill_popularity(name: &str) -> f64 {
    match name {
        "code-review" | "test-writer" | "security-audit" | "lint-fix" | "readme-writer" => 1.0,
        "refactor" | "error-handling" | "dependency-check" | "ci-pipeline" => 0.9,
        "api-design" | "api-docs" | "docker" | "dockerfile" | "auth-setup" => 0.85,
        "perf-audit" | "benchmark" | "dead-code" | "complexity" => 0.75,
        _ => 0.5,
    }
}

fn agent_popularity(name: &str) -> f64 {
    match name {
        "code-reviewer" | "tester" | "debugger" | "security-analyst" => 1.0,
        "architect" | "backend-dev" | "frontend-dev" | "devops-engineer" => 0.9,
        _ => 0.5,
    }
}

/// Keywords that associate a skill with a language or framework.
fn skill_language_keywords(name: &str) -> &'static [&'static str] {
    match name {
        "type-safety" => &["rust", "typescript", "scala", "haskell"],
        "fuzz-test" => &["rust", "c", "c++"],
        "component-gen" => &["react", "vue", "svelte", "typescript", "javascript"],
        "responsive" => &["react", "vue", "angular", "css", "frontend", "javascript"],
        "e2e-test" => &[
            "javascript",
            "typescript",
            "python",
            "playwright",
            "cypress",
        ],
        "etl-pipeline" => &["python", "scala", "spark"],
        "query-optimize" => &["sql", "python", "java", "postgres", "mysql"],
        "k8s-manifest" => &["yaml", "kubernetes", "docker"],
        "terraform" => &["hcl", "terraform", "aws", "azure", "gcp"],
        "ci-pipeline" => &["yaml", "github", "gitlab"],
        "crud-gen" => &["rust", "python", "go", "java", "typescript"],
        _ => &[],
    }
}

fn skill_framework_keywords(name: &str) -> &'static [&'static str] {
    match name {
        "component-gen" | "snapshot-test" | "e2e-test" => &["react", "vue", "svelte", "next"],
        "crud-gen" | "migration" | "schema-design" | "seed-data" => {
            &["sqlx", "diesel", "prisma", "sqlalchemy", "typeorm"]
        }
        "error-handling" | "type-safety" => &["tokio", "axum", "actix"],
        "api-design" => &["axum", "actix", "express", "fastapi", "django"],
        "etl-pipeline" | "query-optimize" => &["spark", "pandas", "dbt", "airflow"],
        "k8s-manifest" | "dockerfile" => &["docker", "kubernetes", "helm"],
        "terraform" => &["terraform", "pulumi", "cdk"],
        _ => &[],
    }
}

fn agent_language_keywords(name: &str) -> &'static [&'static str] {
    match name {
        "backend-dev" | "debugger" => &["rust", "go", "python", "java", "typescript"],
        "frontend-dev" | "accessibility-expert" => {
            &["javascript", "typescript", "react", "vue", "svelte"]
        }
        "data-engineer" => &["python", "scala", "sql"],
        "devops-engineer" | "release-manager" => &["yaml", "shell", "docker"],
        _ => &[],
    }
}

fn matches_any(haystack: &[String], needles: &[&str]) -> bool {
    if needles.is_empty() {
        return false;
    }
    for h in haystack {
        let h_lower = h.to_lowercase();
        for n in needles {
            if h_lower.contains(&n.to_lowercase()) {
                return true;
            }
        }
    }
    false
}

fn prompt_matches(triggers: &[&str], prompt: &str) -> f64 {
    let p = prompt.to_lowercase();
    let matched = triggers.iter().filter(|t| p.contains(*t)).count();
    if triggers.is_empty() {
        0.0
    } else {
        (matched as f64 / triggers.len() as f64).min(1.0)
    }
}

fn score_skill(skill: &CatalogSkill, ctx: &ProjectContext, prompt: Option<&str>) -> f64 {
    let lang_score = if matches_any(&ctx.languages, skill_language_keywords(skill.name)) {
        1.0
    } else {
        0.0
    };

    let fw_score = if matches_any(&ctx.frameworks, skill_framework_keywords(skill.name)) {
        1.0
    } else {
        0.0
    };

    let prompt_score = match prompt {
        Some(p) => prompt_matches(skill.triggers, p),
        None => 0.0,
    };

    let pop = skill_popularity(skill.name);

    lang_score * W_LANGUAGE + fw_score * W_FRAMEWORK + prompt_score * W_PROMPT + pop * W_POPULARITY
}

fn score_agent(agent: &CatalogAgent, ctx: &ProjectContext, prompt: Option<&str>) -> f64 {
    let lang_score = if matches_any(&ctx.languages, agent_language_keywords(agent.name)) {
        1.0
    } else {
        0.0
    };

    let fw_score = 0.0; // agents are language-agnostic by design

    let prompt_score = match prompt {
        Some(p) => prompt_matches(agent.triggers, p),
        None => 0.0,
    };

    let pop = agent_popularity(agent.name);

    lang_score * W_LANGUAGE + fw_score * W_FRAMEWORK + prompt_score * W_PROMPT + pop * W_POPULARITY
}

/// Recommend skills and agents based on project context and optional natural language prompt.
pub fn recommend(project: &ProjectContext, prompt: Option<&str>, limit: usize) -> Recommendations {
    let mut skills: Vec<SkillRecommendation> = BuiltinCatalog::skills()
        .into_iter()
        .map(|s| {
            let score = score_skill(&s, project, prompt);
            SkillRecommendation { skill: s, score }
        })
        .collect();

    skills.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    skills.truncate(limit);

    let mut agents: Vec<AgentRecommendation> = BuiltinCatalog::agents()
        .into_iter()
        .map(|a| {
            let score = score_agent(&a, project, prompt);
            AgentRecommendation { agent: a, score }
        })
        .collect();

    agents.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    agents.truncate(limit);

    Recommendations { skills, agents }
}
