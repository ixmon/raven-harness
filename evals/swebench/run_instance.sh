#!/usr/bin/env bash
# Run one SWE-bench Lite dev instance: materialize repo → Raven → grade.
#
# Usage:
#   ./run_instance.sh <instance_id>              # full run
#   ./run_instance.sh <instance_id> --verify-grade   # grade gold patch (pipeline check)
#   ./run_instance.sh <instance_id> --skip-raven     # materialize only
#   ./run_instance.sh <instance_id> --skip-grade     # Raven only, capture diff
#
# Env:
#   LLM_BASE_URL, LLM_MODEL     — local OpenAI-compatible server
#   RAVEN_MAX_ROUNDS (30)       — tool loop budget
#   RAVEN_APPROVAL=thunderdome  — auto-approve writes/exec (default here)
#
# The script writes the problem_statement to a file and passes it via --prompt-file
# to avoid all shell quoting/escaping issues with complex bug reports.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TUI_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
VENV="$SCRIPT_DIR/.venv"

INSTANCE_ID="${1:?usage: run_instance.sh <instance_id> [--verify-grade|--skip-raven|--skip-grade]}"
shift || true

VERIFY_GRADE=0
SKIP_RAVEN=0
SKIP_GRADE=0
for arg in "$@"; do
  case "$arg" in
    --verify-grade) VERIFY_GRADE=1 ;;
    --skip-raven) SKIP_RAVEN=1 ;;
    --skip-grade) SKIP_GRADE=1 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

INSTANCE_JSON="$SCRIPT_DIR/instances/${INSTANCE_ID}.json"
RESULT_DIR="$SCRIPT_DIR/results/${INSTANCE_ID}"
CACHE_DIR="$SCRIPT_DIR/cache/${INSTANCE_ID}"
REPO_DIR="$CACHE_DIR/repo"

ensure_tooling() {
  if [[ ! -x "$VENV/bin/python" ]]; then
    echo "==> creating swebench venv"
    python3 -m venv "$VENV"
    "$VENV/bin/pip" install -q -r "$SCRIPT_DIR/requirements.txt"
  fi
  if [[ ! -f "$INSTANCE_JSON" ]]; then
    echo "==> exporting instance metadata"
    "$VENV/bin/python" "$SCRIPT_DIR/export_instances.py"
  fi
}

materialize_repo() {
  local repo base_commit
  repo="$("$VENV/bin/python" -c "import json; print(json.load(open('$INSTANCE_JSON'))['repo'])")"
  base_commit="$("$VENV/bin/python" -c "import json; print(json.load(open('$INSTANCE_JSON'))['base_commit'])")"

  mkdir -p "$CACHE_DIR"
  if [[ ! -d "$REPO_DIR/.git" ]]; then
    echo "==> cloning https://github.com/${repo}.git"
    git clone --filter=blob:none "https://github.com/${repo}.git" "$REPO_DIR"
  fi

  echo "==> checkout ${base_commit:0:12}"
  git -C "$REPO_DIR" fetch origin --quiet || true
  git -C "$REPO_DIR" reset --hard "$base_commit"
  git -C "$REPO_DIR" clean -fdx
}

copy_session_artifacts() {
  local workspace_abs log_src session_dir
  workspace_abs="$(cd "$REPO_DIR" && pwd)"
  log_src="$(
    find "${HOME}/.raven-hotel/sessions" -name 'full_log.jsonl' 2>/dev/null \
      | while read -r f; do
          if grep -Fq "\"workspace\": \"${workspace_abs}\"" "$(dirname "$f")/meta.json" 2>/dev/null; then
            echo "$f"
            break
          fi
        done
  )"
  if [[ -n "${log_src:-}" && -f "$log_src" ]]; then
    cp "$log_src" "$RESULT_DIR/raven_log.jsonl"
    session_dir="$(dirname "$log_src")"
    if [[ -f "$session_dir/meta.json" ]]; then
      cp "$session_dir/meta.json" "$RESULT_DIR/meta.json"
    fi
  fi
}

