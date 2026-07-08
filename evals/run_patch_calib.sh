#!/usr/bin/env bash
# Patch-behavior calibration: temp /tmp workspaces, prompt variants, multiple endpoints.
#
# Usage:
#   ./evals/run_patch_calib.sh
#   ./evals/run_patch_calib.sh --variants nested_path,plan_framing,self_authored --keep
#   ./evals/run_patch_calib.sh --variants recovery_after_noop,harness_strict --endpoints local
#   ./evals/run_patch_calib.sh --dry-run
#
# Stress variants (mirror plan-run failures): nested_path, plan_framing, ambiguous_fix,
# session_warmup, self_authored, recovery_after_noop, harness_system, harness_strict
#
# Optional: evals/patch_calib_endpoints.env (see patch_calib_endpoints.example.env)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
HELPERS="$ROOT/evals/patch_calib_helpers.py"

RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/raven-patch-calib.${RUN_ID}.XXXXXX")"
RESULTS_TSV="$OUT_DIR/results.tsv"

DRY_RUN=0
KEEP=0
VARIANTS_FILTER=""
ENDPOINTS_FILTER=""
MAX_ROUNDS=10
TIMEOUT_SECS=300

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --keep) KEEP=1; shift ;;
    --variants) VARIANTS_FILTER="${2:-}"; shift 2 ;;
    --endpoints) ENDPOINTS_FILTER="${2:-}"; shift 2 ;;
    --max-rounds) MAX_ROUNDS="${2:-10}"; shift 2 ;;
    --timeout) TIMEOUT_SECS="${2:-300}"; shift 2 ;;
    -h|--help)
      sed -n '2,14p' "$0"
      exit 0
      ;;
    *) echo "Unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [[ -f "$ROOT/evals/patch_calib_endpoints.env" ]]; then
  # shellcheck disable=SC1091
  source "$ROOT/evals/patch_calib_endpoints.env"
fi

declare -a ENDPOINT_SLUGS=()
declare -A EP_LABEL=() EP_URL=() EP_MODEL=() EP_KEY=()

register_endpoint() {
  local slug="$1" label="$2" url="$3" model="$4" key="${5:-}"
  ENDPOINT_SLUGS+=("$slug")
  EP_LABEL[$slug]="$label"
  EP_URL[$slug]="$url"
  EP_MODEL[$slug]="$model"
  EP_KEY[$slug]="$key"
}

register_endpoint "local" "local default" "${LLM_BASE_URL:-http://127.0.0.1:8080/v1}" "${LLM_MODEL:-qwen3-coder-next}" ""

for var in $(compgen -v | grep -E '^ENDPOINT_[^_]+_URL$' || true); do
  slug="${var#ENDPOINT_}"
  slug="${slug%_URL}"
  [[ "$slug" == "local" ]] && continue
  url="${!var}"
  label_var="ENDPOINT_${slug}_LABEL"
  model_var="ENDPOINT_${slug}_MODEL"
  key_var="ENDPOINT_${slug}_KEY"
  label="${!label_var:-$slug}"
  model="${!model_var:-}"
  key="${!key_var:-}"
  if [[ -n "$url" && -n "$model" ]]; then
    register_endpoint "$slug" "$label" "$url" "$model" "$key"
  fi
done

endpoint_selected() {
  local slug="$1"
  [[ -z "$ENDPOINTS_FILTER" ]] && return 0
  [[ ",$ENDPOINTS_FILTER," == *",$slug,"* ]]
}

variant_selected() {
  local v="$1"
  [[ -z "$VARIANTS_FILTER" ]] && return 0
  [[ ",$VARIANTS_FILTER," == *",$v,"* ]]
}

# ── Variants ──────────────────────────────────────────────────────────────────
# layout: flat | nested | empty_nested
# seed:   none | warmup | noop
declare -A VARIANT_LAYOUT=() VARIANT_SEED=() VARIANT_PROMPT=() VARIANT_MAX_ROUNDS=()
declare -A VARIANT_CALIB_SUFFIX=() VARIANT_STRICT_ERRORS=()

