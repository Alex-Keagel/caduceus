//! Multi-agent worker system — coordinator pattern (open-multi-agent reimplemented in Rust).
//!
//! Key concepts:
//! - `AgentConfig`: spec for an agent (model, prompt, tools, turn limit)
//! - `TaskDefinition` / `TaskDAG`: dependency-aware task graph
//! - `SharedContext`: append-only thread-safe key-value store for inter-task results
//! - `Coordinator`: decomposes a goal, runs the DAG, synthesises a final result
//! - `Team` / `run_team`: top-level entry point

use caduceus_core::{ModelId, ProviderId, TokenUsage};
use caduceus_providers::{ChatRequest, LlmAdapter, Message};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::RwLock;

// ── AgentConfig ────────────────────────────────────────────────────────────────

/// Configuration for a single agent within a multi-agent team.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub model: ModelId,
    pub provider: ProviderId,
    pub system_prompt: String,
    /// Allowed tool names (empty = no tools).
    pub tools: Vec<String>,
    pub max_turns: usize,
}

impl AgentConfig {
    pub fn new(name: impl Into<String>, model: ModelId, provider: ProviderId) -> Self {
        Self {
            name: name.into(),
            model,
            provider,
            system_prompt: String::new(),
            tools: Vec::new(),
            max_turns: 10,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }
}

// ── TaskStatus ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    /// Waiting for dependencies to complete.
    Pending,
    /// Actively executing.
    Running,
    /// Finished successfully; carries the output string.
    Completed(String),
    /// Finished with an error; carries the error message.
    Failed(String),
    /// Cancelled (e.g. a dependency failed).
    Cancelled,
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed(_) | Self::Failed(_) | Self::Cancelled)
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Completed(_))
    }
}

// ── TaskDefinition ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TaskDefinition {
    pub id: String,
    pub title: String,
    pub description: String,
    /// Name of the `AgentConfig` that should execute this task.
    pub assignee: Option<String>,
    /// IDs of tasks that must complete before this one can run.
    pub depends_on: Vec<String>,
    pub max_retries: usize,
    pub status: TaskStatus,
}

impl TaskDefinition {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: String::new(),
            assignee: None,
            depends_on: Vec::new(),
            max_retries: 0,
            status: TaskStatus::Pending,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn with_assignee(mut self, name: impl Into<String>) -> Self {
        self.assignee = Some(name.into());
        self
    }

    pub fn depends_on(mut self, ids: Vec<String>) -> Self {
        self.depends_on = ids;
        self
    }
}

// ── TaskDAG ────────────────────────────────────────────────────────────────────

/// Dependency-aware task graph.  Insertion validates for cycles.
#[derive(Debug, Default)]
pub struct TaskDAG {
    tasks: HashMap<String, TaskDefinition>,
}

impl TaskDAG {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }

    /// Insert a task.  Returns `Err` if a cycle would be introduced.
    pub fn add_task(&mut self, task: TaskDefinition) -> Result<(), String> {
        let task_id = task.id.clone();
        self.tasks.insert(task_id.clone(), task);
        if self.has_cycle() {
            self.tasks.remove(&task_id);
            return Err(format!("Adding task '{task_id}' would create a cycle"));
        }
        Ok(())
    }

    /// Returns tasks whose dependencies are all `Completed` and which are
    /// themselves still `Pending`.
    pub fn ready_tasks(&self) -> Vec<&TaskDefinition> {
        self.tasks
            .values()
            .filter(|t| {
                t.status == TaskStatus::Pending
                    && t.depends_on.iter().all(|dep_id| {
                        self.tasks
                            .get(dep_id)
                            .map(|d| d.status.is_success())
                            .unwrap_or(false)
                    })
            })
            .collect()
    }

    /// Mark a task `Completed` with the given result string.
    pub fn complete_task(&mut self, id: &str, result: String) -> Result<(), String> {
        match self.tasks.get_mut(id) {
            Some(t) => {
                t.status = TaskStatus::Completed(result);
                Ok(())
            }
            None => Err(format!("Task '{id}' not found")),
        }
    }

    /// Mark a task `Failed` and cascade `Cancelled` to all transitive dependents.
    pub fn fail_task(&mut self, id: &str, error: String) -> Result<(), String> {
        match self.tasks.get_mut(id) {
            Some(t) => t.status = TaskStatus::Failed(error),
            None => return Err(format!("Task '{id}' not found")),
        }

        // BFS cascade
        let mut to_cancel: VecDeque<String> = VecDeque::new();
        for task in self.tasks.values() {
            if task.depends_on.contains(&id.to_string()) {
                to_cancel.push_back(task.id.clone());
            }
        }
        while let Some(cid) = to_cancel.pop_front() {
            if let Some(t) = self.tasks.get_mut(&cid) {
                if matches!(t.status, TaskStatus::Pending | TaskStatus::Running) {
                    t.status = TaskStatus::Cancelled;
                    // Cascade further
                    let dependents: Vec<String> = self
                        .tasks
                        .values()
                        .filter(|x| x.depends_on.contains(&cid))
                        .map(|x| x.id.clone())
                        .collect();
                    to_cancel.extend(dependents);
                }
            }
        }
        Ok(())
    }

    /// Returns `true` when every task is in a terminal state.
    pub fn is_complete(&self) -> bool {
        self.tasks.values().all(|t| t.status.is_terminal())
    }

    pub fn tasks(&self) -> &HashMap<String, TaskDefinition> {
        &self.tasks
    }

    // ── internal cycle detection (DFS) ───────────────────────────────────────

    fn has_cycle(&self) -> bool {
        let mut visited: HashSet<String> = HashSet::new();
        let mut rec_stack: HashSet<String> = HashSet::new();

        for id in self.tasks.keys() {
            if self.dfs_cycle(id, &mut visited, &mut rec_stack) {
                return true;
            }
        }
        false
    }

    fn dfs_cycle(
        &self,
        id: &str,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
    ) -> bool {
        if rec_stack.contains(id) {
            return true;
        }
        if visited.contains(id) {
            return false;
        }
        visited.insert(id.to_string());
        rec_stack.insert(id.to_string());

        if let Some(task) = self.tasks.get(id) {
            for dep in &task.depends_on {
                if self.dfs_cycle(dep, visited, rec_stack) {
                    return true;
                }
            }
        }
        rec_stack.remove(id);
        false
    }
}

