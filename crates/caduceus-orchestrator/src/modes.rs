use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// ── Agent execution modes ──────────────────────────────────────────────────────

/// Controls how the agent behaves during a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentMode {
    /// Read-only analysis, strategy discussion, NO code changes.
    Plan,
    /// Execute code changes with approval.
    Act,
    /// Read-only exploration, summarize findings.
    Research,
    /// Fully autonomous — plan + act + test + commit.
    Autopilot,
    /// High-level design — architecture, dependencies, modules.
    Architect,
    /// Investigate errors, trace bugs, propose fixes.
    Debug,
    /// Code review — read code, find issues, suggest improvements.
    Review,
}

impl AgentMode {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "plan" => Some(Self::Plan),
            "act" => Some(Self::Act),
            "research" => Some(Self::Research),
            "autopilot" | "auto" => Some(Self::Autopilot),
            "architect" | "arch" => Some(Self::Architect),
            "debug" | "dbg" => Some(Self::Debug),
            "review" => Some(Self::Review),
            _ => None,
        }
    }

    pub fn config(&self) -> ModeConfig {
        match self {
            Self::Plan => ModeConfig {
                system_prompt_prefix: "You are in PLAN mode. Analyze only — do NOT modify any files, do NOT execute any write operations. \
                    Produce a numbered action plan. For any tool call, respond with what you WOULD do instead of executing it. \
                    Output a structured markdown plan with numbered steps."
                    .into(),
                tool_access: ToolAccess::ReadOnly,
                approval_required: false,
                output_style: OutputStyle::MarkdownPlan,
                intercept_writes: true,
            },
            Self::Act => ModeConfig {
                system_prompt_prefix: "You are in ACT mode. Execute code changes as requested. \
                    Each write operation requires user approval before proceeding."
                    .into(),
                tool_access: ToolAccess::All,
                approval_required: true,
                output_style: OutputStyle::Standard,
                intercept_writes: false,
            },
            Self::Research => ModeConfig {
                system_prompt_prefix: "You are in RESEARCH mode. Read-only exploration. \
                    Search the codebase, read files, and summarize your findings. \
                    Do NOT modify any files."
                    .into(),
                tool_access: ToolAccess::ReadOnly,
                approval_required: false,
                output_style: OutputStyle::Standard,
                intercept_writes: false,
            },
            Self::Autopilot => ModeConfig {
                system_prompt_prefix: "You are in AUTOPILOT mode. Fully autonomous execution. \
                    Plan, implement, test, and commit changes without waiting for approval. \
                    Be thorough and verify your changes work before committing."
                    .into(),
                tool_access: ToolAccess::All,
                approval_required: false,
                output_style: OutputStyle::Standard,
                intercept_writes: false,
            },
            Self::Architect => ModeConfig {
                system_prompt_prefix: "You are in ARCHITECT mode. Focus on high-level design. \
                    Discuss architecture, dependencies, module boundaries, and system design. \
                    You may read files for context but do NOT make code changes."
                    .into(),
                tool_access: ToolAccess::ReadOnly,
                approval_required: false,
                output_style: OutputStyle::Standard,
                intercept_writes: false,
            },
            Self::Debug => ModeConfig {
                system_prompt_prefix: "You are in DEBUG mode. Investigate errors and trace bugs. \
                    Read files, check logs, run diagnostic commands, and propose fixes. \
                    Output a step-by-step trace of your investigation."
                    .into(),
                tool_access: ToolAccess::All,
                approval_required: true,
                output_style: OutputStyle::StepByStepTrace,
                intercept_writes: false,
            },
            Self::Review => ModeConfig {
                system_prompt_prefix: "You are in REVIEW mode. Perform a code review. \
                    Read code, identify issues, suggest improvements. \
                    Do NOT modify any files. Output a structured findings list."
                    .into(),
                tool_access: ToolAccess::ReadOnly,
                approval_required: false,
                output_style: OutputStyle::FindingsList,
                intercept_writes: false,
            },
        }
    }

    pub fn all_modes() -> &'static [AgentMode] {
        &[
            Self::Plan,
            Self::Act,
            Self::Research,
            Self::Autopilot,
            Self::Architect,
            Self::Debug,
            Self::Review,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Act => "act",
            Self::Research => "research",
            Self::Autopilot => "autopilot",
            Self::Architect => "architect",
            Self::Debug => "debug",
            Self::Review => "review",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Plan => "Read-only analysis, strategy discussion, NO code changes",
            Self::Act => "Execute code changes with approval",
            Self::Research => "Read-only exploration, summarize findings",
            Self::Autopilot => "Fully autonomous — plan + act + test + commit",
            Self::Architect => "High-level design — architecture, dependencies, modules",
            Self::Debug => "Investigate errors, trace bugs, propose fixes",
            Self::Review => "Code review — read code, find issues, suggest improvements",
        }
    }
}

