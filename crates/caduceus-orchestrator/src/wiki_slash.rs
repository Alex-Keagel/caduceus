//! Slash-command handlers for `/wiki` and `/init`.
//!
//! Wave-2 wiki Phase D sub-task D3. The REPL in `headless.rs` parses bare
//! slash commands into a `ReplAction::SlashCommand(name, args)` variant; the
//! actual dispatch lives in callers (CLI binary, zed UI). This module
//! provides the library-level handlers those callers invoke, so each surface
//! prints identical output and shares the same `LintReport` round-trip with
//! the auto-inject path (D1) and the turn-end maintenance path (D2-z).
//!
//! Both handlers are pure side-effect-on-disk + in-memory result; they emit
//! no telemetry directly. Callers tag dispatch with `command:` and route
//! through their own telemetry sink (orchestrator caller logs;
//! caduceus_bridge zed-side emits `wiki.slash_command.dispatched`).

use caduceus_core::CaduceusError;
use caduceus_storage::{LintReport, WikiEngine};
use std::path::Path;

/// Structured result of a wiki slash-command invocation.
///
/// `message` is the human-readable text the REPL should print.
/// `report` carries the structured `LintReport` for `/wiki` (None for
/// `/init` since init has no scan output). `command` lets callers tag
/// telemetry without re-parsing the original input.
#[derive(Debug, Clone)]
pub struct WikiSlashOutput {
    pub message: String,
    pub report: Option<LintReport>,
    pub command: &'static str,
}

/// Handle the `/wiki` slash command: run a maintenance scan and format the
/// `LintReport` for REPL display.
///
/// Args are accepted for forward-compatibility (e.g. `--json`, `--quiet`)
/// but the v1 contract ignores them — extension is additive.
pub fn handle_wiki(
    project_root: &Path,
    _args: &[String],
) -> Result<WikiSlashOutput, CaduceusError> {
    let engine = WikiEngine::new(project_root);
    let report = engine.maintain()?;
    let message = format_report(&report);
    Ok(WikiSlashOutput {
        message,
        report: Some(report),
        command: "wiki",
    })
}

/// Handle the `/init` slash command: idempotently create the wiki scaffold.
///
/// `WikiEngine::init` is itself idempotent (re-creating an existing layout
/// is a no-op), so this handler simply forwards and reports the current
/// state. Re-running `/init` after pages exist does not destroy them.
pub fn handle_init(
    project_root: &Path,
    _args: &[String],
) -> Result<WikiSlashOutput, CaduceusError> {
    let engine = WikiEngine::new(project_root);
    engine.init()?;
    // `init` is idempotent — it fills any missing scaffold pieces (wiki_dir,
    // index.md, log.md) without clobbering existing pages. Reporting a
    // simple "ready" state avoids lying to the user when the directory
    // existed but was missing scaffold files (a partial-init recovery is
    // a no-op from the caller's point of view).
    let message = format!("Wiki ready at {}", engine.wiki_dir().display());
    Ok(WikiSlashOutput {
        message,
        report: None,
        command: "init",
    })
}

/// Render a `LintReport` into a human-readable summary block.
///
/// Stable enough that callers can grep the first line ("Wiki maintenance
/// scan complete") for a quick "did this run" probe in tests.
fn format_report(r: &LintReport) -> String {
    let mut s = format!(
        "Wiki maintenance scan complete\n  pages examined: {}\n  findings: {}\n  schema: v{}\n  elapsed: {}ms",
        r.pages_examined,
        r.findings.len(),
        r.schema_version,
        r.elapsed_ms
    );
    if !r.findings.is_empty() {
        s.push_str("\n\nFindings:");
        for f in &r.findings {
            s.push_str(&format!(
                "\n  [{:?}] {}: {}",
                f.category, f.page, f.description
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_init_creates_wiki_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join(".caduceus").join("wiki").exists());
        let out = handle_init(dir.path(), &[]).unwrap();
        assert!(dir.path().join(".caduceus").join("wiki").exists());
        assert!(
            out.message.starts_with("Wiki ready at"),
            "got: {}",
            out.message
        );
        assert_eq!(out.command, "init");
        assert!(out.report.is_none());
    }

    #[test]
    fn handle_init_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let first = handle_init(dir.path(), &[]).unwrap();
        assert!(first.message.starts_with("Wiki ready at"));
        // Write a page so we can prove init doesn't clobber.
        let engine = WikiEngine::new(dir.path());
        engine
            .write_page("survives", "# Survives idempotent init\n")
            .unwrap();
        // Second init: same neutral message, page preserved.
        let second = handle_init(dir.path(), &[]).unwrap();
        assert!(second.message.starts_with("Wiki ready at"));
        assert!(engine.page_exists("survives"));
    }

    #[test]
    fn handle_init_recovers_from_partial_scaffold() {
        // Pre-existing dir without scaffold files (simulates a partial init
        // or a manually created directory). `handle_init` must complete the
        // scaffold AND not lie to the user about whether work was done.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".caduceus").join("wiki")).unwrap();
        assert!(!dir
            .path()
            .join(".caduceus")
            .join("wiki")
            .join("index.md")
            .exists());
        let out = handle_init(dir.path(), &[]).unwrap();
        assert!(dir
            .path()
            .join(".caduceus")
            .join("wiki")
            .join("index.md")
            .exists());
        assert!(out.message.starts_with("Wiki ready at"));
    }

    #[test]
    fn handle_wiki_returns_lint_report_and_formatted_message() {
        let dir = tempfile::tempdir().unwrap();
        // /wiki should work even if /init was never run — handle_wiki goes
        // through WikiEngine::maintain which tolerates a missing wiki dir.
        let out = handle_wiki(dir.path(), &[]).unwrap();
        assert_eq!(out.command, "wiki");
        let report = out.report.expect("/wiki must produce a report");
        assert_eq!(report.pages_examined, 0);
        assert!(report.findings.is_empty());
        assert!(out.message.contains("Wiki maintenance scan complete"));
        assert!(out.message.contains("pages examined: 0"));
    }

    #[test]
    fn handle_wiki_with_pages_includes_findings() {
        // Mirror the caduceus-storage maintain test: 2 mutually-linked pages
        // plus 1 orphan, so WikiLinter has something concrete to flag.
        let dir = tempfile::tempdir().unwrap();
        let engine = WikiEngine::new(dir.path());
        engine.init().unwrap();
        engine.write_page("alpha", "# Alpha\n\n[[beta]]\n").unwrap();
        engine.write_page("beta", "# Beta\n\n[[alpha]]\n").unwrap();
        engine
            .write_page("loner", "# Loner\n\nNo links.\n")
            .unwrap();
        let out = handle_wiki(dir.path(), &[]).unwrap();
        let report = out.report.unwrap();
        assert_eq!(report.pages_examined, 3);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.category, caduceus_storage::LintCategory::OrphanPage)),
            "expected at least one OrphanPage finding, got {:?}",
            report.findings
        );
        assert!(out.message.contains("OrphanPage"));
    }
}