write_run_timing() {
  local started_ms ended_ms raven_exit
  started_ms="${1:?}"
  ended_ms="${2:?}"
  raven_exit="${3:-0}"
  "$VENV/bin/python" - "$RESULT_DIR/run_timing.json" "$started_ms" "$ended_ms" "$raven_exit" <<'PY'
import json, sys
out, start, end, code = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
json.dump({
    "started_ms": start,
    "ended_ms": end,
    "wall_duration_ms": max(0, end - start),
    "raven_exit_code": code,
}, open(out, "w"), indent=2)
PY
}

extract_metrics() {
  local mode="${1:-verify-grade}"
  if [[ ! -x "$VENV/bin/python" ]]; then
    return 0
  fi
  "$VENV/bin/python" "$SCRIPT_DIR/extract_harness_metrics.py" \
    "$RESULT_DIR" --mode "$mode" --out "$RESULT_DIR/metrics.json" \
    >"$RESULT_DIR/metrics.stdout" 2>"$RESULT_DIR/metrics.stderr" || true
}

run_raven() {
  local started_ms ended_ms raven_exit
  local prompt
  prompt="$("$VENV/bin/python" -c "import json; print(json.load(open('$INSTANCE_JSON'))['problem_statement'])")"

  mkdir -p "$RESULT_DIR"
  # Write prompt to file to avoid any shell quoting/escaping issues with large bug reports
  printf '%s' "$prompt" > "$RESULT_DIR/prompt.txt"
  echo "==> running Raven (max_rounds=${RAVEN_MAX_ROUNDS:-30})"
  started_ms="$("$VENV/bin/python" -c 'import time; print(int(time.time()*1000))')"
  # Prepare a project-specific venv so the agent sees a working `python` with the package installed.
  # This lets exec("python ..."), pytest, imports etc. succeed inside the workspace.
  PROJ_VENV="$CACHE_DIR/venv"
  if [[ ! -x "$PROJ_VENV/bin/python" ]]; then
    echo "==> creating project venv for agent execution"
    python3 -m venv "$PROJ_VENV" -q 2>/dev/null || python3 -m venv "$PROJ_VENV" || true
    if [[ -x "$PROJ_VENV/bin/python" ]]; then
      "$PROJ_VENV/bin/pip" install -U pip setuptools wheel --quiet 2>/dev/null || true
      (
        cd "$REPO_DIR"
        "$PROJ_VENV/bin/pip" install -e . --quiet 2>&1 | tail -3 || true
        [[ -f requirements.txt ]] && "$PROJ_VENV/bin/pip" install -r requirements.txt --quiet 2>&1 | tail -2 || true
        [[ -f requirements_dev.txt ]] && "$PROJ_VENV/bin/pip" install -r requirements_dev.txt --quiet 2>&1 | tail -2 || true
        if [[ -f pyproject.toml ]]; then
          "$PROJ_VENV/bin/pip" install -e ".[test,dev]" --quiet 2>/dev/null || \
          "$PROJ_VENV/bin/pip" install -e ".[dev]" --quiet 2>/dev/null || true
        fi
      )
    else
      echo "warning: failed to create project venv, proceeding with system python"
    fi
  fi

  (
    cd "$TUI_ROOT"
    export RAVEN_APPROVAL="${RAVEN_APPROVAL:-thunderdome}"
    export RAVEN_METRICS_OUT="$RESULT_DIR/harness_turn.json"

    # Ensure 'python' symlink if only python3 exists (some venvs)
    if [[ -x "$PROJ_VENV/bin/python3" && ! -e "$PROJ_VENV/bin/python" ]]; then
      ln -sf python3 "$PROJ_VENV/bin/python"
    fi

    # Explicit PATH for the cargo invocation so the raven-tui process (and its exec tool children)
    # see the venv first. This should make bare `python` resolve to the project's venv python.
    # (The venv from `python3 -m venv` always creates a `python` symlink to python3.)
    PATH="$PROJ_VENV/bin:$PATH" cargo run --release --quiet --bin raven-tui -- \
      --workspace "$REPO_DIR" \
      --prompt-file "$RESULT_DIR/prompt.txt" \
      --max-rounds "${RAVEN_MAX_ROUNDS:-30}" \
      --max-tokens "${RAVEN_MAX_TOKENS:-16384}" \
      --temperature "${RAVEN_TEMPERATURE:-0}" \
      --base-url "${LLM_BASE_URL:-http://127.0.0.1:8080/v1}" \
      ${LLM_MODEL:+--model "$LLM_MODEL"} \
      >"$RESULT_DIR/raven.stdout" 2>"$RESULT_DIR/raven.stderr"
  )
  raven_exit=$?
  ended_ms="$("$VENV/bin/python" -c 'import time; print(int(time.time()*1000))')"
  write_run_timing "$started_ms" "$ended_ms" "$raven_exit"
  copy_session_artifacts
  return "$raven_exit"
}

