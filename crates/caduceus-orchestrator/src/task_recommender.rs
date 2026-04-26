//! Smart task recommender.
//!
//! Extracted from `lib.rs` — see ST-B1 Wave 0b.

use crate::prd_parser::PrdTask;
use serde::Serialize;

// ── #237: Smart Task Recommender ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct TaskRecommendation {
    pub task_id: usize,
    pub score: f64,
    pub reason: String,
}

pub struct TaskRecommender;

impl TaskRecommender {
    /// Rank incomplete tasks by readiness, priority, and inverse complexity.
    pub fn recommend_next(tasks: &[PrdTask], completed: &[usize]) -> Vec<TaskRecommendation> {
        let mut recs: Vec<TaskRecommendation> = tasks
            .iter()
            .filter(|t| !completed.contains(&t.id))
            .map(|t| {
                let dep_s = Self::dependency_score(t, completed);
                let pri_s = Self::priority_score(t);
                let cmp_s = Self::complexity_score(t);
                let score = 0.4 * dep_s + 0.35 * pri_s + 0.25 * cmp_s;
                let reason =
                    format!("dep_ready={dep_s:.2} priority={pri_s:.2} complexity_inv={cmp_s:.2}");
                TaskRecommendation {
                    task_id: t.id,
                    score,
                    reason,
                }
            })
            .collect();

        recs.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        recs
    }

    fn dependency_score(task: &PrdTask, completed: &[usize]) -> f64 {
        if task.dependencies.is_empty() || task.dependencies.iter().all(|d| completed.contains(d)) {
            1.0
        } else {
            0.0
        }
    }

    fn priority_score(task: &PrdTask) -> f64 {
        f64::from(task.priority) / 10.0
    }

    fn complexity_score(task: &PrdTask) -> f64 {
        if task.complexity == 0 {
            1.0
        } else {
            1.0 / f64::from(task.complexity)
        }
    }
}
