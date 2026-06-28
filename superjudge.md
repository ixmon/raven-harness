# Super Judge Experiment Plan (run-mode = "work")

## Motivation and Goal of the Experiment
The primary purpose is to observe how far an agent can autonomously progress in "work" mode using existing nudges followed by Super Judge feedback, until it either achieves the goal or enters a visible death spiral. 

We want to minimize manual intervention ("continue") and see if the agent can self-correct or complete tasks.

This is an experiment to gather data on behavior before adding limits, better anti-spiral logic, or plan tracking.

If things go wrong during self-refactoring, roll back to this baseline commit.

## Current State (before this experiment)
- `run-mode` (stored as `agent_mode` in `SessionMeta`): UI-selectable via `/run-mode` or `/work-mode` menu. Values: talk, think, research, work, dream. Currently mostly display-only.
- Normal `Judge` (in `judge.rs`): tool-less, short-context LLM call on no-tool turns or stop conditions. Returns Fulfilled/Continue/Stuck.
- Goal injection: `get_injection_block()` stuffs `current_goal`, `achievement_tests`, `completion_criteria`, pitfalls, discoveries, recent_turns_summary etc. into **every** system prompt when `goal_tracking` enabled. Can overwhelm.
- Nudges: plan-narration, empty response, length recovery, some judge-driven. Happen inside `drive_turn`.
- `drive_turn` (agent_driver.rs): core loop. Ends on no tools + stop, judge Fulfilled, budgets, stuck, etc.
- In TUI: after `drive_turn` returns + `UiUpdate::Done`, `is_processing = false`, UI idles until user input.
- `define_done` / `completion_criteria`: exist but not strongly prompted or nudged outside eval/`--enable-judge` paths.
- Thinking: separate stream (reasoning_content), shown in right pane, logged with role="thinking", not always fed back to main conversation.
- Approval: inherited via current `ExecApprovalMode`.

## Super Judge Design
A more powerful, post-idle reviewer that activates only in `run-mode == "work"`.

### Trigger Point
- Exactly when the "Processing" indicator (above input box) would turn off after a normal turn (i.e., any exit from `drive_turn` that leaves the UI idle).
- Let Processing briefly turn off for UX (short sleep ~300ms), then turn it back **on** for the Super Judge.
- Synchronous (local model has no parallel inference slots; also makes sense for sequencing).
- Re-enables `is_processing = true` so the UI shows activity.
- Runs after existing normal nudges have had their chance.

