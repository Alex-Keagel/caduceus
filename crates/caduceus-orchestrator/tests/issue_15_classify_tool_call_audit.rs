//! Issue #15 — `classify_tool_call` envelope-enforcement audit.
//!
//! Pins the workaround landed for caduceus#15: every registered tool that
//! has side-effects (write / exec / network) must route through its
//! correct capability check. Tools that fall through to `Read` evade
//! both deny and over-grant logic.
//!
//! These tests are deliberately blunt: they call `preflight_envelope_of`
//! with a closed envelope (everything denies) and assert the outcome is
//! `Deny` for the exact capability string we expect. A regression that
//! drops a tool back to the default Read fallback would either:
//!
//!   * Allow the call (open-all read default) → assertion fails because
//!     outcome is `Allow` instead of `Deny`, OR
//!   * Deny but with `capability == "read"` → assertion fails on the
//!     capability tag.
//!
//! The "right fix" — capability metadata on `ToolRegistry` — is tracked
//! separately. When that lands, this test stays valid because the
//! observable contract (correct capability per tool) doesn't change.

use caduceus_orchestrator::{preflight_envelope_of, PreflightOutcome};
use caduceus_permissions::envelope::{
    ExecPolicy, NetworkPolicy, PathAllowlist, PermissionEnvelope,
};
use serde_json::json;

/// Closed envelope that denies every side-effecting capability.
///
/// We start from `plan_preset` (read open_all, exec disabled) and
/// override `write` to `closed()` and `network` to `disabled()` so:
///
///   * read   → Allow
///   * write  → Deny (NotInAllowList)
///   * network → Deny (NetworkDisabled)
///   * exec   → Deny (ExecDisabled)
///
/// We deliberately don't use `act_preset(vec![], vec![])` — its network
/// is `open()` and exec is `enabled_with_guards()`, so most non-write
/// tools would Allow rather than Deny and our "did this tool route to
/// the right capability?" assertion would silently pass with the wrong
/// reason.
fn closed_envelope() -> PermissionEnvelope {
    let mut env = PermissionEnvelope::plan_preset();
    env.write = PathAllowlist::closed();
    env.network = NetworkPolicy::disabled();
    env.exec = ExecPolicy::disabled();
    env
}

fn assert_deny_with_capability(
    outcome: &PreflightOutcome,
    expected_capability: &str,
    tool_label: &str,
) {
    match outcome {
        PreflightOutcome::Deny { capability, .. } => {
            assert_eq!(
                capability, expected_capability,
                "tool `{tool_label}`: expected capability=`{expected_capability}`, got `{capability}` \
                 — likely fell through to Read default (envelope bypass)"
            );
        }
        PreflightOutcome::Allow => panic!(
            "tool `{tool_label}`: expected Deny under closed envelope, got Allow — \
             classify_tool_call did not route this tool to the {expected_capability} check"
        ),
        PreflightOutcome::Intercept(_) => panic!(
            "tool `{tool_label}`: expected Deny, got Intercept — \
             closed envelope shouldn't trigger plan-mode interception"
        ),
    }
}

// ── Exec class ──────────────────────────────────────────────────────────

#[test]
fn exec_tools_route_to_exec_capability() {
    let env = closed_envelope();
    for (tool, input) in [
        ("bash", json!({ "command": "ls" })),
        ("shell", json!({ "command": "ls" })),
        ("terminal", json!({ "command": "ls" })),
        ("exec", json!({ "command": "ls" })),
        ("run_command", json!({ "command": "ls" })),
        ("unsafe_shell", json!({ "command": "ls" })),
        ("powershell", json!({ "command": "Get-ChildItem" })),
        ("repl", json!({ "code": "print('hi')" })),
    ] {
        let outcome = preflight_envelope_of(&env, tool, &input);
        assert_deny_with_capability(&outcome, "exec", tool);
    }
}

// ── Network class ───────────────────────────────────────────────────────

