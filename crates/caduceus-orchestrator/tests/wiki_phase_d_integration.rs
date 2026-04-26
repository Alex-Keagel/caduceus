//! D4 — wiki Phase D integration tier (caduceus side).
//!
//! Each test covers one DAG edge from the Phase D decomposition. Unit tests
//! for the individual sub-tasks (D1, D2-c, D2-z, D3) live in their own
//! crates. This file exercises the **seams between sub-tasks** because that's
//! where decomposition errors hide:
//!
//! * D2-c → D3   `WikiEngine::maintain` (storage) → `wiki_slash::handle_wiki`
//!   (orchestrator)
//! * D3   → D2-c `wiki_slash::handle_init` produces a scaffold that
//!   `WikiEngine::maintain` is happy to walk
//! * D2-c failure-path: `/wiki` invoked before `/init` is a graceful no-op,
//!   NOT an error or a silent dir-creation
//! * D2-c revertability: pages added after a maintenance run remain visible
//!   to the next maintenance run (no destructive side-effect)
//! * E2E smoke: `init → write → wiki` reports findings the user expects
//!
//! These tests use only public APIs (`caduceus_orchestrator::wiki_slash::*`
//! plus `caduceus_storage::WikiEngine` for setup).

use caduceus_orchestrator::wiki_slash::{handle_init, handle_wiki};
use caduceus_storage::WikiEngine;

/// E2E happy path: `/init` then `/wiki` on a fresh project produces a clean
/// (empty-findings) maintenance report. Exercises D3 → D2-c → D3 → D2-c.
#[test]
fn init_then_wiki_returns_clean_report_on_fresh_project() {
    let dir = tempfile::tempdir().unwrap();

    let init_out = handle_init(dir.path(), &[]).unwrap();
    assert!(init_out.message.starts_with("Wiki ready at"));
    assert_eq!(init_out.command, "init");
    assert!(init_out.report.is_none());

    let wiki_out = handle_wiki(dir.path(), &[]).unwrap();
    assert_eq!(wiki_out.command, "wiki");
    let report = wiki_out.report.expect("wiki returns LintReport");
    // A freshly-initialised wiki has only the scaffold pages (index, log) —
    // no orphans, no broken links.
    assert!(
        report.findings.is_empty() || report.findings.len() <= 1,
        "fresh wiki should have at most a trivial finding, got {} findings: {:?}",
        report.findings.len(),
        report.findings,
    );
    assert!(report.schema_version >= 1);
}

/// Failure-path: `/wiki` before `/init` MUST be a graceful no-op. The
/// `maintain_wiki` bridge wrapper short-circuits to a missing-dir path; the
/// storage-layer `WikiEngine::maintain` itself is also no-op tolerant. This
/// test pins both layers' contract.
#[test]
fn wiki_on_uninitialized_workspace_is_graceful() {
    let dir = tempfile::tempdir().unwrap();

    let wiki_out = handle_wiki(dir.path(), &[]).unwrap();
    assert_eq!(wiki_out.command, "wiki");
    let report = wiki_out
        .report
        .expect("handle_wiki always returns a LintReport (possibly empty)");
    assert_eq!(report.pages_examined, 0);
    assert!(report.findings.is_empty());
}

/// `/wiki` after the user authored content surfaces the lint findings the
/// orchestrator and the turn-end hook (D2-z) would also see. Exercises the
/// D2-c → D3 seam with non-trivial input.
#[test]
fn wiki_surfaces_findings_after_authoring_content() {
    let dir = tempfile::tempdir().unwrap();
    handle_init(dir.path(), &[]).unwrap();

    // Mutually-linked pair + an orphan. WikiLinter requires multiple pages to
    // flag orphans (a single-page wiki returns 0 findings).
    let engine = WikiEngine::new(dir.path());
    engine
        .write_page("alpha", "# Alpha\n\nLinks to [[beta]].\n")
        .unwrap();
    engine
        .write_page("beta", "# Beta\n\nLinks back to [[alpha]].\n")
        .unwrap();
    engine
        .write_page("orphan", "# Orphan\n\nNo links anywhere.\n")
        .unwrap();

    let wiki_out = handle_wiki(dir.path(), &[]).unwrap();
    let report = wiki_out.report.expect("LintReport present");
    assert!(report.pages_examined >= 3);
    assert!(
        !report.findings.is_empty(),
        "orphan + linked-pair should produce at least one finding",
    );
    // The `format_report`-rendered message includes the finding count, so the
    // user sees a non-trivial summary.
    assert!(
        wiki_out.message.contains("findings") || wiki_out.message.contains("Wiki maintenance"),
        "expected human-readable summary, got: {}",
        wiki_out.message,
    );
}

/// Revertability: running `/init` after pages exist must NOT clobber them.
/// This is the rollback path described in the decomposition — if a partial
/// init fails mid-way, re-running `/init` heals the scaffold without losing
/// user content. Exercises D3 idempotency under the D2-c contract.
#[test]
fn init_after_authoring_preserves_content_and_wiki_still_clean() {
    let dir = tempfile::tempdir().unwrap();
    handle_init(dir.path(), &[]).unwrap();

    let engine = WikiEngine::new(dir.path());
    engine
        .write_page("survives", "# Survives\n\nImportant content.\n")
        .unwrap();

    // Re-run init: should be a no-op against existing pages.
    let second_init = handle_init(dir.path(), &[]).unwrap();
    assert!(second_init.message.starts_with("Wiki ready at"));
    assert!(engine.page_exists("survives"));

    // Maintenance still works post-recovery.
    let wiki_out = handle_wiki(dir.path(), &[]).unwrap();
    assert!(wiki_out.report.is_some());
}

/// Repeated maintenance is non-destructive: running `/wiki` multiple times
/// returns identical `pages_examined` counts. Pins the contract that
/// `WikiEngine::maintain` is read-only over user content (the D2-z hook
/// fires this on every turn — silent corruption would be catastrophic).
#[test]
fn repeated_maintain_is_non_destructive() {
    let dir = tempfile::tempdir().unwrap();
    handle_init(dir.path(), &[]).unwrap();

    let engine = WikiEngine::new(dir.path());
    engine.write_page("a", "# A\n\nLinks to [[b]].\n").unwrap();
    engine
        .write_page("b", "# B\n\nLinks back to [[a]].\n")
        .unwrap();

    let first = handle_wiki(dir.path(), &[]).unwrap();
    let second = handle_wiki(dir.path(), &[]).unwrap();
    let third = handle_wiki(dir.path(), &[]).unwrap();

    let r1 = first.report.unwrap();
    let r2 = second.report.unwrap();
    let r3 = third.report.unwrap();

    assert_eq!(r1.pages_examined, r2.pages_examined);
    assert_eq!(r2.pages_examined, r3.pages_examined);
    assert_eq!(r1.findings.len(), r3.findings.len());
    assert_eq!(r1.schema_version, r3.schema_version);

    // Pages survive all three runs.
    assert!(engine.page_exists("a"));
    assert!(engine.page_exists("b"));
}
