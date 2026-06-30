# Opus — Steering Extraction & Next Steps

## The Problem

`drive_turn()` in `agent_driver.rs` is an 800-line async function with ~10 interleaved
decision points, implicit state encoded in counters (`text_nudges`, `judge_nudges`,
`tools_used_this_turn`, `completed_naturally`), and duplicated judge-call patterns.
The ordering of `continue`/`break` targets is critical and easy to break.

The Super Judge needs the same decision logic for its own mini-loop, and right now
there's no way to reuse it without copy-pasting.

## The Split: `steering.rs` + `agent_driver.rs`

### `steering.rs` — pure decision engine (no async, no I/O)

Everything that answers "what should I do now?" moves here.

```rust
// steering.rs — pure, sync, testable

pub struct SteeringState {
    pub text_nudges: u32,
    pub judge_nudges: u32,
    pub tools_used: usize,
    pub total_rounds: u32,
    pub max_rounds: u32,
    pub enable_judge: bool,
    deadline: Option<Instant>,
}

pub enum SteeringDecision {
    Continue,                        // keep looping, no nudge needed
    Nudge(String),                   // push this message as user, then continue
    JudgeNeeded,                     // caller should invoke judge, then call apply_judge_verdict()
    Accept,                          // turn complete (natural stop / fulfilled)
    Stuck(String, String),           // reason + suggested guidance
    Timeout,                         // wall-clock limit hit
}

impl SteeringState {
    pub fn new(config: &Config) -> Self { ... }
    pub fn for_super_judge(config: &Config) -> Self { ... }  // different budgets
    pub fn past_deadline(&self) -> bool { ... }

    /// Given a no-tool round outcome, decide what to do.
    pub fn decide_no_tools(
        &mut self,
        effective_text: &str,
        finish_reason: Option<&str>,
        is_llm_error: bool,
        criteria: Option<&str>,
        last_user_request: Option<&str>,
        has_exec_in_recent: bool,
        has_write_in_recent: bool,
    ) -> SteeringDecision { ... }

    /// Given a round where finish_reason != "stop" and tools were used previously,
    /// decide whether to accept, nudge, or call the judge.
    pub fn decide_ambiguous_stop(
        &mut self,
        effective_text: &str,
        criteria: Option<&str>,
        last_user_request: Option<&str>,
        has_exec_in_recent: bool,
        has_write_in_recent: bool,
    ) -> SteeringDecision { ... }

    /// After a judge returns a verdict, map it to a concrete decision.
    pub fn apply_judge_verdict(
        &mut self,
        verdict: &TurnJudge,
        criteria: Option<&str>,
    ) -> SteeringDecision { ... }
}

// Pure detection functions — independently testable

pub fn detect_plan_narration(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("let me ")
        || lower.contains("i will ")
        || lower.contains("the fix is")
        || lower.contains("implement this")
        || lower.contains("re-read the code")
        || (lower.contains("implement") && lower.contains("fix"))
}

pub fn detect_malformed_tool_syntax(text: &str) -> bool {
    crate::llm::contains_tool_xml_syntax(text)
}

pub fn request_implies_execution(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("run") || lower.contains("exec")
        || lower.contains("show") || lower.contains("output")
}
```

#### What moves to `steering.rs`

| Current location (agent_driver.rs) | New home |
|---|---|
| L345–615: "no tool calls" decision tree (empty recovery, plan narration, length recovery, define-done reminders, criteria judge nudges, empty-response judge escalation) | `SteeringState::decide_no_tools()` |
| L663–850: "ambiguous stop" block (implies-run safety, write-but-no-exec, judge + verdict mapping, unbounded judge-continue) | `SteeringState::decide_ambiguous_stop()` |
| L403–482: mapping `TurnJudge` → actions (Fulfilled→break, Stuck→guidance, Continue→nudge) | `SteeringState::apply_judge_verdict()` |
| L621–638: plan narration detection | `steering::detect_plan_narration()` |
| L649–656: malformed XML tool syntax detection | `steering::detect_malformed_tool_syntax()` |
| L673–682: "implies run" heuristic | `steering::request_implies_execution()` |
| Counters: `text_nudges`, `judge_nudges`, `tools_used_this_turn`, `completed_naturally` | Fields on `SteeringState` |
| Constants: `MAX_TEXT_NUDGES`, `NUDGE_BUDGET`, `CONTINUATION_NUDGE`, `LENGTH_RECOVERY_NUDGE` | Move to `steering.rs` (re-export if needed) |