impl std::fmt::Display for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

// ── Agent personas ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentPersona {
    pub name: String,
    pub description: String,
    pub system_prompt_prefix: String,
    pub default_mode: String,
    pub preferred_tools: Vec<String>,
    pub temperature: f64,
    pub max_tokens: u32,
}

pub struct PersonaRegistry {
    personas: HashMap<String, AgentPersona>,
}

impl PersonaRegistry {
    pub fn new() -> Self {
        Self {
            personas: HashMap::new(),
        }
    }

    pub fn register(&mut self, persona: AgentPersona) {
        self.personas.insert(persona.name.clone(), persona);
    }

    pub fn get(&self, name: &str) -> Option<&AgentPersona> {
        self.personas.get(name)
    }

    pub fn list(&self) -> Vec<&AgentPersona> {
        let mut personas: Vec<&AgentPersona> = self.personas.values().collect();
        personas.sort_by(|left, right| left.name.cmp(&right.name));
        personas
    }

    pub fn builtin_personas() -> Self {
        let mut registry = Self::new();
        for persona in [
            AgentPersona {
                name: "builder".to_string(),
                description: "Implementation-focused persona for making and validating code changes."
                    .to_string(),
                system_prompt_prefix:
                    "You are a builder persona. Prioritize executable changes, validation, and direct progress."
                        .to_string(),
                default_mode: AgentMode::Act.name().to_string(),
                preferred_tools: vec!["read_file".into(), "edit_file".into(), "bash".into()],
                temperature: 0.2,
                max_tokens: 4_096,
            },
            AgentPersona {
                name: "planner".to_string(),
                description: "Strategic persona for breaking work into safe, ordered steps."
                    .to_string(),
                system_prompt_prefix:
                    "You are a planner persona. Focus on sequencing, risks, and crisp action plans."
                        .to_string(),
                default_mode: AgentMode::Plan.name().to_string(),
                preferred_tools: vec!["read_file".into(), "glob_search".into(), "grep_search".into()],
                temperature: 0.1,
                max_tokens: 3_072,
            },
            AgentPersona {
                name: "explorer".to_string(),
                description: "Investigative persona for understanding unfamiliar codebases and flows."
                    .to_string(),
                system_prompt_prefix:
                    "You are an explorer persona. Search broadly, connect findings, and summarize what matters."
                        .to_string(),
                default_mode: AgentMode::Research.name().to_string(),
                preferred_tools: vec!["glob_search".into(), "grep_search".into(), "read_file".into()],
                temperature: 0.4,
                max_tokens: 4_096,
            },
            AgentPersona {
                name: "reviewer".to_string(),
                description: "Critical persona for identifying defects, regressions, and missing validation."
                    .to_string(),
                system_prompt_prefix:
                    "You are a reviewer persona. Look for correctness issues, edge cases, and quality gaps."
                        .to_string(),
                default_mode: AgentMode::Review.name().to_string(),
                preferred_tools: vec!["read_file".into(), "git_diff".into(), "grep_search".into()],
                temperature: 0.1,
                max_tokens: 3_072,
            },
            AgentPersona {
                name: "researcher".to_string(),
                description: "Evidence-driven persona for gathering context, sources, and alternatives."
                    .to_string(),
                system_prompt_prefix:
                    "You are a researcher persona. Collect evidence, compare options, and surface trade-offs."
                        .to_string(),
                default_mode: AgentMode::Research.name().to_string(),
                preferred_tools: vec!["web_fetch".into(), "read_file".into(), "grep_search".into()],
                temperature: 0.3,
                max_tokens: 6_144,
            },
        ] {
            registry.register(persona);
        }
        registry
    }
}

impl Default for PersonaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tool access policy ─────────────────────────────────────────────────────────

/// Controls which categories of tools are available in a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolAccess {
    /// Only read operations: read_file, glob_search, grep_search, list_files, git_status, git_diff.
    ReadOnly,
    /// All tools including write, bash, edit, patch.
    All,
}

