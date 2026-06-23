//! Shared agent driving loop — the single source of truth for nudges,
//! auto-continue, tool dispatch, and round budget management.
//!
//! Both the interactive TUI and headless `--prompt` mode call [`drive_turn()`].
//! The only difference is the [`TurnObserver`] implementation:
//! - TUI provides one that renders UI events
//! - Headless provides one that auto-approves and logs to stdout
//! - Tests provide a silent one

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::agent::{Agent, ActionRecord, TurnJudge, TurnResult, TurnMetrics};
use crate::llm::{StreamChunk, ToolCall, Usage};
use crate::tools;

// ── Policy constants ─────────────────────────────────────────────────────────
// These are now primarily *safety backstops*.
// The primary decision for "is the request fulfilled?" is now made by
// inference via Agent::judge_goal_fulfilled (using the session's goal +
// achievement_tests). Hard limits prevent infinite loops.

/// Safety limit on how many times we will explicitly nudge for continuation
/// before accepting a text-only response.
pub const MAX_TEXT_NUDGES: u32 = 3;

/// Safety limit on auto-continuing when the inner round budget is exhausted
/// but the model was still actively calling tools.
pub const MAX_AUTO_CONTINUES: u32 = 3;

/// Hard cap on tool rounds per budget window.
const MAX_TOOL_ROUNDS: u32 = 120;

/// The nudge message pushed when the model pauses to narrate mid-turn.
pub const CONTINUATION_NUDGE: &str =
    "[Continue working. You paused to describe your plan instead of calling the next tool. \
     Call the next tool now — do not narrate.]";

/// The recovery message pushed when finish_reason is "length" (model hit max_tokens).
pub const LENGTH_RECOVERY_NUDGE: &str =
    "[system: Your previous response was truncated (output limit). \
     Please use your tools now to investigate and solve the task. \
     Start with `list` or `read` to explore the codebase.]";

/// Max transparent retries for a single streaming LLM call on transient
/// errors (e.g. "error decoding response body"). We retry the send+consume
/// without pushing error text into conversation history.
const MAX_STREAM_RETRIES: u32 = 2;

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
    async fn approve_tool(&mut self, _tc: &ToolCall) -> bool { true }
    /// Model paused to narrate — we're pushing a continuation nudge.
    fn on_nudge(&mut self, _count: u32, _max: u32) {}
    /// Hit round limit; auto-continuing (or exhausted if `exhausted` is true).
    fn on_round_limit(&mut self, _continuation: u32, _max: u32, _exhausted: bool) {}
    /// Updated context token estimate.
    fn on_context_usage(&mut self, _tokens: u32) {}
    /// Check if the caller wants to abort the turn (Ctrl+C, etc.).
    fn should_stop(&self) -> bool { false }

    /// A live user message was injected mid-turn (interject).
    /// The driver has already called on_new_user_input with it.
    fn on_interject(&mut self, _msg: &str) {}

    /// If the user interjected with a new instruction while the turn was
    /// in progress, return it here. The driver will inject it and continue
    /// the loop (fresh model request). This allows live redirection without
    /// ending the overall turn.
    fn take_interject(&mut self) -> Option<String> { None }

    /// After a denial (or at any point), should further tool calls from the
    /// current model response be skipped? (e.g. after too many user denials)
    fn stop_tool_processing(&self) -> bool { false }

    /// The judge detected unproductive looping. The driver has already
    /// injected guidance so the final output should ask the user for help.
    fn on_stuck(&mut self, _reason: &str, _suggested_guidance: &str) {}
}

// ── Built-in observers ───────────────────────────────────────────────────────

