//! P13.1 — Lex-only path normalisation, shared across the speculative
//! cache (`caduceus_tools::SpecKey`) and the resource-key extractor used
//! by the parallel-tool dispatcher (`caduceus_mcp::resource_keys`).
//!
//! Why **lex-only**: this code runs inside hot paths (tool dispatch,
//! cache hashing) where touching the filesystem is forbidden — `stat()`
//! / `realpath()` block, can race with the very edits we're trying to
//! serialise, and would also fail for paths that don't exist yet
//! (`write_file` of a new file). Lex-only normalisation gives us the
//! single biggest correctness win — collapsing equivalent textual
//! representations of the same path — without buying any of those
//! problems.
//!
//! Rules (deterministic and pure):
//! 1. Backslash separators (`\\`) are folded to forward slashes so the
//!    cache key for `src\\foo.rs` and `src/foo.rs` is the same on every
//!    OS. We do this before splitting; downstream the path stays in
//!    forward-slash form.
//! 2. Repeated separators are collapsed (`a//b///c` → `a/b/c`).
//! 3. `.` segments are dropped.
//! 4. `..` segments pop the previous concrete segment when one exists
//!    *and* it isn't itself `..`. When the path is relative and the
//!    stack is empty (or only contains leading `..`), `..` is preserved
//!    so we don't silently escape upward — `../foo` stays `../foo`,
//!    `bar/../foo` becomes `foo`, `../../foo` stays `../../foo`.
//! 5. An absolute path's leading `/` is preserved; `..` cannot pop
//!    above the root (`/a/../../b` → `/b`).
//! 6. Trailing slashes are stripped except for the bare root (`/`).
//! 7. The empty string normalises to `.` (the canonical "current dir")
//!    so cache keys and lock keys never collide on `""`.
//!
//! Cited motivation: SWE-agent (Yang et al., NeurIPS 2024
//! arXiv:2405.15793) showed that strongly-typed file/edit tool
//! interfaces lift SWE-bench pass@1 from 1.7 % → 12.5 %. Path
//! normalisation is the simplest piece of that contract: equivalent
//! references must hash to one key.

/// Lex-only normalise a path-shaped string. Pure; never touches the
/// filesystem. See module docs for the exact rules.
pub fn normalize_lex(input: &str) -> String {
    if input.is_empty() {
        return ".".to_string();
    }
    // Fold backslashes early so the rest of the algorithm only deals
    // with one separator. We deliberately do NOT try to parse Windows
    // drive letters — caduceus runs on Linux/macOS in practice and
    // treating `C:\\foo` as a relative path is fine for cache-key
    // purposes (it would still hash consistently with itself).
    let folded: String = input.chars().map(|c| if c == '\\' { '/' } else { c }).collect();

    let is_absolute = folded.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();

    for raw in folded.split('/') {
        match raw {
            "" | "." => continue,
            ".." => match stack.last() {
                Some(&top) if top != ".." => {
                    stack.pop();
                }
                _ => {
                    if !is_absolute {
                        // Relative path with no concrete segment to
                        // pop — preserve the leading `..` so we don't
                        // silently change semantics.
                        stack.push("..");
                    }
                    // Absolute path: `..` at root is a no-op.
                }
            },
            seg => stack.push(seg),
        }
    }

    let mut out = String::with_capacity(input.len());
    if is_absolute {
        out.push('/');
    }
    if stack.is_empty() {
        if is_absolute {
            return out;
        }
        return ".".to_string();
    }
    for (i, seg) in stack.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(seg);
    }
    out
}

/// Field names — in priority order — that are treated as carrying a
/// filesystem-path-shaped value. Both the speculative cache and the
/// MCP resource-key extractor pull from this list so they agree on
/// which JSON fields participate in lex normalisation.
pub const PATH_LIKE_FIELDS: &[&str] = &[
    "path",
    "file",
    "filename",
    "filepath",
    "file_path",
    "src",
    "dest",
    "destination",
    "from",
    "to",
];

/// Returns `true` if a given JSON field name is treated as path-shaped
/// for normalisation purposes.
pub fn is_path_like_field(name: &str) -> bool {
    PATH_LIKE_FIELDS.iter().any(|&p| p == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p13_1_dot_segments_dropped() {
        assert_eq!(normalize_lex("./foo"), "foo");
        assert_eq!(normalize_lex("foo/./bar"), "foo/bar");
        assert_eq!(normalize_lex("./"), ".");
        assert_eq!(normalize_lex("."), ".");
    }

    #[test]
    fn p13_1_dotdot_pops_relative_segment() {
        assert_eq!(normalize_lex("foo/../bar"), "bar");
        assert_eq!(normalize_lex("a/b/../c"), "a/c");
        assert_eq!(normalize_lex("a/b/c/../.."), "a");
    }

    #[test]
    fn p13_1_dotdot_preserved_when_no_segment_to_pop() {
        // Critical: must NOT silently turn ../foo into foo —
        // they refer to different files.
        assert_eq!(normalize_lex("../foo"), "../foo");
        assert_eq!(normalize_lex("../../foo"), "../../foo");
        assert_eq!(normalize_lex(".."), "..");
    }

    #[test]
    fn p13_1_absolute_root_anchored() {
        assert_eq!(normalize_lex("/a/b/../c"), "/a/c");
        // Absolute `..` at root is a no-op (cannot escape filesystem
        // root by string manipulation).
        assert_eq!(normalize_lex("/../../etc"), "/etc");
        assert_eq!(normalize_lex("/"), "/");
        assert_eq!(normalize_lex("/foo/"), "/foo");
    }

    #[test]
    fn p13_1_collapses_duplicate_separators() {
        assert_eq!(normalize_lex("a//b///c"), "a/b/c");
        assert_eq!(normalize_lex("//root//"), "/root");
    }

    #[test]
    fn p13_1_backslash_folded() {
        assert_eq!(normalize_lex("src\\foo.rs"), "src/foo.rs");
        assert_eq!(normalize_lex("src\\..\\bar"), "bar");
    }

    #[test]
    fn p13_1_empty_input_is_dot() {
        assert_eq!(normalize_lex(""), ".");
    }

    #[test]
    fn p13_1_equivalent_paths_collapse_to_same_string() {
        // The whole reason this module exists: every textual variant
        // of the same logical path must hash to the same key.
        let canon = normalize_lex("src/foo.rs");
        assert_eq!(normalize_lex("./src/foo.rs"), canon);
        assert_eq!(normalize_lex("src/./foo.rs"), canon);
        assert_eq!(normalize_lex("src/../src/foo.rs"), canon);
        assert_eq!(normalize_lex("src//foo.rs"), canon);
        assert_eq!(normalize_lex("src\\foo.rs"), canon);
        assert_eq!(normalize_lex("a/b/../../src/foo.rs"), canon);
    }

    #[test]
    fn p13_1_is_path_like_field_known_set() {
        assert!(is_path_like_field("path"));
        assert!(is_path_like_field("file"));
        assert!(is_path_like_field("filepath"));
        assert!(is_path_like_field("src"));
        assert!(!is_path_like_field("uri"));
        assert!(!is_path_like_field("url"));
        assert!(!is_path_like_field("name"));
    }
}