impl ToolAccess {
    /// Returns the set of tool names allowed under this access level.
    pub fn allowed_tools(&self) -> HashSet<&'static str> {
        match self {
            Self::ReadOnly => {
                let mut s = HashSet::new();
                s.insert("read_file");
                s.insert("glob_search");
                s.insert("grep_search");
                s.insert("list_files");
                s.insert("git_status");
                s.insert("git_diff");
                s.insert("web_fetch");
                s
            }
            Self::All => HashSet::new(), // empty = no restriction
        }
    }

    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        match self {
            Self::All => true,
            Self::ReadOnly => self.allowed_tools().contains(tool_name),
        }
    }
}

// ── Output style ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputStyle {
    Standard,
    MarkdownPlan,
    StepByStepTrace,
    FindingsList,
}

// ── Mode configuration ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ModeConfig {
    pub system_prompt_prefix: String,
    pub tool_access: ToolAccess,
    pub approval_required: bool,
    pub output_style: OutputStyle,
    /// When true, write tool calls return simulated results instead of executing.
    pub intercept_writes: bool,
}

// ── Plan/Act separation ────────────────────────────────────────────────────────

/// A single action planned during Plan mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannedAction {
    pub step: usize,
    pub tool_name: String,
    pub args: serde_json::Value,
    pub description: String,
    /// Per-step revision counter (gap G4 / P3.1). Bumps on every
    /// successful in-place amendment of THIS step. Used by external
    /// amend-IPC to detect stale edits: a UI form that opened against
    /// `revision = 2` MUST attach `expected_revision = 2` to its
    /// amendment; if the planner has since advanced and bumped it to
    /// `3`, the amendment is rejected as stale.
    #[serde(default)]
    pub revision: u64,
}

impl PlannedAction {
    pub fn new(step: usize, tool_name: &str, args: &serde_json::Value) -> Self {
        let description = format!("{}({})", tool_name, args);
        Self {
            step,
            tool_name: tool_name.to_string(),
            args: args.clone(),
            description,
            revision: 0,
        }
    }
}

/// In-flight amendment to an [`ActionPlan`] (gap G4 / P3.1).
///
/// All variants carry an `expected_revision` against which the plan
/// validates before applying; this is the per-step revision for
/// `Replace` / `Remove` and the plan-level revision for `Insert`.
/// Stale amendments fail loudly via [`AmendError::StaleRevision`] so
/// the UI can re-fetch and re-display.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanAmendment {
    /// Replace the args / description of an existing step in place.
    /// Step ordering is preserved.
    Replace {
        step: usize,
        args: serde_json::Value,
        description: String,
        expected_revision: u64,
    },
    /// Insert a new action AFTER `after_step` (use 0 to prepend).
    /// Subsequent steps are renumbered; their per-step revisions are
    /// preserved (the *contents* didn't change).
    Insert {
        after_step: usize,
        tool_name: String,
        args: serde_json::Value,
        description: String,
        expected_plan_revision: u64,
    },
    /// Remove the step at index `step`.
    Remove {
        step: usize,
        expected_revision: u64,
    },
}

/// Reasons an amendment can fail. Returned by
/// [`ActionPlan::apply_amendment`] without panicking.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AmendError {
    #[error("step {step} does not exist (plan has {len} actions)")]
    StepOutOfRange { step: usize, len: usize },
    #[error(
        "stale revision: amendment expected {expected}, current is {actual}"
    )]
    StaleRevision { expected: u64, actual: u64 },
    #[error("plan is empty; cannot amend")]
    EmptyPlan,
}

/// Result of a successful [`PlanAmendment`] (gap G4 / P3.1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedAmendment {
    /// Which step was affected (post-amendment index for Insert; the
    /// removed step's old index for Remove; same index for Replace).
    pub step: usize,
    /// New per-step revision (for Insert/Remove this is 0; for Replace
    /// this is the bumped value).
    pub new_revision: u64,
    /// New plan-level revision after this amendment.
    pub new_plan_revision: u64,
    /// Short human-readable summary, e.g. `"replaced step 2"`.
    pub summary: String,
}

/// An ordered list of actions produced during Plan mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionPlan {
    pub actions: Vec<PlannedAction>,
    /// Plan-level revision counter (gap G4 / P3.1). Bumps on EVERY
    /// structural change (add / replace / insert / remove). Lets the UI
    /// cheap-poll for "has anything changed?" without diffing actions.
    #[serde(default)]
    pub revision: u64,
}

