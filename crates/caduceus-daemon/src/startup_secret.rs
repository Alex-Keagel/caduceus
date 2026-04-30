//! Daemon-startup HMAC secret (xs01).
//!
//! Per the implementation DAG, this module provides a short-lived
//! daemon-startup HMAC key that the snapshot RPC (P4 sn13) cross-checks
//! when serving privileged data over a local transport.  Spec #4 §1.2
//! references this as part of the local-only-transport gate's
//! defence-in-depth.
//!
//! The key is generated at daemon startup via a CSPRNG (32 bytes).
//! It is held in memory only — never persisted, never logged.  Clients
//! retrieve it via the IPC handshake (peer-creds-gated; xs05 future
//! spec hardening).
//!
//! V1 scope:
//!
//! - 32-byte random key, generated once per daemon process.
//! - HMAC-BLAKE3 over a request payload + the key.
//! - Constant-time comparison via `subtle`-style bitwise OR.
//!
//! Deferred / out-of-v1:
//!
//! - **Z-namespace registry appendix** (xs02) — spec text addition;
//!   tracked separately on the ship/p0-specs-iter27 branch.
//! - **Key rotation** — v1 has a single per-process key.

use blake3::Hasher;

/// 32-byte secret.  In-memory only.
#[derive(Clone)]
pub struct DaemonStartupSecret {
    bytes: [u8; 32],
}

impl DaemonStartupSecret {
    /// Generate a fresh secret using the OS CSPRNG.
    pub fn generate() -> std::io::Result<Self> {
        let mut bytes = [0u8; 32];
        #[cfg(unix)]
        {
            use std::io::Read;
            let mut f = std::fs::File::open("/dev/urandom")?;
            f.read_exact(&mut bytes)?;
        }
        #[cfg(not(unix))]
        {
            // On Windows this would use BCryptGenRandom; v1 placeholder
            // returns an error to surface the gap.
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "DaemonStartupSecret CSPRNG not implemented on this platform",
            ));
        }
        Ok(Self { bytes })
    }

    /// Construct from raw bytes.  Test-only.
    #[doc(hidden)]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }

    /// Compute HMAC-BLAKE3 over `payload` using the secret as key.
    /// Returns the first 32 bytes of the keyed digest.
    pub fn mac(&self, payload: &[u8]) -> [u8; 32] {
        let mut h = Hasher::new_keyed(&self.bytes);
        h.update(b"caduceusd.daemon-startup.v1");
        h.update(b"\x1f");
        h.update(payload);
        let digest = h.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_bytes());
        out
    }

    /// Verify a MAC tag against `payload`.  Constant-time comparison.
    pub fn verify(&self, payload: &[u8], tag: &[u8; 32]) -> bool {
        let expected = self.mac(payload);
        ct_eq(&expected, tag)
    }
}

impl std::fmt::Debug for DaemonStartupSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NEVER log the bytes.
        f.debug_struct("DaemonStartupSecret")
            .field("bytes", &"<redacted>")
            .finish()
    }
}

/// Constant-time byte equality.  Avoids early termination on first
/// mismatch so the comparison time does not leak position of differing
/// bytes.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff: u8 = 0;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_is_deterministic_for_same_payload() {
        let secret = DaemonStartupSecret::from_bytes([7u8; 32]);
        let m1 = secret.mac(b"hello");
        let m2 = secret.mac(b"hello");
        assert_eq!(m1, m2);
    }

    #[test]
    fn mac_changes_with_payload() {
        let secret = DaemonStartupSecret::from_bytes([7u8; 32]);
        assert_ne!(secret.mac(b"a"), secret.mac(b"b"));
    }

    #[test]
    fn mac_changes_with_secret() {
        let s1 = DaemonStartupSecret::from_bytes([7u8; 32]);
        let s2 = DaemonStartupSecret::from_bytes([8u8; 32]);
        assert_ne!(s1.mac(b"x"), s2.mac(b"x"));
    }

    #[test]
    fn verify_accepts_correct_tag() {
        let secret = DaemonStartupSecret::from_bytes([7u8; 32]);
        let tag = secret.mac(b"payload");
        assert!(secret.verify(b"payload", &tag));
    }

    #[test]
    fn verify_rejects_wrong_tag() {
        let secret = DaemonStartupSecret::from_bytes([7u8; 32]);
        let mut tag = secret.mac(b"payload");
        tag[0] ^= 0x01;
        assert!(!secret.verify(b"payload", &tag));
    }

    #[test]
    fn verify_rejects_wrong_payload() {
        let secret = DaemonStartupSecret::from_bytes([7u8; 32]);
        let tag = secret.mac(b"payload");
        assert!(!secret.verify(b"different", &tag));
    }

    #[test]
    fn debug_redacts_secret_bytes() {
        let secret = DaemonStartupSecret::from_bytes([7u8; 32]);
        let s = format!("{secret:?}");
        assert!(!s.contains("7"));
        assert!(s.contains("redacted"));
    }

    #[cfg(unix)]
    #[test]
    fn generate_produces_unique_secrets() {
        let s1 = DaemonStartupSecret::generate().unwrap();
        let s2 = DaemonStartupSecret::generate().unwrap();
        // Probability of collision in 256 random bits is essentially 0.
        assert_ne!(s1.bytes, s2.bytes);
    }

    #[test]
    fn ct_eq_returns_true_for_equal() {
        let a = [42u8; 32];
        let b = [42u8; 32];
        assert!(ct_eq(&a, &b));
    }

    #[test]
    fn ct_eq_returns_false_for_unequal() {
        let a = [42u8; 32];
        let mut b = [42u8; 32];
        b[31] = 41;
        assert!(!ct_eq(&a, &b));
    }
}
