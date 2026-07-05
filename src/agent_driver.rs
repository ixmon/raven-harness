//! Shared agent driving loop — the single source of truth for streaming,
//! tool dispatch, and round orchestration.
//!
//! Both the interactive TUI and headless `--prompt` mode call [`drive_turn()`].
//! The only difference is the [`TurnObserver`] implementation:
//! - TUI provides one that renders UI events
//! - Headless provides one that auto-approves and logs to stdout
//! - Tests provide a silent one
//!
//! **Policy decisions** (nudge, judge, accept, stuck) live in
//! [`crate::steering`].  This module is responsible for the async I/O
//! that feeds data into the steering engine and executes its decisions.

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::agent::{ActionRecord, Agent, TurnJudge, TurnMetrics, TurnResult};
use crate::llm::{StreamChunk, ToolCall, Usage};
use crate::steering::{
    self, RecentActionsSummary, RoundContext, SteeringDecision, SteeringState,
    MAX_AUTO_CONTINUES, MAX_STREAM_RETRIES,
};
use crate::tools;

// Re-export constants that external code (TUI observer, tests) may reference.
pub use crate::steering::{
    CONTINUATION_NUDGE, LENGTH_RECOVERY_NUDGE, MAX_TEXT_NUDGES, MAX_TOOL_ROUNDS, NUDGE_BUDGET,
};

// ── Observer trait ────────────────────────────────────────────────────────────

/// The seam between the agent driving loop and its presentation layer.
///
/// The observer handles:
/// - Presentation / logging (on_token, on_tool_result, etc.)
/// - Policy decisions that differ by environment (approval, stop, interject)
///
/// Default implementations do nothing / auto-approve — suitable for headless
/// tests and --prompt mode.
///
/// Only add methods here when the TUI (or other frontends) need to influence
/// control flow in ways the existing hooks cannot express.
#[async_trait]
pub trait TurnObserver: Send {
    /// Streaming token from the model's visible response.
    fn on_token(&mut self, _token: &str) {}
    /// Thinking/reasoning content (Qwen reasoning_content, etc.).
    fn on_thinking(&mut self, _text: &str) {}
    /// A tool is about to execute (called before approve decision).
    fn on_tool_start(&mut self, _name: &str, _args: &str) {}
    /// A tool finished executing.
    fn on_tool_result(&mut self, _record: &ActionRecord) {}
    /// Should this tool call be allowed to execute?
    /// For interactive use this may prompt the user.
    async fn approve_tool(&mut self, _tc: &ToolCall) -> bool {
        true
    }
    /// Model paused to narrate — we're pushing a continuation nudge.
    fn on_nudge(&mut self, _count: u32, _max: u32) {}
    /// Hit round limit; auto-continuing (or exhausted if `exhausted` is true).
    fn on_round_limit(&mut self, _continuation: u32, _max: u32, _exhausted: bool) {}
    /// Updated context token estimate.
    fn on_context_usage(&mut self, _tokens: u32) {}
    /// Check if the caller wants to abort the turn (Ctrl+C, etc.).
    fn should_stop(&self) -> bool {
        false
    }

    /// A live user message was injected mid-turn (interject).
    /// The driver has already called on_new_user_input with it.
    fn on_interject(&mut self, _msg: &str) {}

    /// If the user interjected with a new instruction while the turn was
    /// in progress, return it here. The driver will inject it and continue
    /// the loop (fresh model request). This allows live redirection without
    /// ending the overall turn.
    fn take_interject(&mut self) -> Option<String> {
        None
    }

    /// After a denial (or at any point), should further tool calls from the
    /// current model response be skipped? (e.g. after too many user denials)
    fn stop_tool_processing(&self) -> bool {
        false
    }

    /// The judge detected unproductive looping. The driver has already
    /// injected guidance so the final output should ask the user for help.
    fn on_stuck(&mut self, _reason: &str, _suggested_guidance: &str) {}
}

// ── Built-in observers ───────────────────────────────────────────────────────

/// Silent observer for integration tests and `Agent::run_turn()` backward compat.
#[allow(dead_code)]
pub struct SilentObserver;
impl TurnObserver for SilentObserver {
    fn on_stuck(&mut self, _reason: &str, _suggested: &str) {}
}

