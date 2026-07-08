# Agent Calibration — Per-Model Harness Tuning

Date noted: 2026-07-08

This document records an idea for **calibrating harness behavior per model**: run a battery of small, deterministic eval scenarios against each inference endpoint, measure which prompting / tool / edit strategies produce the most reliable results, and **apply the winning profile automatically** when that model is active.

Motivation came from recurring failure modes in session logs — e.g. agents calling `patch` with identical `search` and `replace`, then misreading `✅ Patched (no net change)` as "search didn't match." Different models exhibit different failure signatures (XML tool calls vs native function calling, `write` vs `patch`, tolerance for narration, etc.). A one-size-fits-all system prompt and tool policy leaves reliability on the table.

Related:

- [testing-ideas.md](./testing-ideas.md) — eval pyramid, mock tools, scenario assertions
- [agent-operating-modes.md](./agent-operating-modes.md) — stance/mode knobs (work, talk, think)
- [nudge-judge.md](./nudge-judge.md) — continuation pressure and criteria enforcement
- [context-ideas.md](./context-ideas.md) — tiered inference, session memory
- `src/config.rs` `ContextBudget` — precedent for **probe once, adapt runtime** (context window → truncation limits)

---

## 1. Core Idea

Treat harness configuration as a **calibration profile** with two layers:

| Layer | When computed | Purpose |
|-------|---------------|---------|
| **Universal defaults** | CI / release gate; offline matrix across many models | Safe baseline that works "well enough" on most backends |
| **Per-model profile** | First use of a model (or on demand); cheap probe battery | Override defaults only where measurement shows a clear win |

At runtime, when the user selects (or probes) an endpoint + model id, the harness loads `~/.raven-hotel/calibration/<model-key>.json` (or falls back to universal defaults). No manual per-model prompt editing.

```
                    ┌──────────────────────┐
                    │ Universal defaults   │  ← matrix run in CI / nightly
                    │ (agent-calibration   │
                    │  baselines.json)     │
                    └──────────┬───────────┘
                               │ fallback
     probe / first session     ▼
┌─────────────┐      ┌──────────────────────┐      ┌─────────────────┐
│ User picks  │ ──►  │ Calibration probe    │ ──►  │ Per-model       │
│ model M     │      │ (fast, ~10 scenarios)│      │ profile for M   │
└─────────────┘      └──────────────────────┘      └────────┬────────┘
                                                             │
                                                             ▼
                                                    Agent loop uses profile:
                                                    prompt blocks, tool policy,
                                                    error shaping, nudge tier, …
```

---

## 2. What We Calibrate ("Methods")

Each **method** is a discrete harness choice the agent loop can switch on. Calibration does not tune model weights — only **how the harness presents tools, errors, and instructions**.

### 2.1 Edit strategy

| Variant | Description |
|---------|-------------|
| `patch_preferred` | Current default: prefer `patch`, `read` before edit |
| `write_preferred` | Steer toward full-file `write` for small files |
| `patch_with_near_line_hint` | Extra prompt line: always pass `near_line` when patching |
| `patch_strict_errors` | Custom error text when `search == replace` or no net change (see §5) |

**Probe tasks:** single-line edit, multi-match disambiguation, "fix dependency version" (Cargo.toml-style), idempotent re-patch.

### 2.2 Tool transport

| Variant | Description |
|---------|-------------|
| `native_tools` | OpenAI-style `tool_calls` in API response |
| `xml_tools` | Parse `function=patch>` blocks from content (`tool_xml.rs`) |
| `hybrid` | Native first; XML fallback when model emits XML |

Some local models only reliably emit XML; others break on native schemas. Calibration picks the lane with highest **correct tool invocation rate**.

### 2.3 Tool schema exposure

| Variant | Description |
|---------|-------------|
| `full_schema` | All tools in every request |
| `minimal_schema` | Drop rarely-needed tools per scenario class |
| `no_tools_ping` | Connectivity-only (existing `smoke_ping` pattern) |

### 2.4 Prompt / discipline blocks

Inject or omit modular system-prompt sections (already partially structured in `agent_system_prompt.rs`):

- Tool discipline (`read` before `patch`, verbatim `search`)
- Patch failure interpretation ("no net change means search matched")
- `define_done` / goal usage
- Context-management hints for small context windows

### 2.5 Loop / nudge policy

| Variant | Description |
|---------|-------------|
| `aggressive_continue` | Strong empty-response and narration nudges |
| `gentle_continue` | Fewer nudges; higher pure-text tolerance |
| `judge_on` / `judge_off` | Full V2 judge path vs lightweight completion |

### 2.6 Read / context shaping

| Variant | Description |
|---------|-------------|
| `default_read_limit` | From `ContextBudget` |
| `tight_read_limit` | Force `lines=` ranges in probe scenarios |
| `read_summary_first` | Require `read_summary` before full `read` on large files |

