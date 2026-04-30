//! Workspace identity + path primitives (P1 foundations).
//!
//! Per the implementation DAG, this module ships the pure-function P1
//! foundation todos:
//!
//! - **`ws01-workspace-id-derivation`** — `workspace_id(...)` per spec
//!   #3 §I-6.  BLAKE3-128 keyed mode with a 32-byte daemon-instance key.
//!   Iter-28 #3-3 + #3-10 absorbed (32-byte key, `safe_run_id` input).
//! - **`ws02-sanitize-run-id`** — `sanitize_run_id(run_id)` per spec
//!   #3 §3.2.  Rejects path traversal, dot directories, and over-long
//!   inputs.  Iter-28 #3-1 absorbed (regex consistency).
//! - **`ws02b-sanitize-repo-slug`** — `sanitize_repo_slug(remote_url)`
//!   per spec #3 §3.1.  Host-prefixed canonical form; collision-rewrite
//!   is policy in `ws05-registry-store`.  Sticky once recorded (I-4).
//! - **`ws03-build-workspace-path`** — `build_workspace_path(...)`
//!   per spec #3 §3.3.  Pure string construction; no filesystem.
//! - **`ws04-validate-workspace-path`** — `validate_workspace_path(...)`
//!   per spec #3 §3.4.  Pre-canonicalization `..` rejection +
//!   longest-existing-prefix canonicalization (Symphony port from
//!   `workspace.ex:358-384`).
//!
//! Higher-level workspace operations (`create_workspace`,
//! `cleanup_workspace`, `OrphanReclaim`) build on these primitives in
//! later DAG phases.

use crate::error::WorkspaceError;
use std::path::{Component, Path, PathBuf};

/// Daemon-instance-stable BLAKE3 key (32 bytes).  Spec #3 I-6 mandates a
/// 32-byte key; iter-28 #3-10 corrected the 16-byte typo in the spec.
///
/// The key MUST be derived deterministically from the daemon's
/// `workspace_root` so two daemons running against the same root
/// compute identical workspace_ids — but it MUST NOT be a security
/// secret (workspace_id is for diagnostics, not auth).
#[derive(Debug, Clone, Copy)]
pub struct WorkspaceIdKey([u8; 32]);

impl WorkspaceIdKey {
    /// Derive a key from the canonicalized `workspace_root` path plus a
    /// hardcoded domain separator.  Stable across daemon restarts on
    /// the same root.
    pub fn derive(workspace_root: &Path) -> Self {
        const DOMAIN: &[u8] = b"caduceus.workspace_id.v1";
        let mut h = blake3::Hasher::new();
        h.update(DOMAIN);
        h.update(b"\x1F");
        h.update(workspace_root.as_os_str().as_encoded_bytes());
        let digest = h.finalize();
        let mut key = [0u8; 32];
        key.copy_from_slice(digest.as_bytes());
        Self(key)
    }

    /// Construct from raw bytes.  Test/fixture only.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Sanitized run identifier.  Wrapper around the inner `String` to make
/// it impossible to accidentally pass an unvalidated `run_id` to
/// `build_workspace_path` or `workspace_id`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SafeRunId(String);

impl SafeRunId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Test-only constructor; bypasses `sanitize_run_id`.  Production
    /// code MUST go through `sanitize_run_id`.
    #[doc(hidden)]
    pub fn from_string_unchecked(s: String) -> Self {
        Self(s)
    }
}
impl std::fmt::Display for SafeRunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Sanitized repo slug.  Sticky once recorded in the registry (I-4).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoSlug(String);

impl RepoSlug {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Test-only constructor.
    #[doc(hidden)]
    pub fn from_string_unchecked(s: String) -> Self {
        Self(s)
    }
}
impl std::fmt::Display for RepoSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ──────────────────────────── ws02-sanitize-run-id ───────────────────────────

