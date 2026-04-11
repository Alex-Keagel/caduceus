use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAction {
    pub step: usize,
    pub tool_name: String,
    pub args: serde_json::Value,
    pub description: String,
}

impl PlannedAction {
    pub fn new(step: usize, tool_name: &str, args: &serde_json::Value) -> Self {
        let description = format!("{}({})", tool_name, args);
        Self {
            step,
            tool_name: tool_name.to_string(),
            args: args.clone(),
            description,
        }
    }
}

/// An ordered list of actions produced during Plan mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionPlan {
    pub actions: Vec<PlannedAction>,
}

impl ActionPlan {
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
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
}