/// Headless observer for `--prompt` mode: auto-approves, prints final text to stdout.
pub struct HeadlessObserver;
impl TurnObserver for HeadlessObserver {
    fn on_tool_start(&mut self, name: &str, _args: &str) {
        eprint!("  {} → ", name);
    }
    fn on_tool_result(&mut self, record: &ActionRecord) {
        eprintln!("{}", record.summary.lines().next().unwrap_or(""));
    }
    /// Print thinking/reasoning_content live (llama.cpp, Qwen, etc.).
    fn on_thinking(&mut self, text: &str) {
        if !text.is_empty() {
            eprint!("{}", text);
        }
    }
    fn on_nudge(&mut self, count: u32, max: u32) {
        eprintln!("  [nudge {}/{}]", count, max);
    }
    fn on_round_limit(&mut self, continuation: u32, max: u32, exhausted: bool) {
        if exhausted {
            eprintln!("  [round limit exhausted {}/{}]", continuation, max);
        } else {
            eprintln!("  [auto-continue {}/{}]", continuation, max);
        }
    }
    fn on_stuck(&mut self, reason: &str, suggested: &str) {
        eprintln!("  [agent stuck: {}  Ask user: {}]", reason, suggested);
    }
}

// ── Flow control for execute_decision ────────────────────────────────────────

/// Internal flow control returned by `execute_decision`.
enum Flow {
    /// Continue the inner round loop.
    Continue,
    /// Break the inner round loop (completed_naturally = true).
    Break,
    /// Break the outer 'auto_continue loop.
    BreakOuter,
}

// ── The canonical driving loop ───────────────────────────────────────────────