/// Spec #3 §3.2.  Validate + sanitize a raw `run_id` into a filesystem-safe
/// segment.  Idempotent.
pub fn sanitize_run_id(run_id: &str) -> Result<SafeRunId, WorkspaceError> {
    // Rule 1: empty or > 128 bytes UTF-8.
    if run_id.is_empty() || run_id.len() > 128 {
        return Err(WorkspaceError::InvalidRunId(run_id.to_string()));
    }
    // Defence-in-depth (rule 5, pre-canonicalization): reject `..` in the
    // raw input.  This is what catches `../etc/passwd` per the §3.2
    // worked example, where the post-trim result would otherwise be
    // `etc_passwd` (i.e., would silently accept obvious traversal).
    if run_id.contains("..") {
        return Err(WorkspaceError::InvalidRunId(run_id.to_string()));
    }
    // Rule 2: replace each maximal run of non-[A-Za-z0-9._-] with `_`.
    let mut buf = String::with_capacity(run_id.len());
    let mut last_was_underscore = false;
    for ch in run_id.chars() {
        if is_run_id_char(ch) {
            buf.push(ch);
            last_was_underscore = false;
        } else if !last_was_underscore {
            buf.push('_');
            last_was_underscore = true;
        }
    }
    // Rule 3: trim leading and trailing `_` and `.`.
    let trimmed = buf.trim_matches(|c| c == '_' || c == '.').to_string();
    // Rule 4: reject `.`, `..`, empty, all-dots.
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '.') {
        return Err(WorkspaceError::InvalidRunId(run_id.to_string()));
    }
    // Rule 5 (post-canonicalization pass): also reject `..` substring in
    // the result (defence-in-depth).
    if trimmed.contains("..") {
        return Err(WorkspaceError::InvalidRunId(run_id.to_string()));
    }
    // Re-validate: result MUST match `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`.
    if !run_id_regex_match(&trimmed) {
        return Err(WorkspaceError::InvalidRunId(run_id.to_string()));
    }
    Ok(SafeRunId(trimmed))
}

fn is_run_id_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-'
}

/// Run_id canonical regex: `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`.
fn run_id_regex_match(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(is_run_id_char)
}

// ─────────────────────────── ws02b-sanitize-repo-slug ────────────────────────

/// Spec #3 §3.1.  Convert a repo's remote URL into a filesystem-safe slug.
///
/// Produces a `RepoSlug` matching `^[a-z0-9][a-z0-9_]{0,63}$`.  Host
/// segment is included unconditionally (N-5 fix; same-host omission is
/// FORBIDDEN).  Collision rewrites are applied at registry-write time
/// (`ws05-registry-store` / `ws06-registry-store`), not here.
pub fn sanitize_repo_slug(remote_url: &str) -> Result<RepoSlug, WorkspaceError> {
    let (host, path) = parse_remote_url(remote_url)
        .ok_or_else(|| WorkspaceError::InvalidRepoSlug(remote_url.to_string()))?;

    // Step 2: strip a single trailing `.git` from path.
    let path = path.strip_suffix(".git").unwrap_or(&path);
    // Step 3: strip leading `/`.
    let path = path.trim_start_matches('/');

    // Step 4: lowercase host + path.
    let host = host.to_ascii_lowercase();
    let path = path.to_ascii_lowercase();

    // Step 5+6: replace runs of non-[a-z0-9] with `_`, trim leading/trailing `_`.
    let host_seg = collapse_to_underscores(&host);
    let path_seg = collapse_to_underscores(&path);

    if host_seg.is_empty() {
        return Err(WorkspaceError::InvalidRepoSlug(remote_url.to_string()));
    }

    // Step 7: slug_body = host + "_" + path (host always included).
    let mut slug_body = if path_seg.is_empty() {
        host_seg.clone()
    } else {
        format!("{host_seg}_{path_seg}")
    };

    // Step 8: if > 64, truncate to 56 + "_" + first 7 hex of BLAKE3-128 over canonical (host, path).
    if slug_body.len() > 64 {
        let canon = format!("{host}/{path}");
        let h = blake3::hash(canon.as_bytes());
        let hex7: String = h.to_hex().chars().take(7).collect();
        let head: String = slug_body.chars().take(56).collect();
        slug_body = format!("{head}_{hex7}");
        debug_assert!(slug_body.len() <= 64, "slug_body must fit 64");
    }

    // Final shape check.
    if !slug_regex_match(&slug_body) {
        return Err(WorkspaceError::InvalidRepoSlug(remote_url.to_string()));
    }
    Ok(RepoSlug(slug_body))
}