// ── SharedContext ──────────────────────────────────────────────────────────────

/// Append-only, thread-safe store mapping task IDs → task output strings.
#[derive(Clone, Debug, Default)]
pub struct SharedContext {
    inner: Arc<RwLock<HashMap<String, String>>>,
}

impl SharedContext {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Write a result for `key`.  Overwrites if the key already exists
    /// (tasks are only written once in practice, but idempotency is safe).
    pub async fn write(&self, key: impl Into<String>, value: impl Into<String>) {
        let mut map = self.inner.write().await;
        map.insert(key.into(), value.into());
    }

    /// Read the result for `key`, if present.
    pub async fn read(&self, key: &str) -> Option<String> {
        let map = self.inner.read().await;
        map.get(key).cloned()
    }

    /// Snapshot the whole map (for synthesis prompt building).
    pub async fn snapshot(&self) -> HashMap<String, String> {
        self.inner.read().await.clone()
    }
}

// ── TeamResult ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct TeamResult {
    pub success: bool,
    pub output: String,
    pub task_outputs: HashMap<String, String>,
    pub usage: TokenUsage,
}

// ── Coordinator ────────────────────────────────────────────────────────────────

/// Drives the coordinator pattern:
///
/// 1. Send goal to coordinator LLM → get JSON task list
/// 2. Parse into `TaskDAG`
/// 3. Execute ready tasks in parallel (each task runs the assigned agent's prompt)
/// 4. Synthesise final result from all outputs
pub struct Coordinator {
    agents: Vec<AgentConfig>,
    context: SharedContext,
}

impl Coordinator {
    pub fn new(agents: Vec<AgentConfig>) -> Self {
        Self {
            agents,
            context: SharedContext::new(),
        }
    }

    pub fn with_context(mut self, ctx: SharedContext) -> Self {
        self.context = ctx;
        self
    }

    /// Decompose `goal` into a `TaskDAG` by calling the coordinator LLM.
    /// Falls back to one task per agent if parsing fails.
    pub async fn decompose(
        &self,
        goal: &str,
        provider: &dyn LlmAdapter,
        model: &ModelId,
    ) -> TaskDAG {
        let prompt = build_decompose_prompt(goal, &self.agents);
        let req = ChatRequest {
            model: model.clone(),
            messages: vec![Message::user(&prompt)],
            system: Some(coordinator_system_prompt()),
            max_tokens: 4096,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
        };

        let raw = match provider.chat(req).await {
            Ok(resp) => resp.content,
            Err(_) => return self.fallback_dag(goal),
        };

        match parse_task_json(&raw) {
            Some(dag) if !dag.tasks().is_empty() => dag,
            _ => self.fallback_dag(goal),
        }
    }