/// Drive one agent turn to completion.
///
/// This is the **single source of truth** for:
/// - Streaming inference + retry
/// - Tool execution and approval
/// - Interject handling
///
/// All policy decisions (nudge, judge, accept, stuck) are delegated to
/// [`SteeringState`] in the [`steering`](crate::steering) module.
pub async fn drive_turn(
    agent: &mut Agent,
    prompt: &str,
    observer: &mut dyn TurnObserver,
) -> Result<TurnResult> {
    // Record the user prompt.
    if !prompt.trim().is_empty() {
        agent.on_new_user_input(prompt);
    }

    let cfg = agent.current_config();
    let max_duration_secs = cfg.flags.max_duration_secs;

    // Choose tool surface based on agent mode.
    // In plan mode we give a curated set focused on exploration + planning state.
    let tools_schema = if agent.current_agent_mode() == "plan" {
        tools::plan_mode_tools(&cfg.flags)
    } else {
        tools::all_tools(&cfg.flags)
    };
    let tools_for_request = cfg.tools_enabled.then(|| tools_schema.clone());

    let mut steering = SteeringState::new(
        cfg.max_rounds,
        cfg.enable_judge,
        max_duration_secs,
    );

    let mut all_actions: Vec<ActionRecord> = vec![];
    let mut last_assistant_text = String::new();
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;
    let mut estimated_tool_tokens: u64 = 0;
    let mut cache_summary_hits: u32 = 0;
    let mut estimated_summary_tokens: u64 = 0;

    // Outer auto-continue loop
    'auto_continue: for continuation in 0..=MAX_AUTO_CONTINUES {
        let mut completed_naturally = false;

        // Inner tool-use loop (one "budget" of rounds)
        for _round in 0..steering.max_rounds {
            if observer.should_stop() {
                break 'auto_continue;
            }

            // Wall-clock timeout
            if steering.past_deadline() {
                let secs = max_duration_secs.unwrap_or(0);
                let msg = format!("[system: wall-clock timeout after {}s — stopping]", secs);
                // Note: in TUI mode this message is also logged via agent events; avoid raw eprint to protect display.
                agent.log_harness_event("timeout", &msg);
                last_assistant_text = format!("(timed out after {}s)", secs);
                break 'auto_continue;
            }

            // ── 1. Send streaming request with transparent retry ──
            let (raw_round_text, tool_calls, usage, finish_reason, round_thinking) = {
                let mut attempt = 0u32;
                loop {
                    let stream = match agent
                        .send_streaming_request(tools_for_request.clone())
                        .await
                    {
                        Ok(s) => s,
                        Err(_) if attempt < MAX_STREAM_RETRIES => {
                            attempt += 1;
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    steering.total_rounds += 1;

                    let (text, calls, usg, fr, thk) = consume_stream(stream, observer).await;

                    if text.starts_with("LLM error:") && attempt < MAX_STREAM_RETRIES {
                        attempt += 1;
                        continue;
                    }
                    break (text, calls, usg, fr, thk);
                }
            };

            // ── 2. Accumulate usage stats ──
            if let Some(u) = &usage {
                prompt_tokens += u.prompt_tokens.unwrap_or(0) as u64;
                completion_tokens += u.completion_tokens.unwrap_or(0) as u64;
            }

            let is_llm_error = raw_round_text.starts_with("LLM error:");
            let round_text = if is_llm_error {
                raw_round_text
            } else {
                crate::llm::strip_xml_tool_call_blocks(&raw_round_text)
            };

            // ── 3. Persist thinking content ──
            if !is_llm_error && !round_thinking.trim().is_empty() {
                if let Some(s) = &agent.session {
                    let entry = serde_json::json!({
                        "ts": chrono::Utc::now().to_rfc3339(),
                        "role": "thinking",
                        "round": steering.total_rounds,
                        "content": round_thinking,
                    });
                    let _ = s.append_log(&entry.to_string());
                }
            }

            // Fall back to thinking if content channel was empty.
            let effective_text = if !round_text.trim().is_empty() {
                round_text.clone()
            } else {
                round_thinking.clone()
            };

            // ── 4. Interject handling ──
            if let Some(msg) = observer.take_interject() {
                if !effective_text.trim().is_empty() && !is_llm_error {
                    agent.push_assistant_text(&effective_text);
                    last_assistant_text = effective_text.clone();
                }
                agent.on_new_user_input(&msg);
                observer.on_interject(&msg);
                continue;
            }

            // Hard stop (Esc) with no interject
            if observer.should_stop() {
                if !effective_text.trim().is_empty() && !is_llm_error {
                    agent.push_assistant_text(&effective_text);
                    last_assistant_text = effective_text.clone();
                }
                break 'auto_continue;
            }

            // ── 5. Commit assistant text ──
            if !effective_text.trim().is_empty() && !is_llm_error {
                agent.push_assistant_text(&effective_text);
                last_assistant_text = effective_text.clone();
            }

            // ── 6. No tool calls → delegate to steering ──
            if tool_calls.is_empty() {
                let ctx = build_round_context(
                    agent,
                    &effective_text,
                    &round_text,
                    finish_reason.as_deref(),
                    is_llm_error,
                    false,
                );
                let recent = build_recent_summary(&all_actions);

                let decision = steering.decide_no_tools(&ctx, &recent);

                match execute_decision(
                    decision,
                    agent,
                    observer,
                    &mut steering,
                    &all_actions,
                    &effective_text,
                    &ctx,
                )
                .await
                {
                    Flow::Continue => continue,
                    Flow::Break => {
                        completed_naturally = true;
                        break;
                    }
                    Flow::BreakOuter => break 'auto_continue,
                }
            }

            // ── 7. Execute tool calls ──
            if observer.should_stop() {
                break 'auto_continue;
            }
            steering.tools_used += tool_calls.len();

            let mut to_execute: Vec<ToolCall> = vec![];
            let _current_mode = agent.current_agent_mode();
            for tc in &tool_calls {
                if observer.should_stop() || observer.stop_tool_processing() {
                    break;
                }

                // Hard gate for Plan mode: never show babysitter approvals or execute
                // mutating actions (write/patch/exec) until user explicitly says "proceed".
                // This prevents action approval popups from appearing alongside the
                // "approve the plan?" question.
                if let Some(denial) = agent.plan_mode_denial(&tc.function.name, &tc.function.arguments) {
                    agent.record_tool_denial(tc, &denial);
                    // Surface the denial in the trace pane immediately.
                    let rec = crate::agent::ActionRecord {
                        tool: tc.function.name.clone(),
                        args: tc.function.arguments.clone(),
                        summary: denial.clone(),
                        output_to_model: denial.clone(),
                        raw_bytes: 0,
                        truncated: false,
                        estimated_tokens: 5,
                    };
                    let _ = observer.on_tool_result(&rec);
                    continue;
                }

                if observer.approve_tool(tc).await {
                    observer.on_tool_start(&tc.function.name, &tc.function.arguments);
                    to_execute.push(tc.clone());
                } else {
                    let deny = format!(
                        "DENIED: The user refused to approve this {} action. \
                         Do NOT retry the same action. \
                         Either try a different approach, ask the user what they want, \
                         or explain what you were trying to do and why.",
                        tc.function.name
                    );
                    agent.record_tool_denial(tc, &deny);
                    if observer.should_stop() || observer.stop_tool_processing() {
                        break;
                    }
                }
            }

            if observer.should_stop() {
                break 'auto_continue;
            }

            // Execute the approved tools for this round one-by-one so that a stop
            // asserted mid-batch (e.g. Esc) can prevent the *remaining* tools in
            // the current model response from running. The tool already in flight
            // will complete (tools are not cancelled mid-exec), then we abort before
            // the next one and before any subsequent LLM round.
            for tc in to_execute {
                if observer.should_stop() {
                    break 'auto_continue;
                }
                let recs = agent.execute_and_record_tool_calls(&[tc]).await;
                for r in &recs {
                    observer.on_tool_result(r);
                    estimated_tool_tokens += r.estimated_tokens as u64;
                    if r.tool == "read_summary" && r.output_to_model.contains("(fresh summary)") {
                        cache_summary_hits += 1;
                        estimated_summary_tokens += r.estimated_tokens as u64;
                    }
                }
                all_actions.extend(recs);
            }

            observer.on_context_usage(agent.estimated_context_tokens());
        }

        // Model stopped calling tools — done
        if completed_naturally {
            break 'auto_continue;
        }

        // Hit round limit — auto-continue?
        if continuation >= MAX_AUTO_CONTINUES {
            observer.on_round_limit(continuation + 1, MAX_AUTO_CONTINUES + 1, true);
            break 'auto_continue;
        }

        observer.on_round_limit(continuation + 1, MAX_AUTO_CONTINUES + 1, false);
    }

    agent.force_flush_session().await;
    observer.on_context_usage(agent.estimated_context_tokens());

    // Final authoritative pass: strip XML from session log tail.
    // Never fall back to unstripped last_assistant_text; if the log tail was pure
    // tool XML it will become empty (correct — don't show naked calls in history).
    let final_text = agent
        .session
        .as_ref()
        .and_then(|s| s.last_assistant_content())
        .map(|tail| crate::llm::strip_xml_tool_call_blocks(&tail))
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| crate::llm::strip_xml_tool_call_blocks(&last_assistant_text));

    let tool_call_count = all_actions.len() as u32;

    let judge = agent.session.as_ref().and_then(|s| {
        s.meta
            .last_judge
            .as_deref()
            .and_then(TurnJudge::from_log_content)
    });

    let result = TurnResult {
        final_text,
        actions: all_actions,
        rounds_used: steering.total_rounds,
        judge,
        metrics: TurnMetrics {
            llm_rounds: steering.total_rounds,
            tool_calls: tool_call_count,
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            estimated_tool_tokens,
            cache_summary_hits,
            estimated_summary_tokens,
            round_limit_hit: steering.total_rounds >= steering.max_rounds,
        },
    };

    observer.on_context_usage(agent.estimated_context_tokens());
    Ok(result)
}

