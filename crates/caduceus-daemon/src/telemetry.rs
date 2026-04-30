//! Structured logging + lightweight metrics.
//!
//! Per the implementation DAG (todo `f08-logging-telemetry`), this module
//! exposes (a) a single `init_tracing()` entry point that the binary calls
//! at startup and (b) a small `Counter` primitive subsystems use to
//! register named metrics that the snapshot RPC can later surface.
//!
//! The full metrics story (Prometheus / OpenTelemetry exporters, dashboards)
//! lands in `ops02-observability-dashboards`.  Here we ship the minimum the
//! orchestrator needs: lock-free atomic counters, name-keyed lookup, and
//! a snapshot iteration.
//!
//! Tracing fields (canonical):
//!
//! | Field            | Spec ref                           | Type   |
//! |------------------|------------------------------------|--------|
//! | `run_id`         | spec #1 §3.0                       | string |
//! | `runner_seq`     | spec #2 §4.4 Z-23                  | u64    |
//! | `attempt`        | spec #1 §3.0 RunAttempt            | u32    |
//! | `workspace_id`   | spec #3 I-6                        | string |
//! | `slug`           | spec #3 §3.1                       | string |
//! | `stream_seq`     | spec #4 §3.4                       | u64    |
//! | `fingerprint`    | spec #4 I-7                        | string |

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

/// Initialize tracing-subscriber from the `CADUCEUSD_LOG` env filter.
/// Falls back to `info` if unset or invalid.  Idempotent (subsequent calls
/// are no-ops, useful for tests that may re-init).
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let filter =
            EnvFilter::try_from_env("CADUCEUSD_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
        // Use try_init so concurrent test bins don't panic.
        let _ = fmt()
            .with_env_filter(filter)
            .with_target(false)
            .compact()
            .try_init();
    });
}

/// Lock-free atomic counter, used as the building block for daemon metrics.
///
/// Spec context: this is the type underlying livelock-guard fire counts
/// (spec #1 Z-9), `signal_error` / `reap_timeout` counts (spec #2 §3.3 +
/// iter-28 #2-2), `protocol_violation` counts (spec #2 §4.1), and
/// snapshot fingerprint churn (spec #4 §4.6).
#[derive(Debug, Default)]
pub struct Counter {
    inner: AtomicU64,
}

impl Counter {
    pub const fn new() -> Self {
        Self {
            inner: AtomicU64::new(0),
        }
    }

    #[inline]
    pub fn incr(&self) {
        self.inner.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn add(&self, n: u64) {
        self.inner.fetch_add(n, Ordering::Relaxed);
    }

    #[inline]
    pub fn get(&self) -> u64 {
        self.inner.load(Ordering::Relaxed)
    }
}

/// Process-wide registry of named counters.  Cheap to clone (`Arc`).
///
/// Subsystems register at startup via `Metrics::counter(name)` and stash
/// the returned handle locally.  The snapshot RPC reads via `snapshot()`.
#[derive(Debug, Default, Clone)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

#[derive(Debug, Default)]
struct MetricsInner {
    counters: RwLock<BTreeMap<String, Arc<Counter>>>,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get-or-insert a counter by name.  Subsequent calls return the
    /// same handle — counters are never replaced once registered.
    pub fn counter(&self, name: &str) -> Arc<Counter> {
        // Fast path: existing counter, read lock only.
        {
            let g = self.inner.counters.read().unwrap();
            if let Some(c) = g.get(name) {
                return Arc::clone(c);
            }
        }
        // Slow path: insert under write lock; re-check to dedupe with a
        // racing inserter.
        let mut g = self.inner.counters.write().unwrap();
        if let Some(c) = g.get(name) {
            return Arc::clone(c);
        }
        let c = Arc::new(Counter::new());
        g.insert(name.to_string(), Arc::clone(&c));
        c
    }

    /// Snapshot all counter (name, value) pairs.  Sorted by name for
    /// deterministic output.  Used by the snapshot RPC and ops dashboards.
    pub fn snapshot(&self) -> Vec<(String, u64)> {
        let g = self.inner.counters.read().unwrap();
        g.iter().map(|(name, c)| (name.clone(), c.get())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn init_tracing_is_idempotent() {
        // Calling twice must not panic.
        init_tracing();
        init_tracing();
    }

    #[test]
    fn counter_starts_at_zero() {
        let c = Counter::new();
        assert_eq!(c.get(), 0);
    }

    #[test]
    fn counter_incr_increments_by_one() {
        let c = Counter::new();
        c.incr();
        c.incr();
        c.incr();
        assert_eq!(c.get(), 3);
    }

    #[test]
    fn counter_add_takes_arbitrary_value() {
        let c = Counter::new();
        c.add(100);
        c.add(50);
        assert_eq!(c.get(), 150);
    }

    #[test]
    fn counter_concurrent_increments_are_atomic() {
        let c = Arc::new(Counter::new());
        let mut handles = Vec::new();
        for _ in 0..16 {
            let c = Arc::clone(&c);
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    c.incr();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(c.get(), 16_000);
    }

    #[test]
    fn metrics_dedupes_counter_lookups() {
        let m = Metrics::new();
        let c1 = m.counter("dispatch.attempts");
        let c2 = m.counter("dispatch.attempts");
        // Same Arc — increments via either handle visible through the other.
        c1.incr();
        assert_eq!(c2.get(), 1);
    }

    #[test]
    fn metrics_snapshot_is_sorted_and_complete() {
        let m = Metrics::new();
        m.counter("z.last").add(3);
        m.counter("a.first").add(1);
        m.counter("m.middle").add(2);
        let snap = m.snapshot();
        let names: Vec<&str> = snap.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a.first", "m.middle", "z.last"]);
        let values: Vec<u64> = snap.iter().map(|(_, v)| *v).collect();
        assert_eq!(values, [1, 2, 3]);
    }

    #[test]
    fn metrics_concurrent_register_and_increment() {
        // Spec #1 Z-9 livelock-guard fires concurrently with dispatch
        // increments; the registry MUST be safe under that contention.
        let m = Metrics::new();
        let mut handles = Vec::new();
        for i in 0..8 {
            let m = m.clone();
            handles.push(thread::spawn(move || {
                let c = m.counter(&format!("counter.{i}"));
                for _ in 0..100 {
                    c.incr();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let snap = m.snapshot();
        assert_eq!(snap.len(), 8);
        for (_, v) in snap {
            assert_eq!(v, 100);
        }
    }
}
