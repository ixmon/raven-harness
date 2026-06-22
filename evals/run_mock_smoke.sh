#!/usr/bin/env bash
# Fully offline harness smoke: mock LLM + mock tools (CI-safe, no network).
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> harness mock smoke (offline agent loop)"

export RAVEN_EVAL=1
export RAVEN_EVAL_MOCK_LLM=1
export RAVEN_EVAL_ASSERT_STRICT="${RAVEN_EVAL_ASSERT_STRICT:-1}"
export RAVEN_EVAL_APPROVAL="${RAVEN_EVAL_APPROVAL:-thunderdome}"

if [[ -n "${RAVEN_EVAL_SCENARIO:-}" ]]; then
  SCENARIOS=("$RAVEN_EVAL_SCENARIO")
else
  SCENARIOS=(
    mock_tool_loop
    mock_churn_then_answer
    mock_huge_grep
    mock_secrets_in_read
  )
fi

for SCENARIO in "${SCENARIOS[@]}"; do
  echo ""
  echo "--- scenario: $SCENARIO ---"
  export RAVEN_EVAL_SCENARIO="$SCENARIO"
  export RAVEN_EVAL_WORKSPACE="$(mktemp -d "${TMPDIR:-/tmp}/raven-mock-eval.XXXXXX")"
  cargo run --release --quiet --bin raven-tui
  rm -rf "$RAVEN_EVAL_WORKSPACE"
done

echo ""
echo "==> PASS: mock smoke (${#SCENARIOS[@]} scenario(s))"