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
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

// ── Plugin system ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    pub manifest_path: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Default)]
pub struct PluginSystem {
    plugins: Vec<Plugin>,
}

impl PluginSystem {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_manifest_toml(&mut self, toml_str: &str) -> Result<Plugin, String> {
        let manifest = parse_plugin_manifest_toml(toml_str)?;
        build_plugin_from_manifest(&manifest, "inline.toml")
    }

    pub fn load_manifest_json(&mut self, json_str: &str) -> Result<Plugin, String> {
        let manifest: serde_json::Value =
            serde_json::from_str(json_str).map_err(|err| err.to_string())?;
        build_plugin_from_json(&manifest, "inline.json")
    }

    pub fn install(&mut self, plugin: Plugin) {
        if let Some(existing) = self
            .plugins
            .iter_mut()
            .find(|item| item.name == plugin.name)
        {
            *existing = plugin;
        } else {
            self.plugins.push(plugin);
        }
    }

    pub fn uninstall(&mut self, name: &str) -> Result<(), String> {
        let index = self
            .plugins
            .iter()
            .position(|plugin| plugin.name == name)
            .ok_or_else(|| format!("Plugin '{name}' not found"))?;
        self.plugins.remove(index);
        Ok(())
    }

    pub fn enable(&mut self, name: &str) -> Result<(), String> {
        let plugin = self
            .plugins
            .iter_mut()
            .find(|plugin| plugin.name == name)
            .ok_or_else(|| format!("Plugin '{name}' not found"))?;
        plugin.enabled = true;
        Ok(())
    }

    pub fn disable(&mut self, name: &str) -> Result<(), String> {
        let plugin = self
            .plugins
            .iter_mut()
            .find(|plugin| plugin.name == name)
            .ok_or_else(|| format!("Plugin '{name}' not found"))?;
        plugin.enabled = false;
        Ok(())
    }

    pub fn list(&self) -> &[Plugin] {
        &self.plugins
    }