/// Parse a remote URL into `(host, path)`.  Accepts:
/// - `https://github.com/owner/repo`
/// - `https://github.com/owner/repo.git`
/// - `git@github.com:owner/repo.git`
/// - `ssh://git@github.com/owner/repo.git`
fn parse_remote_url(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    // Form: "scheme://[user@]host/path"
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://"))
    {
        // Optionally strip "user@" prefix from authority.
        let rest = if let Some(idx) = rest.find('@') {
            // user@authority/path — strip user.
            // But only if `@` appears before the first `/`.
            let slash = rest.find('/').unwrap_or(rest.len());
            if idx < slash {
                &rest[idx + 1..]
            } else {
                rest
            }
        } else {
            rest
        };
        let (authority, path) = match rest.split_once('/') {
            Some((a, p)) => (a.to_string(), p.to_string()),
            None => (rest.to_string(), String::new()),
        };
        // Strip ":port" from authority.
        let host = authority
            .split_once(':')
            .map(|(h, _)| h.to_string())
            .unwrap_or(authority);
        if host.is_empty() {
            return None;
        }
        return Some((host, path));
    }
    // Form: "user@host:path" (scp-like).
    if let Some((before, after)) = url.split_once(':') {
        if let Some((_user, host)) = before.split_once('@') {
            return Some((host.to_string(), after.to_string()));
        }
    }
    None
}

/// Replace each maximal run of non-[a-z0-9] with `_`, then trim leading/trailing `_`.
fn collapse_to_underscores(s: &str) -> String {
    let mut buf = String::with_capacity(s.len());
    let mut last_was_underscore = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            buf.push(ch.to_ascii_lowercase());
            last_was_underscore = false;
        } else if !last_was_underscore {
            buf.push('_');
            last_was_underscore = true;
        }
    }
    buf.trim_matches('_').to_string()
}

fn slug_regex_match(s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

// ────────────────────────── ws03-build-workspace-path ────────────────────────

/// Spec #3 §3.3.  Pure path construction.  Does NOT touch the filesystem.
pub fn build_workspace_path(
    workspace_root: &Path,
    slug: &RepoSlug,
    safe_run_id: &SafeRunId,
) -> Result<PathBuf, WorkspaceError> {
    // Rule 1: workspace_root MUST be absolute.
    if !workspace_root.is_absolute() {
        return Err(WorkspaceError::PathValidationFailed(format!(
            "workspace_root MUST be absolute, got: {}",
            workspace_root.display()
        )));
    }
    // Rule 2 + 3: slug + run_id are pre-validated by their newtypes.
    debug_assert!(slug_regex_match(slug.as_str()));
    debug_assert!(run_id_regex_match(safe_run_id.as_str()));

    let mut p = workspace_root.to_path_buf();
    p.push(slug.as_str());
    p.push(safe_run_id.as_str());
    // Trailing slash form: append empty component.
    let mut s = p.into_os_string();
    s.push(std::path::MAIN_SEPARATOR_STR);
    let path = PathBuf::from(s);

    // Defence-in-depth: assert no `.`/`..` components after construction.
    for comp in path.components() {
        match comp {
            Component::CurDir | Component::ParentDir => {
                return Err(WorkspaceError::PathValidationFailed(format!(
                    "constructed path contains illegal component: {}",
                    path.display()
                )));
            }
            _ => {}
        }
    }
    Ok(path)
}

// ───────────────────────── ws04-validate-workspace-path ──────────────────────

/// Spec #3 §3.4.  Verify a candidate workspace path is safe to act on.
///
/// Performs the **pre-canonicalization** rejects (rule 1) and the
/// **longest-existing-prefix** canonicalization (rule 2).  The full
/// Symphony port at `workspace.ex:358-384` is preserved in observable
/// behaviour.
///
/// `path` MAY be hostile input (e.g. a registry row read after a
/// tampered DB).  The function MUST NOT trust it.
pub fn validate_workspace_path(
    path: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, WorkspaceError> {
    // Rule 1: pre-canonicalization rejects of literal `..`.
    for comp in path.components() {
        if comp == Component::ParentDir {
            return Err(WorkspaceError::PathValidationFailed(format!(
                "path contains `..`: {}",
                path.display()
            )));
        }
    }
    // Workspace_root must be canonical and absolute (caller responsibility).
    if !workspace_root.is_absolute() {
        return Err(WorkspaceError::PathValidationFailed(format!(
            "workspace_root MUST be absolute: {}",
            workspace_root.display()
        )));
    }
    // Rule 2: walk left-to-right; find longest existing prefix; canonicalize it;
    // append the remaining (non-existent) suffix unchanged.
    let mut existing_prefix = PathBuf::new();
    let mut suffix_components: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = PathBuf::new();
    let mut found_first_missing = false;
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => {
                current.push(comp.as_os_str());
                existing_prefix.push(comp.as_os_str());
            }
            Component::Normal(name) => {
                if found_first_missing {
                    suffix_components.push(name);
                } else {
                    current.push(name);
                    if current.symlink_metadata().is_ok() {
                        existing_prefix.push(name);
                    } else {
                        found_first_missing = true;
                        suffix_components.push(name);
                    }
                }
            }
            Component::CurDir => { /* skip */ }
            Component::ParentDir => unreachable!("filtered above"),
        }
    }
    // Canonicalize the existing prefix (resolves any symlinks under it).
    let canon_prefix = if existing_prefix.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        existing_prefix.canonicalize().map_err(|e| {
            WorkspaceError::PathValidationFailed(format!(
                "canonicalize({}): {e}",
                existing_prefix.display()
            ))
        })?
    };
    // Re-check: the canonicalized prefix MUST still be inside workspace_root.
    let canon_root = workspace_root.canonicalize().map_err(|e| {
        WorkspaceError::PathValidationFailed(format!(
            "canonicalize(workspace_root={}): {e}",
            workspace_root.display()
        ))
    })?;
    if !canon_prefix.starts_with(&canon_root) {
        return Err(WorkspaceError::PathValidationFailed(format!(
            "path escapes workspace_root: canonicalized to {}",
            canon_prefix.display()
        )));
    }
    // Reassemble.
    let mut result = canon_prefix;
    for c in suffix_components {
        result.push(c);
    }
    Ok(result)
}

