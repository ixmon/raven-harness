#!/usr/bin/env bash
# Harness smoke: replay (offline) + live local LLM with mock tools (RAVEN_EVAL=1).
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> harness replay (offline)"
"$(dirname "$0")/run_replay.sh"
echo ""

BASE_URL="${LLM_BASE_URL:-http://127.0.0.1:8080/v1}"
MODELS_URL="${BASE_URL%/}/models"
SCENARIO="${RAVEN_EVAL_SCENARIO:-smoke_ping}"

echo "==> harness smoke (local LLM + mock tools)"
echo "    LLM_BASE_URL=$BASE_URL"
echo "    RAVEN_EVAL_SCENARIO=$SCENARIO"

if ! curl -sf --max-time 5 "$MODELS_URL" >/dev/null 2>&1; then
  echo "SKIP: no LLM reachable at $MODELS_URL"
  echo "      Start llama.cpp (or set LLM_BASE_URL) and re-run."
  exit 0
fi

export RAVEN_EVAL=1
export RAVEN_EVAL_SCENARIO="$SCENARIO"
export RAVEN_EVAL_APPROVAL="${RAVEN_EVAL_APPROVAL:-thunderdome}"

# Isolated workspace unless caller set RAVEN_EVAL_WORKSPACE
if [[ -z "${RAVEN_EVAL_WORKSPACE:-}" ]]; then
  export RAVEN_EVAL_WORKSPACE
  RAVEN_EVAL_WORKSPACE="$(mktemp -d "${TMPDIR:-/tmp}/raven-eval.XXXXXX")"
  trap 'rm -rf "$RAVEN_EVAL_WORKSPACE"' EXIT
fi

cargo run --release --quiet --bin raven-tui -- \
  --base-url "$BASE_URL" \
  --temperature 0 \
  ${LLM_MODEL:+--model "$LLM_MODEL"}

echo "==> PASS: harness smoke ($SCENARIO)"