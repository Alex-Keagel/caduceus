#!/usr/bin/env bash
# NW-4 native-loop smoke — backend half (non-interactive).
#
# Exercises AgentHarness::run — the path Zed dispatches turns into when
# `agent.caduceus_native_loop` is ON — against a scripted MockLlmAdapter.
#
# Covers S1 (plain prompt), S2 (read-only tool), S4 (fetch-shaped tool),
# and S5 (multi-round tool chain). S3 (destructive-tool approval) and
# S6 (mid-stream cancel) still require a manual Zed dev-build dogfood.
#
# Usage:
#   scripts/nw4-smoke.sh            # run the backend smoke
#   scripts/nw4-smoke.sh --verbose  # with test output

set -euo pipefail

cd "$(dirname "$0")/.."

args=( -p caduceus-orchestrator --test nw4_native_loop_smoke )
if [[ "${1:-}" == "--verbose" ]]; then
    args+=( -- --nocapture )
fi

echo "▶ cargo test ${args[*]}"
cargo test "${args[@]}"

cat <<'EOF'

─── NW-4 backend smoke: PASS ───

Still required (manual, Zed dev build with `agent.caduceus_native_loop = true`):

  S3  Destructive-tool approval:
      Ask the agent to edit_file a throwaway path.
      → Approval prompt MUST appear.
      → "Allow" runs the edit; "Deny" returns "denied by user" and the
        loop continues cleanly (no hang, no retry storm).

  S6  Mid-stream cancel:
      Send a long-running tool task; hit Cancel while it streams.
      → Stream aborts.
      → No zombie tool spawn in the next turn.
      → No duplicated system preamble on the subsequent prompt.

Report pass/fail per step; any failure → file symptoms + Zed log excerpt.
EOF
