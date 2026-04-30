//! 3-tier in-process lock primitives (ws07).
//!
//! Per the implementation DAG, this module provides the canonical
//! acquire order required by spec #3 §3.5 / §3.7:
//!
//! ```text
//! registry-wide  →  per-slug  →  per-workspace
//! ```
//!
//! The acquire order is the **only** correct order; deviations risk
//! deadlock when `OrphanReclaim` and synchronous `create_workspace`
//! contend.  Iter-28 #3-6 surfaces this as a single normative sentence.
//!
//! Locks are **in-process**.  Cross-process single-writer-per-root is
//! provided by the `.caduceusd.lock` advisory file lock (spec #3 I-8),
//! which is owned by `ws06-registry-store` and is independent of the
//! types here.
//!
//! Try-lock vs blocking:
//!
//! - **v1 strategy (a)** synchronous create — `acquire_for_create` uses
//!   try-lock; if the per-slug guard is held, `create_workspace` MUST
//!   release the registry-wide mutex and return `SharedRepoLocked`.
//! - **`OrphanReclaim` worker** — uses blocking `acquire_for_reclaim`;
//!   it is allowed to wait because reclaim is non-time-sensitive.

use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::Arc;

/// Top-level lock manager.  Cheap to clone (`Arc`).
#[derive(Debug, Default, Clone)]
pub struct WorkspaceLocks {
    inner: Arc<LocksInner>,
}

#[derive(Debug, Default)]
struct LocksInner {
    /// Brief mutation lock for the registry row table.
    registry: Mutex<()>,
    /// Per-slug shared-repo guards.  RwLock so consumer code can model
    /// "writers serialize, readers concurrent" if needed; v1 strategy
    /// (a) only ever takes the write side.  Keyed by slug string.
    per_slug: Mutex<HashMap<String, Arc<RwLock<()>>>>,
    /// Per-workspace exclusive locks.  Keyed by workspace_id string.
    per_workspace: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl WorkspaceLocks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire the registry-wide brief mutex.  Hold ONLY during row
    /// table mutations; release before doing filesystem I/O.
    pub fn registry_lock(&self) -> RegistryGuard<'_> {
        RegistryGuard {
            _g: self.inner.registry.lock(),
        }
    }

    /// Get-or-insert the per-slug guard.  Cheap; the inner `RwLock`
    /// is what consumers ultimately acquire.
    fn slug_guard(&self, slug: &str) -> Arc<RwLock<()>> {
        let mut g = self.inner.per_slug.lock();
        if let Some(existing) = g.get(slug) {
            return Arc::clone(existing);
        }
        let new = Arc::new(RwLock::new(()));
        g.insert(slug.to_string(), Arc::clone(&new));
        new
    }

    /// Get-or-insert the per-workspace lock.
    fn workspace_lock(&self, workspace_id: &str) -> Arc<Mutex<()>> {
        let mut g = self.inner.per_workspace.lock();
        if let Some(existing) = g.get(workspace_id) {
            return Arc::clone(existing);
        }
        let new = Arc::new(Mutex::new(()));
        g.insert(workspace_id.to_string(), Arc::clone(&new));
        new
    }

    /// Synchronous-create acquisition (v1 strategy a, spec #3 §3.7
    /// caller table).  Try-lock on the per-slug guard; on contention,
    /// release the registry-wide mutex and return `None` so the caller
    /// can surface `Error::SharedRepoLocked`.
    ///
    /// MUST be called with the registry-wide guard already held by the
    /// caller (the caller passes it in).  This function transfers the
    /// guard responsibility on success.
    pub fn try_acquire_for_create(
        &self,
        registry_guard: RegistryGuard<'_>,
        slug: &str,
        workspace_id: &str,
    ) -> Option<CreateGuards> {
        let slug_arc = self.slug_guard(slug);
        let slug_guard = match slug_arc.try_write_arc() {
            Some(g) => g,
            None => {
                // Release registry guard explicitly via drop (move into block).
                drop(registry_guard);
                return None;
            }
        };
        // Registry guard released as we transition into the per-slug
        // critical section. Holding registry while doing filesystem I/O
        // would block all other registry operations — explicit drop.
        drop(registry_guard);
        let ws_arc = self.workspace_lock(workspace_id);
        let ws_guard = ws_arc.lock_arc();
        Some(CreateGuards {
            _slug: slug_guard,
            _ws: ws_guard,
        })
    }

    /// `OrphanReclaim` acquisition (spec #3 §5B.2).  Blocks on the
    /// per-slug guard if held; reclaim is permitted to wait.
    pub fn acquire_for_reclaim(&self, slug: &str, workspace_id: &str) -> CreateGuards {
        // OrphanReclaim does NOT hold the registry guard for the duration
        // (it'd block all create/cleanup); it acquires the registry guard
        // briefly elsewhere for row mutations.
        let slug_arc = self.slug_guard(slug);
        let slug_guard = slug_arc.write_arc();
        let ws_arc = self.workspace_lock(workspace_id);
        let ws_guard = ws_arc.lock_arc();
        CreateGuards {
            _slug: slug_guard,
            _ws: ws_guard,
        }
    }
}

