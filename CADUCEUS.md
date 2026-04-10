# Caduceus Project Instructions

## Overview
Caduceus is a 6-layer AI development environment built with Rust (Tauri 2) + React/TypeScript.

## Architecture
- 12 Rust crates in a workspace at crates/
- Tauri 2 desktop app at src-tauri/
- React frontend at src/
- All crates follow the dependency direction: core → storage/providers → permissions/runtime/tools → orchestrator → shell

## Coding Conventions
- Rust: follow clippy lints, use thiserror for errors, async-trait for async traits
- TypeScript: strict mode, no any types, functional components with hooks
- All public APIs must have doc comments
- Tests required for every new function

## Commands
- Build: cargo check --workspace
- Test: cargo test --workspace
- Format: cargo fmt --all
- Lint: cargo clippy --workspace
- Frontend: npx tsc --noEmit && npm run build

## Do Not Edit
- Files in target/ or node_modules/
- Auto-generated files

## Testing
- Use tempfile for filesystem tests
- Use MockLlmAdapter for LLM tests
- Every crate must have at least 5 tests
