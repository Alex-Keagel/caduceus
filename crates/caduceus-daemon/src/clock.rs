//! Clock + timer abstraction.
//!
//! Per the implementation DAG (todo `f04-clock-abstraction`), this module
//! provides a `Clock` trait that subsystems hold by reference rather than
//! calling `std::time::Instant::now()` / `SystemTime::now()` directly.
//! The split lets tests substitute a `VirtualClock` that does not advance
//! on its own, eliminating a major source of flakiness in retry-timer,
//! disconnect-timer, and heartbeat-timeout tests.
//!
//! Spec cross-references:
//!
//! - **`spec-caduceus-orchestrator-algorithm.md` §3.5** — `RetryToken` is
//!   NOT clock-derived; the clock here is for timer scheduling, not for
//!   identity.
//! - **`spec-caduceus-orchestrator-algorithm.md` §8.7** — disconnect timer
//!   uses monotonic time; wall clock is used only for diagnostic logs.
//! - **`spec-caduceus-agent-runner-contract.md` §4.1** — heartbeat-timeout
//!   tracker (iter-28 #2-3) requires monotonic deadlines.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Abstraction over time sources.  Implementations MUST be cheap to call;
/// daemon hot paths read the monotonic clock thousands of times per second.
pub trait Clock: Send + Sync + 'static {
    /// Read the monotonic clock.  Used for timer deadlines, heartbeat
    /// freshness, and any duration arithmetic.
    fn now_monotonic(&self) -> Instant;

    /// Read the wall clock.  Used for diagnostic logs and `created_at`
    /// timestamps in registry rows.
    fn now_wall(&self) -> SystemTime;
}

/// Real (system) clock.  Default for production; never advances under
/// test control.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealClock;

impl Clock for RealClock {
    #[inline]
    fn now_monotonic(&self) -> Instant {
        Instant::now()
    }

    #[inline]
    fn now_wall(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Test-controlled virtual clock.  Frozen at construction; advances only
/// via `advance()`.  Cheap to clone (`Arc<Mutex<...>>`).
#[derive(Debug, Clone)]
pub struct VirtualClock {
    inner: Arc<std::sync::Mutex<VirtualClockInner>>,
}

#[derive(Debug)]
struct VirtualClockInner {
    monotonic: Instant,
    wall: SystemTime,
}

impl VirtualClock {
    /// Construct a virtual clock anchored to a specific monotonic point
    /// (typically `Instant::now()` at test start) and a specific wall
    /// time (typically `UNIX_EPOCH + N years` for deterministic logs).
    pub fn new(anchor_monotonic: Instant, anchor_wall: SystemTime) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(VirtualClockInner {
                monotonic: anchor_monotonic,
                wall: anchor_wall,
            })),
        }
    }

    /// Convenience: anchor at `Instant::now()` and `UNIX_EPOCH`.  Useful
    /// for unit tests that don't care about the exact wall-time anchor.
    pub fn frozen_now() -> Self {
        Self::new(Instant::now(), SystemTime::UNIX_EPOCH)
    }

    /// Advance both monotonic and wall clocks by the same delta.
    pub fn advance(&self, delta: Duration) {
        let mut g = self.inner.lock().expect("VirtualClock mutex poisoned");
        g.monotonic += delta;
        g.wall += delta;
    }
}

impl Clock for VirtualClock {
    fn now_monotonic(&self) -> Instant {
        self.inner
            .lock()
            .expect("VirtualClock mutex poisoned")
            .monotonic
    }

    fn now_wall(&self) -> SystemTime {
        self.inner.lock().expect("VirtualClock mutex poisoned").wall
    }
}

/// Convenience type alias for a shared clock handle.  Subsystems hold
/// this type and call methods through the `Clock` trait.
pub type SharedClock = Arc<dyn Clock>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn real_clock_advances_naturally() {
        let c = RealClock;
        let t0 = c.now_monotonic();
        thread::sleep(Duration::from_millis(2));
        let t1 = c.now_monotonic();
        assert!(t1.duration_since(t0) >= Duration::from_millis(1));
    }

    #[test]
    fn real_clock_wall_is_after_unix_epoch() {
        let c = RealClock;
        assert!(c.now_wall().duration_since(SystemTime::UNIX_EPOCH).is_ok());
    }

    #[test]
    fn virtual_clock_does_not_advance_without_command() {
        let vc = VirtualClock::frozen_now();
        let t0 = vc.now_monotonic();
        thread::sleep(Duration::from_millis(2));
        let t1 = vc.now_monotonic();
        assert_eq!(
            t0, t1,
            "virtual clock MUST be frozen between advance() calls"
        );
    }

    #[test]
    fn virtual_clock_advance_increments_both_clocks() {
        let vc = VirtualClock::frozen_now();
        let m0 = vc.now_monotonic();
        let w0 = vc.now_wall();
        vc.advance(Duration::from_millis(500));
        assert_eq!(vc.now_monotonic() - m0, Duration::from_millis(500));
        assert_eq!(
            vc.now_wall().duration_since(w0).unwrap(),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn virtual_clock_clones_share_state() {
        let vc1 = VirtualClock::frozen_now();
        let vc2 = vc1.clone();
        let t0 = vc2.now_monotonic();
        vc1.advance(Duration::from_secs(10));
        assert_eq!(vc2.now_monotonic() - t0, Duration::from_secs(10));
    }

    #[test]
    fn shared_clock_is_object_safe() {
        let real: SharedClock = Arc::new(RealClock);
        let virt: SharedClock = Arc::new(VirtualClock::frozen_now());
        // Just exercise the trait object.
        let _ = real.now_monotonic();
        let _ = virt.now_monotonic();
    }

    #[test]
    fn virtual_clock_advance_is_thread_safe() {
        // Advances from multiple threads must be additive, no panics.
        let vc = VirtualClock::frozen_now();
        let t0 = vc.now_monotonic();
        let mut handles = Vec::new();
        for _ in 0..16 {
            let vc = vc.clone();
            handles.push(thread::spawn(move || {
                vc.advance(Duration::from_millis(10));
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(vc.now_monotonic() - t0, Duration::from_millis(160));
    }
}