ALL_VARIANTS=(
  baseline discipline anti_noop write_preferred
  nested_path plan_framing ambiguous_fix
  session_warmup self_authored recovery_after_noop
  harness_system harness_strict
)

for v in "${ALL_VARIANTS[@]}"; do
  VARIANT_LAYOUT[$v]=flat
  VARIANT_SEED[$v]=none
  VARIANT_MAX_ROUNDS[$v]=$MAX_ROUNDS
  VARIANT_CALIB_SUFFIX[$v]=""
  VARIANT_STRICT_ERRORS[$v]=""
done

VARIANT_LAYOUT[nested_path]=nested
VARIANT_LAYOUT[plan_framing]=nested
VARIANT_LAYOUT[ambiguous_fix]=nested
VARIANT_LAYOUT[session_warmup]=nested
VARIANT_LAYOUT[self_authored]=empty_nested
VARIANT_LAYOUT[recovery_after_noop]=nested
VARIANT_LAYOUT[harness_system]=nested
VARIANT_LAYOUT[harness_strict]=nested

VARIANT_SEED[session_warmup]=warmup
VARIANT_SEED[recovery_after_noop]=noop

VARIANT_MAX_ROUNDS[self_authored]=14

HARNESS_PATCH_SUFFIX='## Patch tool (critical)
- replace must differ from search — identical values produce no net change.
- If you see "no net change", search MATCHED; fix replace (do not assume search missed).
- Always read immediately before patch; search must be verbatim file text.'

VARIANT_CALIB_SUFFIX[harness_system]=$HARNESS_PATCH_SUFFIX
VARIANT_STRICT_ERRORS[harness_strict]=1
VARIANT_CALIB_SUFFIX[harness_strict]=$HARNESS_PATCH_SUFFIX

VARIANT_PROMPT[baseline]="cargo check fails: crates.io has no tui 0.26 (latest stable is 0.19).
Change ONLY Cargo.toml (tui version → 0.19). Do not add or modify other files.
Then run cargo check to verify."

VARIANT_PROMPT[discipline]="${VARIANT_PROMPT[baseline]}

Tool discipline:
- read Cargo.toml immediately before patch
- patch search must be verbatim text from the file (no line numbers from read output)
- replace must be the NEW text — search and replace must not be identical"

VARIANT_PROMPT[anti_noop]="${VARIANT_PROMPT[baseline]}

If patch returns 'no net change', that means search MATCHED but replace was wrong (often search == replace).
Re-read the file and patch again with a different replace string."

VARIANT_PROMPT[write_preferred]="${VARIANT_PROMPT[baseline]}

For this small file, prefer write (full file) over patch."

VARIANT_PROMPT[nested_path]="cd single && cargo check fails: crates.io has no tui 0.26.
Change ONLY single/Cargo.toml (use a valid crates.io version). Do not modify other files.
Then run cargo check from single/ to verify."

VARIANT_PROMPT[plan_framing]="Execute the approved plan.
Goal: Tarot TUI in Rust (tui-rs)
Project root for this plan: single/ (prefix read/write/patch paths with single/ unless verification shows otherwise).

Approved steps:
  1. Create single/ project (Cargo.toml + src/main.rs) and verify cargo check ← CURRENT

cargo check in single/ failed: crates.io has no tui 0.26.
Fix the dependency in single/Cargo.toml using a valid crates.io version, then run cargo check from single/."

VARIANT_PROMPT[ambiguous_fix]="cd single && cargo check fails because the tui crate version in Cargo.toml is not available on crates.io.
Fix ONLY single/Cargo.toml so the dependency resolves on crates.io. Do not name a specific version in your reasoning — pick a valid one from the registry error.
Then run cargo check from single/ to verify."

VARIANT_PROMPT[session_warmup]="${VARIANT_PROMPT[ambiguous_fix]}"

