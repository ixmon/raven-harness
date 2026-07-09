# Plan Mode — Design & Implementation Status

Date: 2026-07-07  
Status: **mostly implemented** on branch `refactor/plan-sync`  
Audience: contributors working on the Raven TUI plan workflow

## Purpose

The Raven TUI **plan mode** is a two-phase workflow:

1. **Clarify** — agent runs in `agent_mode == "plan"`, asks structured questions, drafts `wiki/plan.md`.
2. **Execute** — user says proceed; harness switches to `work` mode, shows the Plan pane, and gates progress through `complete_plan_step`.

This document describes the verification model, module layout, what is landed, and what remains.

---

## Architecture (current)

| Layer | Module | Role |
|-------|--------|------|
| UI state | `plan_state.rs` | `PlanState`, steps, tiers, loop phase, `project_workdir` |
| Lifecycle | `plan_flow.rs` | Entry, JSON clarify/propose loop, proceed, `parse_plan_md`, `/plan` slash |
| Drafting LLM | `plan_loop.rs` | `fetch_clarification`, `fetch_proposal` (with verification retry) |
| Verification QA | `plan_verification.rs` | Auto-upgrade weak checks, validate, lint, adversarial retry |
| Protocol | `plan_protocol.rs` | Clarify/proposal JSON types and formatting |
| Prompts | `plan_prompts.rs` | Wiki template, execution user prompt, status formatting |
| Markdown | `plan_md.rs` | `plan-steps:json` block parse/serialize |
| Execution | `plan_execution.rs` | `complete_plan_step`, tier runners, cwd resolution, injection |
| Sync | `plan_sync.rs` | `PlanState` ↔ `PlanExecutionState` ↔ wiki execution log |
| UI render | `plan_pane_render.rs` | Orange = gathering; green = approved; tier badges + progress bar |
| Steering | `steering.rs` | Plan-mode exemptions; **plan-execution nudge before criteria judge** |
| Tools | `tools/mod.rs` | `tools_for_agent()` — mode-scoped schema |
| System prompt | `agent_system_prompt.rs` | Plan vs work execution instructions |
| Entry | `input_handler.rs` | NL triggers, plan-loop submit, proceed → `start_plan_execution` |

**Persistence:** `wiki/plan.md` (structured steps + execution log) and `session.meta` (goal / achievement tests).

**Tests:** `plan_flow`, `plan_state`, `plan_sync`, `plan_prompts`, `plan_verification`, `plan_execution`, and `tools` mode-scoping tests (~37 plan-related unit tests).

---

## Verification model (decided & implemented)

**Empirical-first at every step; negotiate checks during planning; execute with minimal surprise.**

### Planning phase = verification design

For every step the agent proposes a real check. Weaker tiers (`attested`, `observe`) require explicit justification. The user confirms the whole bundle at proceed — goal, steps, per-step tier + verification, any human-observation steps, and whole-task success criteria.

The JSON-driven loop (`plan_loop.rs`) fetches clarify questions, then a proposal. Before the user sees the recap, `improve_proposal()` in `plan_verification.rs` runs harness-side fixes and validation.

### Verification tiers

| Tier | Planning | On `complete_plan_step` (execution) |
|------|----------|-------------------------------------|
| `exec` | Propose exact runnable command | Harness runs command; exit 0 → Done, else Failed |
| `check` | `file_exists:path` or `grep:pattern:path` | Harness runs check spec |
| `attested` | Note explaining why no automated check | Requires non-empty `evidence`; logged to wiki execution log |
| `observe` | Prompt text for the user | Harness pauses; user reply via `submit_user_observation` completes step |

### Execution phase = run the approved script

- Progress advances **only** via `complete_plan_step` or observe submission. Per-tool / per-turn auto-advance is **removed**.
- Step verifications run from the plan's **project workdir** (see below) unless the command includes `cd … &&` or `workdir:dir\|cmd`.
- Whole-plan completion: judge `WORK_COMPLETE`, last step passes, or user abandons via `/plan cancel` (no `/plan done` yet).

---

## Verification quality gates (`plan_verification.rs`)

Implemented in two phases:

### Phase A — structural validation + auto-upgrade

On every proposal (and on LLM retry when errors remain):

| Anti-pattern | Harness action |
|--------------|------------------|
| `cat >`, `tee`, `touch` as verification | Upgrade → `check` + `file_exists:<path>` |
| bare `mkdir` as verification | Upgrade → `exec` + `test -d <paths>` |
| acquire/download + bare `test -d` | Upgrade → `test -d … && test -n "$(ls -A …)"` |
| Paths prefixed with project dir | Normalize to workdir-relative paths |
| observe without prompt / attested without note | Blocking error → proposal retry |
| creation commands as verification | Blocking error → proposal retry |
| Create-file with bare `file_exists` | Recipe → `min_bytes:<path>:<N>` (extension floors) |
| Binary `grep` on `build/*` | Recipe rewrite to build-only / source check; else block |

