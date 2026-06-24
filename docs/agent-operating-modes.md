# Agent Operating Modes — Work, Talk, Think, Adversarial

Date noted: 2026-06-24

These are ideas for different "stances" or operating modes the harness can put the agent into. The goal is to give the *environment* (the "ectoplasm") more explicit control over the agent's behavior profile, beyond just the model prompt or per-task tweaks. This lets any model perform better by matching the operational context to the desired style of work.

Related to:
- Goal tracking (`current_goal`, `update_goal` tool, injection in session context, `judge_turn`)
- Existing `ExecApprovalMode` (`/mode`: thunderdome, springbreak, babysitter, vegas)
- Nudging, auto-continue, and drive_turn loop
- Session log (`full_log.jsonl`) for reflection
- User commands like `/goal`

## Core Idea

Instead of a single "agent" behavior, support named modes that modulate:
- Nudge aggressiveness and messages (empty response, plan-narration, malformed)
- When and how `judge_turn` is consulted
- Tone and content of system injection (goal emphasis, log analysis encouragement)
- Tolerance for pure-text responses vs. tool calls
- Whether to auto-inject or analyze session history
- Goal enforcement (some modes may de-emphasize or ignore `current_goal`)
- Continuation vs. discussion

Modes should be controllable by the *user* (via `/stance <mode>` or similar command, or launch flags for evals) *and* potentially by the harness itself for recovery.

## Proposed Modes

### work (default for evals / production agentic tasks)
- Merciless focus on the goal.
- High aggression: strong nudges to keep using tools, low tolerance for narration or "thinking out loud".
- Judge biased toward "Continue" unless very clearly fulfilled (or stuck).
- Minimal pure text responses; push for action.
- Goal is prominent in injection and nudges.
- "Do not stop until verified complete."
- Good for SWE-bench style where you want relentless progress.

### talk
- Conversational / collaborative stance.
- Lower auto-continue pressure.
- More willingness to respond with explanations, ask clarifying questions, or discuss tradeoffs.
- Nudges are gentler or suppressed.
- Useful when the human wants to steer interactively rather than let it run.
- Goal is still known but de-emphasized in favor of dialogue.

### think (or research)
- Reflective / analytical mode.
- Encourages reading more context, using `read_summary`, analyzing previous actions, and looking at the session log.
- Higher tolerance for longer text responses and exploration.
- Can inject recent `full_log.jsonl` excerpts or summaries on demand.
- "Look at things from a different angle", consider alternatives, deeply analyze.
- Goal is present but the mode prioritizes understanding over immediate action.
- Good for debugging why something is hard, or when the agent is looping.

### adversarial
- Self-critique / red-team stance.
- Prompts the model (and harness) to find weaknesses in its own plan, partial fixes, or assumptions.
- Judge can be used in a "find holes" mode rather than pure fulfillment.
- Explicitly feeds session history / log for the model to attack its previous reasoning ("what did I miss?").
- Can combine with "research" to deeply analyze its own `full_log.jsonl`.
- Useful for hard problems or after a "work" pass that got stuck.

(Older brainstorm also included "research" which overlaps with think/adversarial.)

## Interaction with Goals

- Modes can change how strictly the `current_goal` + `achievement_tests` are enforced.
- Proposal for experiments: ability to disable `update_goal` tool entirely (so the model cannot mutate the goal).
- Start sessions with *no initial goal* (empty `current_goal`, no auto-seeding from first prompt) to test whether other mechanisms (last_user_request anchoring, judge on actions, explicit task in prompt, nudges) are sufficient.
- User should be able to `/goal clear`, `/goal set "..."` independently of the model.

This tests whether the harness "ectoplasm" can compensate for missing or absent goal machinery.

## Implementation Sketch (Future)

- Add `AgentMode` (or `OperatingStance`) enum: Work | Talk | Think | Adversarial (plus perhaps Research).
- Store in SessionMeta (or transient in driver for a turn).
- `/stance work` etc. user command (like existing `/mode` for approval).
- In `agent_driver.rs` drive_turn: condition nudge messages, text_nudge limits, when to call judge, etc. on current mode.
- In `agent.rs` / `session.rs`: modulate injection block and judge prompt based on mode.
- For log analysis: a mode or special action that surfaces tail of full_log.jsonl or structured recent turns for reflection.
- Eval harness: flags like `--agent-mode work`, and ability to launch with `update_goal` disabled + no initial goal.
- Keep modes mostly *harness-side* so weaker models aren't confused by prompt bloat; only light hints to the model ("You are operating in work mode: prioritize tool calls...").

## Why This Matters for the Harness

The goal of raven-hotel / raven-eval is to improve the *environment* so any model pointed at it does its best possible work. Modes + explicit goal controls are levers the harness (not the model) can use to:
- Prevent forgetting (re-anchor via nudges).
- Give nudges when needed.
- Adapt behavior without per-model or per-test hacks.
- Allow the human to set the "vibe" (talk vs merciless work).
- Enable powerful behaviors like self-adversarial log analysis using the durable full_log.

See also: existing goal injection, judge_turn, last_user_request safety checks, full_log usage for recovery, and ExecApprovalMode.

---

Next steps (as of this note):
- Documented here.
- **Baseline collected**: ran `./evals/run_replay.sh` (deterministic mocks) with goals + `update_goal` enabled. Saved to /tmp/baseline_with_goal.txt. Git updated with `docs/agent-operating-modes.md` commit.
- **Experiment run**: re-ran with `RAVEN_EVAL_DISABLE_UPDATE_GOAL=1 RAVEN_EVAL_NO_INITIAL_GOAL=1`. Replays still PASS (as expected for scripted cases). Outputs in /tmp/experiment_no_goal.txt.
- To evaluate real impact: launch full easy_bench / swebench evals (or interactive `launch_interactive`) under the two envs and compare scorecard (turns, tool calls, resolve rate, recovery success).
- Next: wire `/goal` user command + `/stance` (or `/mode` extension), modulate driver/judge/nudges per mode, filter tool schema when disabled.
- Update baselines/metrics + git after real comparison runs.

Use the envs for clean experiments:
- `RAVEN_EVAL_DISABLE_UPDATE_GOAL=1` (or `RAVEN_NO_GOAL=1`) — removes `update_goal` from tool list.
- `RAVEN_EVAL_NO_INITIAL_GOAL=1` — prevents auto-seeding goal from first request; sessions start with empty `current_goal`.

This lets us measure how much the *rest of the harness* (nudges, judge on actions/malformed, last_user_request anchoring, raw task in prompt, log tail recovery) can carry without explicit goal tracking.

This should be general harness improvement, not test-specific.