/// Guard for the registry-wide brief mutex.
pub struct RegistryGuard<'a> {
    _g: parking_lot::MutexGuard<'a, ()>,
}

/// Bundled per-slug + per-workspace guards held during create / cleanup.
pub struct CreateGuards {
    _slug: parking_lot::ArcRwLockWriteGuard<parking_lot::RawRwLock, ()>,
    _ws: parking_lot::ArcMutexGuard<parking_lot::RawMutex, ()>,
}

impl std::fmt::Debug for CreateGuards {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateGuards").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn registry_lock_is_exclusive() {
        let locks = WorkspaceLocks::new();
        let _g1 = locks.registry_lock();
        // Try to acquire from a thread; it must block.
        let locks2 = locks.clone();
        let handle = thread::spawn(move || {
            let _g = locks2.registry_lock();
            "got it"
        });
        thread::sleep(Duration::from_millis(50));
        assert!(!handle.is_finished(), "registry lock must be exclusive");
        drop(_g1);
        assert_eq!(handle.join().unwrap(), "got it");
    }

    #[test]
    fn try_acquire_for_create_succeeds_when_uncontended() {
        let locks = WorkspaceLocks::new();
        let g = locks.registry_lock();
        let r = locks.try_acquire_for_create(g, "slug_a", "wsp_aaa");
        assert!(r.is_some());
    }

    #[test]
    fn try_acquire_for_create_returns_none_when_slug_locked() {
        let locks = WorkspaceLocks::new();

        // Acquire per-slug guard from a parallel "reclaim" path.
        let _held = locks.acquire_for_reclaim("slug_x", "wsp_aaa");

        // Synchronous-create attempt on the same slug must try-fail.
        let g = locks.registry_lock();
        let r = locks.try_acquire_for_create(g, "slug_x", "wsp_bbb");
        assert!(r.is_none(), "must fail try-lock when slug is held");
    }

    #[test]
    fn try_acquire_releases_registry_guard_on_failure() {
        // After a try-lock failure, the registry mutex MUST be released so
        // unrelated registry ops can proceed.
        let locks = WorkspaceLocks::new();
        let _held = locks.acquire_for_reclaim("slug_x", "wsp_aaa");
        let g = locks.registry_lock();
        let r = locks.try_acquire_for_create(g, "slug_x", "wsp_bbb");
        assert!(r.is_none());
        // Now we should be able to acquire the registry guard again from this
        // same thread without deadlocking.
        let _g2 = locks.registry_lock();
    }

    #[test]
    fn try_acquire_releases_registry_guard_on_success() {
        // After successful try-lock, the registry mutex MUST be released
        // so other ops can proceed while the per-slug + per-workspace
        // guards are held during filesystem I/O.
        let locks = WorkspaceLocks::new();
        let g = locks.registry_lock();
        let _held = locks
            .try_acquire_for_create(g, "slug_y", "wsp_yyy")
            .unwrap();
        // Registry guard should be released; we can re-acquire from this thread.
        let _g2 = locks.registry_lock();
    }

    #[test]
    fn different_slugs_do_not_block_each_other() {
        let locks = WorkspaceLocks::new();
        let _h1 = locks.acquire_for_reclaim("slug_a", "wsp_aaa");
        // Concurrent acquire on a DIFFERENT slug must succeed.
        let g = locks.registry_lock();
        let r = locks.try_acquire_for_create(g, "slug_b", "wsp_bbb");
        assert!(r.is_some(), "different slugs must not block each other");
    }

    #[test]
    fn reclaim_blocks_when_slug_held() {
        let locks = WorkspaceLocks::new();
        let _h1 = locks.acquire_for_reclaim("slug_z", "wsp_zzz");

        let locks2 = locks.clone();
        let handle = thread::spawn(move || {
            let _g = locks2.acquire_for_reclaim("slug_z", "wsp_other");
            "got it"
        });
        thread::sleep(Duration::from_millis(50));
        assert!(
            !handle.is_finished(),
            "second reclaim on same slug must block"
        );
        drop(_h1);
        assert_eq!(handle.join().unwrap(), "got it");
    }

    #[test]
    fn locks_are_cheap_to_clone() {
        // Arc<...> => O(1) clone + atomic refcount; suitable for stashing
        // in many subsystems without contention.
        let locks = WorkspaceLocks::new();
        let arcs: Vec<WorkspaceLocks> = (0..32).map(|_| locks.clone()).collect();
        // All clones share the same underlying state.
        let _g = arcs[0].registry_lock();
        // Drop quickly; just exercising the API.
        drop(_g);
        drop(arcs);
    }
}