Not every axis needs per-model tuning on day one. **Phase 1** should focus on edit strategy + tool transport + patch error shaping — the highest-impact failures seen in `full_log.jsonl`.

---

## 3. Calibration Test Battery

### 3.1 Design principles

- **Small and fast** — each scenario completes in 1–3 tool rounds with mock workspace fixtures (extend `evals/scenarios/`).
- **Deterministic grading** — assert on tool args, file hash, log substrings; no subjective LLM judge required for calibration itself.
- **Factorial, not exhaustive** — run one scenario per method axis first; only run cross-products when a model fails the universal default on that axis.
- **Live LLM required** — replay/mock scenarios validate harness plumbing; calibration measures **model behavior**.

### 3.2 Proposed scenario suite (`evals/calibration/`)

| Scenario | Tests | Pass criteria |
|----------|-------|---------------|
| `cal_patch_single_hunk` | `read` → `patch` one line | File content changed; `search ≠ replace` |
| `cal_patch_multi_match` | Two identical lines; must use `near_line` | Correct hunk patched |
| `cal_patch_no_op_detect` | Prompt: change X→Y; file already has Y | Skip patch or succeed without `search==replace` mistake |
| `cal_write_small_file` | Create 10-line file via `write` | Exact bytes on disk |
| `cal_read_then_patch` | Must `read` before `patch` (log order) | `read` appears in log before `patch` |
| `cal_xml_tool_call` | Model on XML-only backend | Tool parsed from content, not native |
| `cal_native_tool_call` | Model on API with tools | Valid `tool_calls` JSON |
| `cal_recovery_search_miss` | Patch fails "not found"; recover | Second patch succeeds after `read` |
| `cal_no_line_numbers_in_search` | `read` output has `7 \|` prefix | `search` does not include line-number decoration |
| `cal_exec_after_edit` | Edit + run verification command | `exec` after `patch` when asked to "run tests" |

Each scenario is parameterized by **method profile** (env or CLI flags). Example:

```bash
RAVEN_EVAL=1 cargo run -q --bin raven-eval -- \
  calibration --model llama-3.1-8b \
  --profile patch_preferred,native_tools \
  --scenarios cal_patch_single_hunk,cal_patch_multi_match
```

### 3.3 Scoring

Per (model, profile, scenario):

| Metric | Weight | Notes |
|--------|--------|-------|
| Pass/fail | High | Binary task success |
| Tool rounds | Medium | Fewer is better, capped |
| Invalid tool args | High | e.g. `search == replace`, malformed JSON |
| Recovery steps | Low | Extra `read` after failure is OK if eventual pass |
| Log violations | Medium | `log_must_not_contain` patterns |

Aggregate to a **reliability score** per profile. Pick the profile with highest score; break ties by fewer rounds and simpler config (fewer overrides from universal default).

---

## 4. Universal Defaults vs Per-Model Probe

### 4.1 Universal defaults (offline matrix)

Run the full battery across a **reference model set** (small local quant, one mid-size API model, one frontier API model) on CI nightly or `workflow_dispatch`.

Output: `evals/calibration/baselines/universal_profile.json`

```json
{
  "version": 1,
  "defaults": {
    "edit_strategy": "patch_preferred",
    "tool_transport": "hybrid",
    "patch_error_mode": "strict",
    "nudge_tier": "aggressive_continue",
    "tool_discipline_block": "full"
  },
  "matrix_summary": {
    "patch_preferred+hybrid": { "pass_rate": 0.91, "models_passing": 12 },
    "write_preferred+native_tools": { "pass_rate": 0.78, "models_passing": 9 }
  }
}
```

Ship universal defaults **in the binary or as a bundled JSON** so fresh installs work before any probe.

### 4.2 Per-model efficient probe

When the user selects a model that has **no cached profile** (or profile older than N days / harness version bump):

1. Run **universal default profile** against a **minimal probe subset** (3–5 scenarios, ~30–60s).
2. If all pass → cache `{ "source": "universal_default", "verified_at": … }` for that model; no overrides.
3. If any fail → run **axis-specific variants** only for failed axes (not full factorial).

Example: model fails `cal_patch_single_hunk` with `patch_preferred` but passes with `write_preferred` → set `edit_strategy: write_preferred` for that model only.

Probe runs in background after endpoint probe (`eval_operator/probe.rs`) or lazily on first agent turn (with a one-line status: "Calibrating harness for &lt;model&gt;…").

### 4.3 Cache layout

```
~/.raven-hotel/calibration/
  by-model/
    sha256(openrouter/anthropic/claude-sonnet-4).json
    sha256(local/llama-3.1-8b-q4).json
  baselines/
    universal_profile.json   # copied from repo on upgrade
```

Profile schema (sketch):

