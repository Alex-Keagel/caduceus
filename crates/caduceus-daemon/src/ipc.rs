//! Local IPC transport (Unix domain socket).
//!
//! Per the implementation DAG (todo `f06-ipc-transport-local-uds`), this
//! module provides the daemon's local-only listener.  It binds a UDS
//! socket at a configured path, accepts connections, and exposes peer
//! identity (PID + UID) so the snapshot RPC can enforce the local-only
//! gate from spec #4 §1.2 (iter-28 backlog #4-1).
//!
//! ## Peer identity APIs
//!
//! - **Linux**: `getsockopt(SO_PEERCRED)` → `(pid, uid, gid)`.
//! - **macOS**: `getsockopt(LOCAL_PEERPID)` for PID + `getpeereid` for
//!   UID/GID. (Iter-28 #2 / B26b correction — `LOCAL_PEERCRED` does
//!   NOT supply PID on macOS; only `LOCAL_PEERPID` does.)
//! - **Windows**: not implemented in this module; spec mandates a named
//!   pipe transport with `GetNamedPipeServerProcessId`.  That lands in
//!   a separate Windows-only module wired by `f06-ipc-transport-windows`
//!   (not yet on the DAG; tracked as a follow-up since v1 ships POSIX).
//!
//! ## Local-only enforcement (spec #4 §1.2)
//!
//! UDS is inherently local — non-local clients literally cannot connect
//! because the AF_UNIX socket has no network presence.  We additionally
//! verify that the peer's UID matches the daemon's UID (or is the
//! configured `allowed_uid`) before accepting any RPC.  Connections from
//! mismatched UIDs are immediately rejected.
//!
//! ## Lifetime
//!
//! The listener is owned by the daemon's main loop.  Each accepted
//! connection becomes an `IpcConnection` carrying the verified peer
//! credentials and a tokio framed I/O pair.  Wire-shape parsing belongs
//! to higher layers (snapshot RPC for spec #4, runner-codec for spec #2);
//! this module deals only in raw bytes.

#![cfg(unix)]

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::net::{UnixListener, UnixStream};

