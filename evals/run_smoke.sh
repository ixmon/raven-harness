#!/usr/bin/env bash
# Harness smoke: real local LLM + mock tools (mock tools not wired yet — runs replay + hint).
set -euo pipefail
cd "$(dirname "$0")/.."
"$(dirname "$0")/run_replay.sh"
echo ""
echo "==> harness smoke (local LLM) — stub"
echo "    Next: RAVEN_EVAL=1 + scenarios with LLM_BASE_URL=${LLM_BASE_URL:-http://127.0.0.1:8080/v1}"
echo "    Skipping live LLM until ToolBackend mock lands."