//! Shared loop detector — canonical implementation used by both the
//! orchestrator (engine tool loop) and the caduceus_bridge (Zed safety layer).
//!
//! Semantics: fires `LoopDetected` when the same `(tool_name, input_hash)`
//! pair has been seen `threshold` times consecutively. Any change to either
//! the tool name or the input hash resets the counter.
//!
//! Prior to F2 there were two independent implementations with subtly
//! different semantics living in `caduceus-orchestrator::LoopDetector` and
//! `caduceus_bridge::safety::LoopDetector`. This is the single source of
//! truth.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopCheckResult {
    /// Tool call is allowed.
    Ok,
    /// Loop detected — contains the tool name that looped.
    LoopDetected(String),
}

#[derive(Debug, Clone)]
pub struct LoopDetector {
    current_key: Option<(String, u64)>,
    consecutive_count: usize,
    threshold: usize,
}

impl LoopDetector {
    pub fn new(threshold: usize) -> Self {
        assert!(threshold > 0, "LoopDetector threshold must be > 0");
        Self {
            current_key: None,
            consecutive_count: 0,
            threshold,
        }
    }

    fn hash_input(input: &str) -> u64 {
        let mut h = DefaultHasher::new();
        input.hash(&mut h);
        h.finish()
    }

    /// Record a tool call by name only (assumes empty input).
    /// Prefer `record_call(name, input)` for accurate loop detection.
    pub fn record_tool(&mut self, tool_name: &str) -> LoopCheckResult {
        self.record_call(tool_name, "")
    }

    /// Record a tool call with its serialized input. Returns `LoopDetected`
    /// only if the same `(tool_name, input)` pair has been seen `threshold`
    /// times consecutively.
    pub fn record_call(&mut self, tool_name: &str, input: &str) -> LoopCheckResult {
        let key = (tool_name.to_string(), Self::hash_input(input));

        let is_loop = match &self.current_key {
            Some(prev) => *prev == key && self.consecutive_count >= self.threshold,
            None => false,
        };

        if is_loop {
            self.reset();
            return LoopCheckResult::LoopDetected(tool_name.to_string());
        }

        match &self.current_key {
            Some(prev) if *prev == key => {
                self.consecutive_count += 1;
            }
            _ => {
                self.current_key = Some(key);
                self.consecutive_count = 1;
            }
        }

        LoopCheckResult::Ok
    }

    /// Reset the detector (called internally after loop detection).
    pub fn reset(&mut self) {
        self.current_key = None;
        self.consecutive_count = 0;
    }

    /// Current consecutive count for the active (tool, input) pair.
    pub fn consecutive_count(&self) -> usize {
        self.consecutive_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_below_threshold() {
        let mut ld = LoopDetector::new(3);
        assert_eq!(ld.record_tool("a"), LoopCheckResult::Ok);
        assert_eq!(ld.record_tool("a"), LoopCheckResult::Ok);
    }

    #[test]
    fn detects_at_threshold() {
        let mut ld = LoopDetector::new(3);
        assert_eq!(ld.record_tool("a"), LoopCheckResult::Ok);
        assert_eq!(ld.record_tool("a"), LoopCheckResult::Ok);
        assert_eq!(ld.record_tool("a"), LoopCheckResult::Ok);
        assert_eq!(
            ld.record_tool("a"),
            LoopCheckResult::LoopDetected("a".to_string())
        );
    }

    #[test]
    fn different_input_resets() {
        let mut ld = LoopDetector::new(2);
        ld.record_call("a", "{\"x\":1}");
        ld.record_call("a", "{\"x\":1}");
        // different input → reset
        assert_eq!(ld.record_call("a", "{\"x\":2}"), LoopCheckResult::Ok);
    }

    #[test]
    fn different_tool_resets() {
        let mut ld = LoopDetector::new(2);
        ld.record_tool("a");
        ld.record_tool("a");
        assert_eq!(ld.record_tool("b"), LoopCheckResult::Ok);
    }
}