impl ActionPlan {
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            revision: 0,
        }
    }

    pub fn add(&mut self, tool_name: &str, args: &serde_json::Value) -> String {
        let step = self.actions.len() + 1;
        let action = PlannedAction::new(step, tool_name, args);
        let msg = format!(
            "Step {}: Would execute `{}`({})",
            step,
            tool_name,
            serde_json::to_string(args).unwrap_or_else(|_| "{}".into())
        );
        self.actions.push(action);
        self.revision += 1;
        msg
    }

    pub fn summary(&self) -> String {
        if self.actions.is_empty() {
            return "No actions planned.".to_string();
        }
        let mut out = String::from("## Action Plan\n\n");
        for action in &self.actions {
            out.push_str(&format!(
                "{}. `{}`({})\n",
                action.step, action.tool_name, action.description
            ));
        }
        out
    }

    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Apply an external mid-flight amendment (gap G4 / P3.1).
    ///
    /// Validates revisions, mutates the plan, and bumps both the
    /// affected step's revision (for `Replace`) and the plan-level
    /// revision. NEVER panics on bad input — returns
    /// [`AmendError`] with enough detail for the UI to recover.
    pub fn apply_amendment(
        &mut self,
        amendment: PlanAmendment,
    ) -> Result<AppliedAmendment, AmendError> {
        match amendment {
            PlanAmendment::Replace {
                step,
                args,
                description,
                expected_revision,
            } => {
                let idx = self.checked_index(step)?;
                let action = &mut self.actions[idx];
                if action.revision != expected_revision {
                    return Err(AmendError::StaleRevision {
                        expected: expected_revision,
                        actual: action.revision,
                    });
                }
                action.args = args;
                action.description = description;
                action.revision += 1;
                let new_revision = action.revision;
                self.revision += 1;
                Ok(AppliedAmendment {
                    step,
                    new_revision,
                    new_plan_revision: self.revision,
                    summary: format!("replaced step {}", step),
                })
            }
            PlanAmendment::Insert {
                after_step,
                tool_name,
                args,
                description,
                expected_plan_revision,
            } => {
                if self.revision != expected_plan_revision {
                    return Err(AmendError::StaleRevision {
                        expected: expected_plan_revision,
                        actual: self.revision,
                    });
                }
                // `after_step = 0` → insert at the very front. Otherwise
                // validate the step exists, then insert after it.
                let insert_at = if after_step == 0 {
                    0
                } else {
                    self.checked_index(after_step)? + 1
                };
                let mut new_action = PlannedAction::new(
                    insert_at + 1, // step number; will be re-numbered below
                    &tool_name,
                    &args,
                );
                new_action.description = description;
                self.actions.insert(insert_at, new_action);
                // Renumber steps so they stay 1-indexed and contiguous.
                for (i, a) in self.actions.iter_mut().enumerate() {
                    a.step = i + 1;
                }
                self.revision += 1;
                Ok(AppliedAmendment {
                    step: insert_at + 1,
                    new_revision: 0,
                    new_plan_revision: self.revision,
                    summary: format!("inserted step at {}", insert_at + 1),
                })
            }
            PlanAmendment::Remove {
                step,
                expected_revision,
            } => {
                let idx = self.checked_index(step)?;
                if self.actions[idx].revision != expected_revision {
                    return Err(AmendError::StaleRevision {
                        expected: expected_revision,
                        actual: self.actions[idx].revision,
                    });
                }
                self.actions.remove(idx);
                for (i, a) in self.actions.iter_mut().enumerate() {
                    a.step = i + 1;
                }
                self.revision += 1;
                Ok(AppliedAmendment {
                    step,
                    new_revision: 0,
                    new_plan_revision: self.revision,
                    summary: format!("removed step {}", step),
                })
            }
        }
    }

    /// Translate a 1-indexed step to a 0-indexed `Vec` position with
    /// range-checking. Folds the `EmptyPlan` short-circuit so callers
    /// get the more specific error rather than a generic out-of-range.
    fn checked_index(&self, step: usize) -> Result<usize, AmendError> {
        if self.actions.is_empty() {
            return Err(AmendError::EmptyPlan);
        }
        if step == 0 || step > self.actions.len() {
            return Err(AmendError::StepOutOfRange {
                step,
                len: self.actions.len(),
            });
        }
        Ok(step - 1)
    }
}

