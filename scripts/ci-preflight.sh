#!/usr/bin/env bash
# scripts/ci-preflight.sh — run the same gates GitHub Actions runs, locally.
#
# Mirrors .github/workflows/ci.yml exactly so a green local run = a green CI run.
# Designed to be invoked by .githooks/pre-push, but is also fine to run on demand:
#
#     scripts/ci-preflight.sh                          # full gate (matches CI)
#     scripts/ci-preflight.sh --skip-rust-when-docs-only
#         # auto-skip if only docs/specs/private/markdown changed since origin/main
#
# Bypass entirely with `git push --no-verify` only in true emergencies.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

SKIP_DOCS_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --skip-rust-when-docs-only) SKIP_DOCS_ONLY=1 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

# ── Optional fast-path: skip the rust gate if every changed file matches
# a docs-only globbed path. Tracked-but-uncommitted changes don't count
# (the hook is invoked at push time, so we look at HEAD..origin/main).
if [ "$SKIP_DOCS_ONLY" = "1" ]; then
  base="$(git merge-base HEAD origin/main 2>/dev/null || echo '')"
  if [ -n "$base" ]; then
    nonmarkdown=$(git diff --name-only "$base"..HEAD | grep -Ev '\.md$|^docs/|^private/|^\.github/' || true)
    if [ -z "$nonmarkdown" ]; then
      echo "✓ ci-preflight: docs/markdown-only diff vs origin/main; skipping rust gate."
      exit 0
    fi
  fi
fi

step() { echo; echo "▶ ci-preflight: $1"; }

step "cargo fmt --all --check"
cargo fmt --all --check

step "cargo clippy --workspace -- -D warnings"
cargo clippy --workspace -- -D warnings

step "cargo test --workspace"
cargo test --workspace

echo
echo "✓ ci-preflight: all CI gates green."