**Recipe catalog** (`plan_recipes.rs`): table-driven match on step description (create file/dir, acquire, build, implement). Injected into the proposal system prompt and applied in `improve_proposal`.

Recap surfaces three buckets:

- **Harness adjusted** (auto-fixes)
- **Remaining verification issues** (errors — user should revise before proceed)
- **Verification advisories** (warnings — non-blocking)

### Phase B — weak-verification lints + adversarial retry

Non-blocking warnings for known footguns (seeded from real session logs):

- Shell pipes in success criteria without `pipefail` (masks build failures)
- Non-terminating commands in success criteria (`./galaga`, `cargo run`, servers, etc.)
- Acquire steps verified only by existence (empty dir/file passes)
- `grep:class` / `grep:struct` in `.cpp` (C/C++ convention mismatch)
- Implementation steps verified only by `grep` (symbol ≠ compiles)

When warnings exist and errors do not, `fetch_proposal` optionally retries with an **adversarial critique nudge** — asks the model to harden verifications for *this* stack (cargo, cmake, pytest, etc.). Retry is accepted only if errors stay empty and warning count does not increase.

**Note:** Warnings and residual errors do not hard-block proceed in the UI; the user can still say proceed. Execution gates remain the backstop.

---

## Project directory (`project_workdir`)

