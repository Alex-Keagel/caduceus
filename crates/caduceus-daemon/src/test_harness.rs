//! Test harness primitives.
//!
//! Per the implementation DAG (todo `f09-test-harness`), this module
//! provides a single `TestEnv` entry point for unit and integration tests.
//! It bundles a virtual clock, a temporary directory, and a fresh metrics
//! registry so individual tests don't have to wire those by hand.
//!
//! NOT a runtime dependency.  The whole module is gated to `#[cfg(any(test, feature = "test-harness"))]`
//! so production builds never link the temp-dir crate transitively.
//!
//! Spec context:
//!
//! - Tests that touch the registry (spec #3 §4) want a fresh
//!   `workspace_root` per test to avoid lock contention on `.caduceusd.lock`.
//! - Timer tests (spec #1 §3.5 retry, §8.7 disconnect; spec #2 §4.1
//!   heartbeat-timeout) need a virtual clock.
//! - Snapshot fingerprint tests want a deterministic wall clock.

#![cfg(any(test, feature = "test-harness"))]

use crate::{
    clock::{Clock, SharedClock, VirtualClock},
    telemetry::Metrics,
    Lifecycle,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

/// Bundled test fixture.  Drop to release the temp directory.
pub struct TestEnv {
    pub clock: VirtualClock,
    pub metrics: Metrics,
    pub lifecycle: Lifecycle,
    /// Working directory; usable as `workspace_root` for spec #3 tests.
    pub workspace_root: PathBuf,
    _tempdir: tempfile::TempDir,
}

impl TestEnv {
    /// Construct a fresh test environment with a frozen virtual clock and
    /// a unique temp directory.  Lifecycle starts in `Booting`.
    pub fn new() -> Self {
        let tempdir = tempfile::TempDir::new().expect("create temp dir for test env");
        let workspace_root = tempdir.path().to_path_buf();
        Self {
            clock: VirtualClock::new(Instant::now(), SystemTime::UNIX_EPOCH),
            metrics: Metrics::new(),
            lifecycle: Lifecycle::new(),
            workspace_root,
            _tempdir: tempdir,
        }
    }

    /// Return the clock as a shared trait object handle.
    pub fn shared_clock(&self) -> SharedClock {
        Arc::new(self.clock.clone())
    }

    /// Make the env "ready" — equivalent to `Lifecycle::mark_ready()`.
    pub fn mark_ready(&self) -> bool {
        self.lifecycle.mark_ready()
    }

    /// Path under the test workspace_root.  Useful for spec #3 fixtures.
    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.workspace_root.join(relative)
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_env_starts_with_frozen_clock_and_booting_lifecycle() {
        let env = TestEnv::new();
        let t0 = env.clock.now_monotonic();
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(
            env.clock.now_monotonic(),
            t0,
            "virtual clock must be frozen"
        );
        assert!(!env.lifecycle.is_ready());
    }

    #[test]
    fn test_env_workspace_root_exists_and_is_unique() {
        let env1 = TestEnv::new();
        let env2 = TestEnv::new();
        assert!(env1.workspace_root.exists());
        assert!(env2.workspace_root.exists());
        assert_ne!(env1.workspace_root, env2.workspace_root);
    }

    #[test]
    fn test_env_workspace_root_dropped_on_env_drop() {
        let path = {
            let env = TestEnv::new();
            env.workspace_root.clone()
        };
        // After drop, the temp directory is removed.
        assert!(!path.exists());
    }

    #[test]
    fn test_env_path_joins_relative() {
        let env = TestEnv::new();
        let p = env.path("registry/rows.db");
        assert!(p.starts_with(&env.workspace_root));
        assert!(p.ends_with("registry/rows.db"));
    }

    #[test]
    fn test_env_metrics_is_isolated_per_env() {
        let env1 = TestEnv::new();
        let env2 = TestEnv::new();
        env1.metrics.counter("x").incr();
        // env2 has its own registry — counter "x" reads zero.
        assert_eq!(env2.metrics.counter("x").get(), 0);
        assert_eq!(env1.metrics.counter("x").get(), 1);
    }

    #[test]
    fn test_env_mark_ready_transitions_lifecycle() {
        let env = TestEnv::new();
        assert!(env.mark_ready());
        assert!(env.lifecycle.is_ready());
    }

    #[test]
    fn test_env_shared_clock_observes_advance() {
        let env = TestEnv::new();
        let sc = env.shared_clock();
        let t0 = sc.now_monotonic();
        env.clock.advance(Duration::from_millis(250));
        assert_eq!(sc.now_monotonic() - t0, Duration::from_millis(250));
    }
}
