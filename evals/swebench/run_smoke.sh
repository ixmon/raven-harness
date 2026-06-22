#!/usr/bin/env bash
# SWE-bench Lite dev smoke trio — materialize + optional Raven + grade.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

INSTANCES=()
while IFS= read -r line; do
  INSTANCES+=("$line")
done < <("$SCRIPT_DIR/.venv/bin/python" -c "
import json
from pathlib import Path
data = json.loads((Path('$SCRIPT_DIR') / 'instances.json').read_text())
for iid in data['smoke_trio']:
    print(iid)
" 2>/dev/null || python3 -c "
import json
from pathlib import Path
data = json.loads((Path('$SCRIPT_DIR') / 'instances.json').read_text())
for iid in data['smoke_trio']:
    print(iid)
")

if [[ ${#INSTANCES[@]} -eq 0 ]]; then
  INSTANCES=(
    marshmallow-code__marshmallow-1343
    sqlfluff__sqlfluff-1625
    pvlib__pvlib-python-1707
  )
fi

MODE="${SWEBENCH_SMOKE_MODE:-verify-grade}"
echo "==> SWE-bench Lite dev smoke (${#INSTANCES[@]} instances, mode=$MODE)"

PASS=0
FAIL=0
for id in "${INSTANCES[@]}"; do
  echo ""
  echo "--- $id ---"
  if [[ "$MODE" == "verify-grade" ]]; then
    if "$SCRIPT_DIR/run_instance.sh" "$id" --verify-grade; then
      PASS=$((PASS + 1))
    else
      FAIL=$((FAIL + 1))
    fi
  elif [[ "$MODE" == "full" ]]; then
    if "$SCRIPT_DIR/run_instance.sh" "$id"; then
      PASS=$((PASS + 1))
    else
      FAIL=$((FAIL + 1))
    fi
  else
    echo "unknown SWEBENCH_SMOKE_MODE=$MODE (use verify-grade or full)" >&2
    exit 2
  fi
done

echo ""
echo "==> SWE-bench smoke: $PASS passed, $FAIL failed"
[[ "$FAIL" -eq 0 ]]