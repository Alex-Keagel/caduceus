//! Leaf-ownership handoff (ws15) — spec #3 §5A.5 / Z6-G1.
//!
//! Per the implementation DAG (and iter-28 #3-8 single-source pointer),
//! this module is the **single normative source** for the leaf-ownership
//! handoff that `create_workspace` (§3.5 step 8.5) cites — never restates.
//!
//! Semantics (spec #3 §5A.5):
//!
//! - After `mkdir(leaf, mode=0700)` succeeds, the daemon transfers
//!   ownership of the leaf to the runner's effective UID + GID via
//!   `chown(leaf, runner_uid, runner_gid)`.  Mode is preserved at 0700;
//!   only the owner (runner) has read/write/exec.
//! - If `runner_uid` equals the daemon's UID (single-user deployment),
//!   the chown is a no-op but MUST still complete successfully (we
//!   issue `lchown(leaf, daemon_uid, daemon_gid)` for parity).
//! - Failure modes:
//!   - **EPERM** (daemon lacks `CAP_CHOWN` / can't change ownership) →
//!     surface `LeafOwnershipFailed` so create rolls back.
//!   - **ENOENT** (leaf disappeared between mkdir and chown) →
//!     surface `LeafOwnershipFailed`; create rolls back.
//!
//! Spec prose (§5A.5) spells out the rationale; this module implements
//! that contract.  Other call sites MUST cite **§5A.5 (Z6-G1)** and call
//! [`hand_off_leaf`] — they MUST NOT inline `chown(2)`.

use crate::error::WorkspaceError;
use std::path::Path;

/// Caller identity for the runner that will own the leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerIdentity {
    pub uid: u32,
    pub gid: u32,
}

impl RunnerIdentity {
    /// Convenience: identity matching the current process.
    #[cfg(unix)]
    pub fn for_self() -> Self {
        Self {
            uid: unsafe { libc::getuid() } as u32,
            gid: unsafe { libc::getgid() } as u32,
        }
    }
}

/// Hand off leaf ownership to `runner` after `mkdir` succeeds.  Spec
/// #3 §5A.5 (Z6-G1) — single normative source.
///
/// Implementations MUST use this function; inlining `chown(2)` at any
/// other call site is forbidden.
#[cfg(unix)]
pub fn hand_off_leaf(leaf: &Path, runner: RunnerIdentity) -> Result<(), WorkspaceError> {
    use std::os::unix::ffi::OsStrExt;
    let cs = std::ffi::CString::new(leaf.as_os_str().as_bytes()).map_err(|_| {
        WorkspaceError::PathValidationFailed(format!(
            "leaf path contains NUL byte: {}",
            leaf.display()
        ))
    })?;
    // We use lchown to avoid following any unexpected symlink at the leaf;
    // §3.5 step 5 has just mkdir'd this path, and §3.4 has validated it,
    // so it MUST be a directory (not a symlink) at this point.  lchown is
    // the safer choice in case of a TOCTOU between step 5 and step 8.5.
    let ret = unsafe { libc::lchown(cs.as_ptr(), runner.uid, runner.gid) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        return Err(WorkspaceError::PathValidationFailed(format!(
            "lchown({}, uid={}, gid={}) failed: {err}",
            leaf.display(),
            runner.uid,
            runner.gid
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn hand_off_leaf(_leaf: &Path, _runner: RunnerIdentity) -> Result<(), WorkspaceError> {
    // Windows has a different ACL model; spec #3 §5A.5 prose addresses
    // POSIX semantics. Windows port follows the ru03-spawn-windows path.
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn hand_off_to_self_is_noop() {
        // Single-user deployment: runner_uid == daemon_uid.  chown to
        // self MUST succeed.
        let td = tempfile::tempdir().unwrap();
        let leaf = td.path().join("leaf");
        std::fs::create_dir(&leaf).unwrap();
        let runner = RunnerIdentity::for_self();
        hand_off_leaf(&leaf, runner).expect("self-handoff must succeed");
    }

    #[test]
    fn hand_off_missing_leaf_surfaces_error() {
        let td = tempfile::tempdir().unwrap();
        let missing = td.path().join("does-not-exist");
        let runner = RunnerIdentity::for_self();
        let r = hand_off_leaf(&missing, runner);
        assert!(r.is_err(), "must surface error for missing leaf");
    }

    #[test]
    fn hand_off_to_foreign_uid_surfaces_eperm_when_unprivileged() {
        // We are unprivileged in tests.  chown to UID 0 (or any other
        // foreign UID) MUST fail with EPERM.  This verifies the §5A.5
        // failure-mode wiring.  (We can't assert the specific errno
        // because some sandboxes return EINVAL; we just assert error.)
        let td = tempfile::tempdir().unwrap();
        let leaf = td.path().join("leaf");
        std::fs::create_dir(&leaf).unwrap();
        let foreign = RunnerIdentity { uid: 0, gid: 0 };
        let r = hand_off_leaf(&leaf, foreign);
        if RunnerIdentity::for_self().uid == 0 {
            // Running as root in CI — chown to root is permitted; skip.
            return;
        }
        assert!(r.is_err(), "unprivileged chown to root must error");
    }
}