When the user specifies a subdirectory (e.g. "everything in `./galaga/`):

1. `extract_project_workdir_from_text()` / `resolve_project_workdir_from_context()` pick it up from the initial request and clarify Q&A.
2. Proposal prompts inject a **project directory** section; `constraints` are amended.
3. `PlanState.project_workdir` is stored at proposal time.
4. On execution, `PlanExecutionState.project_workdir` drives:
   - `format_deliverable_location_section()` in system injection and execution prompt
   - `resolve_verification_cwd()` for step verifications
   - `detect_project_workdir()` fallback when cwd is unset but a scored subproject exists on disk

---

## Tool exposure by run mode (implemented)

`tools_for_agent(agent_mode, flags, plan_ctx)` in `tools/mod.rs`:

| Tool | `plan` | `work` + active plan | `work` (no plan) | other modes |
|------|--------|----------------------|------------------|-------------|
| `revise_plan_step` | ✅ | ✅ | ❌ | ❌ |
| `complete_plan_step` | ❌ | ✅ | ❌ | ❌ |
| `define_done` | ❌ | ✅ | ✅ | varies |
| workspace `write`/`patch` | ❌ (wiki only) | ✅ | ✅ | ✅ |

`plan_mode_denial` in `agent.rs` remains the safety net for disallowed workspace writes during clarification.

`tools_list_for_prompt()` is derived from the same function — prompt tool list matches schema per mode.

---

## Steering during execution

When `plan_execution_incomplete()` and `agent_mode == "work"`, the harness injects a **plan-execution nudge** *before* the criteria judge or define-done reminder. This prevents mid-step pauses when the agent narrates "I should call `complete_plan_step`" but does not call it.

**Escalation (implemented):** consecutive text-only stops while plan execution is incomplete escalate after the first soft stall, immediately if the model *narrates* `complete_plan_step` without calling it, or after any tool use without the gate. Escalated messages demand a real `complete_plan_step` tool call and include the current step description/verification. Agents can still refuse after budget exhaustion (turn ends); the user can continue or `/plan done`.

---

## Slash commands (implemented)

| Command | Behavior |
|---------|----------|
| `/plan` | Open plan entry dialog |
| `/plan <goal…>` | Enter plan mode with goal text |
| `/plan status` | Dump goal, steps, progress to conversation |
| `/plan cancel` | Exit plan mode; clear pending entry |

Documented in `/help` via `input_dispatch.rs`.

---

## Phase checklist

### Phase 1 — Extract and deduplicate ✅

- [x] `plan_flow` module (triggers, proceed, defaults, `parse_plan_md`, execution start)
- [x] `derive_verification_defaults` — single function, three call sites (`PlanEntry`, `ProceedFallback`, `AutoActivate`)
- [x] `plan_sync`, `plan_state`, `plan_prompts`, `plan_pane_render` extractions

### Phase 2 — System prompt coherence ✅ (mostly)

- [x] Plan mode prompt: exploration allowed; `define_done` / `record_discovery` suppressed
- [x] `tools_list_for_prompt` aligned with `tools_for_agent`
- [x] Work-mode plan execution block in `agent_system_prompt.rs`
- [ ] Full audit of every stale phrase in `agent.rs` legacy blocks (low priority)

### Phase 3 — Structured `wiki/plan.md` ✅

- [x] `plan-steps:json` comment block (`plan_md.rs`)
- [x] `parse_plan_md` prefers JSON; legacy line heuristics as fallback
- [x] JSON-driven clarify → propose loop with proceed consent
- [x] Verification-first proposal system prompt + wiki skeleton examples

### Phase 4 — `complete_plan_step` + tiered verification ✅

- [x] `tools_for_agent` mode-scoped assembly + tests
- [x] `complete_plan_step` session tool (work + active plan)
- [x] Tier implementations: exec, check, attested, observe
- [x] `revise_plan_step` (plan mode + executing work mode)
- [x] `PlanStep` / `PlanStepTier` schema + pane tier labels
- [x] No per-tool / per-turn auto-advance
- [x] Execution log appended to `wiki/plan.md` on exec/attested/observe
- [x] `plan_sync::reconcile_plan_execution` — wiki log + agent state, no regression
- [x] Hard-block proceed when proposal has validation errors
- [x] `/plan done` to force whole-plan completion

### Phase 5 — UX polish (partial)

- [x] `/plan` slash family
- [x] Plan pane shows tier + verification on steps; progress from `complete_plan_step`
- [x] Approval-mode submenu works during inference (`slash_ok_while_processing_resolved`)
- [ ] Immediate `PlanState` sync on every `update_goal` (partial — poll loop syncs goal/tests during plan mode)
- [ ] Safer entry UX (no auto-prefill `"y"` on trigger dialog)
- [ ] Wiki viewer link from plan pane

### Verification hardening (post-Phase 4, in tree)

- [x] Download/acquire empty-dir upgrade
- [x] Success-criteria pipe / non-terminating lints
- [x] Per-step grep/C++ convention lints
- [x] Adversarial critique retry on warnings
- [ ] Richer `check` tier (`line_count:`, lint integration)
- [x] Escalation when agent skips `complete_plan_step` for N turns

---

## Module map (quick reference)

```
src/plan_flow.rs         Entry, proceed, parse_plan_md, /plan, present_proposal
src/plan_loop.rs         fetch_clarification, fetch_proposal (+ retry logic)
src/plan_verification.rs improve_proposal, lints, workdir resolution
src/plan_execution.rs    complete_plan_step, cwd, injection, revise_plan_step
src/plan_sync.rs         PlanState ↔ PlanExecutionState ↔ wiki log
src/plan_state.rs        PlanState, PlanStep, PlanLoopPhase
src/plan_prompts.rs      Wiki template, execution prompt
src/plan_md.rs           plan-steps:json block
src/plan_protocol.rs     Clarify/proposal JSON
src/plan_pane_render.rs  Side pane UI
```

---

## Testing strategy

| Area | Coverage |
|------|----------|
| `plan_flow` | Triggers, proceed phrases, defaults, JSON + legacy parse, structured proceed |
| `plan_verification` | Auto-upgrade, validation errors, warning lints, adversarial nudge format |
| `plan_execution` | cwd resolution, workdir inference, complete_plan_step reject paths |
| `plan_sync` | No regression on sync; execution log hydration |
| `tools` | Plan vs work schema; `complete_plan_step` only when executing |
| `steering` | Plan-execution nudge beats criteria judge |
| Manual | Full flow: `/plan` → clarify → proposal recap → proceed → execute → verify pane + wiki log |

Recommended manual smoke after changes:

```bash
cd tui
cargo test --no-default-features plan_
cargo clippy --no-default-features -- -D warnings
```

---

## Known gaps (from production sessions)

1. **`complete_plan_step` compliance** — biggest execution risk; steering nudge is not sufficient alone.
2. **Proceed not blocked on validation errors** — recap warns but user can proceed anyway.
3. **Thin `check` tier** — `grep` proves symbol existence, not correctness; draft lints warn but execution accepts.
4. **Triplicate state** — `PlanState`, `PlanExecutionState`, and `wiki/plan.md` are reconciled by `plan_sync` but not collapsed to a single source of truth.
5. **Heuristic lints** — language-specific seeds; adversarial retry generalizes but is best-effort.

---

## Open questions

1. ~~**Advance policy**~~ — **Done:** `complete_plan_step` only.
2. ~~**Structured format**~~ — **Done:** `plan-steps:json` in `wiki/plan.md`.
3. **`define_done` in plan mode** — **Done:** suppressed; work mode after proceed.
4. **Block proceed on errors?** — Open; currently soft (recap only).
5. **`check` tier DSL** — Open; fixed `file_exists` / `grep` only today.
6. **Force `complete_plan_step`** — Open; nudge exists, no escalation ladder yet.

---

## Success criteria (overall)

- [x] Verification defaults defined in exactly one function
- [x] `input_handler` no longer contains monolithic plan lifecycle logic
- [x] Proceed extracts steps from structured `plan-steps:json` block
- [x] Progress advances only via `complete_plan_step` (no per-tool/per-turn auto-advance)
- [x] Each step has a tier; exec/check failures set `Failed`
- [x] Attested steps log evidence; observe steps pause for user input
- [x] `/plan` documented in `/help`
- [x] Plan-only tools omitted from non-plan schemas; `complete_plan_step` only in work + active plan
- [x] `tools_list` matches actual schema per mode
- [x] Draft-time verification quality gates (auto-upgrade, validate, lint, optional retry)
- [x] Project workdir persisted from user text through execution
- [ ] No contradictory plan-mode instructions (mostly; occasional drift possible)
- [ ] Hard-block proceed on unresolved validation errors
- [ ] Reliable agent `complete_plan_step` compliance without manual nudging