VARIANT_PROMPT[recovery_after_noop]="Continue fixing the project. cargo check in single/ still fails because the tui dependency version is invalid on crates.io.
Your last patch on single/Cargo.toml had no effect. Fix the dependency and run cargo check from single/."

VARIANT_PROMPT[self_authored_phase1]="Create a minimal Rust project under single/:
- single/Cargo.toml with tui = \"0.26\" and crossterm = \"0.27\"
- single/src/main.rs (minimal tui-rs hello UI)
Then run: cd single && cargo check"

VARIANT_PROMPT[self_authored_phase2]="cargo check in single/ failed: the tui 0.26 requirement does not resolve on crates.io.
Fix the dependency in single/Cargo.toml using a valid crates.io version, then run cargo check from single/ again.
Do not recreate the project — patch or write single/Cargo.toml only."

VARIANT_PROMPT[harness_system]="${VARIANT_PROMPT[ambiguous_fix]}"
VARIANT_PROMPT[harness_strict]="${VARIANT_PROMPT[ambiguous_fix]}"

# ── Workspace fixtures ────────────────────────────────────────────────────────
seed_workspace_flat() {
  local ws="$1"
  mkdir -p "$ws/src"
  cat > "$ws/Cargo.toml" <<'EOF'
[package]
name = "patch-calib-fixture"
version = "0.1.0"
edition = "2021"

[dependencies]
tui = "0.26"
crossterm = "0.27"
EOF
  cat > "$ws/src/main.rs" <<'EOF'
fn main() {
    println!("patch-calib fixture");
}
EOF
}

seed_workspace_nested() {
  local ws="$1"
  mkdir -p "$ws/single/src"
  cat > "$ws/single/Cargo.toml" <<'EOF'
[package]
name = "tui-tarot"
version = "0.1.0"
edition = "2021"

[dependencies]
tui = "0.26"
crossterm = "0.27"
EOF
  cat > "$ws/single/src/main.rs" <<'EOF'
use std::io;
use tui::backend::CrosstermBackend;
use tui::Terminal;

fn main() -> io::Result<()> {
    let _stdout = io::stdout();
    let backend = CrosstermBackend::new(_stdout);
    let _terminal = Terminal::new(backend)?;
    Ok(())
}
EOF
}

seed_workspace_empty_nested() {
  local ws="$1"
  mkdir -p "$ws"
}

cargo_path_for_variant() {
  local variant="$1"
  case "${VARIANT_LAYOUT[$variant]}" in
    nested|empty_nested) echo "single/Cargo.toml" ;;
    *) echo "Cargo.toml" ;;
  esac
}

setup_workspace() {
  local ws="$1" variant="$2"
  case "${VARIANT_LAYOUT[$variant]}" in
    nested) seed_workspace_nested "$ws" ;;
    empty_nested) seed_workspace_empty_nested "$ws" ;;
    *) seed_workspace_flat "$ws" ;;
  esac
}

seed_session_if_needed() {
  local ws="$1" variant="$2" session="$3"
  local seed="${VARIANT_SEED[$variant]}"
  local cargo_rel
  cargo_rel="$(cargo_path_for_variant "$variant")"
  case "$seed" in
    warmup) python3 "$HELPERS" seed-warmup "$ws" "$session" "$cargo_rel" >/dev/null ;;
    noop) python3 "$HELPERS" seed-noop "$ws" "$session" "$cargo_rel" >/dev/null ;;
    none) ;;
  esac
}

find_session_log() {
  local session_prefix="$1"
  local newest="" newest_mtime=0 dir log mtime
  for dir in "$HOME/.raven-hotel/sessions/${session_prefix}"*; do
    [[ -d "$dir" ]] || continue
    log="$dir/full_log.jsonl"
    [[ -f "$log" ]] || continue
    mtime=$(stat -c %Y "$log" 2>/dev/null || stat -f %m "$log")
    if (( mtime > newest_mtime )); then
      newest_mtime=$mtime
      newest="$log"
    fi
  done
  [[ -n "$newest" ]] && echo "$newest"
}