// ─────────────────────────── ws01-workspace-id-derivation ────────────────────

/// Spec #3 I-6.  Derive the `wsp_<hex>` workspace identifier.  Iter-28
/// #3-3 + #3-10 absorbed: 32-byte key; `safe_run_id` (sanitized) input.
pub fn workspace_id(key: &WorkspaceIdKey, slug: &RepoSlug, safe_run_id: &SafeRunId) -> String {
    let mut h = blake3::Hasher::new_keyed(key.as_bytes());
    h.update(slug.as_str().as_bytes());
    h.update(b"\x1F");
    h.update(safe_run_id.as_str().as_bytes());
    let digest = h.finalize();
    // 128-bit identifier == first 16 bytes of the digest.
    let bytes = &digest.as_bytes()[..16];
    let mut s = String::with_capacity(4 + 32);
    s.push_str("wsp_");
    for b in bytes {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").unwrap();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ─── sanitize_run_id ────────────────────────────────────────────────

    #[test]
    fn sanitize_run_id_passes_clean_ulid() {
        let s = sanitize_run_id("01H8XYZABC").unwrap();
        assert_eq!(s.as_str(), "01H8XYZABC");
    }

    #[test]
    fn sanitize_run_id_collapses_slashes_to_underscores() {
        let s = sanitize_run_id("run/2024-04-10/abc").unwrap();
        assert_eq!(s.as_str(), "run_2024-04-10_abc");
    }

    #[test]
    fn sanitize_run_id_rejects_path_traversal() {
        assert!(sanitize_run_id("../etc/passwd").is_err());
    }

    #[test]
    fn sanitize_run_id_rejects_dot_dirs() {
        assert!(sanitize_run_id("..").is_err());
        assert!(sanitize_run_id(".").is_err());
        assert!(sanitize_run_id("...").is_err());
    }

    #[test]
    fn sanitize_run_id_rejects_empty() {
        assert!(sanitize_run_id("").is_err());
        assert!(sanitize_run_id("   ").is_err());
    }

    #[test]
    fn sanitize_run_id_rejects_too_long() {
        let long = "a".repeat(129);
        assert!(sanitize_run_id(&long).is_err());
    }

    #[test]
    fn sanitize_run_id_is_idempotent() {
        for raw in ["abc", "01H8XYZ", "run-2024-04-10", "a.b.c", "X9_y-z"] {
            let once = sanitize_run_id(raw).unwrap();
            let twice = sanitize_run_id(once.as_str()).unwrap();
            assert_eq!(once.as_str(), twice.as_str(), "non-idempotent on `{raw}`");
        }
    }

    #[test]
    fn sanitize_run_id_first_char_must_be_alnum() {
        // After trim, leading "_" and "." are removed; if result starts
        // with "-" or any non-alnum it is rejected by the regex.
        // "-abc" trims nothing (- is not in trim chars) so first char is "-" → reject.
        assert!(sanitize_run_id("-abc").is_err());
    }

    #[test]
    fn sanitize_run_id_rejects_double_dot_substring_after_collapse() {
        // "a..b" survives the run-collapse (. stays); rule 5 catches it.
        assert!(sanitize_run_id("a..b").is_err());
    }

    // ─── sanitize_repo_slug ─────────────────────────────────────────────

    #[test]
    fn sanitize_repo_slug_https_basic() {
        let s = sanitize_repo_slug("https://github.com/openai/symphony").unwrap();
        assert_eq!(s.as_str(), "github_com_openai_symphony");
    }

    #[test]
    fn sanitize_repo_slug_strips_dot_git() {
        let s = sanitize_repo_slug("https://github.com/openai/symphony.git").unwrap();
        assert_eq!(s.as_str(), "github_com_openai_symphony");
    }

    #[test]
    fn sanitize_repo_slug_scp_form() {
        let s = sanitize_repo_slug("git@github.com:openai/symphony.git").unwrap();
        assert_eq!(s.as_str(), "github_com_openai_symphony");
    }

    #[test]
    fn sanitize_repo_slug_ssh_with_user() {
        let s = sanitize_repo_slug("ssh://git@github.com/openai/symphony.git").unwrap();
        assert_eq!(s.as_str(), "github_com_openai_symphony");
    }

    #[test]
    fn sanitize_repo_slug_case_folded() {
        let s = sanitize_repo_slug("https://github.com/Openai/Symphony").unwrap();
        assert_eq!(s.as_str(), "github_com_openai_symphony");
    }

    #[test]
    fn sanitize_repo_slug_truncates_long_to_64_with_hash() {
        let url = "https://example.com/a/very/deeply/nested/path/to/some/repo.git";
        let s = sanitize_repo_slug(url).unwrap();
        assert!(s.as_str().len() <= 64);
        // First 56 chars + "_" + 7 hex chars.
        assert!(
            s.as_str().contains('_'),
            "truncated form should contain underscore separator"
        );
        // Must still match shape regex.
        assert!(slug_regex_match(s.as_str()), "truncated form invalid: {s}");
    }

    #[test]
    fn sanitize_repo_slug_rejects_empty_or_garbage() {
        assert!(sanitize_repo_slug("").is_err());
        assert!(sanitize_repo_slug("not a url").is_err());
        assert!(sanitize_repo_slug("https://").is_err());
    }

    #[test]
    fn sanitize_repo_slug_includes_host_unconditionally() {
        // Even for single-host deployments, host MUST be in the slug
        // (N-5 fix; spec §3.1 worked example).
        let s = sanitize_repo_slug("https://gitlab.com/acme/app").unwrap();
        assert!(s.as_str().starts_with("gitlab_com_"));
    }

    // ─── build_workspace_path ───────────────────────────────────────────

    #[test]
    fn build_workspace_path_appends_slug_and_run_id() {
        let root = PathBuf::from("/var/lib/caduceus");
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        let rid = sanitize_run_id("01H8XYZ").unwrap();
        let p = build_workspace_path(&root, &slug, &rid).unwrap();
        assert!(p.starts_with(&root));
        assert!(p.ends_with("github_com_o_r/01H8XYZ/"));
        // Ends in MAIN_SEPARATOR.
        let s = p.to_string_lossy();
        assert!(s.ends_with(std::path::MAIN_SEPARATOR));
    }

    #[test]
    fn build_workspace_path_rejects_relative_root() {
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        let rid = sanitize_run_id("x").unwrap();
        assert!(build_workspace_path(Path::new("relative/root"), &slug, &rid).is_err());
    }

    // ─── validate_workspace_path ─────────────────────────────────────────

    #[test]
    fn validate_workspace_path_rejects_literal_dotdot() {
        let p = PathBuf::from("/var/lib/caduceus/foo/../bar");
        let r = validate_workspace_path(&p, Path::new("/var/lib/caduceus"));
        assert!(r.is_err());
    }

    #[test]
    fn validate_workspace_path_canonicalizes_existing_prefix() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().canonicalize().unwrap();
        // Construct: root/slug/run_id/leaf  where slug exists.
        let slug_dir = root.join("github_com_o_r");
        std::fs::create_dir_all(&slug_dir).unwrap();
        let candidate = slug_dir.join("01H8XYZ").join("");
        let result = validate_workspace_path(&candidate, &root).unwrap();
        // Result should still start with the canonicalized root.
        assert!(result.starts_with(&root));
    }

    #[test]
    fn validate_workspace_path_rejects_escape_via_symlink() {
        // Construct workspace_root with a symlinked subdir that points
        // outside the root. validate_workspace_path MUST reject it.
        let td = tempfile::tempdir().unwrap();
        let root = td.path().canonicalize().unwrap();
        let outside = td.path().parent().unwrap().to_path_buf();
        let symlink_path = root.join("escape");
        // Create a symlink "escape" -> outside.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &symlink_path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &symlink_path).unwrap();
        let candidate = symlink_path.join("leaf");
        let r = validate_workspace_path(&candidate, &root);
        assert!(r.is_err(), "must reject symlink-escape, got: {r:?}");
    }

    // ─── workspace_id ────────────────────────────────────────────────────

    #[test]
    fn workspace_id_format_is_wsp_plus_32_hex() {
        let key = WorkspaceIdKey::derive(Path::new("/var/lib/caduceus"));
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        let rid = sanitize_run_id("01H8XYZ").unwrap();
        let id = workspace_id(&key, &slug, &rid);
        assert!(id.starts_with("wsp_"));
        assert_eq!(id.len(), 4 + 32, "wsp_ + 32 hex chars (128-bit ID)");
        assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn workspace_id_is_deterministic() {
        let key = WorkspaceIdKey::derive(Path::new("/var/lib/caduceus"));
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        let rid = sanitize_run_id("01H8XYZ").unwrap();
        let id1 = workspace_id(&key, &slug, &rid);
        let id2 = workspace_id(&key, &slug, &rid);
        assert_eq!(id1, id2);
    }

    #[test]
    fn workspace_id_changes_with_slug_or_run_id() {
        let key = WorkspaceIdKey::derive(Path::new("/var/lib/caduceus"));
        let slug_a = sanitize_repo_slug("https://github.com/a/r").unwrap();
        let slug_b = sanitize_repo_slug("https://github.com/b/r").unwrap();
        let rid = sanitize_run_id("01H8XYZ").unwrap();
        assert_ne!(
            workspace_id(&key, &slug_a, &rid),
            workspace_id(&key, &slug_b, &rid)
        );
        let rid2 = sanitize_run_id("02ABCDE").unwrap();
        assert_ne!(
            workspace_id(&key, &slug_a, &rid),
            workspace_id(&key, &slug_a, &rid2)
        );
    }

    #[test]
    fn workspace_id_changes_with_workspace_root_via_key_derivation() {
        // Two daemons with different roots derive different keys, hence
        // different workspace_ids — desired (no cross-root collision).
        let key1 = WorkspaceIdKey::derive(Path::new("/var/lib/cd1"));
        let key2 = WorkspaceIdKey::derive(Path::new("/var/lib/cd2"));
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        let rid = sanitize_run_id("01H8XYZ").unwrap();
        assert_ne!(
            workspace_id(&key1, &slug, &rid),
            workspace_id(&key2, &slug, &rid)
        );
    }

    #[test]
    fn workspace_id_uses_safe_run_id_consistently() {
        // Iter-28 #3-3 — same logical run with a non-canonical raw input
        // should produce the same workspace_id once sanitized, because
        // the function takes SafeRunId not raw &str.
        let key = WorkspaceIdKey::derive(Path::new("/var/lib/caduceus"));
        let slug = sanitize_repo_slug("https://github.com/o/r").unwrap();
        let rid_a = sanitize_run_id("run/abc").unwrap(); // → "run_abc"
        let rid_b = sanitize_run_id("run//abc").unwrap(); // → "run_abc" (collapse)
        assert_eq!(rid_a.as_str(), rid_b.as_str());
        assert_eq!(
            workspace_id(&key, &slug, &rid_a),
            workspace_id(&key, &slug, &rid_b)
        );
    }
}