```json
{
  "model_id": "llama-3.1-8b-instruct",
  "endpoint_label": "local-llama",
  "harness_version": "0.2.0",
  "verified_at": "2026-07-08T12:00:00Z",
  "source": "probed",
  "overrides": {
    "edit_strategy": "write_preferred",
    "tool_transport": "xml_tools"
  },
  "scores": {
    "cal_patch_single_hunk": { "pass": true, "rounds": 2 },
    "cal_xml_tool_call": { "pass": true, "rounds": 1 }
  }
}
```

---

## 5. Runtime Application

Introduce `AgentCalibration` (or extend `Config` / `RuntimeFlags`) loaded at session start:

```rust
struct AgentCalibration {
    edit_strategy: EditStrategy,
    tool_transport: ToolTransport,
    patch_error_mode: PatchErrorMode,
    prompt_blocks: PromptBlockSet,
    nudge_tier: NudgeTier,
}
```

**Hook points:**

| Location | Effect |
|----------|--------|
| `agent_system_prompt.rs` | Include/omit discipline blocks per profile |
| `tools/fs.rs` `patch_file` | `PatchErrorMode::Strict` → distinct messages for `search == replace` vs true no-op |
| `llm.rs` | Prefer native vs XML parsing per `tool_transport` |
| `agent_driver.rs` | Nudge selection per `nudge_tier` |
| `tools/mod.rs` | Tool defs trimmed or emphasized (e.g. extra `patch` description for patch-heavy models) |

Precedent: `ContextBudget::from_context_tokens` already adapts limits from probe — calibration is the same pattern for **behavioral** knobs.

---

## 6. Patch Error Shaping (Immediate Win)

Regardless of full calibration rollout, one method axis is worth implementing early because it addresses a observed log failure mode:

| Current message | Problem |
|-----------------|---------|
| `✅ Patched (no net change — …)` | Agents read as success or ambiguous failure |
| `⚠️ Search text not found` | Different failure, but agents conflate the two |

**Strict mode** (candidate default after calibration confirms):

```
⚠️ Patch had no effect: search and replace are identical. Set replace to the NEW text you want.
```

```
⚠️ Patch had no effect: replacement produced identical file content. Re-read the file and check replace.
```

```
⚠️ Search text not found in <path>. Re-read the file; search must match verbatim (no line numbers).
```

Calibration scenario `cal_patch_no_op_detect` scores whether the model recovers after each message type.

---

## 7. Implementation Phases

### Phase 0 — Document + fixtures (this file)

- [x] Capture design
- [x] Quick probe script: `evals/run_patch_calib.sh` (temp `/tmp` workspace, prompt variants, multi-endpoint)
- [x] Stress variants: `nested_path`, `plan_framing`, `ambiguous_fix`, `session_warmup`, `self_authored`, `recovery_after_noop`, `harness_system`, `harness_strict` (+ `evals/patch_calib_helpers.py` for session seeding)
- [ ] Add 3–5 `evals/calibration/*.json` scenarios (can fork `minimal_end_to_end_patch.json`)
- [ ] Add `log_must_not_contain: ["search and replace are identical"]` style assertions

### Phase 1 — Probe runner

- [ ] `raven-eval calibration` subcommand (or extend `eval_operator`)
- [ ] `--profile` flag plumbing into harness for method variants
- [ ] Write `universal_profile.json` from nightly matrix

### Phase 2 — Per-model cache + lazy probe

- [ ] Load profile in `main` / session bootstrap from `~/.raven-hotel/calibration/`
- [ ] Trigger minimal probe on unknown model (background task)
- [ ] UI indicator: calibrated ✓ / probing… / fallback defaults

### Phase 3 — Dynamic runtime

- [ ] Wire `AgentCalibration` into prompt builder, patch errors, tool transport
- [ ] Invalidate cache on `harness_version` bump
- [ ] Optional: expose profile in `/settings` or status bar

---

## 8. Open Questions

1. **How often to re-probe?** Model updates behind the same id string (OpenRouter revisions) may need TTL or manual "recalibrate" command.
2. **Cost cap** — probe battery on every new API model could add up; gate behind setting or run minimal subset only.
3. **User overrides** — should `/stance` or settings pin a profile regardless of calibration?
4. **Multi-model sessions** — if user switches model mid-session, swap profile and optionally inject one-turn "harness note" so the model isn't surprised by stricter tool discipline.
5. **Correlation with context size** — small-context models may need `write_preferred` *and* `tight_read_limit`; joint optimization vs independent axes.

---

## 9. Success Criteria

- New model on a fresh machine: universal defaults work without manual prompt edits.
- Known-problematic local model: probe selects `xml_tools` + `write_preferred` within one minute; subsequent patch tasks show fewer `search == replace` failures in `full_log.jsonl`.
- CI: calibration matrix regression — universal default pass rate does not drop on harness changes.
- Developers can run `raven-eval calibration --model <id> --verbose` and get a human-readable report + suggested `overrides` JSON.

---

*This is a design note, not implemented behavior. Update as scenarios land in `evals/calibration/` and hook points are added to the agent loop.*