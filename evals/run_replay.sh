#!/usr/bin/env bash
# Harness replay — JSON scenarios in evals/scenarios/ (no LLM, no network).
set -euo pipefail
cd "$(dirname "$0")/.."
echo "==> harness replay (eval_scenarios)"
cargo test --no-default-features eval_scenarios -- --nocapture
echo "==> PASS: all replay scenarios"