//! Time tracking (TimeEntry + TimeTracker).
//!
//! Extracted from `lib.rs` — see ST-B1 Wave 0b.

// ── #240: Time Tracking ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TimeEntry {
    pub task_id: usize,
    pub estimated_hours: f64,
    pub actual_hours: f64,
    pub started_at: u64,
    pub completed_at: Option<u64>,
}

#[derive(Default)]
pub struct TimeTracker {
    entries: Vec<TimeEntry>,
}

impl TimeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_task(&mut self, task_id: usize, estimated: f64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.entries.push(TimeEntry {
            task_id,
            estimated_hours: estimated,
            actual_hours: 0.0,
            started_at: now,
            completed_at: None,
        });
    }

    pub fn complete_task(&mut self, task_id: usize, actual: f64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Some(e) = self
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.task_id == task_id && e.completed_at.is_none())
        {
            e.actual_hours = actual;
            e.completed_at = Some(now);
        }
    }

    /// Ratio of total estimated to total actual for completed tasks.
    pub fn velocity(&self) -> f64 {
        let completed: Vec<&TimeEntry> = self
            .entries
            .iter()
            .filter(|e| e.completed_at.is_some() && e.actual_hours > 0.0)
            .collect();
        if completed.is_empty() {
            return 1.0;
        }
        let est: f64 = completed.iter().map(|e| e.estimated_hours).sum();
        let act: f64 = completed.iter().map(|e| e.actual_hours).sum();
        if act == 0.0 {
            1.0
        } else {
            est / act
        }
    }

    pub fn total_estimated(&self) -> f64 {
        self.entries.iter().map(|e| e.estimated_hours).sum()
    }

    pub fn total_actual(&self) -> f64 {
        self.entries.iter().map(|e| e.actual_hours).sum()
    }

    /// Tasks that are still running and have exceeded their estimate.
    pub fn overdue_tasks(&self) -> Vec<usize> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.entries
            .iter()
            .filter(|e| {
                e.completed_at.is_none()
                    && (now.saturating_sub(e.started_at)) as f64 / 3600.0 > e.estimated_hours
            })
            .map(|e| e.task_id)
            .collect()
    }
}
