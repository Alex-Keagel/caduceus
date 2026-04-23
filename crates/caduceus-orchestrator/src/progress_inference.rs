//! Progress inference from title/description heuristics.
//!
//! Extracted from `lib.rs` — see ST-B1 Wave 0b.

// ── #246: Progress Inference ──────────────────────────────────────────────────

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct InferredProgress {
    pub task_id: usize,
    pub percentage: f64,
    pub confidence: f64,
    pub evidence: Vec<String>,
}

pub struct ProgressInferrer;

impl ProgressInferrer {
    /// Estimate progress from git commit messages referencing a task title.
    pub fn infer_from_commits(task_title: &str, commit_messages: &[String]) -> InferredProgress {
        if commit_messages.is_empty() {
            return InferredProgress {
                task_id: 0,
                percentage: 0.0,
                confidence: 0.0,
                evidence: Vec::new(),
            };
        }
        let title_lower = task_title.to_lowercase();
        let title_words: Vec<&str> = title_lower.split_whitespace().collect();
        let done_kws = [
            "done",
            "complete",
            "finish",
            "implement",
            "close",
            "resolve",
        ];

        let mut evidence = Vec::new();
        let mut matching = 0usize;
        let mut completion_hints = 0usize;

        for msg in commit_messages {
            let lower = msg.to_lowercase();
            let relevant = title_words.iter().any(|w| lower.contains(*w));
            if relevant {
                matching += 1;
                evidence.push(msg.clone());
                if done_kws.iter().any(|kw| lower.contains(kw)) {
                    completion_hints += 1;
                }
            }
        }

        let confidence = matching as f64 / commit_messages.len() as f64;
        let percentage = if matching == 0 {
            0.0
        } else {
            completion_hints as f64 / matching as f64 * 100.0
        };

        InferredProgress {
            task_id: 0,
            percentage,
            confidence,
            evidence,
        }
    }

    /// Progress from test suite pass rate (0–100).
    pub fn infer_from_tests(total: usize, passing: usize) -> f64 {
        if total == 0 {
            return 0.0;
        }
        (passing as f64 / total as f64 * 100.0).min(100.0)
    }

    /// Progress from file creation ratio (0–100).
    pub fn infer_from_files(files_planned: usize, files_created: usize) -> f64 {
        if files_planned == 0 {
            return 0.0;
        }
        (files_created as f64 / files_planned as f64 * 100.0).min(100.0)
    }

    /// Weighted average: 40% commits, 40% tests, 20% files.
    pub fn combined(commits: f64, tests: f64, files: f64) -> f64 {
        (0.4 * commits + 0.4 * tests + 0.2 * files).min(100.0)
    }
}