// ── Decision execution ───────────────────────────────────────────────────────

/// Map a [`SteeringDecision`] into agent/observer side-effects.
///
/// This is the bridge between the pure steering logic and the async world.
/// Returns [`Flow`] to control the driving loop.
async fn execute_decision(
    decision: SteeringDecision,
    agent: &mut Agent,
    observer: &mut dyn TurnObserver,
    steering: &mut SteeringState,
    all_actions: &[ActionRecord],
    effective_text: &str,
    ctx: &RoundContext,
) -> Flow {
    match decision {
        SteeringDecision::Continue => Flow::Continue,

        SteeringDecision::Nudge {
            message,
            use_continuation_nudge,
            nudge_count,
            nudge_max,
            log_event,
            log_detail,
        } => {
            observer.on_nudge(nudge_count, nudge_max);
            agent.log_harness_event_with_round(&log_event, &log_detail, steering.total_rounds);

            // Emit a system trace record for visibility
            observer.on_tool_result(&ActionRecord {
                tool: "system".into(),
                args: "".into(),
                summary: log_detail,
                output_to_model: "".into(),
                raw_bytes: 0,
                truncated: false,
                estimated_tokens: 0,
            });

            if use_continuation_nudge {
                agent.push_continuation_nudge();
            } else {
                agent.push_message("user", &message);
            }
            Flow::Continue
        }

        SteeringDecision::JudgeNeeded { context } => {
            // Invoke the judge (async)
            let recent: Vec<_> = all_actions.iter().rev().take(8).cloned().collect();
            let verdict = agent.judge_turn(effective_text, &recent).await;

            // Log the judge decision
            let summary = format!("⭐ JUDGE ({}): {:?}", context, verdict);
            observer.on_tool_result(&ActionRecord {
                tool: "system".into(),
                args: "".into(),
                summary: summary.clone(),
                output_to_model: "".into(),
                raw_bytes: 0,
                truncated: false,
                estimated_tokens: 0,
            });
            agent.log_harness_event_with_round("judge", &summary, steering.total_rounds);

            // Store in session for quick access
            if let Some(s) = &mut agent.session {
                s.meta.last_judge = Some(summary);
                let _ = s.save_meta();
            }

            // Map the verdict through steering
            let criteria = ctx.criteria.as_deref();
            let follow_up = steering.apply_judge_verdict(&verdict, criteria, &context);

            // Execute the follow-up decision
            execute_verdict_decision(follow_up, agent, observer, steering).await
        }

        SteeringDecision::Accept { clear_criteria } => {
            if clear_criteria {
                if let Some(s) = &mut agent.session {
                    s.meta.completion_criteria = None;
                    let _ = s.save_meta();
                }
            }
            Flow::Break
        }

        SteeringDecision::Stuck {
            reason,
            suggested_guidance,
            inject_message,
            log_detail,
        } => {
            observer.on_stuck(&reason, &suggested_guidance);
            agent.log_harness_event_with_round("stuck", &log_detail, steering.total_rounds);
            agent.push_message("user", &inject_message);

            // For ambiguous stop with text_nudge budget: give one more chance
            if steering.text_nudges <= steering::MAX_TEXT_NUDGES {
                observer.on_nudge(steering.text_nudges, steering::MAX_TEXT_NUDGES);
                agent.push_continuation_nudge();
                Flow::Continue
            } else {
                Flow::Break
            }
        }

        SteeringDecision::Timeout { seconds } => {
            let msg = format!("[system: wall-clock timeout after {}s — stopping]", seconds);
            // Note: in TUI mode this message is also logged via agent events; avoid raw eprint to protect display.
            agent.log_harness_event("timeout", &msg);
            Flow::BreakOuter
        }
    }
}