/// Silent observer for integration tests and `Agent::run_turn()` backward compat.
pub struct SilentObserver;
impl TurnObserver for SilentObserver {
    fn on_stuck(&mut self, _reason: &str, _suggested: &str) {
        // For tests/headless we just record it in the final state if needed.
        // The guidance message is already pushed into the conversation.
    }
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

// ── The canonical driving loop ───────────────────────────────────────────────

/// Drive one agent turn to completion using the same logic as the interactive TUI.
///
/// This is the **single source of truth** for:
/// - Streaming inference
/// - Tool execution and approval
/// - Text nudges (model narrates instead of acting)
/// - `finish_reason=length` recovery
/// - Auto-continue on round limit
///
/// The `observer` receives events for presentation/logging but does not
/// influence the driving logic (except for `approve_tool` and `should_stop`).
pub async fn drive_turn(
    agent: &mut Agent,
    prompt: &str,
    observer: &mut dyn TurnObserver,
) -> Result<TurnResult> {
    // When we have a session, the initial prompt is injected into the system block
    // as "Latest User Request" on every model call. Using record_user_request()
    // avoids re-appending the (potentially huge) raw task text into conversation
    // history on each headless launch. When there's NO session (integration tests,
    // simple --prompt without workspace), we must push the prompt into conversation
    // directly so the model sees it.
    if agent.has_session() {
        agent.record_user_request(prompt);
    } else {
        agent.on_new_user_input(prompt);
    }

    let max_rounds = agent.current_config().max_rounds.clamp(1, MAX_TOOL_ROUNDS);
    let tools_schema = tools::all_tools();
    let tools_for_request = agent.current_config().tools_enabled.then(|| tools_schema.clone());

    let mut all_actions: Vec<ActionRecord> = vec![];
    let mut last_assistant_text = String::new();
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;
    let mut total_llm_rounds: u32 = 0;
    let mut text_nudges: u32 = 0;
    let mut tools_used_this_turn: usize = 0;

    // Outer auto-continue loop (mirrors TUI 'auto_continue label)
    'auto_continue: for continuation in 0..=MAX_AUTO_CONTINUES {
        let mut completed_naturally = false;

        // Inner tool-use loop (one "budget" of rounds)
        for _round in 0..max_rounds {
            if observer.should_stop() {
                break 'auto_continue;
            }

            // ── Send request (streaming) with limited transparent retry on
            // transient errors (e.g. decode errors from the server). We do
            // not push error text into assistant history and do not treat
            // a retried error as a normal "no tools" round.
            let (round_text, tool_calls, usage, finish_reason) = {
                let mut attempt = 0u32;
                loop {
                    let stream = match agent.send_streaming_request(tools_for_request.clone()).await {
                        Ok(s) => s,
                        Err(e) if attempt < MAX_STREAM_RETRIES => {
                            attempt += 1;
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    total_llm_rounds += 1;

                    let (text, calls, usg, fr) = consume_stream(stream, observer).await;

                    if text.starts_with("LLM error:") && attempt < MAX_STREAM_RETRIES {
                        attempt += 1;
                        // retry the send without polluting conversation
                        continue;
                    }
                    break (text, calls, usg, fr);
                }
            };

            if let Some(u) = &usage {
                prompt_tokens += u.prompt_tokens.unwrap_or(0) as u64;
                completion_tokens += u.completion_tokens.unwrap_or(0) as u64;
            }

            let is_llm_error = round_text.starts_with("LLM error:");

            // Interject takes precedence (user injected a new instruction live).
            // Commit any partial text we received, inject the new user message,
            // notify, and immediately request the next model response.
            // Never commit raw LLM error text as assistant output.
            if let Some(msg) = observer.take_interject() {
                if !round_text.trim().is_empty() && !is_llm_error {
                    agent.push_assistant_text(&round_text);
                    last_assistant_text = round_text.clone();
                }
                agent.on_new_user_input(&msg);
                observer.on_interject(&msg);
                continue;
            }

            // Hard stop (e.g. Esc) with no interject: commit partial and end.
            if observer.should_stop() {
                if !round_text.trim().is_empty() && !is_llm_error {
                    agent.push_assistant_text(&round_text);
                    last_assistant_text = round_text.clone();
                }
                break 'auto_continue;
            }

            // Record assistant text (normal case).
            // Never push "LLM error: ..." strings as model output — they are
            // transient transport problems, not assistant content.
            if !round_text.trim().is_empty() && !is_llm_error {
                agent.push_assistant_text(&round_text);
                last_assistant_text = round_text.clone();
            }

            // ── No tool calls: decide what to do ──
            if tool_calls.is_empty() {
                if is_llm_error {
                    // Exhausted retries for this call. Give the agent a chance
                    // to continue instead of treating the error as completion.
                    if total_llm_rounds < max_rounds {
                        agent.push_message(
                            "user",
                            "[LLM stream error during request. Please continue using your tools to solve the task.]",
                        );
                        continue;
                    }
                }

                // finish_reason=length → recovery nudge
                let hit_length = finish_reason.as_deref() == Some("length");
                if hit_length && total_llm_rounds < max_rounds {
                    agent.push_message("user", LENGTH_RECOVERY_NUDGE);
                    continue;
                }

                // Rich inference judge: decides fulfillment *and* whether the agent
                // is looping unproductively and should ask the user for guidance.
                // Only consult the judge when the stop reason is ambiguous.
                // finish_reason=stop means the model deliberately finished — accept it.
                let model_stopped = finish_reason.as_deref() == Some("stop");
                if !model_stopped && tools_used_this_turn > 0 {
                    let recent: Vec<_> = all_actions.iter().rev().take(8).cloned().collect();

                    // Hard safety: if the *original user request* clearly asks to
                    // "run"/"exec"/"show output" but we have no exec action yet,
                    // do not accept as fulfilled.
                    // We deliberately do NOT scan recent conversation messages here,
                    // because we push our own nudge/debug messages into the user history
                    // and they contain "exec"/"run" words — that would create a self-loop.
                    let request_text = agent.session.as_ref()
                        .and_then(|s| s.meta.last_user_request.as_deref())
                        .unwrap_or("")
                        .to_string();
                    let implies_run = request_text.to_lowercase().contains("run") ||
                                      request_text.to_lowercase().contains("exec") ||
                                      request_text.to_lowercase().contains("show") ||
                                      request_text.to_lowercase().contains("output");
                    let has_exec = recent.iter().any(|a| a.tool == "exec");

                    if implies_run && !has_exec {
                        // Force continuation / nudge; do not trust judge or model claim yet.
                        observer.on_tool_result(&ActionRecord {
                            tool: "system".into(),
                            args: "".into(),
                            summary: "⭐ JUDGE/HARD-SAFETY: request implies run but no exec yet → forcing nudge".into(),
                            output_to_model: "".into(),
                            raw_bytes: 0,
                            truncated: false,
                        });
                        // Do not push the debug into the conversation history for the model.
                        // It pollutes request_text scans and the model's context.
                        // The observer already surfaces it for logs/trace.
                        if text_nudges < MAX_TEXT_NUDGES {
                            text_nudges += 1;
                            observer.on_nudge(text_nudges, MAX_TEXT_NUDGES);
                            agent.push_continuation_nudge();
                            continue;
                        }
                        completed_naturally = true;
                        break;
                    }

                    // Additional safety: if we did a write/patch but the original request
                    // implies a run/show and we still haven't done an exec, nudge once.
                    // Uses the same (clean) implies_run from last_user_request only.
                    let did_write = recent.iter().any(|a| a.tool == "write" || a.tool == "patch");
                    if did_write && implies_run && !has_exec {
                        if text_nudges < MAX_TEXT_NUDGES {
                            text_nudges += 1;
                            observer.on_nudge(text_nudges, MAX_TEXT_NUDGES);
                            agent.push_continuation_nudge();
                            continue;
                        }
                    }

                    let decision = agent.judge_turn(&round_text, &recent).await;
                    observer.on_tool_result(&ActionRecord {
                        tool: "system".into(),
                        args: "".into(),
                        summary: format!("⭐ JUDGE DECISION: {:?}", decision),
                        output_to_model: "".into(),
                        raw_bytes: 0,
                        truncated: false,
                    });
                    // Note: we no longer push judge decisions as user messages (they
                    // polluted request_text and the model's context). The observer
                    // call above is enough for trace / logs.

                    match decision {
                        TurnJudge::Fulfilled { .. } => {
                            completed_naturally = true;
                            break;
                        }
                        TurnJudge::Stuck { reason, suggested_guidance } => {
                            observer.on_stuck(&reason, &suggested_guidance);
                            let guidance = format!(
                                "[You appear to be looping without progress: {}. \
                                 Stop calling tools. Clearly explain the current situation \
                                 to the user and ask for specific guidance: {}]\n\
                                 If the diagnosis is clear, immediately use `patch` or `write` on the relevant source file to apply the fix.",
                                reason, suggested_guidance
                            );
                            agent.push_message("user", &guidance);
                            // For headless/eval runs, give the model one more chance to act on the guidance
                            // instead of hard-terminating. Use continuation nudge path if budget allows.
                            if text_nudges < MAX_TEXT_NUDGES {
                                text_nudges += 1;
                                observer.on_nudge(text_nudges, MAX_TEXT_NUDGES);
                                agent.push_continuation_nudge();
                                continue;
                            }
                            completed_naturally = true;
                            break;
                        }
                        TurnJudge::Continue => {
                            if text_nudges < MAX_TEXT_NUDGES {
                                text_nudges += 1;
                                observer.on_nudge(text_nudges, MAX_TEXT_NUDGES);
                                agent.push_continuation_nudge();
                                continue;
                            }
                            completed_naturally = true;
                            break;
                        }
                    }
                }

                completed_naturally = true;
                break;
            }

            // ── Execute tool calls ──
            tools_used_this_turn += tool_calls.len();

            let mut to_execute: Vec<ToolCall> = vec![];
            for tc in &tool_calls {
                if observer.stop_tool_processing() {
                    break;
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
                    if observer.stop_tool_processing() {
                        break;
                    }
                }
            }

            let records = agent.execute_and_record_tool_calls(&to_execute).await;
            for r in &records {
                observer.on_tool_result(r);
            }
            all_actions.extend(records);

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
        // Loop back for another budget of rounds
    }

    agent.force_flush_session().await;
    observer.on_context_usage(agent.estimated_context_tokens());

    let tool_call_count = all_actions.len() as u32;
    let result = TurnResult {
        final_text: last_assistant_text,
        actions: all_actions,
        rounds_used: total_llm_rounds,
        metrics: TurnMetrics {
            llm_rounds: total_llm_rounds,
            tool_calls: tool_call_count,
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            round_limit_hit: total_llm_rounds >= max_rounds,
        },
    };

    observer.on_context_usage(agent.estimated_context_tokens());
    Ok(result)
}

/// Consume a streaming response, forwarding events to the observer.
/// Returns (content, tool_calls, usage, finish_reason).
async fn consume_stream(
    mut stream: mpsc::Receiver<StreamChunk>,
    observer: &mut dyn TurnObserver,
) -> (String, Vec<ToolCall>, Option<Usage>, Option<String>) {
    let mut text = String::new();
    let mut tool_calls = vec![];
    let mut usage = None;
    let mut finish_reason = None;

    while let Some(chunk) = stream.recv().await {
        if observer.should_stop() {
            // Stop consuming early; interject or hard stop will be handled by caller.
            break;
        }
        match chunk {
            StreamChunk::Token(t) => {
                text.push_str(&t);
                observer.on_token(&t);
            }
            StreamChunk::Thinking(t) => {
                observer.on_thinking(&t);
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
                // Surface as error marker. The driver decides whether to retry
                // without ever treating this as model content.
                if text.is_empty() {
                    text = format!("LLM error: {}", e);
                }
            }
        }
    }

    (text, tool_calls, usage, finish_reason)
}