### Capabilities
- **Tools**: read tools + `exec` (for verification: run tests, check warnings/errors, execute the claimed work, etc.). Stop on first error and report.
- **Permissions**: inherits the app's current `approval-mode` (Babysitter etc.).
- **Context**: full/recent conversation history + **raw recent thinking blocks** (important for Qwen-style models that think/tool-call heavily but talk little).
- **Context management**: Use the probed `context_tokens` value. If tight, summarize (add some randomness in future tuning so identical failed fixes aren't repeated verbatim).
- **Perspective / Psychology**: Explicitly "some dude wrote this code, don't know if it's good — can you find bugs?" Adversarial / external reviewer role (inspired by Rocky trainer or Morpheus). Different from the main agent's self-view. Instruct it to be critical and verify claims with real tool use.
- **Long context review**: Examine session from the point the goal was set. Review recent actions, thinking, code state.
- **Autonomy**: Given its own "mini agent turn" / small loop. Full tool access and ability to think/act like an agent during review. Leverages existing drive_turn machinery where possible for nudges inside the review turn.

### Behavior and Decision Rules
- **If the agent appears productive**: Prefer gentle nudges or ones that provide adversarial evidence against the agent's success claims.
- **If problems found**:
  - Do real verification work (run tests/exec, read code).
  - Stop on first error.
  - Message the (main) agent with:
    - The original goal.
    - Feedback on errors.
    - Suggested concrete next steps.
- **Death spiral detection and handling**:
  - If the review indicates repeating the same failing actions, explicitly note "death spiral detected".
  - Include advice to avoid repeating the mistake.
  - Deliver as a brand new / fresh prompt style to the agent (include goal + spiral history + anti-spiral advice).
  - For now: attempt to correct. No hard limit on number of Super Judge interventions. User will babysit/monitor.
  - Future: if one attempt fails to resolve, give up or roll back work.
- **If work looks complete**:
  - After real verification (tests, exec, etc.), inject exactly: "this work appears to be complete".
- **No involvement of completion_criteria / define_done** in this change (per current scope).

### Integration with "work" mode and normal flow
- Normal agent turn in work mode: last turn as input → works → normal nudges fire (plan-narration, etc.).
- Turn would end → idle.
- Super Judge kicks in (Processing re-on, visible activity).
- Super Judge may push a message.
- Agent receives the message and can continue (or stop if "complete").
- Goal mention: keep light in interactive/work mode (full goal injection every turn can distract). Super Judge re-introduces the original goal + context when it intervenes.
- Possible usage pattern: set goal to "follow someplan.md" (cloud AI plans, local executes).

### Visibility and Logging
- Thinking and tool calls from Super Judge appear in the right (thinking) pane.
- Use distinctive prefix for now: `🔍 SUPER JUDGE ...` (or icon). Different color/styling in a future change if this works well.
- Also logged to trace pane and full_log.jsonl / harness events with clear "super_judge" markers.
- User can watch for progress or spirals in real time.

### Current Implementation State (skeleton)
- Hooked in `tui_app.rs` (around the turn spawn and Done handling).
- Brief sleep for UX idle moment, then `SuperJudgeBegin` update to re-enable processing + trace note.
- Review request injected as user message with the critical/adversarial prompt.
- Re-invokes `drive_turn(..., "")` (drive_turn updated to skip `on_new_user_input` on empty prompt to avoid duplicates).
- This gives the Super Judge its "mini agent turn" autonomy + full existing machinery (tools, thinking, internal nudges).
- SUPER prefix used.
- Placeholder has been replaced with the review injection logic matching the spec.
- Changes also touched agent_mode display/handling in prior work (status bar "Run Mode", /run-mode command, etc.).

## Risks and Considerations
- Death spirals may be amplified or made prettier without actually resolving.
- High local inference cost/time (no limit for now).
- Model may not perfectly adopt the "reviewer" persona or follow "stop on first error".
- Context bloat if not summarized.
- Need babysitting during early runs.
- Qwen's thinking-heavy style: ensure thinking is visible to Super Judge.
- Goal vs Plan: current goal injection may still distract; experiment may reveal need for explicit plan tracking (e.g. goal = "execute someplan.md").

## Monitoring and Experiment Protocol
- Set run-mode to "work".
- Set a clear goal (possibly referencing a plan file).
- Watch the trace pane for normal nudges then `🔍 SUPER JUDGE` activity.
- Observe: does it make progress? Does it complete? Does it spiral? What kind of feedback is produced?
- If south: `git reset --hard` to this commit.
- Log key outcomes (death spiral patterns, successful recoveries, prompt effectiveness).
- Future tuning ideas from observations:
  - Add randomness to summarization (so repeated identical feedback doesn't get retried).
  - More "trainer/Morpheus" style language in Super Judge prompts.
  - Actual limits + rollback on repeated spiral.
  - Better separation or dedicated Super Judge loop if drive_turn re-use isn't enough.
  - Style the trace output.
  - Plan methodology (beyond simple goal).

## Next Steps / Future Work
- Run the experiment with self-refactoring tasks.
- Observe and iterate on Super Judge prompt, detection of spirals, feedback style.
- Add limits, better context mgmt, plan support, etc. based on data.
- Potentially make Super Judge always use tools for verification (stop on first real problem).
- Consider when to re-inject full goal vs. rely on the Super Judge message.

This document serves as the baseline plan and commit message context before handing the agent a self-refactoring task.
