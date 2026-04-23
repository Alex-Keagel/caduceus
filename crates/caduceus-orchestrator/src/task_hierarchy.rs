//! Unlimited task hierarchy (HierarchicalTask + TaskTree).
//!
//! Extracted from `lib.rs` — see ST-B1 Wave 0b.

use std::collections::HashMap;

// ── #239: Unlimited Task Hierarchy ───────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HierarchicalTask {
    pub id: usize,
    pub title: String,
    pub parent_id: Option<usize>,
    pub status: String,
    pub priority: u8,
    pub complexity: u8,
    pub estimated_hours: f64,
    pub actual_hours: f64,
    pub tags: Vec<String>,
    pub level: usize,
}

pub struct TaskTree {
    pub(crate) tasks: HashMap<usize, HierarchicalTask>,
    next_id: usize,
}

impl TaskTree {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            next_id: 0,
        }
    }

    pub fn add_task(&mut self, title: &str, parent_id: Option<usize>) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let level = parent_id.map_or(0, |p| self.depth(p) + 1);
        self.tasks.insert(
            id,
            HierarchicalTask {
                id,
                title: title.to_string(),
                parent_id,
                status: "pending".to_string(),
                priority: 5,
                complexity: 5,
                estimated_hours: 1.0,
                actual_hours: 0.0,
                tags: Vec::new(),
                level,
            },
        );
        id
    }

    pub fn get_task(&self, id: usize) -> Option<&HierarchicalTask> {
        self.tasks.get(&id)
    }

    pub fn children(&self, id: usize) -> Vec<&HierarchicalTask> {
        let mut ch: Vec<&HierarchicalTask> = self
            .tasks
            .values()
            .filter(|t| t.parent_id == Some(id))
            .collect();
        ch.sort_by_key(|t| t.id);
        ch
    }

    /// All descendants of `id`, depth-first.
    pub fn subtree(&self, id: usize) -> Vec<&HierarchicalTask> {
        let mut result = Vec::new();
        for child in self.children(id) {
            result.push(child);
            result.extend(self.subtree(child.id));
        }
        result
    }

    /// Number of ancestors (root = 0).
    pub fn depth(&self, id: usize) -> usize {
        let mut depth = 0;
        let mut current = id;
        while let Some(parent) = self.tasks.get(&current).and_then(|t| t.parent_id) {
            depth += 1;
            current = parent;
        }
        depth
    }

    /// Percentage of immediate children with status `"done"`.
    /// Leaf tasks with `status == "done"` return 100.0, otherwise 0.0.
    pub fn progress(&self, id: usize) -> f64 {
        let ch = self.children(id);
        if ch.is_empty() {
            return if self.tasks.get(&id).is_some_and(|t| t.status == "done") {
                100.0
            } else {
                0.0
            };
        }
        let done = ch.iter().filter(|c| c.status == "done").count();
        done as f64 / ch.len() as f64 * 100.0
    }

    /// Visual tree with indentation.
    pub fn to_tree_string(&self) -> String {
        let mut output = String::new();
        let mut roots: Vec<&HierarchicalTask> = self
            .tasks
            .values()
            .filter(|t| t.parent_id.is_none())
            .collect();
        roots.sort_by_key(|t| t.id);
        for root in roots {
            self.write_node(&mut output, root, 0);
        }
        output
    }

    fn write_node(&self, output: &mut String, task: &HierarchicalTask, depth: usize) {
        let indent = "  ".repeat(depth);
        output.push_str(&format!("{indent}- [{}] {}\n", task.status, task.title));
        for child in self.children(task.id) {
            self.write_node(output, child, depth + 1);
        }
    }
}

impl Default for TaskTree {
    fn default() -> Self {
        Self::new()
    }
}

