#!/usr/bin/env bash
# Fully offline harness smoke: mock LLM + mock tools (CI-safe, no network).
set -euo pipefail
cd "$(dirname "$0")/.."

SCENARIO="${RAVEN_EVAL_SCENARIO:-mock_tool_loop}"

echo "==> harness mock smoke (offline agent loop)"
echo "    RAVEN_EVAL_SCENARIO=$SCENARIO"

export RAVEN_EVAL=1
export RAVEN_EVAL_MOCK_LLM=1
export RAVEN_EVAL_SCENARIO="$SCENARIO"
export RAVEN_EVAL_ASSERT_STRICT="${RAVEN_EVAL_ASSERT_STRICT:-1}"
export RAVEN_EVAL_WORKSPACE="${RAVEN_EVAL_WORKSPACE:-$(mktemp -d "${TMPDIR:-/tmp}/raven-mock-eval.XXXXXX")}"

if [[ -z "${RAVEN_EVAL_WORKSPACE_SET:-}" ]]; then
  trap 'rm -rf "$RAVEN_EVAL_WORKSPACE"' EXIT
fi

cargo run --release --quiet
echo "==> PASS: mock smoke ($SCENARIO)"