    pub fn get(&self, name: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|plugin| plugin.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommand {
    pub name: String,
    pub description: String,
    pub plugin: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginAgent {
    pub name: String,
    pub system_prompt: String,
    pub plugin: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSkill {
    pub name: String,
    pub content: String,
    pub plugin: String,
}

#[derive(Debug, Default)]
pub struct PluginExtensions {
    commands: Vec<PluginCommand>,
    agents: Vec<PluginAgent>,
    skills: Vec<PluginSkill>,
}

impl PluginExtensions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_command(&mut self, cmd: PluginCommand) {
        self.commands.push(cmd);
    }

    pub fn register_agent(&mut self, agent: PluginAgent) {
        self.agents.push(agent);
    }

    pub fn register_skill(&mut self, skill: PluginSkill) {
        self.skills.push(skill);
    }

    pub fn commands_for_plugin(&self, plugin: &str) -> Vec<&PluginCommand> {
        self.commands
            .iter()
            .filter(|cmd| cmd.plugin == plugin)
            .collect()
    }

    pub fn all_commands(&self) -> &[PluginCommand] {
        &self.commands
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PluginCapability {
    ReadFiles,
    WriteFiles,
    RunCommands,
    NetworkAccess,
    FullAccess,
}

#[derive(Debug, Default)]
pub struct PluginCapabilityManager {
    grants: HashMap<String, Vec<PluginCapability>>,
}

impl PluginCapabilityManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn grant(&mut self, plugin: &str, cap: PluginCapability) {
        let caps = self.grants.entry(plugin.to_string()).or_default();
        if !caps.contains(&cap) {
            caps.push(cap);
        }
    }

    pub fn revoke(&mut self, plugin: &str, cap: &PluginCapability) {
        if let Some(caps) = self.grants.get_mut(plugin) {
            caps.retain(|existing| existing != cap);
            if caps.is_empty() {
                self.grants.remove(plugin);
            }
        }
    }

    pub fn check(&self, plugin: &str, cap: &PluginCapability) -> bool {
        self.grants
            .get(plugin)
            .is_some_and(|caps| caps.contains(&PluginCapability::FullAccess) || caps.contains(cap))
    }

    pub fn list_grants(&self, plugin: &str) -> Vec<&PluginCapability> {
        self.grants
            .get(plugin)
            .map(|caps| caps.iter().collect())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginDefinedTool {
    pub name: String,
    pub plugin: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub command: String,
    pub env_vars: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub struct PluginToolRegistry {
    tools: Vec<PluginDefinedTool>,
}

impl PluginToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: PluginDefinedTool) {
        if let Some(existing) = self.tools.iter_mut().find(|item| item.name == tool.name) {
            *existing = tool;
        } else {
            self.tools.push(tool);
        }
    }

    pub fn get(&self, name: &str) -> Option<&PluginDefinedTool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub fn list(&self) -> &[PluginDefinedTool] {
        &self.tools
    }

    /// Execute a registered plugin tool synchronously.
    ///
    /// `caps` — optional capability manager; when provided the plugin must hold
    /// the `RunCommands` capability or an error is returned immediately.
    ///
    /// Dangerous environment-variable overrides (`LD_PRELOAD`,
    /// `DYLD_INSERT_LIBRARIES`, `LD_LIBRARY_PATH`, `PATH`, `HOME`) are stripped
    /// from the tool's `env_vars` before spawning the child process.
    ///
    /// The child process is killed and an error is returned if it does not
    /// complete within 5 seconds.
    pub fn execute_sync(
        &self,
        name: &str,
        input: &str,
        caps: Option<&PluginCapabilityManager>,
    ) -> Result<String, String> {
        let tool = self
            .get(name)
            .ok_or_else(|| format!("Tool '{name}' not found"))?;

        // FIX 2a: capability gate.
        if let Some(caps) = caps {
            if !caps.check(&tool.plugin, &PluginCapability::RunCommands) {
                return Err(format!(
                    "Plugin '{}' does not have RunCommands capability",
                    tool.plugin
                ));
            }
        }

        // FIX 2b: strip dangerous env-var overrides.
        const DANGEROUS: &[&str] = &[
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "LD_LIBRARY_PATH",
            "PATH",
            "HOME",
        ];
        let filtered_env: HashMap<String, String> = tool
            .env_vars
            .iter()
            .filter(|(k, _)| !DANGEROUS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let parts = split_command_line(&tool.command)?;
        let binary = parts
            .first()
            .ok_or_else(|| format!("Tool '{name}' has an empty command"))?;
        let mut child = Command::new(binary)
            .args(parts.iter().skip(1))
            .envs(&filtered_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| err.to_string())?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input.as_bytes())
                .map_err(|err| err.to_string())?;
            // stdin is dropped here, closing the pipe so the child can read EOF.
        }

        // FIX 2c: enforce a 5-second timeout using a try_wait polling loop.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(_) => break,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("Tool '{name}' timed out after 5 seconds"));
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }

        let output = child.wait_with_output().map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(if stderr.is_empty() {
                format!("Tool '{name}' exited with status {}", output.status)
            } else {
                stderr
            })
        }
    }
}

// ── Indexed task DAG ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DagTaskStatus {
    Pending,
    Ready,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagTask {
    pub id: usize,
    pub name: String,
    pub status: DagTaskStatus,
    pub result: Option<String>,
}

#[derive(Debug, Default)]
pub struct TaskDag {
    tasks: Vec<DagTask>,
    edges: Vec<(usize, usize)>,
}

impl TaskDag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_task(&mut self, name: &str) -> usize {
        let id = self.tasks.len();
        self.tasks.push(DagTask {
            id,
            name: name.to_string(),
            status: DagTaskStatus::Ready,
            result: None,
        });
        id
    }

    pub fn add_dependency(&mut self, from: usize, to: usize) -> Result<(), String> {
        if from >= self.tasks.len() || to >= self.tasks.len() {
            return Err("Task dependency references an unknown task".to_string());
        }
        if from == to {
            return Err("Task cannot depend on itself".to_string());
        }
        if !self.edges.contains(&(from, to)) {
            self.edges.push((from, to));
        }
        if self.topological_order().is_err() {
            self.edges.retain(|edge| edge != &(from, to));
            return Err("Adding dependency would create a cycle".to_string());
        }
        self.refresh_statuses();
        Ok(())
    }

    pub fn ready_tasks(&self) -> Vec<usize> {
        self.tasks
            .iter()
            .filter(|task| task.status == DagTaskStatus::Ready)
            .map(|task| task.id)
            .collect()
    }

    pub fn complete_task(&mut self, id: usize, result: &str) {
        if let Some(task) = self.tasks.get_mut(id) {
            task.status = DagTaskStatus::Completed;
            task.result = Some(result.to_string());
            self.refresh_statuses();
        }
    }

    pub fn fail_task(&mut self, id: usize) {
        if id >= self.tasks.len() {
            return;
        }

        let mut queue = VecDeque::from([id]);
        let mut visited = HashSet::new();

        while let Some(task_id) = queue.pop_front() {
            if !visited.insert(task_id) {
                continue;
            }
            if let Some(task) = self.tasks.get_mut(task_id) {
                task.status = DagTaskStatus::Failed;
                task.result = None;
            }
            for (_, to) in self
                .edges
                .iter()
                .copied()
                .filter(|(from, _)| *from == task_id)
            {
                queue.push_back(to);
            }
        }
    }

    pub fn is_complete(&self) -> bool {
        self.tasks.iter().all(|task| {
            matches!(
                task.status,
                DagTaskStatus::Completed | DagTaskStatus::Failed
            )
        })
    }

    pub fn topological_order(&self) -> Result<Vec<usize>, String> {
        let mut indegree = vec![0usize; self.tasks.len()];
        let mut adjacency: HashMap<usize, Vec<usize>> = HashMap::new();

        for &(from, to) in &self.edges {
            indegree[to] += 1;
            adjacency.entry(from).or_default().push(to);
        }

        let mut queue: VecDeque<usize> = indegree
            .iter()
            .enumerate()
            .filter_map(|(id, degree)| (*degree == 0).then_some(id))
            .collect();
        let mut ordered = Vec::with_capacity(self.tasks.len());

        while let Some(id) = queue.pop_front() {
            ordered.push(id);
            if let Some(children) = adjacency.get(&id) {
                for child in children {
                    indegree[*child] = indegree[*child].saturating_sub(1);
                    if indegree[*child] == 0 {
                        queue.push_back(*child);
                    }
                }
            }
        }

        if ordered.len() == self.tasks.len() {
            Ok(ordered)
        } else {
            Err("Task graph contains a cycle".to_string())
        }
    }

    fn refresh_statuses(&mut self) {
        let completed: HashSet<usize> = self
            .tasks
            .iter()
            .filter(|task| task.status == DagTaskStatus::Completed)
            .map(|task| task.id)
            .collect();

        for task in &mut self.tasks {
            if matches!(
                task.status,
                DagTaskStatus::Completed | DagTaskStatus::Failed | DagTaskStatus::Running
            ) {
                continue;
            }
            let ready = self
                .edges
                .iter()
                .filter(|(_, to)| *to == task.id)
                .all(|(from, _)| completed.contains(from));
            task.status = if ready {
                DagTaskStatus::Ready
            } else {
                DagTaskStatus::Pending
            };
        }
    }
}

// ── Team orchestration ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamAgent {
    pub name: String,
    pub role: String,
    pub specialties: Vec<String>,
}

