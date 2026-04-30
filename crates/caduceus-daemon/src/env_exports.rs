//! Workspace environment exports (ws14).
//!
//! Per the implementation DAG, this module computes the canonical
//! `CADUCEUS_*` environment variables that `create_workspace` (§3.5
//! step 6) materializes for the runner subprocess and for `before_create`
//! / `after_create` hooks.
//!
//! Iter-28 #3-5 absorbed: `CADUCEUS_RUN_ID_SAFE` is NOT shell-safe by
//! itself — the documentation here makes that explicit.  Hook authors
//! writing shell-fragment hooks (e.g. `bash -lc "git clone $URL $CADUCEUS_RUN_ID"`)
//! MUST still quote `"$CADUCEUS_RUN_ID_SAFE"`.  A separate base64url
//! variant (`*_SAFE_B64`) is provided for shell-fragment hooks that
//! cannot quote.
//!
//! Spec #3 §3.5 step 6 lists the canonical names:
//!
//! - `CADUCEUS_RUN_ID` — raw run_id (with shell-injection notice).
//! - `CADUCEUS_RUN_ID_SAFE` — `sanitize_run_id(run_id)`.
//! - `CADUCEUS_WORKSPACE_PATH` — canonicalized leaf.
//! - `CADUCEUS_REPO_SLUG` — slug.
//! - `CADUCEUS_REPO_REMOTE_URL` — remote URL or empty.
//! - `CADUCEUS_REPO_REMOTE_URL_SAFE_B64` — base64url-unpadded of the
//!   above (always shell-safe).
//! - `CADUCEUS_REPO_DEFAULT_BRANCH` — default branch or empty.

use crate::registry::RepoCoordinate;
use crate::workspace::SafeRunId;
use std::collections::BTreeMap;
use std::path::Path;

/// Compute the canonical CADUCEUS_* env exports.
///
/// Returns a sorted map (BTreeMap) so callers that hash or compare
/// the export set (e.g. test fixtures) get deterministic output.
///
/// `raw_run_id` is the RAW run_id as supplied to `create_workspace`
/// (NOT sanitized).  `safe_run_id` is the sanitized form.  Spec §3.5
/// step 6 mandates BOTH be exported.
pub fn workspace_env_exports(
    raw_run_id: &str,
    safe_run_id: &SafeRunId,
    workspace_path: &Path,
    repo: &RepoCoordinate,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("CADUCEUS_RUN_ID".into(), raw_run_id.to_string());
    env.insert(
        "CADUCEUS_RUN_ID_SAFE".into(),
        safe_run_id.as_str().to_string(),
    );
    env.insert(
        "CADUCEUS_WORKSPACE_PATH".into(),
        workspace_path.display().to_string(),
    );
    env.insert("CADUCEUS_REPO_SLUG".into(), repo.slug.clone());

    let remote = repo.remote_url.clone().unwrap_or_default();
    env.insert(
        "CADUCEUS_REPO_REMOTE_URL_SAFE_B64".into(),
        base64url_unpadded(remote.as_bytes()),
    );
    env.insert("CADUCEUS_REPO_REMOTE_URL".into(), remote);

    env.insert(
        "CADUCEUS_REPO_DEFAULT_BRANCH".into(),
        repo.default_branch.clone().unwrap_or_default(),
    );
    env
}

