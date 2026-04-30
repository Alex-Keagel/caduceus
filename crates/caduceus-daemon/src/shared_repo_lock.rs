//! Shared-repo lock semantics (ws11).
//!
//! Per the implementation DAG, this module formalizes spec #3 §3.7's
//! per-slug shared-repo lock semantics — specifically the **caller
//! table** that maps each caller class to the correct acquisition
//! mode (try-lock vs blocking-Wait).
//!
//! The lock *primitives* live in [`crate::locks::WorkspaceLocks`] (ws07).
//! This module exposes the **strategy** layer on top:
//!
//! - **Strategy (a)** — synchronous create, try-lock semantics.  v1
//!   default.  Callers: `create_workspace` (§3.5).
//! - **Strategy (b)** — concurrent shared-repo support with
//!   read-locked workspaces.  Out of v1; tracked as future work.
//! - **Strategy (c)** — explicit conflict, queue + serialize.  Out
//!   of v1.
//!
//! The §3.7 caller table is encoded in [`SharedRepoCaller`] as an
//! exhaustive enum so exhaustiveness checks at the call site catch
//! any new caller without an explicit strategy mapping.

use crate::error::WorkspaceError;
use crate::locks::{CreateGuards, RegistryGuard, WorkspaceLocks};

/// Caller class invoking the shared-repo lock.  Spec #3 §3.7 caller table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedRepoCaller {
    /// `create_workspace` (§3.5) — synchronous; try-lock semantics.
    /// Returns `WorkspaceError::SharedRepoLocked` immediately on contention.
    SynchronousCreate,
    /// `OrphanReclaim` worker (§5B.2) — background; blocks on contention.
    OrphanReclaim,
    /// `cleanup_workspace` (§3.6) on Ready -> CleaningUp transition;
    /// blocks (cleanup must wait for any concurrent reader).
    Cleanup,
}

/// Strategy applied for v1.  All three callers route to **strategy (a)**
/// in v1; cleanup uses the blocking variant of strategy (a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedRepoLockStrategy {
    StrategyA,
    // StrategyB,  // out of v1
    // StrategyC,  // out of v1
}

impl SharedRepoCaller {
    /// V1 mapping of caller class -> strategy.  Spec #3 §3.7.
    pub fn strategy(self) -> SharedRepoLockStrategy {
        // V1 commits to strategy (a) for ALL callers.  Strategies (b)
        // and (c) are tracked as out-of-v1; the enum exhaustiveness
        // forces a deliberate decision when a new caller class is
        // added.
        SharedRepoLockStrategy::StrategyA
    }

    /// V1 mapping of caller class -> acquisition mode (try vs blocking).
    pub fn is_blocking(self) -> bool {
        match self {
            SharedRepoCaller::SynchronousCreate => false,
            SharedRepoCaller::OrphanReclaim | SharedRepoCaller::Cleanup => true,
        }
    }
}

/// High-level façade over `WorkspaceLocks` keyed on `SharedRepoCaller`.
/// Callers that don't know their class statically (e.g., a generic
/// retry harness) use this entry point; `create_workspace` may continue
/// to call `WorkspaceLocks::try_acquire_for_create` directly.
pub fn acquire(
    locks: &WorkspaceLocks,
    registry_guard: RegistryGuard<'_>,
    caller: SharedRepoCaller,
    slug: &str,
    workspace_id: &str,
) -> Result<CreateGuards, WorkspaceError> {
    match caller {
        SharedRepoCaller::SynchronousCreate => locks
            .try_acquire_for_create(registry_guard, slug, workspace_id)
            .ok_or_else(|| WorkspaceError::SharedRepoLocked(slug.to_string())),
        SharedRepoCaller::OrphanReclaim | SharedRepoCaller::Cleanup => {
            // Blocking callers must NOT hold the registry guard for the
            // wait; the WorkspaceLocks::acquire_for_reclaim contract
            // accepts that the guard is released by the caller before
            // calling.
            drop(registry_guard);
            Ok(locks.acquire_for_reclaim(slug, workspace_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_table_v1_routes_all_to_strategy_a() {
        for caller in [
            SharedRepoCaller::SynchronousCreate,
            SharedRepoCaller::OrphanReclaim,
            SharedRepoCaller::Cleanup,
        ] {
            assert_eq!(caller.strategy(), SharedRepoLockStrategy::StrategyA);
        }
    }

    #[test]
    fn synchronous_create_is_non_blocking() {
        assert!(!SharedRepoCaller::SynchronousCreate.is_blocking());
    }

    #[test]
    fn orphan_reclaim_and_cleanup_block() {
        assert!(SharedRepoCaller::OrphanReclaim.is_blocking());
        assert!(SharedRepoCaller::Cleanup.is_blocking());
    }

    #[test]
    fn synchronous_create_returns_locked_on_contention() {
        let locks = WorkspaceLocks::new();
        let _held = locks.acquire_for_reclaim("slug_x", "wsp_aaa");
        let g = locks.registry_lock();
        let r = acquire(
            &locks,
            g,
            SharedRepoCaller::SynchronousCreate,
            "slug_x",
            "wsp_bbb",
        );
        match r {
            Err(WorkspaceError::SharedRepoLocked(slug)) => assert_eq!(slug, "slug_x"),
            other => panic!("expected SharedRepoLocked, got {other:?}"),
        }
    }

    #[test]
    fn synchronous_create_succeeds_when_uncontended() {
        let locks = WorkspaceLocks::new();
        let g = locks.registry_lock();
        let r = acquire(
            &locks,
            g,
            SharedRepoCaller::SynchronousCreate,
            "slug_y",
            "wsp_yyy",
        );
        assert!(r.is_ok());
    }
}