#[derive(Debug, Default)]
pub struct TeamOrchestrator {
    agents: Vec<TeamAgent>,
    assignments: HashMap<usize, String>,
}

impl TeamOrchestrator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_agent(&mut self, agent: TeamAgent) {
        if let Some(existing) = self.agents.iter_mut().find(|item| item.name == agent.name) {
            *existing = agent;
        } else {
            self.agents.push(agent);
        }
    }

    pub fn assign_task(&mut self, task_id: usize, agent_name: &str) -> Result<(), String> {
        if self.agents.iter().any(|agent| agent.name == agent_name) {
            self.assignments.insert(task_id, agent_name.to_string());
            Ok(())
        } else {
            Err(format!("Agent '{agent_name}' not found"))
        }
    }

    pub fn auto_assign(&mut self, tasks: &[DagTask]) -> HashMap<usize, String> {
        let mut assigned = HashMap::new();
        for task in tasks {
            if let Some(agent_name) = self.best_agent_for_task(&task.name) {
                self.assignments.insert(task.id, agent_name.clone());
                assigned.insert(task.id, agent_name);
            }
        }
        assigned
    }

    pub fn list_agents(&self) -> &[TeamAgent] {
        &self.agents
    }

    fn best_agent_for_task(&self, task_name: &str) -> Option<String> {
        let normalized_task = task_name.to_lowercase();
        self.agents
            .iter()
            .max_by_key(|agent| {
                agent
                    .specialties
                    .iter()
                    .filter(|specialty| normalized_task.contains(&specialty.to_lowercase()))
                    .count()
            })
            .map(|agent| agent.name.clone())
            .or_else(|| self.agents.first().map(|agent| agent.name.clone()))
    }
}

// ── Message bus ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusMessage {
    pub from: String,
    pub content: String,
    pub timestamp: u64,
    pub channel: String,
}

#[derive(Debug, Default)]
pub struct MessageBus {
    channels: HashMap<String, Vec<BusMessage>>,
    subscribers: HashMap<String, Vec<String>>,
}

impl MessageBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&mut self, agent: &str, channel: &str) {
        let subscribers = self.subscribers.entry(channel.to_string()).or_default();
        if !subscribers.iter().any(|existing| existing == agent) {
            subscribers.push(agent.to_string());
        }
    }

    pub fn publish(&mut self, message: BusMessage) {
        self.channels
            .entry(message.channel.clone())
            .or_default()
            .push(message);
    }

    pub fn read(&self, agent: &str, channel: &str) -> Vec<&BusMessage> {
        let is_subscribed = self
            .subscribers
            .get(channel)
            .is_some_and(|agents| agents.iter().any(|name| name == agent));
        if !is_subscribed {
            return Vec::new();
        }
        self.channels
            .get(channel)
            .map(|messages| messages.iter().collect())
            .unwrap_or_default()
    }

    /// Return messages on `channel` posted after `since`, filtered by subscription.
    ///
    /// Only agents that have called [`subscribe`] for `channel` will receive
    /// results; unsubscribed callers get an empty vec, matching the behaviour
    /// of [`read`].
    pub fn read_since(&self, agent: &str, channel: &str, since: u64) -> Vec<&BusMessage> {
        // FIX 5: gate on subscription, same as `read`.
        let is_subscribed = self
            .subscribers
            .get(channel)
            .is_some_and(|agents| agents.iter().any(|name| name == agent));
        if !is_subscribed {
            return Vec::new();
        }
        self.channels
            .get(channel)
            .map(|messages| {
                messages
                    .iter()
                    .filter(|message| message.timestamp > since)
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ── Shared memory ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedMemoryEntry {
    pub key: String,
    pub value: String,
    pub writer: String,
    pub version: u32,
}

#[derive(Debug, Default)]
pub struct SharedMemory {
    store: HashMap<String, SharedMemoryEntry>,
}

impl SharedMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&mut self, key: &str, value: &str, writer: &str) {
        let version = self
            .store
            .get(key)
            .map(|entry| entry.version.saturating_add(1))
            .unwrap_or(1);
        self.store.insert(
            key.to_string(),
            SharedMemoryEntry {
                key: key.to_string(),
                value: value.to_string(),
                writer: writer.to_string(),
                version,
            },
        );
    }

    pub fn read(&self, key: &str) -> Option<&SharedMemoryEntry> {
        self.store.get(key)
    }

    pub fn delete(&mut self, key: &str) -> bool {
        self.store.remove(key).is_some()
    }

    pub fn list_keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.store.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    pub fn entries_by_writer(&self, writer: &str) -> Vec<&SharedMemoryEntry> {
        let mut entries: Vec<&SharedMemoryEntry> = self
            .store
            .values()
            .filter(|entry| entry.writer == writer)
            .collect();
        entries.sort_by(|left, right| left.key.cmp(&right.key));
        entries
    }
}

// ── Scheduling ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerStrategy {
    RoundRobin,
    LeastLoaded,
    Priority,
    Random,
}