#[test]
fn network_tools_route_to_network_capability() {
    let env = closed_envelope();
    for (tool, input) in [
        ("web_fetch", json!({ "url": "https://example.com" })),
        ("fetch", json!({ "url": "https://example.com" })),
        ("http_get", json!({ "url": "https://example.com" })),
        ("http_post", json!({ "url": "https://example.com" })),
        ("web_search", json!({ "query": "rust async" })),
        ("browser_action", json!({ "url": "https://example.com" })),
    ] {
        let outcome = preflight_envelope_of(&env, tool, &input);
        assert_deny_with_capability(&outcome, "network", tool);
    }
}

// ── Write class ─────────────────────────────────────────────────────────

#[test]
fn write_tools_route_to_write_capability() {
    let env = closed_envelope();
    for (tool, input) in [
        ("write_file", json!({ "path": "/tmp/x" })),
        ("edit_file", json!({ "path": "/tmp/x" })),
        ("edit", json!({ "path": "/tmp/x" })),
        ("create", json!({ "path": "/tmp/x" })),
        ("create_file", json!({ "path": "/tmp/x" })),
        (
            "apply_patch",
            json!({ "patch": "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n" }),
        ),
        ("move_file", json!({ "path": "/tmp/x" })),
        ("delete_file", json!({ "path": "/tmp/x" })),
        ("rename_file", json!({ "path": "/tmp/x" })),
        ("notebook_edit", json!({ "path": "/tmp/x.ipynb" })),
        ("insert_code", json!({ "path": "/tmp/x.rs" })),
        ("multi_edit", json!({ "path": "/tmp/x" })),
    ] {
        let outcome = preflight_envelope_of(&env, tool, &input);
        assert_deny_with_capability(&outcome, "write", tool);
    }
}

// ── Read default ────────────────────────────────────────────────────────
//
// Read tools should NOT deny under the closed envelope, because Read is
// open-all by default in the act preset (matches caduceus's threat model:
// reads are cheap and informative, side-effects are what need gating).
// This documents the intent — if a future PR closes Read by default,
// these assertions will surface so we revisit the read-side defaults.

#[test]
fn read_tools_allow_under_closed_envelope() {
    let env = closed_envelope();
    for (tool, input) in [
        ("read_file", json!({ "path": "/tmp/x" })),
        ("glob_search", json!({ "pattern": "*.rs" })),
        ("grep_search", json!({ "pattern": "fn" })),
        ("list_files", json!({ "path": "/tmp" })),
        ("git_status", json!({})),
        ("git_diff", json!({})),
        ("tree", json!({ "path": "/tmp" })),
        ("diagnostics", json!({})),
        ("context", json!({})),
    ] {
        let outcome = preflight_envelope_of(&env, tool, &input);
        match outcome {
            PreflightOutcome::Allow => {} // expected
            PreflightOutcome::Deny { capability, .. } => panic!(
                "read-class tool `{tool}` denied unexpectedly with capability=`{capability}` \
                 — closed envelope should still allow reads"
            ),
            PreflightOutcome::Intercept(_) => panic!(
                "read-class tool `{tool}` was intercepted — reads should not trigger plan-mode interception"
            ),
        }
    }
}

// ── Resource extraction sanity ──────────────────────────────────────────
//
// Pin the resource string so we know the deny message carries the right
// detail. A regression in the get_str fallback chain would surface here.

#[test]
fn powershell_resource_extracts_command() {
    let env = closed_envelope();
    let out = preflight_envelope_of(
        &env,
        "powershell",
        &json!({ "command": "Get-Process notepad" }),
    );
    match out {
        PreflightOutcome::Deny { resource, .. } => {
            assert_eq!(resource, "Get-Process notepad");
        }
        other => panic!("expected Deny, got {other:?}"),
    }
}

#[test]
fn repl_resource_extracts_code() {
    let env = closed_envelope();
    let out = preflight_envelope_of(&env, "repl", &json!({ "code": "1 + 1" }));
    match out {
        PreflightOutcome::Deny { resource, .. } => {
            assert_eq!(resource, "1 + 1");
        }
        other => panic!("expected Deny, got {other:?}"),
    }
}