capture_model_patch() {
  mkdir -p "$RESULT_DIR"
  git -C "$REPO_DIR" diff >"$RESULT_DIR/model_patch.diff"
  if [[ ! -s "$RESULT_DIR/model_patch.diff" ]]; then
    echo "warning: model produced empty diff" >&2
  fi
}

write_report_stub() {
  "$VENV/bin/python" - "$RESULT_DIR/report.json" "$INSTANCE_ID" <<'PY'
import json, sys
out, iid = sys.argv[1], sys.argv[2]
json.dump({
    "instance_id": iid,
    "skipped_grade": True,
    "note": "grade skipped (--skip-grade or --skip-raven without verify-grade)",
}, open(out, "w"), indent=2)
PY
}

grade_patch() {
  local patch_file="$1"
  mkdir -p "$RESULT_DIR"
  echo "==> grading $(basename "$patch_file")"
  "$VENV/bin/python" "$SCRIPT_DIR/grade_instance.py" "$INSTANCE_ID" \
    --repo "$REPO_DIR" \
    --patch "$patch_file" \
    --out "$RESULT_DIR/report.json"
}

main() {
  ensure_tooling
  materialize_repo
  # Clean stale results (e.g. report.json from a previous verify-grade run)
  rm -rf "$RESULT_DIR"
  mkdir -p "$RESULT_DIR"
  cp "$INSTANCE_JSON" "$RESULT_DIR/instance.json"

  if [[ "$VERIFY_GRADE" -eq 1 ]]; then
    local gold="$RESULT_DIR/gold_patch.diff"
    "$VENV/bin/python" -c "import json; print(json.load(open('$INSTANCE_JSON'))['patch'])" >"$gold"
    grade_patch "$gold"
    extract_metrics "verify-grade"
    echo "==> PASS: verify-grade $INSTANCE_ID"
    return 0
  fi

  if [[ "$SKIP_RAVEN" -eq 0 ]]; then
    run_raven
    capture_model_patch
  fi

  if [[ "$SKIP_GRADE" -eq 1 ]]; then
    write_report_stub
    extract_metrics "full"
    echo "==> DONE: $INSTANCE_ID (grade skipped)"
    return 0
  fi

  if [[ "$SKIP_RAVEN" -eq 1 ]]; then
    echo "error: --skip-raven requires --verify-grade or a model_patch.diff" >&2
    exit 2
  fi

  grade_patch "$RESULT_DIR/model_patch.diff"
  extract_metrics "full"
  if "$VENV/bin/python" -c "import json,sys; r=json.load(open('$RESULT_DIR/report.json')); sys.exit(0 if r.get('resolved') else 1)"; then
    echo "==> RESOLVED: $INSTANCE_ID"
  else
    echo "==> UNRESOLVED: $INSTANCE_ID (see $RESULT_DIR)"
    exit 1
  fi
}

main