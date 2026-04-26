# Caduceus — Roadmap

This file captures aspirational behavior that is **not yet wired into the shipped runtime**. Library types may exist for some of these features, but no caller instantiates or invokes them today. See `FEATURES.md` for the canonical list of feature statuses (✅ Implemented, 🟡 Library / Partial, 🔧 Stubbed, 💡 Planned).

## Wiki — Auto-maintenance and lifecycle integration

The wiki is currently agent-driven via the `caduceus_project_wiki` LLM tool (see `docs/GUIDE.md`). Earlier sketches of an auto-maintenance pipeline (`WikiWatcher`, `WikiAutoTrigger`, `WikiMaintenanceAgent`, `WikiQueryEngine`, `WikiIngestor`, `WikiLog`) were deleted in Phase C — they had zero callers and would have been speculative scaffolding for behavior that was never wired up. When auto-maintenance is wired, it should be rebuilt at the **orchestrator layer** rather than re-introduced into `caduceus-storage`.

Planned work to make the wiki truly self-maintaining:

- **Agent-turn hook.** Add an orchestrator-layer hook in zed's `Thread` that, after each turn, calls `WikiEngine` to refresh the index/lint stale pages. (Replaces the deleted `WikiAutoTrigger.on_agent_turn_complete`.)
- **Session lifecycle hooks.** Wire `on_session_start` / `on_session_end` in the orchestrator so initial scans and end-of-session summaries run automatically.
- **File-system watcher.** Build a thin watcher at the orchestrator layer (or reuse zed's existing fs-watcher infrastructure) so file saves trigger incremental re-analysis. The watcher MUST ignore `.caduceus/` itself to avoid self-trigger loops — the Phase A regression test was deleted alongside `WikiWatcher` but the requirement still stands for any rebuild.
- **`/wiki refresh` slash command.** Register a `/wiki` command in the orchestrator that forces a full maintenance pass on demand.
- **`/init`-driven first scan.** Extend `/init` so it runs an initial deep scan that seeds the wiki with architecture, API, and pattern pages.
- **Automatic context injection.** Replace the current memory-bank "project overview" injection in zed's thread context (`thread.rs:5158`) with a `WikiEngine::search()`-driven pass that surfaces relevant wiki pages as context for each turn.

Prerequisites:

- Wiki Phase A correctness fixes must be in place (path traversal, atomic writes, log history preservation, watcher self-trigger guard, link extractor, schema versioning) — without them, auto-maintenance corrupts the wiki or infinite-loops. ✅ shipped.
- Wiki Phase C cleanup (delete unused speculative architecture). ✅ shipped.

## `/init` slash command

The `/init` flow is referenced in the system prompt (`agent_harness.rs:1224`) but is not registered in the orchestrator's `slash_commands()` table. Planned work: register it as a real slash command so users can invoke it directly, scaffolding `CADUCEUS.md`, `.caduceus/`, and starter config.