analyze_run() {
  local log_file="$1" cargo_file="$2"
  local fixed=0 identical=0 no_net=0 not_found=0 patch_count=0 write_count=0 recovered=0

  if [[ -f "$cargo_file" ]] && grep -qE 'tui\s*=\s*"0\.19' "$cargo_file" 2>/dev/null; then
    fixed=1
  fi

  if [[ -f "$log_file" ]]; then
    identical=$(jq -s -r '
      [.[] | select(.role=="assistant")
        | (.tool_calls // []) | if type == "array" then . else [] end
        | .[] | select(.function.name == "patch")
        | .function.arguments
        | if type == "string" then (try fromjson catch empty) else . end
        | select(.search != null and .search == .replace)] | length
    ' "$log_file" 2>/dev/null || echo 0)
    patch_count=$(jq -s -r '
      [.[] | select(.role=="assistant")
        | (.tool_calls // []) | if type == "array" then . else [] end
        | .[] | select(.function.name == "patch")] | length
    ' "$log_file" 2>/dev/null || echo 0)
    write_count=$(jq -s -r '
      [.[] | select(.role=="assistant")
        | (.tool_calls // []) | if type == "array" then . else [] end
        | .[] | select(.function.name == "write")] | length
    ' "$log_file" 2>/dev/null || echo 0)
    jq -s -r '
      [.[] | select(.role=="assistant")
        | (.tool_calls // []) | if type == "array" then . else [] end
        | .[] | select(.function.name == "patch")
        | .function.arguments
        | if type == "string" then (try fromjson catch empty) else . end
        | {search, replace}] | unique
    ' "$log_file" >"${log_file%.jsonl}.patches.json" 2>/dev/null || true
    no_net=$(grep -c 'no net change\|search and replace are identical\|Patch had no effect' "$log_file" 2>/dev/null || true)
    not_found=$(grep -c 'Search text not found' "$log_file" 2>/dev/null || true)
    # recovered: last patch in log has search != replace (after a prior noop in seeded runs)
    recovered=$(jq -s -r '
      [.[] | select(.role=="assistant")
        | (.tool_calls // []) | if type == "array" then . else [] end
        | .[] | select(.function.name == "patch")
        | .function.arguments
        | if type == "string" then (try fromjson catch empty) else . end
        | select(.search != null and .replace != null and .search != .replace)]
      | if length > 0 then 1 else 0 end
    ' "$log_file" 2>/dev/null || echo 0)
  fi

  echo "$fixed $identical $no_net $not_found $patch_count $write_count $recovered"
}

score_label() {
  local variant="$1" fixed="$2" identical="$3" no_net="$4" recovered="$5"
  if [[ "$variant" == "recovery_after_noop" ]]; then
    if [[ "$fixed" == 1 && "$recovered" == 1 && "$identical" == 0 ]]; then
      echo "PASS_RECOVERED"
    elif [[ "$fixed" == 1 ]]; then
      echo "PASS_WITH_ISSUES"
    elif [[ "$identical" -gt 0 || "$no_net" -gt 0 ]]; then
      echo "PATCH_NOOP"
    else
      echo "FAIL"
    fi
    return
  fi
  if [[ "$fixed" == 1 && "$identical" == 0 ]]; then
    echo "PASS"
  elif [[ "$fixed" == 1 ]]; then
    echo "PASS_WITH_ISSUES"
  elif [[ "$identical" -gt 0 || "$no_net" -gt 0 ]]; then
    echo "PATCH_NOOP"
  else
    echo "FAIL"
  fi
}

probe_endpoint() {
  local url="$1"
  curl -sf --max-time 5 "${url%/}/models" >/dev/null 2>&1
}

run_raven() {
  local ws="$1" session="$2" prompt_file="$3" rounds="$4"
  local url="$5" model="$6" key="$7"
  local stdout="$8" stderr="$9"
  shift 9
  local -a extra_env=("$@")

  local -a cmd=(
    timeout "$TIMEOUT_SECS"
    env RAVEN_APPROVAL=thunderdome RAVEN_GOAL_TRACKING=0
    "${extra_env[@]}"
    "$ROOT/target/release/raven-tui"
    --workspace "$ws"
    --session "$session"
    --approval thunderdome
    --temperature 0
    --max-rounds "$rounds"
    --base-url "$url"
    --model "$model"
    --prompt-file "$prompt_file"
  )
  if [[ -n "$key" ]]; then
    cmd+=(--api-key "$key")
  fi
  "${cmd[@]}" >"$stdout" 2>"$stderr"
}

collect_result() {
  local slug="$1" variant="$2" session="$3" ws="$4" cargo_rel="$5" secs="$6"
  local log_file=""
  log_file="$(find_session_log "$session" || true)"
  if [[ -n "$log_file" ]]; then
    cp "$log_file" "$OUT_DIR/log-${slug}-${variant}.jsonl"
    log_file="$OUT_DIR/log-${slug}-${variant}.jsonl"
  fi
  local fixed identical no_net not_found patch_count write_count recovered
  read -r fixed identical no_net not_found patch_count write_count recovered < <(
    analyze_run "$log_file" "$ws/$cargo_rel"
  )
  local score
  score="$(score_label "$variant" "$fixed" "$identical" "$no_net" "$recovered")"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$slug" "$variant" "$score" "$fixed" "$recovered" "$identical" "$no_net" "$not_found" \
    "$patch_count" "$write_count" "$cargo_rel" "$secs" "${log_file:-missing}" >>"$RESULTS_TSV"
  echo "    score=$score fixed=$fixed recovered=$recovered identical_patch=$identical no_net=$no_net patch_calls=$patch_count write_calls=$write_count (${secs}s)"
}

cleanup() {
  if [[ "$KEEP" == 1 ]]; then
    echo ""
    echo "Kept artifacts: $OUT_DIR"
    return
  fi
  rm -rf "$OUT_DIR"
}
trap cleanup EXIT

printf 'endpoint\tvariant\tscore\tfixed\trecovered\tidentical_patch\tno_net_change\tsearch_miss\tpatch_calls\twrite_calls\tcargo_path\tseconds\tlog\n' >"$RESULTS_TSV"

echo "==> patch calibration run $RUN_ID"
echo "    output dir: $OUT_DIR"
echo ""

if [[ "$DRY_RUN" == 1 ]]; then
  for slug in "${ENDPOINT_SLUGS[@]}"; do
    endpoint_selected "$slug" || continue
    echo "  endpoint: $slug (${EP_LABEL[$slug]})"
    for v in "${ALL_VARIANTS[@]}"; do
      variant_selected "$v" || continue
      echo "    variant: $v (layout=${VARIANT_LAYOUT[$v]} seed=${VARIANT_SEED[$v]} rounds=${VARIANT_MAX_ROUNDS[$v]})"
    done
  done
  exit 0
fi

chmod +x "$HELPERS"
echo "==> building raven-tui (release)"
cargo build --release --no-default-features -q --bin raven-tui

RAN=0
for slug in "${ENDPOINT_SLUGS[@]}"; do
  endpoint_selected "$slug" || continue
  url="${EP_URL[$slug]}"
  model="${EP_MODEL[$slug]}"
  key="${EP_KEY[$slug]}"

  if ! probe_endpoint "$url"; then
    echo "SKIP endpoint $slug — unreachable at ${url%/}/models"
    continue
  fi

  for variant in "${ALL_VARIANTS[@]}"; do
    variant_selected "$variant" || continue

    WS="$(mktemp -d "$OUT_DIR/ws.${slug}.${variant}.XXXXXX")"
    setup_workspace "$WS" "$variant"
    CARGO_REL="$(cargo_path_for_variant "$variant")"
    SESSION="patchcal-${RUN_ID}-${slug}-${variant}"
    ROUNDS="${VARIANT_MAX_ROUNDS[$variant]}"
    STDOUT_FILE="$OUT_DIR/stdout-${slug}-${variant}.log"
    STDERR_FILE="$OUT_DIR/stderr-${slug}-${variant}.log"

    local_env=()
    if [[ -n "${VARIANT_CALIB_SUFFIX[$variant]}" ]]; then
      local_env+=(RAVEN_CALIB_PROMPT_SUFFIX="${VARIANT_CALIB_SUFFIX[$variant]}")
    fi
    if [[ "${VARIANT_STRICT_ERRORS[$variant]}" == "1" ]]; then
      local_env+=(RAVEN_PATCH_STRICT_ERRORS=1)
    fi

    echo "==> run endpoint=$slug variant=$variant session=$SESSION layout=${VARIANT_LAYOUT[$variant]} seed=${VARIANT_SEED[$variant]}"

    SECS=0
    START=$SECONDS
    set +e

    if [[ "$variant" == "self_authored" ]]; then
      PROMPT1="$OUT_DIR/prompt-${slug}-${variant}-p1.txt"
      PROMPT2="$OUT_DIR/prompt-${slug}-${variant}-p2.txt"
      printf '%s\n' "${VARIANT_PROMPT[self_authored_phase1]}" >"$PROMPT1"
      printf '%s\n' "${VARIANT_PROMPT[self_authored_phase2]}" >"$PROMPT2"
      run_raven "$WS" "$SESSION" "$PROMPT1" "$ROUNDS" "$url" "$model" "$key" \
        "$OUT_DIR/stdout-${slug}-${variant}-p1.log" "$OUT_DIR/stderr-${slug}-${variant}-p1.log" \
        "${local_env[@]}"
      RC1=$?
      run_raven "$WS" "$SESSION" "$PROMPT2" "$ROUNDS" "$url" "$model" "$key" \
        "$STDOUT_FILE" "$STDERR_FILE" \
        "${local_env[@]}"
      RC=$?
      cat "$OUT_DIR/stdout-${slug}-${variant}-p1.log" "$STDOUT_FILE" >"$STDOUT_FILE.combined" 2>/dev/null || true
      mv "$STDOUT_FILE.combined" "$STDOUT_FILE"
      [[ "$RC1" -ne 0 && "$RC" -eq 0 ]] && RC=$RC1
    else
      seed_session_if_needed "$WS" "$variant" "$SESSION"
      PROMPT_FILE="$OUT_DIR/prompt-${slug}-${variant}.txt"
      printf '%s\n' "${VARIANT_PROMPT[$variant]}" >"$PROMPT_FILE"
      run_raven "$WS" "$SESSION" "$PROMPT_FILE" "$ROUNDS" "$url" "$model" "$key" \
        "$STDOUT_FILE" "$STDERR_FILE" \
        "${local_env[@]}"
      RC=$?
    fi

    set -e
    SECS=$((SECONDS - START))
    RAN=$((RAN + 1))

    if [[ "$RC" -eq 124 ]]; then
      echo "    TIMEOUT after ${TIMEOUT_SECS}s"
    elif [[ "$RC" -ne 0 ]]; then
      echo "    exit code $RC (see $STDERR_FILE)"
    fi

    collect_result "$slug" "$variant" "$SESSION" "$WS" "$CARGO_REL" "$SECS"
    rm -rf "$WS"
  done
done

if [[ "$RAN" -eq 0 ]]; then
  echo "No runs executed — check endpoints / filters." >&2
  exit 1
fi

echo ""
echo "==> results"
column -t -s $'\t' "$RESULTS_TSV" 2>/dev/null || cat "$RESULTS_TSV"
echo ""
echo "Full artifacts: $OUT_DIR (use --keep to retain after exit)"