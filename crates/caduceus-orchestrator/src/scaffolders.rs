//! ST-B1 Wave 0a — scaffolders module, extracted from lib.rs.
//!
//! This module owns the three *Scaffolder types (Agent / Skill /
//! Instructions) plus the `to_title_case` helper. Code was moved verbatim
//! from `lib.rs` lines 8574–9168 without semantic changes.
//!
//! Visibility: items are re-exported at the crate root by `lib.rs`
//! (`pub use scaffolders::*;`) so external callers — and the in-crate test
//! block that still lives in lib.rs — do not need to change.
//!
//! Follow-up extractions (prd_parser, task_recommender, task_hierarchy,
//! time_tracking, progress_inference, execution_tree, config_loader,
//! effort_levels, history, context_assembler, session_manager, emitter,
//! harness) follow the same pattern — see files/b1-split-design.md.

// ── #259: AgentScaffolder ─────────────────────────────────────────────────────

pub struct AgentScaffoldConfig {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub trigger_phrases: Vec<String>,
    pub persona: String,
    pub instructions: Vec<String>,
}

pub struct AgentScaffolder;

impl AgentScaffolder {
    const TOOL_SETS: &'static [(&'static str, &'static [&'static str])] = &[
        ("read-only", &["read", "search"]),
        ("standard", &["shell", "read", "edit", "search"]),
        (
            "full",
            &["shell", "read", "edit", "search", "browser", "mcp"],
        ),
    ];

    pub fn available_tool_sets() -> Vec<(&'static str, &'static [&'static str])> {
        Self::TOOL_SETS.to_vec()
    }

    pub fn suggest_triggers(description: &str) -> Vec<String> {
        let lower = description.to_lowercase();
        let mut triggers = Vec::new();

        let keyword_map: &[(&str, &[&str])] = &[
            ("review", &["review this", "check this", "look at this"]),
            ("create", &["create a", "generate a", "build a", "make a"]),
            ("analyze", &["analyze this", "examine this", "inspect"]),
            (
                "debug",
                &["debug this", "fix this bug", "why is this failing"],
            ),
            (
                "refactor",
                &["refactor this", "clean up this", "improve this"],
            ),
            ("test", &["write tests for", "add tests to", "test this"]),
            ("document", &["document this", "add docs to", "write docs"]),
            ("deploy", &["deploy this", "ship this", "release this"]),
            ("migrate", &["migrate this", "upgrade this", "convert this"]),
            (
                "optimize",
                &["optimize this", "make this faster", "improve performance"],
            ),
        ];

        for (keyword, phrases) in keyword_map {
            if lower.contains(keyword) {
                for phrase in *phrases {
                    triggers.push(phrase.to_string());
                }
            }
        }

        if triggers.is_empty() {
            triggers.push(format!("help me with {}", description.to_lowercase()));
        }

        triggers
    }

    pub fn generate(config: &AgentScaffoldConfig) -> String {
        let tools_str = config
            .tools
            .iter()
            .map(|t| format!("'{t}'"))
            .collect::<Vec<_>>()
            .join(", ");

        let triggers_block = config
            .trigger_phrases
            .iter()
            .map(|t| format!("- '{t}'"))
            .collect::<Vec<_>>()
            .join("\\n");

        let description_yaml = format!(
            "\"When to invoke {}\\n\\nTrigger phrases:\\n{}\\n\\nExamples:\\n- User says '{}' → invoke this agent\"",
            config.description,
            triggers_block,
            config.trigger_phrases.first().cloned().unwrap_or_default()
        );

        let model_line = match &config.model {
            Some(m) => format!("\nmodel: {m}"),
            None => String::new(),
        };

        let title = to_title_case(&config.name);

        let instructions_md = config
            .instructions
            .iter()
            .enumerate()
            .map(|(i, step)| format!("{}. {step}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "---\ndescription: {description_yaml}\nname: {name}\ntools: [{tools_str}]{model_line}\n---\n\n\
# {title}\n\n\
{persona}\n\n\
## When Invoked\n{instructions_md}\n\n\
## Quality Checklist\n\
- [ ] Understood the full context before acting\n\
- [ ] Solution addresses the root cause, not just symptoms\n\
- [ ] Changes are minimal and targeted\n",
            name = config.name,
            persona = config.persona,
        )
    }

    pub fn quick_generate(name: &str, description: &str) -> String {
        let triggers = Self::suggest_triggers(description);
        let config = AgentScaffoldConfig {
            name: name.to_string(),
            description: description.to_string(),
            tools: vec![
                "shell".into(),
                "read".into(),
                "edit".into(),
                "search".into(),
            ],
            model: None,
            trigger_phrases: triggers,
            persona: format!("You are a senior engineer with expertise in {description}."),
            instructions: vec![
                "First, understand the full context of the request.".to_string(),
                "Then, plan the approach before executing.".to_string(),
                "Finally, verify your output meets the requirements.".to_string(),
            ],
        };
        Self::generate(&config)
    }
}

// ── #260: SkillScaffolder ─────────────────────────────────────────────────────

pub struct SkillScaffoldConfig {
    pub name: String,
    pub description: String,
    pub trigger_phrases: Vec<String>,
    pub steps: Vec<String>,
    pub examples: Vec<(String, String)>,
    pub tools_needed: Vec<String>,
}

pub struct SkillScaffolder;

impl SkillScaffolder {
    pub fn generate(config: &SkillScaffoldConfig) -> String {
        let triggers_inline = config.trigger_phrases.join("', '");
        let description_yaml = format!(
            "\"{}. Trigger on: '{triggers_inline}'.\"",
            config.description
        );

        let title = to_title_case(&config.name);

        let triggers_md = config
            .trigger_phrases
            .iter()
            .map(|t| format!("- {t}"))
            .collect::<Vec<_>>()
            .join("\n");

        let steps_md = config
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {s}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        let tools_md = if config.tools_needed.is_empty() {
            String::new()
        } else {
            let list = config
                .tools_needed
                .iter()
                .map(|t| format!("- {t}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!("\n## Tools Required\n{list}\n")
        };

        let examples_md = config
            .examples
            .iter()
            .enumerate()
            .map(|(i, (inp, out))| {
                format!("### Example {}\n**Input:** {inp}\n**Output:** {out}", i + 1)
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let examples_section = if examples_md.is_empty() {
            String::new()
        } else {
            format!("\n## Examples\n{examples_md}\n")
        };

        format!(
            "---\nname: {name}\ndescription: {description_yaml}\n---\n\n\
# {title}\n\n\
## When to Use\n{triggers_md}\n\n\
## Steps\n{steps_md}\n\
{tools_md}\
{examples_section}",
            name = config.name,
        )
    }

    pub fn quick_generate(name: &str, description: &str) -> String {
        let config = SkillScaffoldConfig {
            name: name.to_string(),
            description: description.to_string(),
            trigger_phrases: vec![format!("'{name}'"), format!("help with {name}")],
            steps: vec![
                "Gather context and understand the request.".to_string(),
                "Execute the core task.".to_string(),
                "Verify and summarize the result.".to_string(),
            ],
            examples: Vec::new(),
            tools_needed: Vec::new(),
        };
        Self::generate(&config)
    }

    /// Extract a skill definition from a chat history by distilling key steps.
    pub fn from_conversation(messages: &[String]) -> String {
        let mut steps: Vec<String> = Vec::new();

        for msg in messages {
            let lower = msg.to_lowercase();
            // Heuristic: lines starting with action verbs are likely steps
            for line in msg.lines() {
                let trimmed = line.trim();
                let l = trimmed.to_lowercase();
                if l.starts_with("first")
                    || l.starts_with("then")
                    || l.starts_with("next")
                    || l.starts_with("finally")
                    || l.starts_with("step")
                    || (l.len() > 5
                        && (l.starts_with("run ")
                            || l.starts_with("create ")
                            || l.starts_with("add ")
                            || l.starts_with("update ")
                            || l.starts_with("check ")))
                {
                    steps.push(trimmed.to_string());
                }
                let _ = lower.len(); // suppress unused binding warning
            }
        }

        if steps.is_empty() {
            steps.push("Review the conversation context.".to_string());
            steps.push("Execute the identified task.".to_string());
        }

        let config = SkillScaffoldConfig {
            name: "extracted-skill".to_string(),
            description: "Skill extracted from conversation history.".to_string(),
            trigger_phrases: vec!["extracted skill".to_string()],
            steps,
            examples: Vec::new(),
            tools_needed: Vec::new(),
        };
        Self::generate(&config)
    }
}

// ── #261: InstructionsScaffolder ─────────────────────────────────────────────

pub struct InstructionsConfig {
    pub project_name: String,
    pub project_type: String,
    pub languages: Vec<String>,
    pub build_command: String,
    pub test_command: String,
    pub lint_command: String,
    pub architecture_notes: Vec<String>,
    pub coding_standards: Vec<String>,
    pub important_files: Vec<String>,
    pub custom_rules: Vec<String>,
}

pub struct InstructionsScaffolder;

impl InstructionsScaffolder {
    pub fn generate(config: &InstructionsConfig) -> String {
        let langs = config.languages.join(", ");

        let arch_md = if config.architecture_notes.is_empty() {
            "- No architecture notes provided.".to_string()
        } else {
            config
                .architecture_notes
                .iter()
                .map(|n| format!("- {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let standards_md = if config.coding_standards.is_empty() {
            "- Follow language idioms and best practices.".to_string()
        } else {
            config
                .coding_standards
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let files_md = if config.important_files.is_empty() {
            "- No important files specified.".to_string()
        } else {
            config
                .important_files
                .iter()
                .map(|f| format!("- `{f}`"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let rules_md = if config.custom_rules.is_empty() {
            "- Always run tests before committing.".to_string()
        } else {
            config
                .custom_rules
                .iter()
                .map(|r| format!("- {r}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        format!(
            "# Project Instructions\n\n\
## Project Overview\n\
- Name: {name}\n\
- Type: {project_type}\n\
- Languages: {langs}\n\n\
## Build & Test\n\
- Build: `{build}`\n\
- Test: `{test}`\n\
- Lint: `{lint}`\n\n\
## Architecture\n{arch_md}\n\n\
## Coding Standards\n{standards_md}\n\n\
## Important Files\n{files_md}\n\n\
## Rules\n{rules_md}\n",
            name = config.project_name,
            project_type = config.project_type,
            build = config.build_command,
            test = config.test_command,
            lint = config.lint_command,
        )
    }

    pub fn auto_detect(
        project_root: &str,
        languages: &[String],
        file_count: usize,
    ) -> InstructionsConfig {
        let is_rust = languages.iter().any(|l| l.eq_ignore_ascii_case("rust"));
        let is_python = languages.iter().any(|l| l.eq_ignore_ascii_case("python"));
        let is_ts = languages
            .iter()
            .any(|l| l.eq_ignore_ascii_case("typescript") || l.eq_ignore_ascii_case("ts"));

        let project_type = if is_rust && is_ts {
            "Rust + TypeScript".to_string()
        } else if is_rust {
            "Rust".to_string()
        } else if is_python {
            "Python".to_string()
        } else if is_ts {
            "TypeScript".to_string()
        } else {
            "Unknown".to_string()
        };

        let (build, test, lint) = if is_rust {
            (
                "cargo build".into(),
                "cargo test --workspace".into(),
                "cargo clippy -- -D warnings".into(),
            )
        } else if is_python {
            (
                "pip install -e .".into(),
                "pytest".into(),
                "ruff check .".into(),
            )
        } else if is_ts {
            ("npm run build".into(), "npm test".into(), "eslint .".into())
        } else {
            ("make build".into(), "make test".into(), "make lint".into())
        };

        let arch_notes = vec![
            format!("Project root: {project_root}"),
            format!("Approximate file count: {file_count}"),
        ];

        InstructionsConfig {
            project_name: project_root
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("project")
                .to_string(),
            project_type,
            languages: languages.to_vec(),
            build_command: build,
            test_command: test,
            lint_command: lint,
            architecture_notes: arch_notes,
            coding_standards: Vec::new(),
            important_files: Vec::new(),
            custom_rules: Vec::new(),
        }
    }

    pub fn template_for(project_type: &str) -> String {
        let config = match project_type.to_lowercase().as_str() {
            "rust" => InstructionsConfig {
                project_name: "my-rust-project".into(),
                project_type: "Rust".into(),
                languages: vec!["Rust".into()],
                build_command: "cargo build".into(),
                test_command: "cargo test --workspace".into(),
                lint_command: "cargo clippy -- -D warnings && cargo fmt --check".into(),
                architecture_notes: vec![
                    "Organized as a Cargo workspace.".into(),
                    "Each crate has a single responsibility.".into(),
                ],
                coding_standards: vec![
                    "Use rustfmt for formatting.".into(),
                    "No clippy warnings allowed (enforced in CI).".into(),
                    "Prefer owned types in public APIs.".into(),
                ],
                important_files: vec![
                    "Cargo.toml — Workspace manifest".into(),
                    "src/main.rs — Entry point".into(),
                ],
                custom_rules: vec![
                    "Always run `cargo fmt --all` before committing.".into(),
                    "All public APIs must have doc comments.".into(),
                ],
            },
            "python" => InstructionsConfig {
                project_name: "my-python-project".into(),
                project_type: "Python".into(),
                languages: vec!["Python".into()],
                build_command: "pip install -e '.[dev]'".into(),
                test_command: "pytest".into(),
                lint_command: "ruff check . && mypy .".into(),
                architecture_notes: vec![
                    "Uses a src-layout for packaging.".into(),
                    "Type annotations required on all public functions.".into(),
                ],
                coding_standards: vec![
                    "Ruff enforces PEP 8 and import ordering.".into(),
                    "mypy runs in strict mode.".into(),
                    "Use virtual environments (venv).".into(),
                ],
                important_files: vec![
                    "pyproject.toml — Project config and dependencies".into(),
                    "src/ — Main package source".into(),
                ],
                custom_rules: vec![
                    "Never commit secrets — use environment variables.".into(),
                    "All tests go in the tests/ directory.".into(),
                ],
            },
            "typescript" => InstructionsConfig {
                project_name: "my-ts-project".into(),
                project_type: "TypeScript".into(),
                languages: vec!["TypeScript".into()],
                build_command: "npm run build".into(),
                test_command: "npx vitest".into(),
                lint_command: "eslint . && prettier --check .".into(),
                architecture_notes: vec![
                    "Strict TypeScript mode enabled.".into(),
                    "ES modules throughout.".into(),
                ],
                coding_standards: vec![
                    "No `any` types.".into(),
                    "Prettier enforces formatting.".into(),
                    "ESLint enforces style rules.".into(),
                ],
                important_files: vec![
                    "tsconfig.json — TypeScript configuration".into(),
                    "package.json — Dependencies".into(),
                ],
                custom_rules: vec![
                    "Run `npm run lint` before committing.".into(),
                    "Prefer named exports over default exports.".into(),
                ],
            },
            "react" => InstructionsConfig {
                project_name: "my-react-app".into(),
                project_type: "React + TypeScript".into(),
                languages: vec!["TypeScript".into(), "CSS".into()],
                build_command: "npm run build".into(),
                test_command: "npx vitest".into(),
                lint_command: "eslint . && prettier --check .".into(),
                architecture_notes: vec![
                    "Component-based architecture.".into(),
                    "State managed via React hooks.".into(),
                    "No class components — functional only.".into(),
                ],
                coding_standards: vec![
                    "Each component in its own file.".into(),
                    "Use React Testing Library for UI tests.".into(),
                    "CSS Modules for scoped styles.".into(),
                ],
                important_files: vec![
                    "src/App.tsx — Root component".into(),
                    "src/components/ — Reusable components".into(),
                ],
                custom_rules: vec![
                    "Never mutate state directly.".into(),
                    "Always handle loading and error states in UI.".into(),
                ],
            },
            "fullstack" => InstructionsConfig {
                project_name: "my-fullstack-app".into(),
                project_type: "Fullstack (Backend + Frontend)".into(),
                languages: vec!["TypeScript".into(), "Rust".into()],
                build_command: "cargo build && npm run build".into(),
                test_command: "cargo test --workspace && npx vitest".into(),
                lint_command: "cargo clippy -- -D warnings && eslint .".into(),
                architecture_notes: vec![
                    "Backend: Rust API server.".into(),
                    "Frontend: TypeScript/React SPA.".into(),
                    "API contract defined with OpenAPI spec.".into(),
                ],
                coding_standards: vec![
                    "Backend follows Rust workspace conventions.".into(),
                    "Frontend follows React + TypeScript conventions.".into(),
                    "All API endpoints must have integration tests.".into(),
                ],
                important_files: vec![
                    "backend/src/main.rs — API entry point".into(),
                    "frontend/src/App.tsx — Frontend root".into(),
                    "api/openapi.yaml — API contract".into(),
                ],
                custom_rules: vec![
                    "Always validate API schema changes don't break the frontend.".into(),
                    "Run both backend and frontend tests in CI.".into(),
                ],
            },
            _ => InstructionsConfig {
                project_name: "my-project".into(),
                project_type: project_type.to_string(),
                languages: vec![project_type.to_string()],
                build_command: "make build".into(),
                test_command: "make test".into(),
                lint_command: "make lint".into(),
                architecture_notes: vec!["Add architecture notes here.".into()],
                coding_standards: vec!["Follow language idioms and best practices.".into()],
                important_files: vec!["README.md — Project documentation".into()],
                custom_rules: vec!["Always run tests before committing.".into()],
            },
        };
        Self::generate(&config)
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

pub(crate) fn to_title_case(s: &str) -> String {
    s.split(['-', '_', ' '])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
