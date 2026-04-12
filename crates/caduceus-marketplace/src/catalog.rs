use crate::manifest::Category;

#[derive(Debug, Clone)]
pub struct CatalogSkill {
    pub name: &'static str,
    pub description: &'static str,
    pub categories: &'static [Category],
    pub triggers: &'static [&'static str],
}

#[derive(Debug, Clone)]
pub struct CatalogAgent {
    pub name: &'static str,
    pub description: &'static str,
    pub categories: &'static [Category],
    pub triggers: &'static [&'static str],
}

pub struct BuiltinCatalog;

impl BuiltinCatalog {
    pub fn skills() -> Vec<CatalogSkill> {
        vec![
            CatalogSkill {
                name: "accessibility",
                description: "Audit and improve accessibility compliance (WCAG, ARIA)",
                categories: &[Category::Testing, Category::Frontend],
                triggers: &["accessibility audit", "a11y check", "wcag compliance"],
            },
            CatalogSkill {
                name: "api-design",
                description: "Design RESTful and GraphQL APIs with best practices",
                categories: &[Category::Backend, Category::Documentation],
                triggers: &["design api", "api schema", "rest api", "graphql api"],
            },
            CatalogSkill {
                name: "api-docs",
                description: "Generate OpenAPI/Swagger documentation for APIs",
                categories: &[Category::Documentation],
                triggers: &[
                    "document api",
                    "openapi",
                    "swagger docs",
                    "api documentation",
                ],
            },
            CatalogSkill {
                name: "architecture-doc",
                description: "Create architecture decision records and system diagrams",
                categories: &[Category::Documentation],
                triggers: &["architecture doc", "adr", "system design doc"],
            },
            CatalogSkill {
                name: "auth-review",
                description: "Review authentication and authorization implementations",
                categories: &[Category::Security, Category::CodeReview],
                triggers: &["review auth", "auth security", "authorization review"],
            },
            CatalogSkill {
                name: "auth-setup",
                description: "Set up authentication flows (JWT, OAuth, sessions)",
                categories: &[Category::Security, Category::Backend],
                triggers: &["setup auth", "implement login", "jwt auth", "oauth setup"],
            },
            CatalogSkill {
                name: "benchmark",
                description: "Create and run performance benchmarks",
                categories: &[Category::Testing],
                triggers: &["benchmark this", "measure performance", "perf benchmark"],
            },
            CatalogSkill {
                name: "caching",
                description: "Implement caching strategies (Redis, in-memory, HTTP)",
                categories: &[Category::Backend],
                triggers: &["add caching", "cache this", "redis cache", "cache layer"],
            },
            CatalogSkill {
                name: "changelog",
                description: "Generate and maintain changelogs from git history",
                categories: &[Category::Documentation, Category::Git],
                triggers: &["generate changelog", "update changelog", "release notes"],
            },
            CatalogSkill {
                name: "ci-pipeline",
                description: "Set up CI/CD pipelines (GitHub Actions, GitLab CI)",
                categories: &[Category::DevOps],
                triggers: &["setup ci", "github actions", "ci pipeline", "gitlab ci"],
            },
            CatalogSkill {
                name: "code-comments",
                description: "Add and improve inline code documentation",
                categories: &[Category::Documentation, Category::CodeReview],
                triggers: &["add comments", "document code", "improve comments"],
            },
            CatalogSkill {
                name: "code-review",
                description: "Comprehensive code review for correctness, style, and security",
                categories: &[Category::CodeReview],
                triggers: &["review code", "code review", "review this pr"],
            },
            CatalogSkill {
                name: "complexity",
                description: "Analyze and reduce cyclomatic complexity",
                categories: &[Category::CodeReview],
                triggers: &["complexity analysis", "reduce complexity", "simplify this"],
            },
            CatalogSkill {
                name: "component-gen",
                description: "Generate UI components (React, Vue, Svelte)",
                categories: &[Category::Frontend],
                triggers: &["generate component", "create component", "new component"],
            },
            CatalogSkill {
                name: "crud-gen",
                description: "Generate CRUD endpoints and data access layers",
                categories: &[Category::Backend],
                triggers: &["generate crud", "create crud", "crud endpoints"],
            },
            CatalogSkill {
                name: "csrf-protection",
                description: "Implement CSRF protection mechanisms",
                categories: &[Category::Security],
                triggers: &["csrf protection", "add csrf", "cross-site request forgery"],
            },
            CatalogSkill {
                name: "data-validation",
                description: "Add input validation and data sanitization",
                categories: &[Category::Backend, Category::Security],
                triggers: &["validate data", "input validation", "sanitize input"],
            },
            CatalogSkill {
                name: "dead-code",
                description: "Find and remove dead, unreachable, or unused code",
                categories: &[Category::CodeReview],
                triggers: &["find dead code", "remove unused", "dead code detection"],
            },
            CatalogSkill {
                name: "dependency-check",
                description: "Audit dependencies for known vulnerabilities",
                categories: &[Category::Security],
                triggers: &[
                    "check dependencies",
                    "vulnerable packages",
                    "dependency audit",
                ],
            },
            CatalogSkill {
                name: "dockerfile",
                description: "Create and optimize Dockerfiles",
                categories: &[Category::DevOps],
                triggers: &["create dockerfile", "containerize", "docker image"],
            },
            CatalogSkill {
                name: "duplication",
                description: "Detect and refactor duplicated code blocks",
                categories: &[Category::CodeReview],
                triggers: &["find duplication", "remove duplication", "dry principle"],
            },
            CatalogSkill {
                name: "e2e-test",
                description: "Write end-to-end tests (Playwright, Cypress, Selenium)",
                categories: &[Category::Testing],
                triggers: &["e2e tests", "end-to-end tests", "playwright", "cypress"],
            },
            CatalogSkill {
                name: "error-handling",
                description: "Improve error handling, propagation, and user messages",
                categories: &[Category::Backend],
                triggers: &[
                    "improve error handling",
                    "error propagation",
                    "better errors",
                ],
            },
            CatalogSkill {
                name: "etl-pipeline",
                description: "Build ETL data pipelines and transformations",
                categories: &[Category::Database],
                triggers: &["build etl", "data pipeline", "etl pipeline"],
            },
            CatalogSkill {
                name: "fuzz-test",
                description: "Write fuzz tests to find unexpected crashes",
                categories: &[Category::Testing, Category::Security],
                triggers: &["fuzz test", "fuzzing", "add fuzz"],
            },
            CatalogSkill {
                name: "imports",
                description: "Clean up and organize import statements",
                categories: &[Category::CodeReview],
                triggers: &["clean imports", "organize imports", "unused imports"],
            },
            CatalogSkill {
                name: "input-validation",
                description: "Validate and sanitize all user-provided inputs",
                categories: &[Category::Security],
                triggers: &["validate inputs", "sanitize user input", "input security"],
            },
            CatalogSkill {
                name: "k8s-manifest",
                description: "Generate Kubernetes manifests and Helm charts",
                categories: &[Category::DevOps, Category::Deployment],
                triggers: &["kubernetes manifest", "k8s deploy", "helm chart"],
            },
            CatalogSkill {
                name: "lint-fix",
                description: "Fix linting errors and enforce code style",
                categories: &[Category::CodeReview],
                triggers: &["fix lint", "lint errors", "clippy fix", "eslint fix"],
            },
            CatalogSkill {
                name: "migration",
                description: "Write and manage database migrations",
                categories: &[Category::Database],
                triggers: &["database migration", "schema migration", "db migrate"],
            },
            CatalogSkill {
                name: "mock-generator",
                description: "Generate mock objects and test doubles",
                categories: &[Category::Testing],
                triggers: &["generate mocks", "create mocks", "test mocks"],
            },
            CatalogSkill {
                name: "naming",
                description: "Improve naming conventions across the codebase",
                categories: &[Category::CodeReview],
                triggers: &["improve naming", "rename variables", "naming conventions"],
            },
            CatalogSkill {
                name: "perf-audit",
                description: "Audit and profile application performance",
                categories: &[Category::Backend],
                triggers: &["perf audit", "performance audit", "profile app"],
            },
            CatalogSkill {
                name: "query-optimize",
                description: "Optimize database queries and indexes",
                categories: &[Category::Database],
                triggers: &["optimize query", "slow query", "query performance"],
            },
            CatalogSkill {
                name: "readme-writer",
                description: "Write and improve project README documentation",
                categories: &[Category::Documentation],
                triggers: &["write readme", "improve readme", "update readme"],
            },
            CatalogSkill {
                name: "refactor",
                description: "Refactor code for better structure and maintainability",
                categories: &[Category::CodeReview],
                triggers: &["refactor this", "clean up code", "restructure"],
            },
            CatalogSkill {
                name: "regression-test",
                description: "Write regression tests to prevent bug recurrence",
                categories: &[Category::Testing],
                triggers: &["regression test", "add regression", "prevent regression"],
            },
            CatalogSkill {
                name: "release",
                description: "Automate release workflows and version bumping",
                categories: &[Category::DevOps, Category::Git],
                triggers: &["create release", "release workflow", "bump version"],
            },
            CatalogSkill {
                name: "responsive",
                description: "Make UI layouts responsive for all screen sizes",
                categories: &[Category::Frontend],
                triggers: &["make responsive", "mobile layout", "responsive design"],
            },
            CatalogSkill {
                name: "schema-design",
                description: "Design normalized database schemas",
                categories: &[Category::Database],
                triggers: &["design schema", "database schema", "table design"],
            },
            CatalogSkill {
                name: "secret-scan",
                description: "Scan codebase for exposed secrets and credentials",
                categories: &[Category::Security],
                triggers: &["scan secrets", "find secrets", "credential leak"],
            },
            CatalogSkill {
                name: "security-audit",
                description: "Comprehensive security audit of the codebase",
                categories: &[Category::Security],
                triggers: &["security audit", "security review", "penetration test"],
            },
            CatalogSkill {
                name: "seed-data",
                description: "Create database seed data and fixtures",
                categories: &[Category::Database],
                triggers: &["seed database", "test data", "database fixtures"],
            },
            CatalogSkill {
                name: "snapshot-test",
                description: "Add snapshot tests for UI components",
                categories: &[Category::Testing, Category::Frontend],
                triggers: &["snapshot test", "snapshot testing", "ui snapshot"],
            },
            CatalogSkill {
                name: "style-guide",
                description: "Enforce and document coding style guidelines",
                categories: &[Category::CodeReview, Category::Documentation],
                triggers: &["style guide", "coding standards", "code style"],
            },
            CatalogSkill {
                name: "terraform",
                description: "Write and manage Terraform infrastructure as code",
                categories: &[Category::DevOps, Category::Deployment],
                triggers: &["terraform", "infrastructure as code", "iac"],
            },
            CatalogSkill {
                name: "test-coverage",
                description: "Increase test coverage and identify untested paths",
                categories: &[Category::Testing],
                triggers: &["test coverage", "increase coverage", "untested code"],
            },
            CatalogSkill {
                name: "test-writer",
                description: "Write unit and integration tests",
                categories: &[Category::Testing],
                triggers: &[
                    "write tests",
                    "add tests",
                    "unit tests",
                    "integration tests",
                ],
            },
            CatalogSkill {
                name: "type-safety",
                description: "Improve type safety and eliminate runtime type errors",
                categories: &[Category::Backend],
                triggers: &["type safety", "stronger types", "type errors"],
            },
            CatalogSkill {
                name: "usage-examples",
                description: "Write usage examples and code samples for documentation",
                categories: &[Category::Documentation],
                triggers: &["usage examples", "code examples", "how to use"],
            },
            CatalogSkill {
                name: "prd-to-tasks",
                description: "Parse a Product Requirements Document into structured tasks with dependencies",
                categories: &[Category::Productivity, Category::Documentation],
                triggers: &["parse prd", "prd to tasks", "requirements to tasks", "break down prd"],
            },
            CatalogSkill {
                name: "git-conventional-commits",
                description: "Enforce and auto-generate conventional commit messages",
                categories: &[Category::Git],
                triggers: &["conventional commits", "commit message", "fix commit", "conventional commit format"],
            },
            CatalogSkill {
                name: "changelog-generator",
                description: "Auto-generate CHANGELOG.md from git history and PR descriptions",
                categories: &[Category::Documentation, Category::Git],
                triggers: &["generate changelog", "changelog from git", "changelog from prs", "auto changelog"],
            },
            CatalogSkill {
                name: "openapi-generator",
                description: "Generate OpenAPI/Swagger specs from code annotations or route definitions",
                categories: &[Category::Documentation, Category::Backend],
                triggers: &["generate openapi", "openapi spec", "swagger spec", "api spec from code"],
            },
            CatalogSkill {
                name: "graphql-schema",
                description: "Design and validate GraphQL schemas with resolver stubs",
                categories: &[Category::Backend, Category::Database],
                triggers: &["graphql schema", "design graphql", "graphql resolver", "schema first graphql"],
            },
            CatalogSkill {
                name: "database-migration",
                description: "Generate and validate database migrations (SQL, Prisma, TypeORM, Diesel)",
                categories: &[Category::Database],
                triggers: &["generate migration", "database migration", "prisma migration", "diesel migration"],
            },
            CatalogSkill {
                name: "env-config",
                description: "Manage .env files, validate required vars, generate .env.example",
                categories: &[Category::DevOps, Category::Backend],
                triggers: &["env config", "env file", "environment variables", "dotenv setup"],
            },
            CatalogSkill {
                name: "license-checker",
                description: "Audit dependencies for license compatibility (MIT, Apache, GPL)",
                categories: &[Category::Security, Category::Productivity],
                triggers: &["license check", "license audit", "dependency licenses", "license compatibility"],
            },
            CatalogSkill {
                name: "i18n-extractor",
                description: "Extract translatable strings, generate locale files, find missing translations",
                categories: &[Category::Frontend, Category::Productivity],
                triggers: &["extract translations", "i18n setup", "internationalization", "missing translations"],
            },
            CatalogSkill {
                name: "monorepo-setup",
                description: "Configure monorepo tooling (Turborepo, Nx, Cargo workspaces, pnpm workspaces)",
                categories: &[Category::DevOps, Category::Productivity],
                triggers: &["monorepo setup", "turborepo", "nx workspace", "cargo workspace"],
            },
            CatalogSkill {
                name: "github-actions",
                description: "Create and debug GitHub Actions workflows",
                categories: &[Category::DevOps, Category::Git],
                triggers: &["github actions", "create workflow", "actions workflow", "debug workflow"],
            },
            CatalogSkill {
                name: "docker-compose",
                description: "Generate and validate docker-compose configurations",
                categories: &[Category::DevOps, Category::Deployment],
                triggers: &["docker compose", "compose file", "docker-compose setup", "multi container"],
            },
            CatalogSkill {
                name: "load-test",
                description: "Generate load test scripts (k6, Artillery, wrk) with realistic scenarios",
                categories: &[Category::Testing, Category::Backend],
                triggers: &["load test", "k6 script", "artillery test", "stress test"],
            },
            CatalogSkill {
                name: "api-mock",
                description: "Generate mock API servers from OpenAPI specs or route definitions",
                categories: &[Category::Testing, Category::Backend],
                triggers: &["api mock", "mock server", "mock api", "stub api"],
            },
            CatalogSkill {
                name: "data-seeder",
                description: "Generate realistic test data for databases and APIs",
                categories: &[Category::Testing, Category::Database],
                triggers: &["seed data", "generate test data", "fake data", "data seeder"],
            },
            CatalogSkill {
                name: "error-boundary",
                description: "Add comprehensive error handling patterns (Result types, try-catch, error boundaries)",
                categories: &[Category::Frontend, Category::Backend],
                triggers: &["error boundary", "error handling pattern", "result type", "add error boundary"],
            },
            CatalogSkill {
                name: "logging-setup",
                description: "Configure structured logging (tracing, winston, pino) with appropriate levels",
                categories: &[Category::Backend, Category::DevOps],
                triggers: &["setup logging", "structured logging", "logging config", "tracing setup"],
            },
            CatalogSkill {
                name: "feature-flag",
                description: "Implement feature flag patterns with gradual rollout support",
                categories: &[Category::Backend, Category::Deployment],
                triggers: &["feature flag", "feature toggle", "gradual rollout", "canary flag"],
            },
            CatalogSkill {
                name: "rate-limiter",
                description: "Add rate limiting middleware with configurable strategies",
                categories: &[Category::Backend, Category::Security],
                triggers: &["rate limit", "add rate limiting", "throttle requests", "rate limiter"],
            },
            CatalogSkill {
                name: "cache-strategy",
                description: "Implement caching patterns (Redis, in-memory, CDN) with invalidation",
                categories: &[Category::Backend],
                triggers: &["cache strategy", "caching pattern", "cache invalidation", "cdn cache"],
            },
            CatalogSkill {
                name: "webhook-handler",
                description: "Create webhook endpoint handlers with signature verification",
                categories: &[Category::Backend, Category::Security],
                triggers: &["webhook handler", "webhook endpoint", "webhook signature", "handle webhook"],
            },
            CatalogSkill {
                name: "oauth-flow",
                description: "Implement OAuth 2.0 / OIDC authentication flows",
                categories: &[Category::Security, Category::Backend],
                triggers: &["oauth flow", "oidc setup", "oauth2", "implement oauth"],
            },
            CatalogSkill {
                name: "websocket-setup",
                description: "Configure WebSocket server/client with reconnection and heartbeat",
                categories: &[Category::Backend, Category::Frontend],
                triggers: &["websocket setup", "websockets", "ws server", "realtime websocket"],
            },
            CatalogSkill {
                name: "queue-worker",
                description: "Set up message queue consumers (RabbitMQ, SQS, Redis streams)",
                categories: &[Category::Backend, Category::DevOps],
                triggers: &["queue worker", "message queue", "rabbitmq consumer", "sqs worker"],
            },
            CatalogSkill {
                name: "health-check",
                description: "Add health check and readiness probe endpoints",
                categories: &[Category::Backend, Category::DevOps],
                triggers: &["health check", "readiness probe", "liveness probe", "health endpoint"],
            },
            CatalogSkill {
                name: "cors-config",
                description: "Configure CORS policies with proper security headers",
                categories: &[Category::Security, Category::Backend],
                triggers: &["cors config", "cors policy", "cross origin", "cors headers"],
            },
            CatalogSkill {
                name: "pagination",
                description: "Implement cursor-based or offset pagination for APIs",
                categories: &[Category::Backend, Category::Database],
                triggers: &["add pagination", "cursor pagination", "offset pagination", "paginate results"],
            },
            CatalogSkill {
                name: "file-upload",
                description: "Handle multipart file uploads with validation, streaming, and storage",
                categories: &[Category::Backend],
                triggers: &["file upload", "multipart upload", "upload handler", "file storage"],
            },
            CatalogSkill {
                name: "search-index",
                description: "Set up full-text search (Elasticsearch, Meilisearch, tantivy)",
                categories: &[Category::Backend, Category::Database],
                triggers: &["search index", "full text search", "elasticsearch setup", "meilisearch"],
            },
            CatalogSkill {
                name: "backup-restore",
                description: "Implement database backup and restore procedures",
                categories: &[Category::Database, Category::DevOps],
                triggers: &["backup database", "restore database", "backup restore", "db backup"],
            },
        ]
    }