#[test]
fn browser_action_resource_extracts_url() {
    let env = closed_envelope();
    let out = preflight_envelope_of(
        &env,
        "browser_action",
        &json!({ "action_type": "navigate", "url": "https://evil.example.com/x" }),
    );
    match out {
        PreflightOutcome::Deny { resource, .. } => {
            assert_eq!(resource, "https://evil.example.com/x");
        }
        other => panic!("expected Deny, got {other:?}"),
    }
}

#[test]
fn notebook_edit_resource_extracts_path() {
    let env = closed_envelope();
    let out = preflight_envelope_of(
        &env,
        "notebook_edit",
        &json!({ "path": "/tmp/x.ipynb", "cell_index": 0, "new_source": "print(1)" }),
    );
    match out {
        PreflightOutcome::Deny { resource, .. } => {
            assert_eq!(resource, "/tmp/x.ipynb");
        }
        other => panic!("expected Deny, got {other:?}"),
    }
}

// ── apply_patch destination-path preflight (PR-A tri-review followup) ───
//
// `apply_patch` doesn't carry its target paths in `path` / `file` / etc.
// — they live inside the unified-diff `+++ b/<path>` headers. Without
// parsing those headers, sensitive-path enforcement always saw
// `<unknown>` and a malicious patch editing `private/**` would slip
// through. These tests pin the parsing + worst-offender selection.

/// A patch whose destination is `private/notes.md` must Deny with
/// capability=write under the act preset (which protects `private/**`
/// via its sensitive list).
#[test]
fn apply_patch_preflight_blocks_sensitive_destination() {
    // Use the act preset — wide allow list, but private/** is in the
    // sensitive list and should win.
    let env = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
    let patch = "--- a/private/notes.md\n+++ b/private/notes.md\n@@ -1 +1 @@\n-old\n+new\n";
    let outcome = preflight_envelope_of(&env, "apply_patch", &json!({ "patch": patch }));
    match outcome {
        PreflightOutcome::Deny {
            capability,
            resource,
            ..
        } => {
            assert_eq!(capability, "write");
            assert_eq!(resource, "private/notes.md");
        }
        other => panic!("expected Deny for sensitive destination, got {other:?}"),
    }
}

/// A patch with mixed destinations — one safe, one sensitive — must
/// surface the sensitive denial (worst-offender ranking).
#[test]
fn apply_patch_preflight_picks_worst_offender() {
    let env = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
    let patch = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-x\n+y\n\
                 --- a/private/secret.md\n+++ b/private/secret.md\n@@ -1 +1 @@\n-x\n+y\n";
    let outcome = preflight_envelope_of(&env, "apply_patch", &json!({ "patch": patch }));
    match outcome {
        PreflightOutcome::Deny {
            capability,
            resource,
            ..
        } => {
            assert_eq!(capability, "write");
            assert_eq!(resource, "private/secret.md");
        }
        other => panic!("expected Deny on the sensitive entry, got {other:?}"),
    }
}

/// A patch with no parseable target headers must fail closed
/// (deny=write) — we never let an unparseable apply_patch slip past
/// envelope enforcement.
#[test]
fn apply_patch_preflight_fails_closed_on_unparseable_patch() {
    let env = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
    let outcome = preflight_envelope_of(&env, "apply_patch", &json!({ "patch": "garbage" }));
    match outcome {
        PreflightOutcome::Deny {
            capability,
            resource,
            ..
        } => {
            assert_eq!(capability, "write");
            assert!(
                resource.contains("apply_patch"),
                "got resource {resource:?}"
            );
        }
        other => panic!("expected fail-closed Deny, got {other:?}"),
    }
}

/// A patch with a missing `patch` field entirely must also fail closed.
#[test]
fn apply_patch_preflight_fails_closed_on_missing_patch_field() {
    let env = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
    let outcome = preflight_envelope_of(&env, "apply_patch", &json!({}));
    match outcome {
        PreflightOutcome::Deny { capability, .. } => {
            assert_eq!(capability, "write");
        }
        other => panic!("expected fail-closed Deny, got {other:?}"),
    }
}