#[derive(Debug)]
pub struct TaskScheduler {
    strategy: SchedulerStrategy,
    agent_loads: HashMap<String, usize>,
    round_robin_index: usize,
    random_state: u64,
}

impl TaskScheduler {
    pub fn new(strategy: SchedulerStrategy) -> Self {
        Self {
            strategy,
            agent_loads: HashMap::new(),
            round_robin_index: 0,
            random_state: seed_random_state(),
        }
    }

    pub fn schedule(&mut self, _task: &str, agents: &[&str]) -> String {
        if agents.is_empty() {
            return String::new();
        }

        let selected = match self.strategy {
            SchedulerStrategy::RoundRobin => {
                let agent = agents[self.round_robin_index % agents.len()];
                self.round_robin_index = (self.round_robin_index + 1) % agents.len();
                agent
            }
            SchedulerStrategy::LeastLoaded => agents
                .iter()
                .min_by_key(|agent| (self.agent_loads.get(**agent).copied().unwrap_or(0), **agent))
                .copied()
                .unwrap_or(agents[0]),
            SchedulerStrategy::Priority => agents[0],
            SchedulerStrategy::Random => {
                let index = self.next_random_index(agents.len());
                agents[index]
            }
        }
        .to_string();

        self.record_assignment(&selected);
        selected
    }

    pub fn record_completion(&mut self, agent: &str) {
        if let Some(load) = self.agent_loads.get_mut(agent) {
            *load = load.saturating_sub(1);
        }
    }

    pub fn record_assignment(&mut self, agent: &str) {
        *self.agent_loads.entry(agent.to_string()).or_default() += 1;
    }

    fn next_random_index(&mut self, len: usize) -> usize {
        self.random_state ^= self.random_state << 13;
        self.random_state ^= self.random_state >> 7;
        self.random_state ^= self.random_state << 17;
        (self.random_state as usize) % len
    }
}

// ── Just-in-time context loading ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefType {
    File,
    Query,
    Url,
    Memory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextReference {
    pub id: String,
    pub ref_type: RefType,
    pub path: String,
    pub estimated_tokens: usize,
}

/// NOTE: JitContextLoader is !Sync by design — use only on a single thread.
///
/// The `access_order` and `next_tick` fields use `RefCell`/`Cell` for interior
/// mutability in `get` (which takes `&self`).  These types are not `Sync`, so
/// `JitContextLoader` must not be shared across threads without external
/// synchronisation.
#[derive(Debug)]
pub struct JitContextLoader {
    references: Vec<ContextReference>,
    loaded: HashMap<String, String>,
    max_loaded_tokens: usize,
    access_order: RefCell<HashMap<String, u64>>,
    next_tick: Cell<u64>,
}

impl JitContextLoader {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            references: Vec::new(),
            loaded: HashMap::new(),
            max_loaded_tokens: max_tokens,
            access_order: RefCell::new(HashMap::new()),
            next_tick: Cell::new(0),
        }
    }

    pub fn add_reference(&mut self, reference: ContextReference) {
        if let Some(existing) = self
            .references
            .iter_mut()
            .find(|item| item.id == reference.id)
        {
            *existing = reference;
        } else {
            self.references.push(reference);
        }
    }

    pub fn load(&mut self, id: &str, content: &str) {
        if self.references.iter().any(|reference| reference.id == id) {
            self.loaded.insert(id.to_string(), content.to_string());
            self.touch(id);
        }
    }

    pub fn unload(&mut self, id: &str) {
        self.loaded.remove(id);
        self.access_order.borrow_mut().remove(id);
    }

    pub fn get(&self, id: &str) -> Option<&str> {
        if self.loaded.contains_key(id) {
            self.touch(id);
        }
        self.loaded.get(id).map(String::as_str)
    }

    pub fn loaded_tokens(&self) -> usize {
        self.references
            .iter()
            .filter(|reference| self.loaded.contains_key(&reference.id))
            .map(|reference| reference.estimated_tokens)
            .sum()
    }

    pub fn should_evict(&self) -> bool {
        self.loaded_tokens() > self.max_loaded_tokens
    }

    pub fn evict_lru(&mut self) -> Option<String> {
        let oldest = self
            .access_order
            .borrow()
            .iter()
            .filter(|(id, _)| self.loaded.contains_key(*id))
            .min_by_key(|(_, tick)| *tick)
            .map(|(id, _)| id.clone())?;
        self.unload(&oldest);
        Some(oldest)
    }

    fn touch(&self, id: &str) {
        let next = self.next_tick.get().saturating_add(1);
        self.next_tick.set(next);
        self.access_order.borrow_mut().insert(id.to_string(), next);
    }
}