    /// Execute the DAG, running ready tasks in parallel batches.
    pub async fn execute(
        &self,
        dag: &mut TaskDAG,
        provider: Arc<dyn LlmAdapter>,
        model: &ModelId,
    ) -> TokenUsage {
        let mut total_usage = TokenUsage::default();

        loop {
            let ready: Vec<String> = dag.ready_tasks().iter().map(|t| t.id.clone()).collect();

            if ready.is_empty() {
                break;
            }

            let mut handles = Vec::new();
            for task_id in &ready {
                if let Some(task) = dag.tasks().get(task_id) {
                    let task = task.clone();
                    let ctx = self.context.clone();
                    let prov = provider.clone();
                    let model = model.clone();
                    let agents = self.agents.clone();

                    let handle =
                        tokio::spawn(async move { run_task(task, ctx, prov, model, agents).await });
                    handles.push((task_id.clone(), handle));
                }
            }

            // Mark running
            for id in &ready {
                if let Some(t) = dag.tasks.get_mut(id) {
                    t.status = TaskStatus::Running;
                }
            }

            for (id, handle) in handles {
                match handle.await {
                    Ok(Ok((output, usage))) => {
                        total_usage.accumulate(&usage);
                        let _ = dag.complete_task(&id, output.clone());
                        self.context.write(&id, output).await;
                    }
                    Ok(Err(e)) => {
                        let _ = dag.fail_task(&id, e.to_string());
                    }
                    Err(e) => {
                        let _ = dag.fail_task(&id, format!("task panicked: {e}"));
                    }
                }
            }
        }

        total_usage
    }

