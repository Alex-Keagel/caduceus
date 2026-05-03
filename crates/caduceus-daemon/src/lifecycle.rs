//! Lifecycle state machine for `caduceusd`.
//!
//! Per the implementation DAG (todo `f01-daemon-scaffold`), this module owns
//! the canonical boot → ready → drain → halt FSM that the daemon's `main()`
//! drives.  All other subsystems (mailbox, IPC, dispatch loop) consume this
//! state via shared atomics; no subsystem maintains its own copy of "is the
//! daemon shutting down?".
//!
//! Spec cross-references:
//!
//! - **`spec-caduceus-orchestrator-algorithm.md` §3.1** — boot reconcile
//!   sweep MUST run BEFORE first dispatch tick.  This module exposes
//!   `LifecycleState::Booting` for the sweep window.
//! - **`spec-caduceus-orchestrator-algorithm.md` §3.5** — `on_shutdown`
//!   sets `state.shutting_down = true` (executable, not comment-only;
//!   iter-28 backlog #1 absorbed via `mark_draining`).
//! - **`spec-orchestrator-status-snapshot.md` §1.2** — snapshot RPC MUST
//!   reject with `SnapshotUnavailableReason::DaemonShuttingDown` once the
//!   FSM enters `Draining`.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// Lifecycle states.  Numbered for atomic representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LifecycleState {
    /// Process started; config loaded; boot reconcile sweep not yet run.
    Booting = 0,
    /// Boot reconcile complete; dispatch loop accepting new runs.
    Ready = 1,
    /// `on_shutdown` invoked; no new dispatches; running runners cascaded.
    Draining = 2,
    /// Drain complete; all runners reaped; final state persisted.
    Halted = 3,
}

impl LifecycleState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => LifecycleState::Booting,
            1 => LifecycleState::Ready,
            2 => LifecycleState::Draining,
            3 => LifecycleState::Halted,
            _ => unreachable!("invalid lifecycle state encoding: {v}"),
        }
    }
}

/// Reason the daemon is shutting down.  Carried in diagnostic logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// SIGTERM / SIGINT received.
    Signal,
    /// Supervisor-issued `Cmd::Shutdown`.
    Supervisor,
    /// Internal panic / unrecoverable invariant breach.
    InternalFailure,
}

/// Shared lifecycle handle.  Cheap to clone (`Arc<AtomicU8>`).
///
/// All subsystems hold a clone and read state via `state()`.  Only the
/// main daemon loop is permitted to call `mark_ready()` /
/// `mark_draining()` / `mark_halted()`.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    inner: Arc<AtomicU8>,
}

impl Lifecycle {
    /// Construct a new lifecycle handle in the `Booting` state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicU8::new(LifecycleState::Booting as u8)),
        }
    }

    /// Read the current lifecycle state.  Lock-free.
    pub fn state(&self) -> LifecycleState {
        LifecycleState::from_u8(self.inner.load(Ordering::Acquire))
    }

    /// Transition `Booting → Ready`.  Idempotent if already Ready;
    /// returns false if the daemon has already begun draining.
    pub fn mark_ready(&self) -> bool {
        self.transition(LifecycleState::Booting, LifecycleState::Ready)
    }

    /// Transition any state to `Draining`.  Returns false if already
    /// `Halted` (terminal).
    pub fn mark_draining(&self) -> bool {
        loop {
            let cur = self.inner.load(Ordering::Acquire);
            if LifecycleState::from_u8(cur) == LifecycleState::Halted {
                return false;
            }
            if LifecycleState::from_u8(cur) == LifecycleState::Draining {
                return true;
            }
            if self
                .inner
                .compare_exchange(
                    cur,
                    LifecycleState::Draining as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Transition `Draining → Halted`.  Returns false if not in `Draining`.
    pub fn mark_halted(&self) -> bool {
        self.transition(LifecycleState::Draining, LifecycleState::Halted)
    }

    /// Convenience: is the daemon currently accepting new work?
    pub fn is_ready(&self) -> bool {
        self.state() == LifecycleState::Ready
    }

    /// Convenience: spec #1 §3.5 `state.shutting_down` predicate.
    pub fn is_shutting_down(&self) -> bool {
        matches!(
            self.state(),
            LifecycleState::Draining | LifecycleState::Halted
        )
    }

    fn transition(&self, from: LifecycleState, to: LifecycleState) -> bool {
        self.inner
            .compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::thread;

    #[test]
    fn new_lifecycle_starts_booting() {
        let lc = Lifecycle::new();
        assert_eq!(lc.state(), LifecycleState::Booting);
        assert!(!lc.is_ready());
        assert!(!lc.is_shutting_down());
    }

    #[test]
    fn boot_to_ready_transitions_only_once() {
        let lc = Lifecycle::new();
        assert!(lc.mark_ready());
        assert!(lc.is_ready());
        // second call: already Ready, returns false
        assert!(!lc.mark_ready());
    }

    #[test]
    fn ready_to_draining() {
        let lc = Lifecycle::new();
        lc.mark_ready();
        assert!(lc.mark_draining());
        assert!(lc.is_shutting_down());
        assert!(!lc.is_ready());
    }

    #[test]
    fn draining_is_idempotent() {
        let lc = Lifecycle::new();
        assert!(lc.mark_draining());
        assert!(lc.mark_draining()); // second call returns true (still draining)
    }

    #[test]
    fn halted_blocks_further_transitions() {
        let lc = Lifecycle::new();
        lc.mark_draining();
        assert!(lc.mark_halted());
        assert_eq!(lc.state(), LifecycleState::Halted);
        // mark_draining after halted returns false (terminal)
        assert!(!lc.mark_draining());
        // mark_halted on already-halted returns false (not in Draining)
        assert!(!lc.mark_halted());
    }

    #[test]
    fn cannot_skip_directly_from_booting_to_halted() {
        let lc = Lifecycle::new();
        // mark_halted requires Draining state
        assert!(!lc.mark_halted());
        assert_eq!(lc.state(), LifecycleState::Booting);
    }

    #[test]
    fn concurrent_mark_draining_is_safe() {
        // Spec #1 §3.5 — multiple producers may race to set shutting_down.
        // The CAS loop in mark_draining MUST serialize them.
        let lc = Lifecycle::new();
        lc.mark_ready();
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let lc = lc.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                lc.mark_draining()
            }));
        }
        let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        // All 8 should report success (idempotent on Draining).
        assert!(results.iter().all(|&r| r));
        assert_eq!(lc.state(), LifecycleState::Draining);
    }

    #[test]
    fn lifecycle_state_round_trips_through_u8() {
        for s in [
            LifecycleState::Booting,
            LifecycleState::Ready,
            LifecycleState::Draining,
            LifecycleState::Halted,
        ] {
            assert_eq!(LifecycleState::from_u8(s as u8), s);
        }
    }
}