/// RFC 4648 §5 base64url encoding without padding.  Always safe to
/// drop into a shell fragment without quoting (alphanumeric + `-_`).
fn base64url_unpadded(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((input.len() * 4 / 3) + 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHABET[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{sanitize_repo_slug, sanitize_run_id};
    use std::path::PathBuf;

    fn coord() -> RepoCoordinate {
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        RepoCoordinate::new(
            slug,
            Some("https://github.com/o/r".to_string()),
            Some("main".to_string()),
        )
    }

    #[test]
    fn exports_contain_all_canonical_names() {
        let rid = sanitize_run_id("01H8XYZ").unwrap();
        let env = workspace_env_exports(
            "01H8XYZ",
            &rid,
            Path::new("/var/lib/caduceus/github_com_o_r/01H8XYZ"),
            &coord(),
        );
        for k in [
            "CADUCEUS_RUN_ID",
            "CADUCEUS_RUN_ID_SAFE",
            "CADUCEUS_WORKSPACE_PATH",
            "CADUCEUS_REPO_SLUG",
            "CADUCEUS_REPO_REMOTE_URL",
            "CADUCEUS_REPO_REMOTE_URL_SAFE_B64",
            "CADUCEUS_REPO_DEFAULT_BRANCH",
        ] {
            assert!(env.contains_key(k), "missing canonical env name: {k}");
        }
    }

    #[test]
    fn run_id_raw_and_safe_are_both_exported() {
        // Iter-28 #3-5: hooks may need both forms; spec mandates both.
        let rid = sanitize_run_id("run/abc").unwrap(); // -> "run_abc"
        let env = workspace_env_exports("run/abc", &rid, Path::new("/x"), &coord());
        assert_eq!(env["CADUCEUS_RUN_ID"], "run/abc");
        assert_eq!(env["CADUCEUS_RUN_ID_SAFE"], "run_abc");
    }

    #[test]
    fn safe_b64_is_shell_safe_alphabet() {
        let rid = sanitize_run_id("01H8XYZ").unwrap();
        let env = workspace_env_exports("01H8XYZ", &rid, Path::new("/x"), &coord());
        let b64 = &env["CADUCEUS_REPO_REMOTE_URL_SAFE_B64"];
        for c in b64.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "non-shell-safe char in SAFE_B64: {c:?}"
            );
        }
    }

    #[test]
    fn missing_remote_url_yields_empty_strings() {
        let rid = sanitize_run_id("x").unwrap();
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        let coord = RepoCoordinate::new(slug, None, None);
        let env = workspace_env_exports("x", &rid, Path::new("/x"), &coord);
        assert_eq!(env["CADUCEUS_REPO_REMOTE_URL"], "");
        assert_eq!(env["CADUCEUS_REPO_DEFAULT_BRANCH"], "");
        // SAFE_B64 of empty bytes is empty — still safe.
        assert_eq!(env["CADUCEUS_REPO_REMOTE_URL_SAFE_B64"], "");
    }

    #[test]
    fn workspace_path_is_string_form_of_input() {
        let rid = sanitize_run_id("x").unwrap();
        let p = PathBuf::from("/var/lib/caduceus/abc/01");
        let env = workspace_env_exports("x", &rid, &p, &coord());
        assert_eq!(env["CADUCEUS_WORKSPACE_PATH"], "/var/lib/caduceus/abc/01");
    }

    #[test]
    fn base64url_known_vectors() {
        // RFC 4648 §10 test vectors (modified for url-safe-no-padding).
        assert_eq!(base64url_unpadded(b""), "");
        assert_eq!(base64url_unpadded(b"f"), "Zg");
        assert_eq!(base64url_unpadded(b"fo"), "Zm8");
        assert_eq!(base64url_unpadded(b"foo"), "Zm9v");
        assert_eq!(base64url_unpadded(b"foob"), "Zm9vYg");
        assert_eq!(base64url_unpadded(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url_unpadded(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64url_uses_url_safe_chars() {
        // 0xFB = 11111011 -> 111110, 110000 -> chars at indices 62, 48
        // 62 -> '-', 48 -> 'w'.  Standard base64 would emit '+'.
        let out = base64url_unpadded(&[0xFB, 0xFC]);
        // First 6 bits of 0xFB = 0b111110 = 62 -> '-'
        assert_eq!(&out[0..1], "-");
        // No `+` or `/` should appear.
        assert!(!out.contains('+'));
        assert!(!out.contains('/'));
    }

    #[test]
    fn exports_are_deterministic_sorted() {
        // BTreeMap iteration order is by key; ensures deterministic
        // diff-friendly output for fixtures.
        let rid = sanitize_run_id("x").unwrap();
        let env = workspace_env_exports("x", &rid, Path::new("/x"), &coord());
        let keys: Vec<&str> = env.keys().map(String::as_str).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }
}