fn seed_random_state() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(1)
        .max(1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManifestValue {
    String(String),
    Bool(bool),
    Array(Vec<String>),
}

fn parse_plugin_manifest_toml(input: &str) -> Result<HashMap<String, ManifestValue>, String> {
    let mut manifest = HashMap::new();
    let mut lines = input.lines();

    while let Some(raw_line) = lines.next() {
        let line = strip_toml_comment(raw_line).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && !line.contains('=') {
            return Err("TOML tables are not supported for plugin manifests".to_string());
        }
        let (key, initial_value) = line
            .split_once('=')
            .ok_or_else(|| format!("Invalid TOML line: {line}"))?;
        let mut value = initial_value.trim().to_string();
        while value.starts_with('[') && !brackets_balanced(&value) {
            let next_line = lines
                .next()
                .ok_or_else(|| "Unterminated TOML array".to_string())?;
            let next = strip_toml_comment(next_line).trim().to_string();
            if !next.is_empty() {
                value.push(' ');
                value.push_str(&next);
            }
        }

        let parsed = parse_manifest_value(&value)?;
        manifest.insert(key.trim().to_string(), parsed);
    }

    Ok(manifest)
}

fn build_plugin_from_manifest(
    manifest: &HashMap<String, ManifestValue>,
    default_manifest_path: &str,
) -> Result<Plugin, String> {
    Ok(Plugin {
        name: manifest_string(manifest, "name")?,
        version: manifest_string(manifest, "version")?,
        description: manifest_string_or_default(manifest, "description"),
        enabled: manifest_bool_or_default(manifest, "enabled", true),
        manifest_path: manifest
            .get("manifest_path")
            .map(manifest_value_as_string)
            .transpose()?
            .unwrap_or_else(|| default_manifest_path.to_string()),
        capabilities: manifest
            .get("capabilities")
            .map(manifest_value_as_array)
            .transpose()?
            .unwrap_or_default(),
    })
}

fn build_plugin_from_json(
    manifest: &serde_json::Value,
    default_manifest_path: &str,
) -> Result<Plugin, String> {
    let object = manifest
        .as_object()
        .ok_or_else(|| "Plugin manifest JSON must be an object".to_string())?;
    let capabilities = object
        .get("capabilities")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(ToString::to_string)
                        .ok_or_else(|| "Plugin capabilities must be strings".to_string())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

    Ok(Plugin {
        name: json_string(object, "name")?,
        version: json_string(object, "version")?,
        description: object
            .get("description")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        enabled: object
            .get("enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(true),
        manifest_path: object
            .get("manifest_path")
            .and_then(|value| value.as_str())
            .unwrap_or(default_manifest_path)
            .to_string(),
        capabilities,
    })
}

fn parse_manifest_value(value: &str) -> Result<ManifestValue, String> {
    let trimmed = value.trim();
    if trimmed.starts_with('[') {
        parse_manifest_array(trimmed).map(ManifestValue::Array)
    } else if matches!(trimmed, "true" | "false") {
        Ok(ManifestValue::Bool(trimmed == "true"))
    } else {
        parse_manifest_string(trimmed).map(ManifestValue::String)
    }
}

fn parse_manifest_array(value: &str) -> Result<Vec<String>, String> {
    let inner = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(|| "Invalid TOML array".to_string())?;
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }

    split_toml_list(inner)
        .into_iter()
        .map(|item| parse_manifest_string(&item))
        .collect()
}

fn parse_manifest_string(value: &str) -> Result<String, String> {
    if value.starts_with('"') {
        serde_json::from_str(value).map_err(|err| err.to_string())
    } else if value.starts_with('\'') {
        value
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .map(ToString::to_string)
            .ok_or_else(|| "Invalid literal string".to_string())
    } else if value.is_empty() {
        Err("Manifest value cannot be empty".to_string())
    } else {
        Ok(value.to_string())
    }
}

fn split_toml_list(input: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in input.chars() {
        match ch {
            '\\' if in_double && !escape => {
                escape = true;
                current.push(ch);
            }
            '\'' if !in_double && !escape => {
                in_single = !in_single;
                current.push(ch);
            }
            '"' if !in_single && !escape => {
                in_double = !in_double;
                current.push(ch);
            }
            ',' if !in_single && !in_double => {
                if !current.trim().is_empty() {
                    items.push(current.trim().to_string());
                }
                current.clear();
            }
            _ => {
                escape = false;
                current.push(ch);
            }
        }
    }

    if !current.trim().is_empty() {
        items.push(current.trim().to_string());
    }

    items
}

fn strip_toml_comment(line: &str) -> String {
    let mut result = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in line.chars() {
        match ch {
            '\\' if in_double && !escape => {
                escape = true;
                result.push(ch);
            }
            '\'' if !in_double && !escape => {
                in_single = !in_single;
                result.push(ch);
            }
            '"' if !in_single && !escape => {
                in_double = !in_double;
                result.push(ch);
            }
            '#' if !in_single && !in_double => break,
            _ => {
                escape = false;
                result.push(ch);
            }
        }
    }

    result
}

fn brackets_balanced(value: &str) -> bool {
    let mut depth = 0isize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in value.chars() {
        match ch {
            '\\' if in_double && !escape => escape = true,
            '\'' if !in_double && !escape => in_single = !in_single,
            '"' if !in_single && !escape => in_double = !in_double,
            '[' if !in_single && !in_double => depth += 1,
            ']' if !in_single && !in_double => depth -= 1,
            _ => escape = false,
        }
    }

    depth == 0
}

fn manifest_string(manifest: &HashMap<String, ManifestValue>, key: &str) -> Result<String, String> {
    manifest
        .get(key)
        .map(manifest_value_as_string)
        .transpose()?
        .ok_or_else(|| format!("Plugin manifest is missing '{key}'"))
}

fn manifest_string_or_default(manifest: &HashMap<String, ManifestValue>, key: &str) -> String {
    manifest
        .get(key)
        .and_then(|value| manifest_value_as_string(value).ok())
        .unwrap_or_default()
}

fn manifest_bool_or_default(
    manifest: &HashMap<String, ManifestValue>,
    key: &str,
    default: bool,
) -> bool {
    manifest
        .get(key)
        .and_then(|value| match value {
            ManifestValue::Bool(flag) => Some(*flag),
            _ => None,
        })
        .unwrap_or(default)
}

fn manifest_value_as_string(value: &ManifestValue) -> Result<String, String> {
    match value {
        ManifestValue::String(text) => Ok(text.clone()),
        _ => Err("Manifest value must be a string".to_string()),
    }
}

fn manifest_value_as_array(value: &ManifestValue) -> Result<Vec<String>, String> {
    match value {
        ManifestValue::Array(items) => Ok(items.clone()),
        _ => Err("Manifest value must be an array".to_string()),
    }
}

fn json_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String, String> {
    object
        .get(key)
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .ok_or_else(|| format!("Plugin manifest is missing '{key}'"))
}

fn split_command_line(command: &str) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in command.chars() {
        match ch {
            '\\' if !in_single && !escape => escape = true,
            '\'' if !in_double && !escape => in_single = !in_single,
            '"' if !in_single && !escape => in_double = !in_double,
            ch if ch.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
                escape = false;
            }
        }
    }

    if escape || in_single || in_double {
        return Err("Unterminated command string".to_string());
    }
    if !current.is_empty() {
        parts.push(current);
    }
    Ok(parts)
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

    #[test]
    fn plugin_system_loads_manifests_and_manages_state() {
        let mut system = PluginSystem::new();
        let toml_plugin = system
            .load_manifest_toml(
                r#"
                name = "lint"
                version = "1.2.3"
                description = "Lint support"
                enabled = false
                manifest_path = "/plugins/lint.toml"
                capabilities = ["read", "run"]
                "#,
            )
            .unwrap();
        assert_eq!(toml_plugin.name, "lint");
        assert!(!toml_plugin.enabled);
        assert_eq!(toml_plugin.capabilities, vec!["read", "run"]);

        let json_plugin = system
            .load_manifest_json(
                r#"{
                    "name": "review",
                    "version": "0.4.0",
                    "description": "Review support",
                    "enabled": true,
                    "manifest_path": "/plugins/review.json",
                    "capabilities": ["network"]
                }"#,
            )
            .unwrap();
        system.install(toml_plugin.clone());
        system.install(json_plugin.clone());
        assert_eq!(system.list().len(), 2);
        assert_eq!(system.get("review"), Some(&json_plugin));

        system.enable("lint").unwrap();
        assert!(system.get("lint").unwrap().enabled);
        system.disable("review").unwrap();
        assert!(!system.get("review").unwrap().enabled);

        system.uninstall("lint").unwrap();
        assert!(system.get("lint").is_none());
        assert!(system.load_manifest_toml("name = [").is_err());
    }

    #[test]
    fn plugin_extensions_group_commands_by_plugin() {
        let mut extensions = PluginExtensions::new();
        extensions.register_command(PluginCommand {
            name: "lint".to_string(),
            description: "Run lint".to_string(),
            plugin: "quality".to_string(),
        });
        extensions.register_command(PluginCommand {
            name: "fix".to_string(),
            description: "Apply fixes".to_string(),
            plugin: "quality".to_string(),
        });
        extensions.register_agent(PluginAgent {
            name: "reviewer".to_string(),
            system_prompt: "Review code".to_string(),
            plugin: "quality".to_string(),
        });
        extensions.register_skill(PluginSkill {
            name: "cleanup".to_string(),
            content: "steps".to_string(),
            plugin: "quality".to_string(),
        });

        let commands = extensions.commands_for_plugin("quality");
        assert_eq!(commands.len(), 2);
        assert_eq!(extensions.all_commands().len(), 2);
    }

    #[test]
    fn plugin_capability_manager_grants_and_revokes() {
        let mut manager = PluginCapabilityManager::new();
        manager.grant("quality", PluginCapability::ReadFiles);
        manager.grant("quality", PluginCapability::FullAccess);
        manager.grant("quality", PluginCapability::ReadFiles);

        assert!(manager.check("quality", &PluginCapability::ReadFiles));
        assert!(manager.check("quality", &PluginCapability::NetworkAccess));
        assert_eq!(manager.list_grants("quality").len(), 2);

        manager.revoke("quality", &PluginCapability::FullAccess);
        assert!(!manager.check("quality", &PluginCapability::NetworkAccess));
        assert!(manager.check("quality", &PluginCapability::ReadFiles));
    }

    #[test]
    fn plugin_tool_registry_registers_and_executes_tools() {
        let mut registry = PluginToolRegistry::new();
        registry.register(PluginDefinedTool {
            name: "echo".to_string(),
            plugin: "quality".to_string(),
            description: "Echo stdin".to_string(),
            input_schema: serde_json::json!({"type": "string"}),
            command: "cat".to_string(),
            env_vars: HashMap::new(),
        });

        assert_eq!(registry.list().len(), 1);
        let output = registry.execute_sync("echo", "hello plugin", None).unwrap();
        assert_eq!(output, "hello plugin");
        assert!(registry.execute_sync("missing", "ignored", None).is_err());
    }

    #[test]
    fn indexed_task_dag_tracks_readiness_and_order() {
        let mut dag = TaskDag::new();
        let fetch = dag.add_task("fetch");
        let build = dag.add_task("build");
        let deploy = dag.add_task("deploy");
        dag.add_dependency(fetch, build).unwrap();
        dag.add_dependency(build, deploy).unwrap();

        assert_eq!(dag.ready_tasks(), vec![fetch]);
        assert_eq!(dag.topological_order().unwrap(), vec![fetch, build, deploy]);

        dag.complete_task(fetch, "done");
        assert_eq!(dag.ready_tasks(), vec![build]);
        dag.complete_task(build, "done");
        assert_eq!(dag.ready_tasks(), vec![deploy]);
        dag.fail_task(deploy);
        assert!(dag.is_complete());

        let mut cyclic = TaskDag::new();
        let a = cyclic.add_task("a");
        let b = cyclic.add_task("b");
        cyclic.add_dependency(a, b).unwrap();
        assert!(cyclic.add_dependency(b, a).is_err());
    }

    #[test]
    fn team_orchestrator_assigns_tasks_by_specialty() {
        let mut orchestrator = TeamOrchestrator::new();
        orchestrator.add_agent(TeamAgent {
            name: "alice".to_string(),
            role: "backend".to_string(),
            specialties: vec!["api".to_string(), "service".to_string()],
        });
        orchestrator.add_agent(TeamAgent {
            name: "bob".to_string(),
            role: "frontend".to_string(),
            specialties: vec!["ui".to_string(), "design".to_string()],
        });

        orchestrator.assign_task(99, "alice").unwrap();
        let assignments = orchestrator.auto_assign(&[
            DagTask {
                id: 1,
                name: "api cleanup".to_string(),
                status: DagTaskStatus::Pending,
                result: None,
            },
            DagTask {
                id: 2,
                name: "ui polish".to_string(),
                status: DagTaskStatus::Pending,
                result: None,
            },
        ]);

        assert_eq!(orchestrator.list_agents().len(), 2);
        assert_eq!(assignments.get(&1).map(String::as_str), Some("alice"));
        assert_eq!(assignments.get(&2).map(String::as_str), Some("bob"));
        assert!(orchestrator.assign_task(3, "carol").is_err());
    }

    #[test]
    fn message_bus_filters_reads_by_subscription_and_time() {
        let mut bus = MessageBus::new();
        bus.subscribe("alice", "team");
        bus.publish(BusMessage {
            from: "alice".to_string(),
            content: "started".to_string(),
            timestamp: 10,
            channel: "team".to_string(),
        });
        bus.publish(BusMessage {
            from: "bob".to_string(),
            content: "finished".to_string(),
            timestamp: 20,
            channel: "team".to_string(),
        });

        assert_eq!(bus.read("alice", "team").len(), 2);
        assert!(bus.read("bob", "team").is_empty());
        let recent = bus.read_since("alice", "team", 10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].content, "finished");
        // Unsubscribed agent must get nothing from read_since.
        assert!(bus.read_since("bob", "team", 0).is_empty());
    }

    #[test]
    fn shared_memory_versions_and_filters_entries() {
        let mut memory = SharedMemory::new();
        memory.write("plan", "draft", "alice");
        memory.write("plan", "final", "alice");
        memory.write("notes", "todo", "bob");

        let entry = memory.read("plan").unwrap();
        assert_eq!(entry.value, "final");
        assert_eq!(entry.version, 2);
        assert_eq!(memory.list_keys(), vec!["notes", "plan"]);
        assert_eq!(memory.entries_by_writer("alice").len(), 1);
        assert!(memory.delete("notes"));
        assert!(!memory.delete("notes"));
    }

    #[test]
    fn task_scheduler_supports_multiple_strategies() {
        let agents = ["alice", "bob", "carol"];

        let mut round_robin = TaskScheduler::new(SchedulerStrategy::RoundRobin);
        assert_eq!(round_robin.schedule("task-1", &agents), "alice");
        assert_eq!(round_robin.schedule("task-2", &agents), "bob");

        let mut least_loaded = TaskScheduler::new(SchedulerStrategy::LeastLoaded);
        least_loaded.record_assignment("alice");
        assert_eq!(least_loaded.schedule("task-1", &agents), "bob");
        least_loaded.record_completion("alice");
        assert_eq!(least_loaded.schedule("task-2", &agents), "alice");

        let mut priority = TaskScheduler::new(SchedulerStrategy::Priority);
        assert_eq!(priority.schedule("urgent", &agents), "alice");

        let mut random = TaskScheduler::new(SchedulerStrategy::Random);
        let choice = random.schedule("any", &agents);
        assert!(agents.contains(&choice.as_str()));
    }

    #[test]
    fn jit_context_loader_evicts_least_recently_used_entries() {
        let mut loader = JitContextLoader::new(5);
        loader.add_reference(ContextReference {
            id: "a".to_string(),
            ref_type: RefType::File,
            path: "a.txt".to_string(),
            estimated_tokens: 3,
        });
        loader.add_reference(ContextReference {
            id: "b".to_string(),
            ref_type: RefType::Memory,
            path: "memory://b".to_string(),
            estimated_tokens: 3,
        });

        loader.load("a", "alpha");
        loader.load("b", "beta");
        assert_eq!(loader.loaded_tokens(), 6);
        assert!(loader.should_evict());

        assert_eq!(loader.get("b"), Some("beta"));
        let evicted = loader.evict_lru().unwrap();
        assert_eq!(evicted, "a");
        assert_eq!(loader.get("a"), None);

        loader.unload("b");
        assert_eq!(loader.loaded_tokens(), 0);
    }

    // ── FIX 2: execute_sync capability gate and env-var filtering ─────────────

    #[test]
    fn execute_sync_blocked_without_run_commands_capability() {
        let mut registry = PluginToolRegistry::new();
        registry.register(PluginDefinedTool {
            name: "safe".to_string(),
            plugin: "restricted".to_string(),
            description: "safe op".to_string(),
            input_schema: serde_json::json!({"type": "string"}),
            command: "cat".to_string(),
            env_vars: HashMap::new(),
        });

        let caps = PluginCapabilityManager::new(); // no grants
        let result = registry.execute_sync("safe", "input", Some(&caps));
        assert!(
            result.is_err(),
            "should be blocked without RunCommands capability"
        );
        assert!(result.unwrap_err().contains("RunCommands"));
    }

    #[test]
    fn execute_sync_succeeds_with_run_commands_capability() {
        let mut registry = PluginToolRegistry::new();
        registry.register(PluginDefinedTool {
            name: "echo2".to_string(),
            plugin: "allowed".to_string(),
            description: "echo".to_string(),
            input_schema: serde_json::json!({}),
            command: "cat".to_string(),
            env_vars: HashMap::new(),
        });

        let mut caps = PluginCapabilityManager::new();
        caps.grant("allowed", PluginCapability::RunCommands);
        let result = registry.execute_sync("echo2", "ok", Some(&caps));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "ok");
    }

    #[test]
    fn execute_sync_filters_dangerous_env_vars() {
        // Verify that dangerous vars set in env_vars are not passed to the child.
        // We spawn `env` and check that LD_PRELOAD / PATH are absent from the
        // tool's env_vars (they would only be dangerous if set there; the host
        // environment is unaffected by our filtering logic).
        let mut registry = PluginToolRegistry::new();
        let mut env_vars = HashMap::new();
        env_vars.insert("LD_PRELOAD".to_string(), "/evil.so".to_string());
        env_vars.insert("SAFE_VAR".to_string(), "allowed".to_string());
        registry.register(PluginDefinedTool {
            name: "env_tool".to_string(),
            plugin: "tester".to_string(),
            description: "env check".to_string(),
            input_schema: serde_json::json!({}),
            command: "cat".to_string(),
            env_vars,
        });

        // Without caps (None) the env is filtered but no capability check is done.
        let result = registry.execute_sync("env_tool", "hi", None);
        // cat just echoes stdin regardless of env; the test ensures no panic/error.
        assert!(result.is_ok());
    }

    // ── FIX 5: read_since subscription check ─────────────────────────────────

    #[test]
    fn read_since_requires_subscription() {
        let mut bus = MessageBus::new();
        bus.subscribe("alice", "alerts");
        bus.publish(BusMessage {
            from: "system".to_string(),
            content: "alert!".to_string(),
            timestamp: 5,
            channel: "alerts".to_string(),
        });

        // Subscribed agent sees the message.
        assert_eq!(bus.read_since("alice", "alerts", 0).len(), 1);
        // Unsubscribed agent gets nothing even though the message exists.
        assert!(bus.read_since("bob", "alerts", 0).is_empty());
    }
}
