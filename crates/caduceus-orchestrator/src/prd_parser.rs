//! PRD (Product Requirements Document) parser.
//!
//! Extracted from `lib.rs` — see ST-B1 Wave 0b.

// ── #236: PRD Parser ─────────────────────────────────────────────────────────

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct PrdTask {
    pub id: usize,
    pub title: String,
    pub description: String,
    pub parent_id: Option<usize>,
    pub priority: u8,
    pub complexity: u8,
    pub estimated_hours: f64,
    pub dependencies: Vec<usize>,
    pub tags: Vec<String>,
}

pub struct PrdParser;

impl PrdParser {
    /// Return (heading, content) pairs for every markdown section.
    pub fn extract_sections(text: &str) -> Vec<(String, String)> {
        let mut sections: Vec<(String, String)> = Vec::new();
        let mut current_title: Option<String> = None;
        let mut buf = String::new();

        for line in text.lines() {
            if line.starts_with('#') {
                if let Some(title) = current_title.take() {
                    sections.push((title, buf.trim().to_string()));
                    buf.clear();
                }
                let title = line.trim_start_matches('#').trim().to_string();
                if !title.is_empty() {
                    current_title = Some(title);
                }
            } else if current_title.is_some() {
                buf.push_str(line);
                buf.push('\n');
            }
        }
        if let Some(title) = current_title {
            sections.push((title, buf.trim().to_string()));
        }
        sections
    }

    /// Parse a markdown PRD document into a flat list of `PrdTask`s.
    pub fn parse(prd_text: &str) -> Vec<PrdTask> {
        // Collect (level, title, content) triples.
        let mut triples: Vec<(usize, String, String)> = Vec::new();
        let mut current: Option<(usize, String)> = None;
        let mut buf = String::new();

        for line in prd_text.lines() {
            if line.starts_with('#') {
                if let Some((lvl, title)) = current.take() {
                    triples.push((lvl, title, buf.trim().to_string()));
                    buf.clear();
                }
                let level = line.chars().take_while(|&c| c == '#').count();
                let title = line[level..].trim().to_string();
                if !title.is_empty() {
                    current = Some((level, title));
                }
            } else if current.is_some() {
                buf.push_str(line);
                buf.push('\n');
            }
        }
        if let Some((lvl, title)) = current {
            triples.push((lvl, title, buf.trim().to_string()));
        }

        // Build tasks with parent tracking via a stack of (task_id, heading_level).
        let mut tasks: Vec<PrdTask> = Vec::new();
        let mut parent_stack: Vec<(usize, usize)> = Vec::new();

        for (id, (level, title, content)) in triples.into_iter().enumerate() {
            while parent_stack.last().is_some_and(|&(_, l)| l >= level) {
                parent_stack.pop();
            }
            let parent_id = parent_stack.last().map(|&(pid, _)| pid);
            let priority = Self::extract_priority(&content);
            let complexity = Self::extract_complexity(&content);
            let estimated_hours = Self::extract_hours(&content);
            let tags = Self::extract_tags(&content);

            tasks.push(PrdTask {
                id,
                title,
                description: content,
                parent_id,
                priority,
                complexity,
                estimated_hours,
                dependencies: Vec::new(),
                tags,
            });
            parent_stack.push((id, level));
        }
        tasks
    }

    /// Infer dependency edges from keyword references between task descriptions.
    /// Returns pairs `(dependent_id, dependency_id)`.
    pub fn infer_dependencies(tasks: &[PrdTask]) -> Vec<(usize, usize)> {
        let mut deps = Vec::new();
        for task in tasks {
            for other in tasks {
                if other.id == task.id {
                    continue;
                }
                if task
                    .description
                    .to_lowercase()
                    .contains(&other.title.to_lowercase())
                {
                    deps.push((task.id, other.id));
                }
            }
        }
        deps
    }

    fn extract_priority(text: &str) -> u8 {
        let lower = text.to_lowercase();
        if lower.contains("priority: high") || lower.contains("priority:high") {
            8
        } else if lower.contains("priority: low") || lower.contains("priority:low") {
            2
        } else {
            5
        }
    }

    fn extract_complexity(text: &str) -> u8 {
        let lower = text.to_lowercase();
        if lower.contains("complexity: high") || lower.contains("complexity:high") {
            8
        } else if lower.contains("complexity: low") || lower.contains("complexity:low") {
            2
        } else {
            5
        }
    }

    fn extract_hours(text: &str) -> f64 {
        for word in text.split_whitespace() {
            let stripped = word.trim_end_matches('h');
            if let Ok(h) = stripped.parse::<f64>() {
                if h > 0.0 && h < 1000.0 {
                    return h;
                }
            }
        }
        1.0
    }

    fn extract_tags(text: &str) -> Vec<String> {
        text.split_whitespace()
            .filter(|w| w.starts_with('#'))
            .map(|w| w.trim_start_matches('#').to_string())
            .collect()
    }
}

