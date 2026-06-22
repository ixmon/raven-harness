# Testing & Evaluation — Current State & Ideas

Date noted: 2026-06-22

This document describes how Raven Harness is tested today, how to automatically tell whether a harness change is **good** (not just **correct**), and a concrete plan for **mock tools / mock MCP** paired with **free local inference**. It also covers using **[SWE-bench Lite](https://www.swebench.com/lite.html) `dev` instances** (23 tasks) as real-world agent evals without touching the held-out `test` split.

Related:

- [context-ideas.md](./context-ideas.md) — session memory, compression, tiered inference
- [headroom-ideas.md](./headroom-ideas.md) — CCR, `raven learn`, eval patterns in Headroom

---

## 1. What Runs Automatically Today

### CI (`.github/workflows/ci.yml`)

On every push/PR to `main`:

```bash
cargo build --no-default-features
cargo test --no-default-features
cargo clippy --no-default-features -- -D warnings
```

### Unit tests (~48)

Strong on **deterministic plumbing**, weak on **agent behavior**:

| Area | Examples |
|------|----------|
| Probe / `/v1/models` | `resolve_server_probe`, llama.cpp hybrid payload |
| Keystore | encrypt/decrypt, launch defaults, endpoint update |
| Tools | patch, line ranges, workspace containment, truncation |
| LLM client | SSE parsing, tool-call delta accumulation, OpenRouter parsers |
| UI / input | slash cursor reset, approval wrap, key edit, palette |
| Session | stable session id per workspace path |

### Not covered in CI

- Full agent loop (`Agent::run_turn` / streaming path)
- Real or mocked LLM chat completions
- Summarization / compression **quality**
- TUI interactive flows (ratatui event loops)
- Whether nudges, budgets, or injection changes **help** the model

### Existing hooks for scripted runs

- **`--prompt "..."`** — non-interactive one-shot (`main.rs`); no test harness wired to it yet
- **`RAVEN_VAULT_PASSWORD`** — non-interactive keystore unlock for scripts/CI

**Bottom line:** CI answers “did we break Rust?” not “did we improve the harness?”

---

## 2. Testing Pyramid for Harness Changes

Four layers: **correctness → invariants → replay → eval**.

```
                    ┌─────────────┐
                    │ Eval (B/C)  │  optional LLM, task success, $$ 
                    ├─────────────┤
                    │ Replay      │  canned LLM + mock tools, deterministic
                    ├─────────────┤
                    │ Invariants  │  token caps, message counts, $0
                    ├─────────────┤
                    │ Unit tests  │  pure logic, every PR
                    └─────────────┘
```

### 2.1 Unit tests (cheap, always in CI)

Add `#[cfg(test)]` for every change that does not need a live LLM:

| Change | Test |
|--------|------|
| Probe / `InferenceBudget` | `from_context_tokens(8192)` vs `65536` → expected bytes, lines, rounds |
| Injection block | Fixture `SessionMeta` → `get_injection_block()` golden snapshot |
| Pruning / masking | Synthetic `conversation[]` → length, masked tool bodies, kept assistant lines |
| Summarize trigger | Mock `LlmClient`: dropped ≥6 msgs → `recent_turns_summary` updated |
| Session tools | `record_discovery`, `update_goal`, `read_summary` mtime logic |
| New tools | `retrieve_session`, resolutions — handler + schema tests |

**Rule:** Pure Rust logic gets a unit test before merge.

### 2.2 Contract / invariant tests (still no LLM)

Properties that must hold after any harness change:

```text
estimated_context_tokens() ≤ probed n_ctx (soft ceiling)
tool result bytes ≤ context_budget.tool_result_bytes
injection block size ≤ inject_summary_chars cap
full_log.jsonl append-only (line count only increases)
load_recent_conversation(20) ⊆ roles {user, assistant}
prune_history: conversation.len() ≤ MAX_CONVERSATION
```

Catches regressions like doubled injection size or prune wiping everything.

### 2.3 Mock HTTP LLM server (CI-friendly integration)

Tiny fake OpenAI-compatible server in `tests/` (wiremock, mockito, or `axum`):

- `GET /v1/models` → llama.cpp-shaped JSON
- `POST /v1/chat/completions` → canned tool-call then final text

Point `raven-tui` or `Agent::run_turn()` at `http://127.0.0.1:PORT/v1` and assert:

- Tool round count
- Tools invoked (names + order)
- `meta.json` fields
- Conversation length after prune

Validates **orchestration** without production models.

### 2.4 Recorded scenario replay (deterministic harness)

Fixtures under `evals/scenarios/`:

```yaml
name: probe_wrong_model_single_fallback
llm:
  - response: { tool_calls: [{ name: read, arguments: { path: src/main.rs } }] }
  - response: { content: "Found the entrypoint." }
tools:
  read:
    src/main.rs: "fn main() { ... }"
assert:
  max_tool_rounds: 3
  injection_contains: ["qwen3-coder-next"]
  context_tokens_max: 12000
```

Replay engine feeds canned LLM responses; mock tool backend returns fixtures; compare final state to baseline.

**No API key. Fast. Locks in loop/nudge/masking behavior.**

### 2.5 Eval suite (optional live LLM — “was it good?”)

Inspired by [Headroom eval workflow](https://github.com/chopratejas/headroom) — **mandatory cheap gates** + **optional live eval** when an endpoint is configured.

| Tier | Cost | Measures |
|------|------|----------|
| **A — Structural** | $0 | Token count before/after compress; mask preserves assistant + tool names; CCR round-trip |
| **B — Task micro-eval** | local $ | `tiny_crate` tasks or a **subset of SWE-bench Lite `dev`**; pass if tests green |
| **C — Regression** | local/API $$ | Full SWE-bench Lite `dev` (23) or gold prompts from `full_log.jsonl`; resolve rate + cost |

CI: run **A + replay** always; run **B/C** on `workflow_dispatch` or self-hosted runner with local llama.

---

## 3. Metrics to Compare Before/After

When changing compression, nudges, budgets, or injection — log and diff:

| Metric | Why |
|--------|-----|
| Tokens per user turn (est. or `usage`) | Cost |
| Tool rounds per turn | Churn / efficiency |
| Injection block bytes | Sparse RAG health |
| `recent_turns_summary` length | Compress not exploding |
| Time to first final answer | UX |
| Eval scenario pass rate | Actual quality |

Store baseline JSON on `main`; PR CI warns or fails if metrics regress (e.g. +20% tokens with same pass rate).

---

## 4. What to Run When You Change X

| Change type | Minimum automatic check |
|-------------|-------------------------|
| Probe / `ContextBudget` / `InferenceBudget` | Unit tests + mock `GET /v1/models` |
| Injection block | Golden `get_injection_block()` snapshot |
| `prune_history` / observation masking | Mock LLM + message count / content assertions |
| New tool (`retrieve_session`, etc.) | Unit + mock agent turn |
| Nudges / auto-continue | Replay fixture: “model stops with text only” |
| Keystore / settings | Existing keystore tests + one integration |
| TUI-only | Render/input tests; optional ratatui buffer assertion (no screenshot) |

---

## 5. Gaps Worth Closing First

1. **`LlmClient` trait** — extract interface so agent tests use a mock without network
2. **`tests/` integration crate** — `mock_llm_server.rs` + one end-to-end `run_turn`
3. **`evals/` directory** — tiny workspace + scenario YAML + `run.sh` driver
4. **CI job `harness-smoke`** — mock-server + replay only (~seconds)
5. **Optional `eval-nightly`** — real local llama on self-hosted runner
6. **README** — update test count (stale “29 tests” reference)

---

## 6. Mock Tools & Mock MCP (Local Inference + Safe Scenarios)

### 6.1 Motivation

With **free local inference**, the LLM is cheap but **real tools** are expensive and risky:

- `exec` → full `cargo build`, CPU/GPU load, flaky CI
- Large `read` / `grep` → huge context, disk I/O
- Real workspace → accidental writes, polluted `full_log.jsonl`

**Sweet spot:** **real local LLM** + **mock tools** → test harness logic (budget, prune, nudges, session meta) without blowing up the machine.

Raven does **not** speak MCP today (`tools/mod.rs` — native `exec`, `read`, `grep`, etc.). Mock MCP is still valuable as a **target architecture**; phase 1 can use in-process mocks with the same fixtures.

Reference: Headroom [`mock_mcp_servers.py`](https://github.com/chopratejas/headroom/blob/main/examples/mcp_demo/mock_mcp_servers.py) — canned Slack/DB/log payloads for compression demos.

### 6.2 What mock tools / mock MCP are good for

| Scenario | Tests |
|----------|-------|
| Huge tool output | 300 log lines, 200-row JSON → truncation, masking, CCR/retrieve |
| Failure then success | `read` 404 then alternate path → resolutions / discoveries |
| Secrets in output | fake `.env` in `read` → redaction before log/summarize |
| Timeout / slow tool | sleep 30s → stop signal, approval path |
| Deterministic repo | `list`/`grep` return fixed tree — no real workspace |
| Churn loop | same wrong tool N times → progress/loop nudge at budget end |
| Probe + budget | `/v1/models` fixture → `ContextBudget` in injection |

**Avoids:** real compiles, writes outside eval dir, live web, OOM from monster tool results.

### 6.3 Architecture options

#### A. In-process mock tools (fastest — no MCP protocol)

```text
RAVEN_EVAL=1  →  tools::execute() → MockToolBackend(scenario)
                 default          → RealToolBackend (today)
```

- Scenarios: YAML/JSON mapping `(tool_name, args)` → fixed response
- Works with existing OpenAI function tool format
- CI: `RAVEN_EVAL=1 cargo run -- --prompt "..." --workspace evals/fixtures/empty`

#### B. Mock MCP servers (stdio)

Per-domain servers under `evals/mcp/`:

- Implement MCP `tools/list` + `tools/call`
- Return bodies from `evals/scenarios/<name>/tools/*.json`
- Raven config points MCP client at mock servers (when MCP client exists)

**Pros:** Realistic wire format; reusable across tools.  
**Cons:** Requires MCP client in Raven; more moving parts than (A).

#### C. Hybrid (recommended path)

```text
evals/
  scenarios/
    probe_65k_context.yaml
    churn_then_answer.yaml
    huge_grep.yaml
    secrets_in_read.yaml
  mock/
    backend.rs          # MockToolBackend (phase 1)
    mcp_fs_server.rs    # phase 2 — same fixtures, MCP transport
  fixtures/
    tiny_crate/           # optional real workspace for tier-B evals
  run.sh                  # local llama + RAVEN_EVAL=1
  baselines/
    metrics.json          # token/round baselines on main
```

Phase 1: in-process mocks. Phase 2: expose same fixtures via mock MCP stdio servers.

### 6.4 Judging “was the change good?” (automated assertions)

Assert on **observable outcomes**, not subjective quality alone:

| Assert on | Example |
|-----------|---------|
| `meta.recent_turns_summary` | Contains solution line; under char cap |
| `estimated_context_tokens()` | After mask, &lt; baseline + 10% |
| Tool round count | ≤ `max_rounds` for scenario |
| `full_log.jsonl` | Tool sequence `read → grep → patch` |
| Workspace | Only allowlisted paths touched (mock → none) |
| `--prompt` stdout | Task completion marker / regex |

Optional: cheap **local condenser model** as rubric judge (“did the answer satisfy scenario?”) — still no real `exec`.

### 6.5 Safety rails (when mixing real tools)

| Guard | Purpose |
|-------|---------|
| `RAVEN_EVAL_WORKSPACE` | Temp dir only; deleted after run |
| `allow_exec` per scenario | Deny `exec` unless explicitly listed |
| `timeout` / `ulimit` in `run.sh` | Cap runaway processes |
| Separate eval model | Small quant for nightly; large model manual only |

With full mock tools, most guards become unnecessary.

---

## 7. SWE-bench Lite `dev` Instances (Real-World Evals)

### 7.1 What it is

[SWE-bench Lite](https://www.swebench.com/lite.html) is a 300-task subset of SWE-bench (real GitHub issues → patch → prove via project tests). It ships two splits on [Hugging Face](https://huggingface.co/datasets/princeton-nlp/SWE-bench_Lite):

| Split | Count | Use |
|-------|-------|-----|
| **`dev`** | **23** | Active harness development, tuning, regression — **use this first** |
| **`test`** | 300 | Held-out benchmark; report only on releases / papers — **do not tune on** |

Official guidance: *“23 development instances that can be useful for active development on the SWE-bench task.”*

Lite tasks are filtered to be more **self-contained**: typically one file, ≤3 edit hunks, functional bug fixes (sqlfluff, marshmallow, pvlib-python, etc.). Easier than full SWE-bench but still real repos, real tests, real churn.

### 7.2 Per-instance fields (what an eval driver needs)

Each row in the dataset includes:

| Field | Role |
|-------|------|
| `instance_id` | e.g. `sqlfluff__sqlfluff-1625` |
| `repo` | `owner/name` on GitHub |
| `base_commit` | Checkout point — broken state |
| `problem_statement` | Issue text → Raven `--prompt` / user message |
| `patch` | Gold code fix (for grading only — **not** shown to agent) |
| `test_patch` | Test changes bundled with fix |
| `FAIL_TO_PASS` | JSON list of tests that must go from fail → pass |
| `PASS_TO_PASS` | Regression tests that must stay green |
| `version` | Repo version pin for environment |

**Pass criterion:** after agent run, apply generated patch (or diff workspace), run project test suite, all `FAIL_TO_PASS` pass and `PASS_TO_PASS` still pass. Same as [SWE-bench harness](https://github.com/swe-bench/SWE-bench).

### 7.3 How this fits Raven’s eval ladder

```text
Mock scenarios (YAML)     → harness logic only, seconds, every PR
SWE-bench Lite dev (23)   → end-to-end agent quality, minutes–hours, nightly/local
SWE-bench Lite test (300) → official score, rare (release / manual)
```

**Complements mock MCP/tools:** mocks prove compression, nudges, and budgets work; SWE-bench dev proves the **whole loop** can fix real bugs when those harness choices matter.

### 7.4 Suggested `evals/swebench/` layout

```text
evals/swebench/
  instances.json          # subset or full dev split (instance_ids)
  run_instance.sh         # one instance: checkout → raven → grade
  run_dev.sh              # loop 23 dev instances, aggregate score
  cache/                  # cloned repos at base_commit (reuse disk)
  results/
    <instance_id>/
      raven_log.jsonl     # copy session log
      model_patch.diff    # what agent produced
      report.json         # pass/fail, rounds, tokens, duration
  baselines/
    dev_resolve_rate.json # e.g. 3/23 @ commit abc, model X
```

Driver sketch:

```bash
# 1. Materialize workspace
git clone --depth 1 <repo> cache/<repo>
cd cache/<repo> && git checkout <base_commit>

# 2. Run Raven (Thunderdome or eval-specific approval mode)
raven-tui --prompt "$(jq -r .problem_statement instance.json)" \
  --workspace "$(pwd)" \
  --max-rounds 30

# 3. Collect patch (git diff workspace vs base) — agent must use write/patch

# 4. Grade (SWE-bench eval script or pytest on FAIL_TO_PASS)
python -m swebench.harness.run_evaluation --instance_id ...
```

### 7.5 What harness changes you can measure on `dev`

| Metric | Harness change being tested |
|--------|----------------------------|
| **Resolve rate** | `/ N` instances passed — primary quality |
| **Median tool rounds** | Nudge / budget / masking |
| **Median est. tokens** | Compression, observation mask, `InferenceBudget` |
| **Timeout rate** | Round limits, auto-continue, progress nudge |
| **Cost per resolved** | Condenser tier, local vs frontier routing |

Compare **before/after** on the same 23 instances + same model — not single-instance anecdotes.

### 7.6 Caveats (important)

| Issue | Mitigation |
|-------|------------|
| **Heavy** | Python repos, conda/docker envs per repo; 23 ≪ 300 but still not CI-on-every-PR |
| **Python-centric** | Raven is Rust-first; SWE-bench Lite is mostly Python — still valid for **agent harness**, not Raven dogfooding |
| **Docker** | Official SWE-bench grading uses containers; can reuse their harness or lightweight `pytest` in venv for dev subset |
| **Overfitting** | Never tune on `test` (300); hold out `dev` sub-splits if iterating many harness versions |
| **Patch extraction** | Raven must actually modify files (`patch`/`write`); `--prompt` one-shot may need multi-turn / higher `max_rounds` |
| **Approval modes** | Eval runs need YOLO or `Thunderdome` — document `RAVEN_EVAL_APPROVAL=thunderdome` |
| **Machine load** | Run 1–3 instances for smoke, full 23 overnight on local GPU box |

### 7.7 Subsetting `dev` for daily smoke

Not all 23 every run. Pick a **fixed smoke trio** (small / medium / hard by historical pass rate) plus rotate the rest nightly:

```text
smoke:  marshmallow-1343, sqlfluff-1625, pvlib-1707   # fast feedback
nightly: all 23 dev
release: optional sample of test (e.g. 30) — never tune harness on this
```

### 7.8 Relation to mock tools

| Layer | SWE-bench dev | Mock scenarios |
|-------|---------------|----------------|
| Grading | Real pytest | Assert on meta / log / tokens |
| Workspace | Real cloned repo | Fixture or empty |
| Tools | Real `exec`/`read`/… | Mock responses |
| LLM | Local real (your box) | Mock or real |

Use **both**: mocks gate PRs; SWE-bench dev validates that harness improvements transfer to real bug-fix trajectories.

---

## 8. Suggested Run Matrix

| Run type | LLM | Tools | When |
|----------|-----|-------|------|
| Unit | — | — | Every PR (today) |
| Harness replay | Mock canned | Mock | Every PR — add this |
| Harness smoke | **Local real** | **Mock** | Pre-merge on dev machine |
| Mock HTTP integration | Mock server | Mock or real tiny FS | Every PR — add this |
| **SWE-bench Lite `dev` smoke** | **Local real** | **Real repo** | 1–3 fixed instances, after harness PR |
| **SWE-bench Lite `dev` full** | **Local real** | **Real repo** | Nightly / weekly (23 instances) |
| SWE-bench Lite `test` | Local/API | Real repo | Release benchmark only |
| Frontier spot-check | API | Mock or tiny repo | Optional regression |

**Operator workflow with free local inference:**

- Day-to-day harness work → **Harness smoke** (mock tools).
- “Did we actually help the agent?” → **SWE-bench `dev` smoke** (3 instances).
- Weekly scorecard → **full `dev` (23)**.

---

## 9. Proposed CLI / Env Surface

| Flag / env | Purpose |
|------------|---------|
| `RAVEN_EVAL=1` | Enable mock tool backend |
| `RAVEN_EVAL_SCENARIO=churn_then_answer` | Load scenario from `evals/scenarios/` |
| `RAVEN_EVAL_WORKSPACE` | Isolated workspace root |
| `--eval <name>` | Non-interactive: run scenario + assertions + exit code |
| `LLM_BASE_URL` | Local llama for smoke; mock HTTP for CI |
| `RAVEN_EVAL_ASSERT_STRICT=1` | Fail on metric regression vs `baselines/metrics.json` |
| `--swebench-instance <id>` | Run one SWE-bench Lite instance (future) |
| `RAVEN_EVAL_APPROVAL=thunderdome` | Non-interactive tool approval for evals |

Extends existing `--prompt` mode; does not require TUI.

---

## 10. Implementation Order

| Priority | Item | Rationale |
|----------|------|-----------|
| 1 | `ToolBackend` trait (`Real` / `Mock`) | Unblocks safe scenarios without MCP |
| 2 | One YAML scenario + assertions | Prove replay pipeline |
| 3 | `LlmClient` trait + mock HTTP server test | Agent loop without network |
| 4 | `evals/run.sh` + `RAVEN_EVAL=1` | Local llama smoke on your machine |
| 5 | CI `harness-smoke` job | Replay + mock LLM only |
| 6 | `baselines/metrics.json` + diff | Regression on tokens/rounds |
| 7 | **`evals/swebench/` driver + 3-instance smoke trio** | Real bug-fix signal on SWE-bench Lite `dev` |
| 8 | Mock MCP stdio servers | Same fixtures, MCP transport when client lands |
| 9 | Full SWE-bench Lite `dev` (23) nightly + `dev_resolve_rate.json` baseline | Harness scorecard |
| 10 | Tier-B `tiny_crate` (optional) | Rust-native micro tasks alongside SWE-bench |

---

## 11. Key Code References (test hooks today)

| Hook | File | Notes |
|------|------|-------|
| Unit tests | `src/**/*.rs` `#[cfg(test)]` | 48 tests |
| `--prompt` | `src/main.rs` | Non-interactive single turn |
| `tools::execute` | `src/tools/mod.rs` | Single dispatch point for mock backend |
| `Agent::run_turn` | `src/agent.rs` | Integration test entry |
| `build_messages_for_model` | `src/agent.rs` | Golden prompt tests |
| `resolve_server_probe` | `src/llm.rs` | Already well tested |
| CI | `.github/workflows/ci.yml` | build + test + clippy |

---

## 12. Open Questions

- Should replay tests live in `tests/` (Rust) or `evals/` (YAML + driver script)?
- Golden files: commit injection snapshots or generate on first run?
- Tier-B evals: `tiny_crate` only, SWE-bench `dev` only, or both?
- SWE-bench grading: full Docker harness vs lightweight `pytest` venv for `dev` subset?
- Which 3 instances are the fixed **smoke trio**? (Need one pilot run on local model.)
- How to extract agent patch — `git diff` after turn vs require explicit `patch` tool?
- Mock MCP: which SDK (`rmcp`, official Rust MCP crate, stdio JSON-RPC hand-roll)?
- Nightly evals: self-hosted GitHub runner with GPU vs manual `evals/run.sh` only?
- Should summarization quality use a **condenser** endpoint in eval rubric (see [context-ideas.md](./context-ideas.md) §12)?
- SWE-bench `test` (300): run at release only, or never automate (manual benchmark)?

---

## 13. References

- [SWE-bench Lite overview](https://www.swebench.com/lite.html)
- [Hugging Face — SWE-bench_Lite](https://huggingface.co/datasets/princeton-nlp/SWE-bench_Lite) (`dev` / `test` splits)
- [SWE-bench GitHub + harness](https://github.com/swe-bench/SWE-bench)
- [JetBrains context management study](https://blog.jetbrains.com/research/2025/12/efficient-context-management/) (SWE-bench Verified methodology)
- [OpenHands context condenser](https://docs.openhands.dev/sdk/guides/context-condenser)

---

*Update this document when `ToolBackend`, `evals/`, `evals/swebench/`, or CI harness jobs land.*