    /// Synthesise the final result from all task outputs.
    pub async fn synthesise(
        &self,
        goal: &str,
        provider: &dyn LlmAdapter,
        model: &ModelId,
    ) -> Result<(String, TokenUsage), String> {
        let outputs = self.context.snapshot().await;
        let prompt = build_synthesis_prompt(goal, &outputs);
        let req = ChatRequest {
            model: model.clone(),
            messages: vec![Message::user(&prompt)],
            system: Some(coordinator_system_prompt()),
            max_tokens: 4096,
            temperature: None,
            thinking_mode: false,
            tool_choice: None,
            response_format: None,
        };

        provider
            .chat(req)
            .await
            .map(|r| {
                let usage = TokenUsage {
                    input_tokens: r.input_tokens,
                    output_tokens: r.output_tokens,
                    ..Default::default()
                };
                (r.content, usage)
            })
            .map_err(|e: caduceus_core::CaduceusError| e.to_string())
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Fallback: create one task per agent using the original goal.
    fn fallback_dag(&self, goal: &str) -> TaskDAG {
        let mut dag = TaskDAG::new();
        for (i, agent) in self.agents.iter().enumerate() {
            let mut task = TaskDefinition::new(
                format!("task-{i}"),
                format!("{} handles: {}", agent.name, &goal[..goal.len().min(60)]),
            );
            task.assignee = Some(agent.name.clone());
            task.description = goal.to_string();
            let _ = dag.add_task(task);
        }
        dag
    }
}

// ── Task execution ─────────────────────────────────────────────────────────────

/// Execute a single task by sending the task prompt to the assigned agent's LLM.
async fn run_task(
    task: TaskDefinition,
    context: SharedContext,
    provider: Arc<dyn LlmAdapter>,
    model: ModelId,
    agents: Vec<AgentConfig>,
) -> Result<(String, TokenUsage), String> {
    let agent_cfg = task
        .assignee
        .as_ref()
        .and_then(|name| agents.iter().find(|a| &a.name == name));

    let system = agent_cfg
        .map(|a| a.system_prompt.clone())
        .unwrap_or_else(|| "You are a helpful assistant.".to_string());

    let used_model = agent_cfg.map(|a| a.model.clone()).unwrap_or(model);

    // Build prompt including prior task outputs for context.
    let snapshot = context.snapshot().await;
    let mut context_block = String::new();
    for (dep_id, output) in &snapshot {
        context_block.push_str(&format!("\n[{dep_id}]: {output}"));
    }

    let user_prompt = if context_block.is_empty() {
        format!("Task: {}\n\n{}", task.title, task.description)
    } else {
        format!(
            "Task: {}\n\n{}\n\nContext from prior tasks:{context_block}",
            task.title, task.description
        )
    };

    let req = ChatRequest {
        model: used_model,
        messages: vec![Message::user(&user_prompt)],
        system: Some(system),
        max_tokens: 4096,
        temperature: None,
        thinking_mode: false,
        tool_choice: None,
        response_format: None,
    };

    provider
        .chat(req)
        .await
        .map(|r| {
            let usage = TokenUsage {
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
                ..Default::default()
            };
            (r.content, usage)
        })
        .map_err(|e: caduceus_core::CaduceusError| e.to_string())
}

// ── JSON parsing ───────────────────────────────────────────────────────────────

/// Parse a coordinator response into a `TaskDAG`.
///
/// Accepted shapes:
/// ```json
/// [{"id":"t1","title":"Do X","description":"...","assignee":"agent","depends_on":[]}]
/// ```
/// Also tries fenced-block extraction and bracket-range fallback.
pub fn parse_task_json(raw: &str) -> Option<TaskDAG> {
    let json_str = extract_json_array(raw)?;
    let arr: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let items = arr.as_array()?;

    let mut dag = TaskDAG::new();
    for (idx, item) in items.iter().enumerate() {
        let id = item["id"]
            .as_str()
            .unwrap_or(&format!("task-{idx}"))
            .to_string();
        let title = item["title"].as_str().unwrap_or("Untitled").to_string();
        let description = item["description"].as_str().unwrap_or("").to_string();
        let assignee = item["assignee"].as_str().map(|s| s.to_string());
        let depends_on: Vec<String> = item["depends_on"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let task = TaskDefinition {
            id,
            title,
            description,
            assignee,
            depends_on,
            max_retries: 0,
            status: TaskStatus::Pending,
        };
        // Ignore tasks that would create a cycle.
        let _ = dag.add_task(task);
    }
    Some(dag)
}

fn extract_json_array(raw: &str) -> Option<String> {
    // 1. Fenced code block: ```json ... ```
    if let Some(start) = raw.find("```json") {
        let after = &raw[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    if let Some(start) = raw.find("```") {
        let after = &raw[start + 3..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if candidate.starts_with('[') {
                return Some(candidate.to_string());
            }
        }
    }
    // 2. Bracket-range fallback: first '[' to matching ']'
    let start = raw.find('[')?;
    let mut depth = 0usize;
    for (i, ch) in raw[start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(raw[start..start + i + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

// ── Prompt builders ────────────────────────────────────────────────────────────

fn coordinator_system_prompt() -> String {
    "You are a coordinator agent responsible for decomposing goals into tasks \
     and synthesising results. Always respond with valid JSON when asked to \
     produce a task list."
        .to_string()
}

fn build_decompose_prompt(goal: &str, agents: &[AgentConfig]) -> String {
    let roster: Vec<String> = agents
        .iter()
        .map(|a| format!("- {} ({})", a.name, a.model.0))
        .collect();
    format!(
        "Decompose the following goal into a JSON task list.\n\n\
         Goal: {goal}\n\n\
         Available agents:\n{}\n\n\
         Return a JSON array where each item has: \
         id (string), title (string), description (string), \
         assignee (agent name string), depends_on (array of task id strings).",
        roster.join("\n")
    )
}

fn build_synthesis_prompt(goal: &str, outputs: &HashMap<String, String>) -> String {
    let parts: Vec<String> = outputs
        .iter()
        .map(|(id, out)| format!("[{id}]:\n{out}"))
        .collect();
    format!(
        "Original goal: {goal}\n\n\
         Task outputs:\n{}\n\n\
         Synthesise a coherent final answer from the above outputs.",
        parts.join("\n\n")
    )
}

// ── Team ───────────────────────────────────────────────────────────────────────

pub struct Team {
    pub name: String,
    pub agents: Vec<AgentConfig>,
    pub shared_context: SharedContext,
}

impl Team {
    pub fn new(name: impl Into<String>, agents: Vec<AgentConfig>) -> Self {
        Self {
            name: name.into(),
            shared_context: SharedContext::new(),
            agents,
        }
    }
}

// ── run_team ───────────────────────────────────────────────────────────────────

/// Run the full coordinator pattern: decompose → execute DAG → synthesise.
pub async fn run_team(
    team: Team,
    goal: &str,
    provider: Arc<dyn LlmAdapter>,
    model: &ModelId,
) -> TeamResult {
    let coordinator = Coordinator::new(team.agents).with_context(team.shared_context);

    let mut dag = coordinator.decompose(goal, provider.as_ref(), model).await;

    let exec_usage = coordinator.execute(&mut dag, provider.clone(), model).await;

    let task_outputs = coordinator.context.snapshot().await;

    let (output, synth_usage) = coordinator
        .synthesise(goal, provider.as_ref(), model)
        .await
        .unwrap_or_else(|e| (format!("Synthesis failed: {e}"), TokenUsage::default()));

    let mut usage = exec_usage;
    usage.accumulate(&synth_usage);

    let success = dag.is_complete() && dag.tasks().values().any(|t| t.status.is_success());

    TeamResult {
        success,
        output,
        task_outputs,
        usage,
    }
}

// ── Task decomposition ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Complexity {
    Simple,
    Medium,
    Complex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecomposedTask {
    pub id: usize,
    pub title: String,
    pub description: String,
    pub estimated_complexity: Complexity,
    pub depends_on: Vec<usize>,
}

pub struct TaskDecomposer;

impl TaskDecomposer {
    pub fn decompose(description: &str) -> Vec<DecomposedTask> {
        let mut steps = split_description_into_steps(description);
        if steps.is_empty() && !description.trim().is_empty() {
            steps.push(description.trim().to_string());
        }

        steps
            .into_iter()
            .enumerate()
            .map(|(index, step)| DecomposedTask {
                id: index,
                title: build_task_title(&step),
                description: step.clone(),
                estimated_complexity: classify_complexity(&step),
                depends_on: if index == 0 {
                    Vec::new()
                } else {
                    vec![index - 1]
                },
            })
            .collect()
    }

    pub fn build_dependency_graph(tasks: &[DecomposedTask]) -> Vec<(usize, usize)> {
        let known_ids: HashSet<usize> = tasks.iter().map(|task| task.id).collect();
        let mut edges = Vec::new();

        for task in tasks {
            for dependency in &task.depends_on {
                if known_ids.contains(dependency) {
                    edges.push((*dependency, task.id));
                }
            }
        }

        edges
    }

    pub fn topological_sort(tasks: &[DecomposedTask], deps: &[(usize, usize)]) -> Vec<usize> {
        let mut indegree = HashMap::new();
        let mut adjacency: HashMap<usize, Vec<usize>> = HashMap::new();

        for task in tasks {
            indegree.insert(task.id, 0usize);
            adjacency.entry(task.id).or_default();
        }

        for (from, to) in deps {
            if indegree.contains_key(from) && indegree.contains_key(to) {
                adjacency.entry(*from).or_default().push(*to);
                *indegree.entry(*to).or_default() += 1;
            }
        }

        let mut ready: Vec<usize> = indegree
            .iter()
            .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
            .collect();
        ready.sort_unstable();

        let mut queue: VecDeque<usize> = ready.into();
        let mut ordered = Vec::new();

        while let Some(node) = queue.pop_front() {
            ordered.push(node);

            if let Some(children) = adjacency.get(&node) {
                let mut next_ready = Vec::new();
                for child in children {
                    if let Some(degree) = indegree.get_mut(child) {
                        *degree = degree.saturating_sub(1);
                        if *degree == 0 {
                            next_ready.push(*child);
                        }
                    }
                }
                next_ready.sort_unstable();
                queue.extend(next_ready);
            }
        }

        if ordered.len() == tasks.len() {
            ordered
        } else {
            Vec::new()
        }
    }
}

fn split_description_into_steps(description: &str) -> Vec<String> {
    description
        .lines()
        .flat_map(|line| line.split(';'))
        .flat_map(|line| line.split('.'))
        .flat_map(|line| line.split(" and then "))
        .flat_map(|line| line.split(" then "))
        .map(str::trim)
        .map(|line| {
            line.trim_start_matches(|ch: char| {
                ch.is_ascii_digit() || matches!(ch, '.' | '-' | '*' | ')' | ' ')
            })
        })
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn build_task_title(step: &str) -> String {
    let title_words = step.split_whitespace().take(5).collect::<Vec<_>>();
    if title_words.is_empty() {
        "Untitled task".to_string()
    } else {
        title_words.join(" ")
    }
}

fn classify_complexity(step: &str) -> Complexity {
    let word_count = step.split_whitespace().count();
    if word_count >= 12 {
        Complexity::Complex
    } else if word_count >= 6 {
        Complexity::Medium
    } else {
        Complexity::Simple
    }
}

// ── Notification routing ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NotificationSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationChannel {
    Terminal,
    Desktop,
    Webhook(String),
    Log,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationRoute {
    pub min_severity: NotificationSeverity,
    pub channels: Vec<NotificationChannel>,
    pub pattern: Option<String>,
}

pub struct NotificationRouter {
    routes: Vec<NotificationRoute>,
}

impl NotificationRouter {
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    pub fn add_route(&mut self, route: NotificationRoute) {
        self.routes.push(route);
    }

    pub fn route(
        &self,
        severity: NotificationSeverity,
        message: &str,
    ) -> Vec<&NotificationChannel> {
        let normalized_message = message.to_lowercase();
        let mut selected = Vec::new();

        for route in &self.routes {
            let pattern_matches = route
                .pattern
                .as_ref()
                .map(|pattern| normalized_message.contains(&pattern.to_lowercase()))
                .unwrap_or(true);

            if severity >= route.min_severity && pattern_matches {
                for channel in &route.channels {
                    if selected.iter().all(|existing| *existing != channel) {
                        selected.push(channel);
                    }
                }
            }
        }

        selected
    }

    pub fn default_routes() -> Self {
        let mut router = Self::new();
        router.add_route(NotificationRoute {
            min_severity: NotificationSeverity::Info,
            channels: vec![NotificationChannel::Terminal],
            pattern: None,
        });
        router.add_route(NotificationRoute {
            min_severity: NotificationSeverity::Warning,
            channels: vec![NotificationChannel::Log],
            pattern: None,
        });
        router.add_route(NotificationRoute {
            min_severity: NotificationSeverity::Error,
            channels: vec![NotificationChannel::Desktop],
            pattern: None,
        });
        router.add_route(NotificationRoute {
            min_severity: NotificationSeverity::Critical,
            channels: vec![NotificationChannel::Webhook(
                "critical://alerts".to_string(),
            )],
            pattern: None,
        });
        router
    }
}

impl Default for NotificationRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Multi-repo workspace ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub is_active: bool,
}

pub struct MultiRepoWorkspace {
    repos: Vec<RepoEntry>,
    active_repo: Option<usize>,
}

impl MultiRepoWorkspace {
    pub fn new() -> Self {
        Self {
            repos: Vec::new(),
            active_repo: None,
        }
    }

    pub fn add_repo(&mut self, name: &str, path: PathBuf) -> Result<(), String> {
        if self.repos.iter().any(|repo| repo.name == name) {
            return Err(format!("Repository '{name}' already exists"));
        }
        if self.repos.iter().any(|repo| repo.path == path) {
            return Err(format!(
                "Repository path '{}' already exists",
                path.display()
            ));
        }

        let should_activate = self.active_repo.is_none();
        self.repos.push(RepoEntry {
            name: name.to_string(),
            branch: detect_branch(&path),
            path,
            is_active: should_activate,
        });
        if should_activate {
            self.active_repo = Some(self.repos.len() - 1);
        }
        Ok(())
    }

    pub fn remove_repo(&mut self, name: &str) -> Result<(), String> {
        let index = self
            .repos
            .iter()
            .position(|repo| repo.name == name)
            .ok_or_else(|| format!("Repository '{name}' not found"))?;

        self.repos.remove(index);

        match self.active_repo {
            Some(active) if active == index => {
                self.active_repo = None;
                if let Some(first_repo) = self.repos.first_mut() {
                    first_repo.is_active = true;
                    self.active_repo = Some(0);
                }
            }
            Some(active) if active > index => {
                self.active_repo = Some(active - 1);
            }
            _ => {}
        }

        Ok(())
    }

    pub fn set_active(&mut self, name: &str) -> Result<(), String> {
        let index = self
            .repos
            .iter()
            .position(|repo| repo.name == name)
            .ok_or_else(|| format!("Repository '{name}' not found"))?;

        for repo in &mut self.repos {
            repo.is_active = false;
        }
        if let Some(repo) = self.repos.get_mut(index) {
            repo.is_active = true;
        }
        self.active_repo = Some(index);
        Ok(())
    }

    pub fn get_active(&self) -> Option<&RepoEntry> {
        self.active_repo.and_then(|index| self.repos.get(index))
    }

    pub fn list_repos(&self) -> &[RepoEntry] {
        &self.repos
    }

    pub fn find_by_path(&self, path: &Path) -> Option<&RepoEntry> {
        self.repos
            .iter()
            .filter(|repo| path.starts_with(&repo.path))
            .max_by_key(|repo| repo.path.components().count())
    }
}

impl Default for MultiRepoWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

fn detect_branch(path: &Path) -> String {
    let head_path = path.join(".git").join("HEAD");
    std::fs::read_to_string(head_path)
        .ok()
        .and_then(|head| head.trim().rsplit('/').next().map(ToString::to_string))
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::{ModelId, ProviderId};

    fn make_agent(name: &str) -> AgentConfig {
        AgentConfig::new(
            name,
            ModelId::new("claude-3-5-sonnet"),
            ProviderId::new("anthropic"),
        )
        .with_system_prompt("You are helpful.")
    }

    // ── 1. add tasks and check ready_tasks ────────────────────────────────────

    #[test]
    fn dag_add_and_ready() {
        let mut dag = TaskDAG::new();
        let t1 = TaskDefinition::new("t1", "First task");
        let t2 = TaskDefinition::new("t2", "Second task");
        dag.add_task(t1).unwrap();
        dag.add_task(t2).unwrap();

        let ready: Vec<&str> = dag.ready_tasks().iter().map(|t| t.id.as_str()).collect();
        assert!(ready.contains(&"t1"));
        assert!(ready.contains(&"t2"));
    }

    // ── 2. complete unblocks dependents ──────────────────────────────────────

    #[test]
    fn dag_complete_unblocks_dependents() {
        let mut dag = TaskDAG::new();
        dag.add_task(TaskDefinition::new("t1", "T1")).unwrap();
        let t2 = TaskDefinition::new("t2", "T2").depends_on(vec!["t1".to_string()]);
        dag.add_task(t2).unwrap();

        // Before completion: only t1 is ready
        assert_eq!(dag.ready_tasks().len(), 1);
        assert_eq!(dag.ready_tasks()[0].id, "t1");

        dag.complete_task("t1", "done".to_string()).unwrap();

        // After completion: t2 becomes ready
        assert_eq!(dag.ready_tasks().len(), 1);
        assert_eq!(dag.ready_tasks()[0].id, "t2");
    }

    // ── 3. fail cascades to dependents ───────────────────────────────────────

    #[test]
    fn dag_fail_cascades() {
        let mut dag = TaskDAG::new();
        dag.add_task(TaskDefinition::new("t1", "T1")).unwrap();
        let t2 = TaskDefinition::new("t2", "T2").depends_on(vec!["t1".to_string()]);
        let t3 = TaskDefinition::new("t3", "T3").depends_on(vec!["t2".to_string()]);
        dag.add_task(t2).unwrap();
        dag.add_task(t3).unwrap();

        dag.fail_task("t1", "oops".to_string()).unwrap();

        assert!(matches!(dag.tasks()["t1"].status, TaskStatus::Failed(_)));
        assert_eq!(dag.tasks()["t2"].status, TaskStatus::Cancelled);
        assert_eq!(dag.tasks()["t3"].status, TaskStatus::Cancelled);
    }

    // ── 4. cycle detection ────────────────────────────────────────────────────

    #[test]
    fn dag_cycle_detected() {
        let mut dag = TaskDAG::new();
        dag.add_task(TaskDefinition::new("t1", "T1").depends_on(vec!["t2".to_string()]))
            .unwrap();
        // t2 depends on t1 → cycle
        let result =
            dag.add_task(TaskDefinition::new("t2", "T2").depends_on(vec!["t1".to_string()]));
        assert!(result.is_err(), "Expected cycle detection error");
    }

    // ── 5. is_complete ────────────────────────────────────────────────────────

    #[test]
    fn dag_is_complete() {
        let mut dag = TaskDAG::new();
        dag.add_task(TaskDefinition::new("t1", "T1")).unwrap();
        dag.add_task(TaskDefinition::new("t2", "T2")).unwrap();

        assert!(!dag.is_complete());

        dag.complete_task("t1", "ok".to_string()).unwrap();
        assert!(!dag.is_complete());

        dag.complete_task("t2", "ok".to_string()).unwrap();
        assert!(dag.is_complete());
    }

    // ── 6. SharedContext write and read ───────────────────────────────────────

    #[tokio::test]
    async fn shared_context_write_and_read() {
        let ctx = SharedContext::new();
        ctx.write("task-1", "output text").await;
        let val = ctx.read("task-1").await;
        assert_eq!(val, Some("output text".to_string()));

        let missing = ctx.read("task-99").await;
        assert_eq!(missing, None);
    }

    #[tokio::test]
    async fn shared_context_snapshot() {
        let ctx = SharedContext::new();
        ctx.write("a", "alpha").await;
        ctx.write("b", "beta").await;
        let snap = ctx.snapshot().await;
        assert_eq!(snap.get("a").map(|s| s.as_str()), Some("alpha"));
        assert_eq!(snap.get("b").map(|s| s.as_str()), Some("beta"));
    }

    // ── 7. parse_task_json ────────────────────────────────────────────────────

    #[test]
    fn coordinator_parse_task_json() {
        let raw = r#"
Here is the task plan:
```json
[
  {"id":"t1","title":"Research","description":"Do research","assignee":"researcher","depends_on":[]},
  {"id":"t2","title":"Write","description":"Write report","assignee":"writer","depends_on":["t1"]}
]
```
"#;
        let dag = parse_task_json(raw).expect("parse should succeed");
        assert_eq!(dag.tasks().len(), 2);
        assert!(dag.tasks().contains_key("t1"));
        assert!(dag.tasks().contains_key("t2"));
        assert_eq!(dag.tasks()["t2"].depends_on, vec!["t1".to_string()]);
    }

    #[test]
    fn coordinator_parse_task_json_bracket_fallback() {
        let raw = r#"The tasks are: [{"id":"a","title":"A","description":"","assignee":null,"depends_on":[]}]"#;
        let dag = parse_task_json(raw).expect("bracket fallback should work");
        assert_eq!(dag.tasks().len(), 1);
    }

    // ── 8. Team creation ──────────────────────────────────────────────────────

    #[test]
    fn team_create_with_agents() {
        let agents = vec![
            make_agent("researcher"),
            make_agent("writer"),
            make_agent("reviewer"),
        ];
        let team = Team::new("my-team", agents);
        assert_eq!(team.name, "my-team");
        assert_eq!(team.agents.len(), 3);
        assert_eq!(team.agents[0].name, "researcher");
        assert_eq!(team.agents[1].name, "writer");
        assert_eq!(team.agents[2].name, "reviewer");
    }

    // ── 9. DAG with no tasks is complete ─────────────────────────────────────

    #[test]
    fn dag_empty_is_complete() {
        let dag = TaskDAG::new();
        assert!(dag.is_complete());
    }

    // ── 10. AgentConfig builder ───────────────────────────────────────────────

    #[test]
    fn agent_config_builder() {
        let cfg = AgentConfig::new("tester", ModelId::new("gpt-4o"), ProviderId::new("openai"))
            .with_system_prompt("Be concise.")
            .with_tools(vec!["bash".to_string(), "read_file".to_string()])
            .with_max_turns(5);

        assert_eq!(cfg.name, "tester");
        assert_eq!(cfg.model.0, "gpt-4o");
        assert_eq!(cfg.tools.len(), 2);
        assert_eq!(cfg.max_turns, 5);
    }

    #[test]
    fn task_decomposer_splits_description_into_linked_tasks() {
        let tasks = TaskDecomposer::decompose(
            "Analyze the bug. Implement the fix. Run the regression tests.",
        );

        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].depends_on, Vec::<usize>::new());
        assert_eq!(tasks[1].depends_on, vec![0]);
        assert_eq!(tasks[2].depends_on, vec![1]);
    }

    #[test]
    fn task_decomposer_builds_dependency_graph() {
        let tasks = vec![
            DecomposedTask {
                id: 0,
                title: "First".to_string(),
                description: "First".to_string(),
                estimated_complexity: Complexity::Simple,
                depends_on: Vec::new(),
            },
            DecomposedTask {
                id: 1,
                title: "Second".to_string(),
                description: "Second".to_string(),
                estimated_complexity: Complexity::Medium,
                depends_on: vec![0],
            },
        ];

        assert_eq!(TaskDecomposer::build_dependency_graph(&tasks), vec![(0, 1)]);
    }

    #[test]
    fn task_decomposer_topological_sort_and_cycle_detection() {
        let tasks = vec![
            DecomposedTask {
                id: 0,
                title: "First".to_string(),
                description: "First".to_string(),
                estimated_complexity: Complexity::Simple,
                depends_on: Vec::new(),
            },
            DecomposedTask {
                id: 1,
                title: "Second".to_string(),
                description: "Second".to_string(),
                estimated_complexity: Complexity::Simple,
                depends_on: vec![0],
            },
            DecomposedTask {
                id: 2,
                title: "Third".to_string(),
                description: "Third".to_string(),
                estimated_complexity: Complexity::Simple,
                depends_on: vec![1],
            },
        ];

        let deps = TaskDecomposer::build_dependency_graph(&tasks);
        assert_eq!(
            TaskDecomposer::topological_sort(&tasks, &deps),
            vec![0, 1, 2]
        );

        let cyclic = TaskDecomposer::topological_sort(&tasks, &[(0, 1), (1, 2), (2, 0)]);
        assert!(cyclic.is_empty());
    }

    #[test]
    fn notification_router_routes_by_severity_and_pattern() {
        let mut router = NotificationRouter::new();
        router.add_route(NotificationRoute {
            min_severity: NotificationSeverity::Warning,
            channels: vec![NotificationChannel::Terminal],
            pattern: None,
        });
        router.add_route(NotificationRoute {
            min_severity: NotificationSeverity::Error,
            channels: vec![NotificationChannel::Webhook("ops".to_string())],
            pattern: Some("deploy".to_string()),
        });

        let warning_channels = router.route(NotificationSeverity::Warning, "minor warning");
        let error_channels = router.route(NotificationSeverity::Error, "deploy failed");

        assert_eq!(warning_channels, vec![&NotificationChannel::Terminal]);
        assert_eq!(
            error_channels,
            vec![
                &NotificationChannel::Terminal,
                &NotificationChannel::Webhook("ops".to_string())
            ]
        );
    }

    #[test]
    fn notification_router_default_routes_cover_critical_alerts() {
        let router = NotificationRouter::default_routes();
        let channels = router.route(NotificationSeverity::Critical, "disk full");

        assert!(channels.contains(&&NotificationChannel::Terminal));
        assert!(channels.contains(&&NotificationChannel::Log));
        assert!(channels.contains(&&NotificationChannel::Desktop));
        assert!(channels.contains(&&NotificationChannel::Webhook(
            "critical://alerts".to_string()
        )));
    }

    #[test]
    fn multi_repo_workspace_add_remove_and_activate() {
        let mut workspace = MultiRepoWorkspace::new();
        workspace
            .add_repo("api", PathBuf::from("/workspace/api"))
            .unwrap();
        workspace
            .add_repo("web", PathBuf::from("/workspace/web"))
            .unwrap();

        assert_eq!(workspace.list_repos().len(), 2);
        assert_eq!(
            workspace.get_active().map(|repo| repo.name.as_str()),
            Some("api")
        );

        workspace.set_active("web").unwrap();
        assert_eq!(
            workspace.get_active().map(|repo| repo.name.as_str()),
            Some("web")
        );
        assert_eq!(
            workspace
                .find_by_path(Path::new("/workspace/web/src/lib.rs"))
                .map(|repo| repo.name.as_str()),
            Some("web")
        );

        workspace.remove_repo("web").unwrap();
        assert_eq!(workspace.list_repos().len(), 1);
        assert_eq!(
            workspace.get_active().map(|repo| repo.name.as_str()),
            Some("api")
        );
    }
}