/// Errors specific to the local IPC transport.
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("failed to bind UDS at {path}: {source}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to accept UDS connection: {0}")]
    Accept(#[source] std::io::Error),
    #[error("could not query peer credentials: {0}")]
    PeerCred(#[source] std::io::Error),
    #[error("peer UID {peer} does not match expected UID {expected}")]
    PeerUidMismatch { peer: u32, expected: u32 },
}

/// Verified peer identity for an accepted connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCreds {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

/// Configuration for the IPC listener.
#[derive(Debug, Clone)]
pub struct IpcConfig {
    /// Filesystem path to bind the UDS socket at.
    pub socket_path: PathBuf,
    /// UID expected on the peer side; mismatched UIDs are rejected.
    /// Default is the daemon's own UID.
    pub allowed_uid: u32,
}

impl IpcConfig {
    /// Construct with the daemon's own UID as the allowed peer.
    pub fn for_self(socket_path: impl Into<PathBuf>) -> Self {
        // SAFETY: `getuid` is always safe; it never fails.
        let uid = unsafe { libc::getuid() } as u32;
        Self {
            socket_path: socket_path.into(),
            allowed_uid: uid,
        }
    }
}

/// Local UDS listener.
pub struct IpcListener {
    inner: UnixListener,
    cfg: IpcConfig,
    socket_path: PathBuf,
}

impl IpcListener {
    /// Bind a UDS socket at `cfg.socket_path` and start listening.
    /// If a stale socket file exists at the path, it is removed first.
    pub fn bind(cfg: IpcConfig) -> Result<Self, IpcError> {
        if let Some(parent) = cfg.socket_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| IpcError::Bind {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        if cfg.socket_path.exists() {
            // Could be a stale socket from a prior daemon. Remove it.
            // The .caduceusd.lock advisory lock (spec #3 I-8) guarantees
            // we are the single writer at this point; cleanup is safe.
            let _ = std::fs::remove_file(&cfg.socket_path);
        }
        let inner = UnixListener::bind(&cfg.socket_path).map_err(|source| IpcError::Bind {
            path: cfg.socket_path.clone(),
            source,
        })?;
        let socket_path = cfg.socket_path.clone();
        Ok(Self {
            inner,
            cfg,
            socket_path,
        })
    }

    /// Accept the next connection and verify peer identity.
    /// Connections from mismatched UIDs are dropped before being yielded.
    pub async fn accept(&self) -> Result<IpcConnection, IpcError> {
        loop {
            let (stream, _addr) = self.inner.accept().await.map_err(IpcError::Accept)?;
            let creds = peer_credentials(&stream)?;
            if creds.uid != self.cfg.allowed_uid {
                tracing::warn!(
                    peer_uid = creds.uid,
                    expected_uid = self.cfg.allowed_uid,
                    "rejected IPC connection from mismatched UID"
                );
                drop(stream); // close immediately
                continue;
            }
            return Ok(IpcConnection { stream, creds });
        }
    }

    /// Path the listener is bound to.
    pub fn path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        // Best-effort cleanup of the socket file.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Accepted connection with verified peer identity.
pub struct IpcConnection {
    pub stream: UnixStream,
    pub creds: PeerCreds,
}

// ─────────────────────────── peer credentials ──────────────────────────

#[cfg(target_os = "linux")]
fn peer_credentials(stream: &UnixStream) -> Result<PeerCreds, IpcError> {
    use std::mem::MaybeUninit;
    let fd = stream.as_raw_fd();
    let mut ucred = MaybeUninit::<libc::ucred>::zeroed();
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            ucred.as_mut_ptr() as *mut _,
            &mut len,
        )
    };
    if ret != 0 {
        return Err(IpcError::PeerCred(std::io::Error::last_os_error()));
    }
    // SAFETY: getsockopt zeroes/writes the struct on success.
    let ucred = unsafe { ucred.assume_init() };
    Ok(PeerCreds {
        pid: ucred.pid,
        uid: ucred.uid,
        gid: ucred.gid,
    })
}

#[cfg(target_os = "macos")]
fn peer_credentials(stream: &UnixStream) -> Result<PeerCreds, IpcError> {
    // Iter-28 #2 / B26b: macOS uses LOCAL_PEERPID (NOT LOCAL_PEERCRED)
    // for PID, and getpeereid for UID/GID.
    const LOCAL_PEERPID: libc::c_int = 0x002;
    let fd = stream.as_raw_fd();
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            LOCAL_PEERPID,
            &mut pid as *mut _ as *mut _,
            &mut len,
        )
    };
    if ret != 0 {
        return Err(IpcError::PeerCred(std::io::Error::last_os_error()));
    }
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let ret = unsafe { libc::getpeereid(fd, &mut uid as *mut _, &mut gid as *mut _) };
    if ret != 0 {
        return Err(IpcError::PeerCred(std::io::Error::last_os_error()));
    }
    Ok(PeerCreds { pid, uid, gid })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_credentials(_stream: &UnixStream) -> Result<PeerCreds, IpcError> {
    // Other Unix variants (e.g. FreeBSD) — not yet implemented.
    Err(IpcError::PeerCred(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "peer credential lookup not implemented for this OS",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn td() -> TempDir {
        TempDir::new().unwrap()
    }

    #[tokio::test]
    async fn bind_creates_socket_file() {
        let d = td();
        let path = d.path().join("caduceusd.sock");
        let cfg = IpcConfig::for_self(&path);
        let _listener = IpcListener::bind(cfg).unwrap();
        assert!(path.exists(), "UDS socket file must exist after bind");
    }

    #[tokio::test]
    async fn drop_removes_socket_file() {
        let d = td();
        let path = d.path().join("caduceusd.sock");
        {
            let cfg = IpcConfig::for_self(&path);
            let _listener = IpcListener::bind(cfg).unwrap();
            assert!(path.exists());
        }
        assert!(!path.exists(), "drop must remove socket file");
    }

    #[tokio::test]
    async fn stale_socket_file_is_replaced_on_bind() {
        let d = td();
        let path = d.path().join("stale.sock");
        // Create a stale plain file at the socket path.
        std::fs::write(&path, "stale").unwrap();
        let cfg = IpcConfig::for_self(&path);
        let listener = IpcListener::bind(cfg).expect("bind must replace stale file");
        drop(listener);
    }

    #[tokio::test]
    async fn accept_yields_local_peer_with_self_uid() {
        let d = td();
        let path = d.path().join("c.sock");
        let cfg = IpcConfig::for_self(&path);
        let listener = IpcListener::bind(cfg).unwrap();
        let path_for_client = path.clone();
        let server = tokio::spawn(async move { listener.accept().await });
        let mut client = UnixStream::connect(&path_for_client).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut conn = server.await.unwrap().unwrap();
        // Verify peer creds match our own UID (we are the same process).
        let our_uid = unsafe { libc::getuid() } as u32;
        assert_eq!(conn.creds.uid, our_uid);
        assert!(conn.creds.pid > 0);
        // Read back the bytes to confirm the stream is functional.
        let mut buf = [0u8; 5];
        conn.stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[tokio::test]
    async fn rejects_uid_mismatch_and_continues_accepting() {
        // We cannot easily impersonate another UID in a test, but we can
        // exercise the rejection path by configuring an allowed_uid that
        // does NOT match our own and confirming that accept() does not
        // yield (it would loop and reject the connection). This test
        // therefore verifies that a connection from us is rejected when
        // the allowed_uid is wrong, by setting a tight timeout.
        let d = td();
        let path = d.path().join("reject.sock");
        let cfg = IpcConfig {
            socket_path: path.clone(),
            allowed_uid: 0xFFFF_FFFE, // not our UID
        };
        let listener = IpcListener::bind(cfg).unwrap();
        let path_for_client = path.clone();
        let accept_task = tokio::spawn(async move { listener.accept().await });

        // Connect; the listener should reject our UID and keep waiting.
        let _client = UnixStream::connect(&path_for_client).await.unwrap();

        // accept() should remain pending because it loops on rejection.
        let timed = tokio::time::timeout(std::time::Duration::from_millis(100), accept_task).await;
        assert!(
            timed.is_err(),
            "accept() must keep looping after UID mismatch"
        );
    }
}