// ── Mode manager ───────────────────────────────────────────────────────────────

/// Tracks the current agent mode and accumulated action plan.
pub struct ModeManager {
    current: AgentMode,
    plan: ActionPlan,
}

impl ModeManager {
    pub fn new(mode: AgentMode) -> Self {
        Self {
            current: mode,
            plan: ActionPlan::new(),
        }
    }

    pub fn current(&self) -> AgentMode {
        self.current
    }

    /// Switch to a new mode. Returns the old mode name for logging.
    pub fn switch(&mut self, new_mode: AgentMode) -> &'static str {
        let old_name = self.current.name();
        // Clear plan when leaving Plan mode
        if self.current == AgentMode::Plan && new_mode != AgentMode::Plan {
            // Plan is preserved for Act to consume
        }
        self.current = new_mode;
        old_name
    }

    pub fn config(&self) -> ModeConfig {
        self.current.config()
    }

    pub fn plan(&self) -> &ActionPlan {
        &self.plan
    }

    pub fn plan_mut(&mut self) -> &mut ActionPlan {
        &mut self.plan
    }

    /// Simulate a tool call in plan mode: records it and returns a description.
    pub fn record_planned_action(&mut self, tool_name: &str, args: &serde_json::Value) -> String {
        self.plan.add(tool_name, args)
    }

    /// Take the plan for execution in Act mode, resetting it.
    pub fn take_plan(&mut self) -> ActionPlan {
        std::mem::take(&mut self.plan)
    }

    /// Apply an external mid-flight plan amendment (gap G4 / P3.1).
    /// Convenience delegate so callers don't have to navigate through
    /// `plan_mut()`. Returns the same `Result` shape so the IPC layer
    /// can stay shallow.
    pub fn apply_amendment(
        &mut self,
        amendment: PlanAmendment,
    ) -> Result<AppliedAmendment, AmendError> {
        self.plan.apply_amendment(amendment)
    }
}