### `agent_driver.rs` — async orchestration shell (~150–200 lines)

Everything that does I/O stays here. The driver becomes a flat dispatcher:

```rust
// agent_driver.rs after refactor — sketch

pub async fn drive_turn(
    agent: &mut Agent,
    prompt: &str,
    observer: &mut dyn TurnObserver,
) -> Result<TurnResult> {
    if !prompt.trim().is_empty() {
        agent.on_new_user_input(prompt);
    }
    let mut steering = SteeringState::new(agent.current_config());
    let tools_schema = tools::all_tools(&agent.current_config().flags);
    let tools_for_request = agent.current_config().tools_enabled
        .then(|| tools_schema.clone());

    let mut all_actions: Vec<ActionRecord> = vec![];
    let mut last_assistant_text = String::new();
    // ... token counters ...

    'auto_continue: for continuation in 0..=MAX_AUTO_CONTINUES {
        for _round in 0..steering.max_rounds {
            if observer.should_stop() { break 'auto_continue; }
            if steering.past_deadline() {
                // log timeout
                break 'auto_continue;
            }

            // ── 1. Stream request (async I/O — stays here) ──
            let round = send_and_consume(agent, &tools_for_request, observer).await?;
            steering.total_rounds += 1;

            // ── 2. Handle interject (observer interaction — stays here) ──
            if let Some(msg) = observer.take_interject() {
                if !round.effective_text.trim().is_empty() {
                    agent.push_assistant_text(&round.effective_text);
                }
                agent.on_new_user_input(&msg);
                observer.on_interject(&msg);
                continue;
            }

            // ── 3. Commit assistant text (agent mutation — stays here) ──
            if !round.effective_text.trim().is_empty() && !round.is_llm_error {
                agent.push_assistant_text(&round.effective_text);
                last_assistant_text = round.effective_text.clone();
            }

            // ── 4. No tool calls → ask steering ──
            if round.tool_calls.is_empty() {
                let ctx = steering_context(agent, &all_actions);
                let decision = steering.decide_no_tools(
                    &round.effective_text,
                    round.finish_reason.as_deref(),
                    round.is_llm_error,
                    ctx.criteria.as_deref(),
                    ctx.last_user_request.as_deref(),
                    ctx.has_exec,
                    ctx.has_write,
                );
                match execute_decision(decision, agent, observer, &mut steering, &all_actions).await {
                    Flow::Continue => continue,
                    Flow::Break => break,
                    Flow::BreakOuter => break 'auto_continue,
                }
            }

            // ── 5. Execute tool calls (async dispatch — stays here) ──
            let mut approved = vec![];
            for tc in &round.tool_calls {
                if observer.stop_tool_processing() { break; }
                if observer.approve_tool(tc).await {
                    observer.on_tool_start(&tc.function.name, &tc.function.arguments);
                    approved.push(tc.clone());
                } else {
                    agent.record_tool_denial(tc, &deny_message(&tc.function.name));
                }
            }
            let records = agent.execute_and_record_tool_calls(&approved).await;
            for r in &records { observer.on_tool_result(r); }
            all_actions.extend(records);
            steering.tools_used += approved.len();
        }
        // auto-continue logic
        ...
    }

    agent.force_flush_session().await;
    Ok(build_turn_result(agent, last_assistant_text, all_actions, steering))
}

/// Map a SteeringDecision into agent/observer actions.
/// Returns whether to continue, break inner, or break outer.
async fn execute_decision(
    decision: SteeringDecision,
    agent: &mut Agent,
    observer: &mut dyn TurnObserver,
    steering: &mut SteeringState,
    recent_actions: &[ActionRecord],
) -> Flow {
    match decision {
        SteeringDecision::Continue => Flow::Continue,
        SteeringDecision::Nudge(msg) => {
            observer.on_nudge(steering.text_nudges, MAX_TEXT_NUDGES);
            agent.push_message("user", &msg);
            Flow::Continue
        }
        SteeringDecision::JudgeNeeded => {
            let recent: Vec<_> = recent_actions.iter().rev().take(8).cloned().collect();
            let verdict = agent.judge_turn(&last_text, &recent).await;
            let follow_up = steering.apply_judge_verdict(&verdict, criteria);
            execute_decision(follow_up, agent, observer, steering, recent_actions).await
        }
        SteeringDecision::Accept => Flow::Break,
        SteeringDecision::Stuck(reason, guidance) => {
            observer.on_stuck(&reason, &guidance);
            agent.push_message("user", &format!("[{}. {}]", reason, guidance));
            Flow::Break
        }
        SteeringDecision::Timeout => Flow::BreakOuter,
    }
}
```