/// Execute a decision that came from `apply_judge_verdict` (post-judge).
///
/// This is separated from `execute_decision` to avoid the recursive async
/// complexity — judge verdicts never produce another `JudgeNeeded`.
async fn execute_verdict_decision(
    decision: SteeringDecision,
    agent: &mut Agent,
    observer: &mut dyn TurnObserver,
    steering: &mut SteeringState,
) -> Flow {
    match decision {
        SteeringDecision::Accept { clear_criteria } => {
            if clear_criteria {
                if let Some(s) = &mut agent.session {
                    s.meta.completion_criteria = None;
                    let _ = s.save_meta();
                }
            }
            Flow::Break
        }

        SteeringDecision::Nudge {
            message,
            use_continuation_nudge,
            nudge_count,
            nudge_max,
            log_event,
            log_detail,
        } => {
            observer.on_nudge(nudge_count, nudge_max);
            agent.log_harness_event_with_round(&log_event, &log_detail, steering.total_rounds);

            if use_continuation_nudge {
                agent.push_continuation_nudge();
            } else {
                agent.push_message("user", &message);
            }
            Flow::Continue
        }

        SteeringDecision::Stuck {
            reason,
            suggested_guidance,
            inject_message,
            log_detail,
        } => {
            observer.on_stuck(&reason, &suggested_guidance);
            agent.log_harness_event_with_round("stuck", &log_detail, steering.total_rounds);
            agent.push_message("user", &inject_message);

            // Give one more chance if budget allows
            if steering.text_nudges <= steering::MAX_TEXT_NUDGES {
                observer.on_nudge(steering.text_nudges, steering::MAX_TEXT_NUDGES);
                agent.push_continuation_nudge();
                Flow::Continue
            } else {
                Flow::Break
            }
        }

        // JudgeNeeded should never come from apply_judge_verdict
        SteeringDecision::JudgeNeeded { .. } => {
            // raven: BUG — judge verdict produced another JudgeNeeded (logged via harness)
            Flow::Break
        }

        SteeringDecision::Continue => Flow::Continue,

        SteeringDecision::Timeout { seconds } => {
            let msg = format!("[system: wall-clock timeout after {}s — stopping]", seconds);
            agent.log_harness_event("timeout", &msg);
            Flow::BreakOuter
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Build a [`RoundContext`] from the current agent state.
fn build_round_context(
    agent: &Agent,
    effective_text: &str,
    round_text: &str,
    finish_reason: Option<&str>,
    is_llm_error: bool,
    has_tool_calls: bool,
) -> RoundContext {
    let criteria = agent
        .session
        .as_ref()
        .and_then(|s| s.meta.completion_criteria.clone());
    let last_user_request = agent
        .session
        .as_ref()
        .and_then(|s| s.meta.last_user_request.clone());
    let log_tail = agent
        .session
        .as_ref()
        .and_then(|s| s.last_assistant_content())
        .unwrap_or_default();
    let agent_mode = agent.current_agent_mode();

    RoundContext {
        effective_text: effective_text.to_string(),
        round_text: round_text.to_string(),
        finish_reason: finish_reason.map(String::from),
        is_llm_error,
        has_tool_calls,
        criteria,
        last_user_request,
        has_session: agent.session.is_some(),
        log_tail,
        agent_mode,
    }
}

/// Summarize recent actions for steering safety checks.
fn build_recent_summary(actions: &[ActionRecord]) -> RecentActionsSummary {
    let recent: Vec<_> = actions.iter().rev().take(8).collect();
    RecentActionsSummary {
        has_exec: recent.iter().any(|a| a.tool == "exec"),
        has_write_or_patch: recent.iter().any(|a| a.tool == "write" || a.tool == "patch"),
        count: recent.len(),
    }
}

/// Consume a streaming response, forwarding events to the observer.
/// Returns (content, tool_calls, usage, finish_reason, thinking).
async fn consume_stream(
    mut stream: mpsc::Receiver<StreamChunk>,
    observer: &mut dyn TurnObserver,
) -> (String, Vec<ToolCall>, Option<Usage>, Option<String>, String) {
    let mut text = String::new();
    let mut tool_calls = vec![];
    let mut usage = None;
    let mut finish_reason = None;
    let mut thinking = String::new();

    while let Some(chunk) = stream.recv().await {
        if observer.should_stop() {
            break;
        }
        match chunk {
            StreamChunk::Token(t) => {
                text.push_str(&t);
                observer.on_token(&t);
            }
            StreamChunk::Thinking(t) => {
                observer.on_thinking(&t);
                thinking.push_str(&t);
            }
            StreamChunk::Done {
                content,
                tool_calls: tcs,
                usage: u,
                finish_reason: fr,
            } => {
                if !content.is_empty() && text.is_empty() {
                    text = content;
                }
                tool_calls = tcs;
                usage = u;
                finish_reason = fr;
            }
            StreamChunk::Error(e) => {
                if text.is_empty() {
                    text = format!("LLM error: {}", e);
                }
            }
        }
    }

    (text, tool_calls, usage, finish_reason, thinking)
}