impl Default for ModeManager {
    fn default() -> Self {
        Self::new(AgentMode::Act)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_from_str_all_variants() {
        assert_eq!(AgentMode::from_str_loose("plan"), Some(AgentMode::Plan));
        assert_eq!(AgentMode::from_str_loose("act"), Some(AgentMode::Act));
        assert_eq!(
            AgentMode::from_str_loose("research"),
            Some(AgentMode::Research)
        );
        assert_eq!(
            AgentMode::from_str_loose("autopilot"),
            Some(AgentMode::Autopilot)
        );
        assert_eq!(
            AgentMode::from_str_loose("auto"),
            Some(AgentMode::Autopilot)
        );
        assert_eq!(
            AgentMode::from_str_loose("architect"),
            Some(AgentMode::Architect)
        );
        assert_eq!(
            AgentMode::from_str_loose("arch"),
            Some(AgentMode::Architect)
        );
        assert_eq!(AgentMode::from_str_loose("debug"), Some(AgentMode::Debug));
        assert_eq!(AgentMode::from_str_loose("dbg"), Some(AgentMode::Debug));
        assert_eq!(AgentMode::from_str_loose("review"), Some(AgentMode::Review));
        assert_eq!(AgentMode::from_str_loose("PLAN"), Some(AgentMode::Plan));
        assert_eq!(AgentMode::from_str_loose("unknown"), None);
    }

    #[test]
    fn plan_mode_is_read_only() {
        let config = AgentMode::Plan.config();
        assert_eq!(config.tool_access, ToolAccess::ReadOnly);
        assert!(config.intercept_writes);
        assert!(!config.approval_required);
    }

    #[test]
    fn act_mode_allows_all_tools_with_approval() {
        let config = AgentMode::Act.config();
        assert_eq!(config.tool_access, ToolAccess::All);
        assert!(config.approval_required);
        assert!(!config.intercept_writes);
    }

    #[test]
    fn autopilot_mode_no_approval() {
        let config = AgentMode::Autopilot.config();
        assert_eq!(config.tool_access, ToolAccess::All);
        assert!(!config.approval_required);
    }

    #[test]
    fn research_mode_read_only() {
        let config = AgentMode::Research.config();
        assert_eq!(config.tool_access, ToolAccess::ReadOnly);
    }

    #[test]
    fn review_mode_read_only_findings() {
        let config = AgentMode::Review.config();
        assert_eq!(config.tool_access, ToolAccess::ReadOnly);
        assert_eq!(config.output_style, OutputStyle::FindingsList);
    }

    #[test]
    fn tool_access_read_only_blocks_writes() {
        let access = ToolAccess::ReadOnly;
        assert!(access.is_tool_allowed("read_file"));
        assert!(access.is_tool_allowed("glob_search"));
        assert!(access.is_tool_allowed("grep_search"));
        assert!(access.is_tool_allowed("git_status"));
        assert!(!access.is_tool_allowed("write_file"));
        assert!(!access.is_tool_allowed("bash"));
        assert!(!access.is_tool_allowed("edit_file"));
    }

    #[test]
    fn tool_access_all_allows_everything() {
        let access = ToolAccess::All;
        assert!(access.is_tool_allowed("write_file"));
        assert!(access.is_tool_allowed("bash"));
        assert!(access.is_tool_allowed("read_file"));
    }

    #[test]
    fn mode_manager_switch() {
        let mut manager = ModeManager::new(AgentMode::Plan);
        assert_eq!(manager.current(), AgentMode::Plan);
        let old = manager.switch(AgentMode::Act);
        assert_eq!(old, "plan");
        assert_eq!(manager.current(), AgentMode::Act);
    }

    #[test]
    fn plan_mode_records_actions() {
        let mut manager = ModeManager::new(AgentMode::Plan);
        let msg = manager.record_planned_action(
            "write_file",
            &serde_json::json!({"path": "test.rs", "content": "fn main() {}"}),
        );
        assert!(msg.contains("Would execute"));
        assert!(msg.contains("write_file"));
        assert_eq!(manager.plan().len(), 1);
    }

    #[test]
    fn action_plan_summary() {
        let mut plan = ActionPlan::new();
        plan.add("read_file", &serde_json::json!({"path": "src/lib.rs"}));
        plan.add(
            "write_file",
            &serde_json::json!({"path": "out.txt", "content": "data"}),
        );
        let summary = plan.summary();
        assert!(summary.contains("Action Plan"));
        assert!(summary.contains("read_file"));
        assert!(summary.contains("write_file"));
        assert_eq!(plan.len(), 2);
    }

    #[test]
    fn take_plan_resets() {
        let mut manager = ModeManager::new(AgentMode::Plan);
        manager.record_planned_action("bash", &serde_json::json!({"command": "ls"}));
        let plan = manager.take_plan();
        assert_eq!(plan.len(), 1);
        assert!(manager.plan().is_empty());
    }

    #[test]
    fn all_modes_have_configs() {
        for mode in AgentMode::all_modes() {
            let config = mode.config();
            assert!(!config.system_prompt_prefix.is_empty());
        }
    }

    #[test]
    fn mode_display() {
        assert_eq!(format!("{}", AgentMode::Plan), "plan");
        assert_eq!(format!("{}", AgentMode::Autopilot), "autopilot");
    }

    #[test]
    fn builtin_personas_are_registered() {
        let registry = PersonaRegistry::builtin_personas();

        assert_eq!(registry.list().len(), 5);
        assert_eq!(
            registry
                .get("builder")
                .map(|persona| persona.default_mode.as_str()),
            Some("act")
        );
        assert_eq!(
            registry
                .get("explorer")
                .map(|persona| persona.default_mode.as_str()),
            Some("research")
        );
    }

    #[test]
    fn persona_lookup_returns_registered_persona() {
        let mut registry = PersonaRegistry::new();
        registry.register(AgentPersona {
            name: "custom".to_string(),
            description: "Custom persona".to_string(),
            system_prompt_prefix: "Custom prompt".to_string(),
            default_mode: "plan".to_string(),
            preferred_tools: vec!["read_file".to_string()],
            temperature: 0.5,
            max_tokens: 1_024,
        });

        let persona = registry.get("custom").unwrap();
        assert_eq!(persona.description, "Custom persona");
        assert_eq!(persona.preferred_tools, vec!["read_file"]);
    }

    #[test]
    fn persona_listing_is_sorted_by_name() {
        let mut registry = PersonaRegistry::new();
        for name in ["reviewer", "builder", "planner"] {
            registry.register(AgentPersona {
                name: name.to_string(),
                description: name.to_string(),
                system_prompt_prefix: name.to_string(),
                default_mode: "plan".to_string(),
                preferred_tools: Vec::new(),
                temperature: 0.0,
                max_tokens: 256,
            });
        }

        let names: Vec<&str> = registry
            .list()
            .iter()
            .map(|persona| persona.name.as_str())
            .collect();
        assert_eq!(names, vec!["builder", "planner", "reviewer"]);
    }

    // ── Orchestrator mode tests ──────────────────────────────────────────

    #[test]
    fn test_mode_plan_denies_writes() {
        let config = AgentMode::Plan.config();
        assert_eq!(config.tool_access, ToolAccess::ReadOnly);
        assert!(!config.tool_access.is_tool_allowed("write_file"));
        assert!(!config.tool_access.is_tool_allowed("edit_file"));
        assert!(!config.tool_access.is_tool_allowed("bash"));
        assert!(config.intercept_writes, "Plan mode should intercept writes");
    }

    #[test]
    fn test_mode_research_readonly() {
        let config = AgentMode::Research.config();
        assert_eq!(config.tool_access, ToolAccess::ReadOnly);
        // Research should allow read tools
        assert!(config.tool_access.is_tool_allowed("read_file"));
        assert!(config.tool_access.is_tool_allowed("glob_search"));
        assert!(config.tool_access.is_tool_allowed("grep_search"));
        assert!(config.tool_access.is_tool_allowed("list_files"));
        // But not write tools
        assert!(!config.tool_access.is_tool_allowed("write_file"));
        assert!(!config.tool_access.is_tool_allowed("bash"));
        assert!(!config.tool_access.is_tool_allowed("edit_file"));
    }

    #[test]
    fn test_mode_autopilot_skips_approval() {
        let config = AgentMode::Autopilot.config();
        assert!(
            !config.approval_required,
            "Autopilot should not require approval"
        );
        assert_eq!(config.tool_access, ToolAccess::All);
        assert!(
            !config.intercept_writes,
            "Autopilot should not intercept writes"
        );
        // All tools should be allowed
        assert!(config.tool_access.is_tool_allowed("write_file"));
        assert!(config.tool_access.is_tool_allowed("bash"));
        assert!(config.tool_access.is_tool_allowed("read_file"));
    }

    // ── G4 / P3.1 — Mid-flight plan amendment tests ───────────────────────

    fn seed_plan() -> ActionPlan {
        let mut p = ActionPlan::new();
        p.add("bash", &serde_json::json!({"command": "ls"}));
        p.add("read_file", &serde_json::json!({"path": "src/lib.rs"}));
        p.add("write_file", &serde_json::json!({"path": "out.txt"}));
        p
    }

    #[test]
    fn amend_replace_bumps_step_revision_and_plan_revision() {
        let mut p = seed_plan();
        let plan_rev_before = p.revision;
        let step_rev_before = p.actions[0].revision;
        let res = p.apply_amendment(PlanAmendment::Replace {
            step: 1,
            args: serde_json::json!({"command": "ls -la"}),
            description: "list with details".into(),
            expected_revision: step_rev_before,
        });
        let applied = res.expect("amendment should apply");
        assert_eq!(applied.step, 1);
        assert_eq!(applied.new_revision, step_rev_before + 1);
        assert_eq!(applied.new_plan_revision, plan_rev_before + 1);
        assert_eq!(p.actions[0].args["command"], "ls -la");
        assert_eq!(p.actions[0].description, "list with details");
        assert_eq!(p.actions[0].revision, step_rev_before + 1);
    }

    #[test]
    fn amend_replace_rejects_stale_revision() {
        let mut p = seed_plan();
        let res = p.apply_amendment(PlanAmendment::Replace {
            step: 1,
            args: serde_json::json!({}),
            description: "stale edit".into(),
            // Plan starts every step at revision 0; passing 99 is stale.
            expected_revision: 99,
        });
        assert!(matches!(res, Err(AmendError::StaleRevision { .. })));
        // Nothing should have changed.
        assert_eq!(p.actions[0].args["command"], "ls");
    }

    #[test]
    fn amend_replace_out_of_range_returns_step_error() {
        let mut p = seed_plan();
        let res = p.apply_amendment(PlanAmendment::Replace {
            step: 99,
            args: serde_json::json!({}),
            description: "nope".into(),
            expected_revision: 0,
        });
        assert!(matches!(
            res,
            Err(AmendError::StepOutOfRange { step: 99, .. })
        ));
    }

    #[test]
    fn amend_insert_renumbers_subsequent_steps() {
        let mut p = seed_plan();
        let plan_rev = p.revision;
        let res = p.apply_amendment(PlanAmendment::Insert {
            after_step: 1,
            tool_name: "grep".into(),
            args: serde_json::json!({"pattern": "TODO"}),
            description: "grep TODOs".into(),
            expected_plan_revision: plan_rev,
        });
        let applied = res.expect("insert should apply");
        assert_eq!(applied.step, 2);
        assert_eq!(p.actions.len(), 4);
        assert_eq!(p.actions[1].tool_name, "grep");
        // Subsequent steps must be renumbered to 1..=N.
        for (i, a) in p.actions.iter().enumerate() {
            assert_eq!(a.step, i + 1);
        }
    }

    #[test]
    fn amend_insert_at_zero_prepends() {
        let mut p = seed_plan();
        let res = p.apply_amendment(PlanAmendment::Insert {
            after_step: 0,
            tool_name: "echo".into(),
            args: serde_json::json!({"msg": "start"}),
            description: "preamble".into(),
            expected_plan_revision: p.revision,
        });
        let applied = res.expect("prepend should apply");
        assert_eq!(applied.step, 1);
        assert_eq!(p.actions[0].tool_name, "echo");
    }

    #[test]
    fn amend_insert_rejects_stale_plan_revision() {
        let mut p = seed_plan();
        let res = p.apply_amendment(PlanAmendment::Insert {
            after_step: 1,
            tool_name: "x".into(),
            args: serde_json::json!({}),
            description: "stale".into(),
            expected_plan_revision: 0, // plan is at revision 3 after seed
        });
        assert!(matches!(res, Err(AmendError::StaleRevision { .. })));
    }

    #[test]
    fn amend_remove_compacts_and_renumbers() {
        let mut p = seed_plan();
        let res = p.apply_amendment(PlanAmendment::Remove {
            step: 2,
            expected_revision: 0,
        });
        let applied = res.expect("remove should apply");
        assert_eq!(applied.step, 2);
        assert_eq!(p.actions.len(), 2);
        assert_eq!(p.actions[0].tool_name, "bash");
        assert_eq!(p.actions[1].tool_name, "write_file");
        assert_eq!(p.actions[1].step, 2, "must be renumbered to 2");
    }

    #[test]
    fn amend_on_empty_plan_returns_empty_error() {
        let mut p = ActionPlan::new();
        let res = p.apply_amendment(PlanAmendment::Remove {
            step: 1,
            expected_revision: 0,
        });
        assert!(matches!(res, Err(AmendError::EmptyPlan)));
    }

    #[test]
    fn amend_serde_roundtrip_for_all_variants() {
        for v in [
            PlanAmendment::Replace {
                step: 1,
                args: serde_json::json!({"k": "v"}),
                description: "d".into(),
                expected_revision: 0,
            },
            PlanAmendment::Insert {
                after_step: 0,
                tool_name: "t".into(),
                args: serde_json::json!({}),
                description: "d".into(),
                expected_plan_revision: 0,
            },
            PlanAmendment::Remove {
                step: 1,
                expected_revision: 0,
            },
        ] {
            let json = serde_json::to_string(&v).unwrap();
            let back: PlanAmendment = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn agent_event_plan_pending_serde_roundtrip() {
        let ev = caduceus_core::AgentEvent::PlanStepPending {
            step: 2,
            revision: 0,
            plan_revision: 5,
            tool_name: "bash".into(),
            description: "ls -la".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"PlanStepPending\""));
        let back: caduceus_core::AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            caduceus_core::AgentEvent::PlanStepPending {
                step, revision, plan_revision, tool_name, description,
            } => {
                assert_eq!(step, 2);
                assert_eq!(revision, 0);
                assert_eq!(plan_revision, 5);
                assert_eq!(tool_name, "bash");
                assert_eq!(description, "ls -la");
            }
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    #[test]
    fn agent_event_plan_amended_serde_roundtrip() {
        let ev = caduceus_core::AgentEvent::PlanAmended {
            kind: "replace".into(),
            step: 2,
            ok: false,
            reason: "stale revision".into(),
            plan_revision: 7,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: caduceus_core::AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            caduceus_core::AgentEvent::PlanAmended {
                kind, step, ok, reason, plan_revision,
            } => {
                assert_eq!(kind, "replace");
                assert_eq!(step, 2);
                assert!(!ok);
                assert_eq!(reason, "stale revision");
                assert_eq!(plan_revision, 7);
            }
            other => panic!("unexpected variant: {:?}", other),
        }
    }
}
