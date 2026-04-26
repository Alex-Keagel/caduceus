# Caduceus — Roadmap

This file captures aspirational behavior that is **not yet wired into the shipped runtime**. Library types may exist for some of these features, but no caller instantiates or invokes them today. See `FEATURES.md` for the canonical list of feature statuses (✅ Implemented, 🟡 Library / Partial, 🔧 Stubbed, 💡 Planned).

## Wiki — Auto-maintenance and lifecycle integration

The wiki is currently agent-driven via the `caduceus_project_wiki` LLM tool (see `docs/GUIDE.md`). The library exposes additional types — `WikiWatcher`, `WikiAutoTrigger`, `WikiMaintenanceAgent` — that are designed to keep the wiki current without explicit prompts, but nothing instantiates them in the shipped runtime.

Planned work to make the wiki truly self-maintaining:

- **Agent-turn hook.** Instantiate `WikiAutoTrigger` per-thread in zed's `Thread` and call `on_agent_turn_complete` at turn end so the maintenance agent can rebuild the index and refresh stale pages without explicit prompts.
- **Session lifecycle hooks.** Wire `on_session_start` / `on_session_end` to the host runtime so initial scans and end-of-session summaries run automatically.
- **File-system watcher.** Start `WikiWatcher` on session open so file saves trigger incremental re-analysis. Watcher must ignore `.caduceus/` itself to avoid self-trigger loops (this guard ships in Phase A; the start-up wiring does not).
- **`/wiki refresh` slash command.** Register a `/wiki` command in the orchestrator that forces a full maintenance pass on demand.
- **`/init`-driven first scan.** Extend `/init` so it runs an initial deep scan that seeds the wiki with architecture, API, and pattern pages.
- **Automatic context injection.** Replace the current memory-bank "project overview" injection in zed's thread context (`thread.rs:5158`) with a `WikiEngine::search()`-driven pass that surfaces relevant wiki pages as context for each turn.

Prerequisites:

- Wiki Phase A correctness fixes must be in place (path traversal, atomic writes, log history preservation, watcher self-trigger guard, link extractor, schema versioning) — without them, auto-maintenance corrupts the wiki or infinite-loops. ✅ shipped.
- Wiki Phase C cleanup (delete unused speculative architecture) should land first to avoid wiring up code paths that will be deleted.

## `/init` slash command

The `/init` flow is referenced in the system prompt (`agent_harness.rs:1224`) but is not registered in the orchestrator's `slash_commands()` table. Planned work: register it as a real slash command so users can invoke it directly, scaffolding `CADUCEUS.md`, `.caduceus/`, and starter config.