    pub fn agents() -> Vec<CatalogAgent> {
        vec![
            CatalogAgent {
                name: "accessibility-expert",
                description: "Expert in WCAG accessibility standards and ARIA patterns",
                categories: &[Category::Frontend, Category::Testing],
                triggers: &["accessibility expert", "a11y expert", "wcag specialist"],
            },
            CatalogAgent {
                name: "api-designer",
                description: "Designs clean, consistent RESTful and GraphQL APIs",
                categories: &[Category::Backend, Category::Documentation],
                triggers: &["api designer", "design this api", "api architect"],
            },
            CatalogAgent {
                name: "architect",
                description: "System architect for high-level design and ADRs",
                categories: &[Category::Backend, Category::DevOps],
                triggers: &[
                    "system architect",
                    "design architecture",
                    "architecture review",
                ],
            },
            CatalogAgent {
                name: "backend-dev",
                description: "Backend developer for server-side logic and APIs",
                categories: &[Category::Backend],
                triggers: &[
                    "backend dev",
                    "server side",
                    "implement api",
                    "backend feature",
                ],
            },
            CatalogAgent {
                name: "code-reviewer",
                description: "Reviews code for correctness, security, and style",
                categories: &[Category::CodeReview, Category::Security],
                triggers: &["review this code", "check for bugs", "security review"],
            },
            CatalogAgent {
                name: "data-engineer",
                description: "Builds data pipelines, ETL workflows, and analytics",
                categories: &[Category::Database],
                triggers: &["data engineer", "data pipeline", "etl workflow"],
            },
            CatalogAgent {
                name: "debugger",
                description: "Diagnoses and fixes bugs with systematic root cause analysis",
                categories: &[Category::Backend, Category::Testing],
                triggers: &["debug this", "find the bug", "root cause analysis"],
            },
            CatalogAgent {
                name: "dependency-manager",
                description: "Manages project dependencies, upgrades, and vulnerability fixes",
                categories: &[Category::Security, Category::DevOps],
                triggers: &["manage dependencies", "upgrade deps", "dependency update"],
            },
            CatalogAgent {
                name: "devops-engineer",
                description: "Sets up CI/CD, Docker, Kubernetes, and cloud infrastructure",
                categories: &[Category::DevOps, Category::Deployment],
                triggers: &["devops setup", "ci cd setup", "kubernetes deploy"],
            },
            CatalogAgent {
                name: "documenter",
                description: "Writes comprehensive technical documentation",
                categories: &[Category::Documentation],
                triggers: &["write docs", "document this", "technical documentation"],
            },
            CatalogAgent {
                name: "frontend-dev",
                description: "Frontend developer for UI components and user experience",
                categories: &[Category::Frontend],
                triggers: &["frontend dev", "build ui", "react component", "ui feature"],
            },
            CatalogAgent {
                name: "incident-responder",
                description: "Handles production incidents with structured runbooks",
                categories: &[Category::DevOps, Category::Backend],
                triggers: &["incident response", "production issue", "outage response"],
            },
            CatalogAgent {
                name: "migrator",
                description: "Manages database and code migrations safely",
                categories: &[Category::Database],
                triggers: &["migration plan", "migrate database", "code migration"],
            },
            CatalogAgent {
                name: "onboarding-guide",
                description: "Creates onboarding documentation for new team members",
                categories: &[Category::Documentation, Category::Productivity],
                triggers: &["onboarding guide", "new developer setup", "team onboarding"],
            },
            CatalogAgent {
                name: "optimizer",
                description: "Optimizes performance across frontend, backend, and database",
                categories: &[Category::Backend, Category::Database],
                triggers: &["optimize this", "performance optimizer", "speed up"],
            },
            CatalogAgent {
                name: "performance-engineer",
                description: "Profiles and tunes application performance",
                categories: &[Category::Backend, Category::Testing],
                triggers: &["performance engineer", "perf tuning", "latency reduction"],
            },
            CatalogAgent {
                name: "refactorer",
                description: "Refactors legacy code with zero behavior change",
                categories: &[Category::CodeReview],
                triggers: &["refactor agent", "legacy refactor", "clean this up"],
            },
            CatalogAgent {
                name: "release-manager",
                description: "Manages release cycles, versioning, and deployment",
                categories: &[Category::DevOps, Category::Git, Category::Deployment],
                triggers: &["release manager", "manage release", "deploy release"],
            },
            CatalogAgent {
                name: "reviewer",
                description: "General-purpose code and content reviewer",
                categories: &[Category::CodeReview],
                triggers: &["review this", "reviewer agent", "give feedback"],
            },
            CatalogAgent {
                name: "security-analyst",
                description: "Security expert for threat modeling and vulnerability analysis",
                categories: &[Category::Security],
                triggers: &["security analyst", "threat model", "vulnerability analysis"],
            },
            CatalogAgent {
                name: "test-writer",
                description: "Writes unit, integration, and e2e tests",
                categories: &[Category::Testing],
                triggers: &["test writer agent", "write test suite", "generate tests"],
            },
            CatalogAgent {
                name: "tester",
                description: "Runs test suites and identifies test gaps",
                categories: &[Category::Testing],
                triggers: &["run tests", "test this", "find test gaps"],
            },
            CatalogAgent {
                name: "prd-analyst",
                description: "Analyzes PRDs and requirements docs, breaks them into implementable tasks",
                categories: &[Category::Productivity, Category::Documentation],
                triggers: &["analyze prd", "prd analyst", "break down requirements", "requirements analysis"],
            },
            CatalogAgent {
                name: "api-architect",
                description: "Designs REST/GraphQL APIs with proper resource modeling and versioning",
                categories: &[Category::Backend, Category::Documentation],
                triggers: &["api architect", "design rest api", "resource modeling", "api versioning"],
            },
            CatalogAgent {
                name: "database-architect",
                description: "Designs schemas, indexes, and query optimization strategies",
                categories: &[Category::Database],
                triggers: &["database architect", "design schema", "index strategy", "query optimization"],
            },
            CatalogAgent {
                name: "devops-pipelines",
                description: "Creates and troubleshoots CI/CD pipelines across platforms",
                categories: &[Category::DevOps, Category::Deployment],
                triggers: &["devops pipelines", "ci cd pipeline", "pipeline setup", "pipeline debug"],
            },
            CatalogAgent {
                name: "sre-responder",
                description: "Handles incidents: alert triage, runbook execution, postmortem writing",
                categories: &[Category::DevOps, Category::Backend],
                triggers: &["sre responder", "alert triage", "run runbook", "write postmortem"],
            },
            CatalogAgent {
                name: "compliance-auditor",
                description: "Checks code against regulatory requirements (GDPR, SOC2, HIPAA)",
                categories: &[Category::Security],
                triggers: &["compliance audit", "gdpr check", "soc2 review", "hipaa compliance"],
            },
            CatalogAgent {
                name: "mobile-dev",
                description: "Specializes in React Native, Flutter, Swift, Kotlin mobile development",
                categories: &[Category::Frontend],
                triggers: &["mobile dev", "react native", "flutter app", "ios development", "android development"],
            },
            CatalogAgent {
                name: "ml-engineer",
                description: "Machine learning pipeline design, model training, experiment tracking",
                categories: &[Category::AI],
                triggers: &["ml engineer", "machine learning pipeline", "model training", "experiment tracking"],
            },
            CatalogAgent {
                name: "tech-writer",
                description: "Generates comprehensive technical documentation, API docs, guides",
                categories: &[Category::Documentation],
                triggers: &["tech writer", "technical writer", "write api docs", "documentation guide"],
            },
            CatalogAgent {
                name: "cost-optimizer",
                description: "Analyzes cloud costs, recommends right-sizing and reserved instances",
                categories: &[Category::DevOps, Category::Deployment],
                triggers: &["cost optimizer", "cloud costs", "right sizing", "reserved instances"],
            },
            CatalogAgent {
                name: "chaos-engineer",
                description: "Designs chaos experiments, game days, and resilience testing",
                categories: &[Category::Testing, Category::DevOps],
                triggers: &["chaos engineer", "chaos experiment", "game day", "resilience testing"],
            },
        ]
    }

    pub fn skills_by_category(category: &Category) -> Vec<CatalogSkill> {
        Self::skills()
            .into_iter()
            .filter(|s| s.categories.contains(category))
            .collect()
    }

    pub fn agents_by_category(category: &Category) -> Vec<CatalogAgent> {
        Self::agents()
            .into_iter()
            .filter(|a| a.categories.contains(category))
            .collect()
    }
}
