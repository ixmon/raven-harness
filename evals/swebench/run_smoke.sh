#!/usr/bin/env bash
# SWE-bench Lite dev smoke trio — stub until instance driver lands.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTANCES=(
  marshmallow-code__marshmallow-1343
  sqlfluff__sqlfluff-1625
  pvlib__pvlib-python-1707
)
echo "==> SWE-bench Lite dev smoke (stub)"
for id in "${INSTANCES[@]}"; do
  echo "  TODO: run_instance.sh $id"
done
echo "See docs/testing-ideas.md §7. Dataset: princeton-nlp/SWE-bench_Lite split=dev"
exit 0