#### What stays in `agent_driver.rs`

- **`TurnObserver` trait** + `SilentObserver` + `HeadlessObserver` (presentation, not decisions)
- **`drive_turn()`** as the async orchestrator (~150–200 lines)
- **`consume_stream()`** (async stream consumption + channel recv)
- **`send_and_consume()`** helper with transparent retry logic
- **`execute_decision()`** helper that maps `SteeringDecision` → agent/observer calls
- **`TurnResult` assembly** at the end

### The key principle

`agent_driver.rs` = **what** happens in what order (I/O, stream, tools)
`steering.rs` = **should** we continue, nudge, judge, or stop (pure logic)

The driver never contains `if text_lower.contains("let me ")` or
`if judge_nudges < NUDGE_BUDGET` — all of that lives in steering where
it's unit-testable without mocking an LLM.

---

## Second Move: `tool_xml.rs` from `llm.rs`

`llm.rs` at 1,103 lines mixes three concerns:
- **HTTP client** (endpoint switching, headers, auth, metering)
- **Streaming** (SSE parsing, delta accumulation, channel dispatch)
- **XML tool-call parsing** (`parse_xml_tool_calls_from_content`, `strip_xml_tool_call_blocks`, `contains_tool_xml_syntax`)

The XML parsing has ~10 edge cases (truncated prefix, bare `function=`, stray closers,
mixed with narrative) and is the most fragile subsystem.

Extract into `tool_xml.rs`:
- All existing XML parsing functions + tests
- `steering.rs` calls `tool_xml::contains_tool_syntax()` instead of `crate::llm::contains_tool_xml_syntax()`
- The streaming path calls `tool_xml::parse()` the same way non-streaming does
- Future: fuzz-style property tests for round-tripping

**Effort:** Low — mechanical move of ~200 lines + tests. No behavior change.

---

## Third Move: Super Judge Implementation

The skeleton is in place (see `superjudge.md`). With `steering.rs` extracted:

1. `super_judge.rs` creates `SteeringState::for_super_judge(config)` with:
   - Smaller budgets (e.g. 2 nudges max, 10 rounds)
   - Adversarial reviewer persona in system prompt
   - Tool access: read + exec (verification only)

2. After `drive_turn()` returns in work mode → brief 300ms pause → Super Judge activates

3. Super Judge gets its own `drive_turn()` call with a `SuperJudgeObserver` that:
   - Logs with `🔍 SUPER JUDGE` prefix
   - Shows activity in the thinking/trace pane
   - Inherits the session's approval mode

4. Super Judge either:
   - Nudges the main agent to continue (injects feedback as user message)
   - Declares "work appears complete" (turn truly ends)
   - Detects death spiral and injects anti-spiral guidance

**This depends on step 1** — without `steering.rs`, the Super Judge would duplicate
all the nudge/judge decision logic.

---

## Parking Lot (not now)

| Item | Why defer |
|------|-----------|
| Separate judge endpoint | Works fine with same model. Add `judge_endpoint: Option<Endpoint>` when second GPU / API available |
| Grammar-constrained tool calls | Would eliminate XML parsing entirely via llama.cpp grammar config. Do when Qwen XML issues stabilize |
| Eval-as-CI | Easy-bench passes but needs small quantized model in CI. Infrastructure, not code |
| Session cleanup | `--fresh-session` accumulates dirs. Add TTL pruning when it becomes a problem |
| System prompt unification | Eval vs interactive prompts diverge. Important but invasive — do after steering stabilizes |

## Execution Order

```
1. steering.rs extraction  ← unlocks testability + Super Judge reuse
2. tool_xml.rs split       ← low effort, high safety
3. super_judge.rs impl     ← the payoff, uses steering.rs
```

Steps 1 and 2 are independent. Step 3 depends on step 1.
