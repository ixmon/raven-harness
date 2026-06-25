# Nudge and Judge System — Nudge-v1 (Current Behavior)

**This document describes the current baseline behavior, referred to as "nudge-v1".**

All test and eval results captured while this document is the active description (including the baselines below) were obtained using this Nudge-v1 implementation. Nudge-v2 has not yet been implemented.

The goal of this document is to provide a clear, accurate record of the starting point before any changes for "nudge-v2".

Later sections will capture proposed v2 designs, experiments, and comparative results.

## Overview

The nudge/judge system is a hybrid of:

- Hardcoded heuristics that catch common "I narrated instead of acting" or "early empty response" patterns.
- An inference-based LLM judge (`judge_turn`) that decides whether the task (or the agent's self-declared criteria) appears complete.
- Special aggressive nudging when the agent has used `define_done()` to declare completion criteria.

The central implementation lives in:
- `src/agent_driver.rs`: `drive_turn()` (the main loop), all nudge decision points, criteria handling, and auto-continue logic.
- `src/agent.rs`: `judge_turn()`, `push_continuation_nudge()`, `define_done` / `completion_criteria` handling, and harness event logging.

The system is used for both interactive TUI runs and headless `--prompt` / eval runs.

### Behavior in Normal Interactive Mode (vs. Eval)

In **normal interactive use** of `raven-tui` (no `RAVEN_EVAL` or `RAVEN_EVAL_MOCK_LLM` environment variables):

- The aggressive criteria-based nudging path is **mostly inactive**.
- The system prompt does **not** strongly instruct the agent to call `define_done()` early (this instruction is suppressed outside eval mode).
- As a result, `completion_criteria` is rarely set by the agent, so the "judge on every no-tool round while criteria is active" logic does not trigger.
- Most nudging that occurs is from the **hardcoded heuristic paths** (plan-narration detection, empty-response recovery, length recovery, etc.).
- The LLM `judge_turn` can still be called in limited situations:
  - On the 3rd empty response recovery.
  - In the general "rich inference judge" path after the model has used tools (or produced malformed tool syntax) and `finish_reason != "stop"`.
- Goal tracking is off by default (no `RAVEN_GOAL_TRACKING`), which further reduces judge activity.

In contrast, under `RAVEN_EVAL` (or when the agent explicitly calls `define_done`), the full judge + aggressive nudging (with 999 limit) becomes active.

This distinction is intentional: strong define_done / criteria-driven judging behavior is primarily intended for evaluation harnesses.

## Key Constants

- `MAX_TEXT_NUDGES = 3` — normal safety limit on explicit nudges before accepting a text-only response.
- When `completion_criteria` is active, many paths use `999` as the displayed max and continue nudging on judge "Continue".
- `MAX_AUTO_CONTINUES = 3` — how many times we will auto-continue when the inner tool-round budget is exhausted but the model is still calling tools.
- Hard per-budget cap: `MAX_TOOL_ROUNDS = 120` (quite high for safety).

## Types of Nudges / Interventions

### 1. Hardcoded / Heuristic Nudges

These fire before or outside the main LLM judge in many cases:

- **Plan-narration detection** (`looks_like_plan_narration`): Detects phrases like "let me ", "I will ", "the fix is", "implement this", "re-read the code", etc. with no tool calls. Pushes `CONTINUATION_NUDGE` and continues. Limited by `MAX_TEXT_NUDGES`.
- **Empty response recovery**: On early turns with no content and no tools (common with pure system + injection prompts), nudge the agent to start exploring. Up to 3 times; on the 3rd it consults the judge.
- **Length recovery**: When `finish_reason == "length"`, push `LENGTH_RECOVERY_NUDGE` encouraging tool use.
- **LLM stream error recovery**: On transient decode errors, push a simple "continue" message (not counted against normal nudge budget in some paths).
- **Hard safety for "implies run"**: If the original request text (from `last_user_request`) contains "run"/"exec"/"show"/"output" but no `exec` action has been observed yet, force a nudge (even if judge might otherwise accept). There is also a secondary check after a write/patch.
- **Malformed tool syntax detection**: If the model output or recent log tail contains tool XML fragments but no parsed tool calls, this can route to the judge.

`push_continuation_nudge()` (and direct `push_message("user", ...)` for others) injects the nudge as a user-role message so the model sees it on the next turn.

In `RAVEN_EVAL` mode, some nudges (especially continuation) also append a short "(Original request reminder: ...)" re-anchor.

### 2. Criteria-Based Judging (define_done path)

This is the most aggressive path and the main motivation for many recent changes.

- When the agent calls the `define_done` tool, it sets `completion_criteria` (one-time only; subsequent calls are rejected).
- On **every** no-tool round while `completion_criteria` is non-empty:
  - The LLM `judge_turn()` is called.
  - **Fulfilled**: Clear the criteria in session meta and accept completion.
  - **Stuck**: Surface guidance and stop.
  - **Continue**: Increment a special counter (displayed as `/999`), log a "criteria-continue nudge", push a user message containing the exact criteria text plus "Now use tools to satisfy the criteria...", and `continue`.
- This path intentionally bypasses the normal `MAX_TEXT_NUDGES=3` limit ("the judge should keep nudging if the criteria for done is not met, not just on the 3rd call").

The nudge text is of the form:
`[You defined done as: <criteria>. Now use tools to satisfy the criteria (read the files and patch the source).]`

### 3. General LLM Judge (`judge_turn`)

Called when a stop is "ambiguous":

- After the model has used tools in the turn, or
- When malformed tool syntax was detected, and
- `finish_reason != "stop"` (i.e. the model did not deliberately finish).

The judge receives a carefully constructed prompt containing:
- Current goal (from session)
- `achievement_tests` (if any)
- `completion_criteria` (if set via define_done)
- `last_user_request` (the original prompt, for context)
- Up to 6 recent actions (tool name + summary)
- The latest model output text

**Critical rules** in the judge prompt (paraphrased):
- Only FULFILLED if actions provide *clear evidence* the entire request is complete (not just the model's claim).
- For requests that imply "run/exec/show output", an `exec` with matching output is generally required.
- A write/patch alone is not enough for "run it" requests.
- For bug-fix style tasks: at least one successful edit on a *main source file* (not just a temp diagnostic script).
- When the agent defined criteria via `define_done`, FULFILLED only if recent actions clearly satisfy that *exact* definition.
- Claims without supporting actions → not fulfilled.

The LLM is asked to reply with the first line being exactly `FULFILLED`, `CONTINUE`, or `STUCK`, followed by a short reason (and guidance if stuck).

`judge_turn` returns `TurnJudge::Fulfilled { note }`, `Continue`, or `Stuck { reason, suggested_guidance }`.

In the driver, most judge decisions are sent to the observer (for UI/logs) and logged via harness events, but are **not** pushed as user messages into the conversation (to avoid polluting `last_user_request` scans and self-referential loops).

### 4. Round-Budget Auto-Continue

When the inner `max_rounds` (per budget window) is exhausted but the model was still calling tools, the outer loop can auto-continue up to `MAX_AUTO_CONTINUES` times (observer sees `on_round_limit`).

## Overall Flow Sketch (no-tool round, simplified)

```
if completion_criteria is set and non-empty:
    decision = judge_turn(...)
    if Fulfilled: clear criteria; stop
    if Stuck: guidance; stop
    if Continue: nudge-with-criteria (count/999); continue

if plan-narration keywords and no tools: nudge; continue

if empty text (early turn): normal empty-recovery nudge (or judge on 3rd)

if looks like malformed tool syntax or (tools used this turn and not explicit stop):
    apply hard-safety checks (implies-run without exec, etc.)
    if still ambiguous:
        decision = judge_turn(...)
        match decision:
            Fulfilled -> stop (clear criteria if set)
            Stuck -> guidance (+ optional nudge)
            Continue -> normal nudge (respect MAX or 999); continue

otherwise:
    stop (completed_naturally)
```

After executing tools, the loop continues until a no-tool decision or outer limits.

## Logging, Observability, and Side Effects

- Harness events: `nudge`, `judge`, `stuck`, `define_done_instruction`, `define_done_called`, `llm-stream-error-recovery`, `length-recovery`, etc. (with round numbers in many cases).
- `TurnObserver::on_nudge(count, max)` — TUI uses this for display; headless prints `[nudge N/M]`.
- Nudges that are meant to influence the *model* are pushed into `conversation` as user messages.
- Judge decisions are deliberately kept out of the normal user message stream in most paths (see comments about polluting `request_text`).
- `full_log.jsonl` (and sometimes `raven_log.jsonl` copies) contain the events for post-run analysis.
- In eval runs, some re-anchoring using `last_user_request` is added to nudges and summaries.

## Interaction with define_done and Sessions

- `define_done` is the primary way an agent declares its own success criteria for the current task. It is one-time.
- The criteria lives in session meta and is injected in the SESSION CONTEXT block.
- The judge is the main enforcement mechanism: it is what actually clears the criteria on fulfillment.
- `last_user_request` is used for safety checks and (in eval) for re-anchoring nudges.
- The system coexists with (and sometimes overrides) the older "3 nudge then accept" idea.

## Known Characteristics / Rough Edges (as of nudge-v1)

- Criteria nudging can produce many "You defined done as: ..." messages if the judge stays on Continue for a long time.
- Plan-narration detection is keyword-based and can fire on legitimate planning text.
- The LLM judge is only as good as the model being used for judging; conservative judges can cause extra nudges even after substantial correct work.
- Hard safety rules (run-without-exec) can force nudges even when other logic might accept completion.
- Some paths only consult the judge after tools have been used or on ambiguous stops; pure text responses on the first turn often hit the empty/plan paths first.
- In non-eval runs the re-anchoring is lighter; in eval runs more context is injected into nudges.
- Judge and some nudges still occur even after the "push latest user input onto conversation" change.

## Baseline for Nudge-v2 Experiments

**All baselines below (and any runs taken while this document describes the active implementation) were measured using the current Nudge-v1 behavior.**

**Nudge-v1 baseline** (before V2 changes):

**run_id: 20260624T212842Z**

- easy-hello-world: passed (4 turns, 3 tool calls, ~18s wall)
- easy-fizzbuzz: passed (4 turns, 3 tool calls, ~20s wall)
- marshmallow-code__marshmallow-1343: resolved ("swebench:full resolved") (37 turns, 33 tool calls, ~16 min wall)

## First Nudge-v2 Measurement

After implementing the Nudge-v2 algorithm (budgeted nudges at NUDGE_BUDGET=3, proactive `define_done` reminders when criteria not yet set, and helpful suggestions in criteria nudges):

**run_id: 20260625T020220Z**

- easy-hello-world: passed (6 turns, 4 tool calls, ~28s wall)
- easy-fizzbuzz: passed (8 turns, 4 tool calls, ~44s wall)
- marshmallow-code__marshmallow-1343: resolved ("swebench:full resolved") (36 turns, 30 tool calls, ~11.75 min wall)

**Comparison to Nudge-v1 baseline:**

- Marshmallow: 37 → 36 turns (-1), 33 → 30 calls (-3). Modest improvement in efficiency while still fully resolving the task.
- Easy cases show modestly higher turn counts. This is expected: the proactive reminders now cause the agent to call `define_done()` (which V1 runs often skipped or did later), and the agent receives a couple of budgeted suggestion nudges.

**Observed V2 behaviors in the marshmallow run:**
- Early `[nudge 1/3]` followed by the agent calling `define_done()` (the reminder worked).
- Nudges respected the budget (stayed at /3 instead of the old 999 on the criteria path).
- Multiple `JUDGE (criteria active): Continue` + nudge cycles.
- Eventually reached `JUDGE (criteria active): Fulfilled`.
- Only one `define_done` call (as intended).
- The agent produced a working patch that resolved the bug (and all PASS_TO_PASS tests).

These are the first measured results with the Nudge-v2 logic on the branch. The easy cases "pay a small tax" for the new proactive behavior, while the longer SWE case shows a small net win in turn/call count.

To lock a fresh baseline on the `nudge-v2` branch before implementing changes:

```bash
cd tui
cargo build --release --bin raven-eval  # or however you invoke it
# run the profile
./target/release/raven-eval easy-bench-live   # (adjust invocation as used in your env)
```

Record the new `run_id` from the generated `summary.json` (and any scorecard) in this document before making behavioral changes.

If broader swebench smoke/live numbers are desired for the branch, capture those as well (e.g. via `swebench-smoke` or `swebench-live` profiles) and note the relevant scorecard or per-instance resolved/unresolved status + turn counts.

---

## Nudge V1 algorithm

High-level pseudocode for current behavior (inside `drive_turn`, no-tool responses):

```
after model response with no tool calls:

if LLM error:
    push recovery nudge (limited retries)
    continue

if response truncated (length):
    push recovery nudge
    continue

if completion_criteria set (from define_done):
    decision = judge_turn(...)          // LLM judge
    if Fulfilled:
        clear criteria
        stop
    if Stuck:
        push guidance
        stop
    if Continue:
        nudge with criteria reminder (count → 999)
        continue

if early empty response:
    if under MAX_TEXT_NUDGES (3):
        push "start working" nudge
    else:
        decision = judge_turn(...)
        ... (Fulfilled/Stuck/Continue handling)
    continue

if text looks like planning ("let me", "I will", "the fix is"...):
    if under 3 nudges:
        push_continuation_nudge()
        continue

if after tools or malformed syntax, and not explicit stop:
    if request implies "run/exec" but no exec yet:
        force nudge (hard safety)
    decision = judge_turn(...)
    if Fulfilled: stop (clear criteria if set)
    if Stuck: push guidance (optional nudge)
    if Continue:
        nudge (up to 3, or 999 if criteria)
        continue

stop (accept completion)
```

After inner round budget exhausted: auto-continue a few times if still active.

**Key traits of V1:**
- Mix of hardcoded triggers + occasional judge.
- When criteria active → very persistent nudging (999 limit).
- Nudges are mostly "keep going" or repeat the criteria.
- Almost no proactive suggestions.
- Limited reminders to call `define_done`.

## Nudge V2 algorithm

(Original stub content preserved / to be expanded here as experiments are designed.)

The judge checks agent progress only when the agent is not busy doing inference.

The judge will by default consider nudging the agent up to NUDGE_BUDGET times between user interactions. Let's say the default NUDGE_BUDGET is 3.

If the agent has not used define_done() to come up with a possible measurement for completion of their work the judge may reprompt them and ask them to call define_done()

When the judge checks on the agent, if they have called define_done() it checks on their progress. if it appears to have met the define_done() criteria, it clears the define_done() and does not nudge the agent. The round is over. If the agent has not met the define_done() criteria it encourages the agent to continue reinforcing the definition of done and making helpful suggestions.
If the NUDGE_BUDGET has been consumed but the judge thinks the agent is making progress it will increment it by 1 and nudge the agent to continue. If the judge thinks it is pointless it will log that and not nudge the agent again until the next external stimulus.
