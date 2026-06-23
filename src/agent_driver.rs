//! Shared agent driving loop — the single source of truth for nudges,
//! auto-continue, tool dispatch, and round budget management.
//!
//! Both the interactive TUI and headless `--prompt` mode call [`drive_turn()`].
//! The only difference is the [`TurnObserver`] implementation:
//! - TUI provides one that renders UI events
//! - Headless provides one that auto-approves and logs to stdout
//! - Tests provide a silent one

use anyhow::Result;
use tokio::sync::mpsc;

use crate::agent::{Agent, ActionRecord, TurnResult, TurnMetrics};
use crate::llm::{StreamChunk, ToolCall, Usage};
use crate::tools;

// ── Policy constants ─────────────────────────────────────────────────────────
// Change these once → both TUI and headless reflect it.

/// Max times we nudge the model when it pauses to narrate instead of calling tools.
pub const MAX_TEXT_NUDGES: u32 = 2;

/// Max auto-continues when the model hits the round limit but was still active.
pub const MAX_AUTO_CONTINUES: u32 = 3;

/// Hard cap on tool rounds per budget window.
const MAX_TOOL_ROUNDS: u32 = 120;

/// The nudge message pushed when the model pauses to narrate mid-turn.
pub const CONTINUATION_NUDGE: &str =
    "[Continue working. You paused to describe your plan instead of executing it. \
     Call the next tool now — do not narrate.]";

/// The recovery message pushed when finish_reason is "length" (model hit max_tokens).
pub const LENGTH_RECOVERY_NUDGE: &str =
    "[system: Your previous response was truncated (output limit). \
     Please use your tools now to investigate and solve the task. \
     Start with `list` or `read` to explore the codebase.]";

// ── Observer trait ────────────────────────────────────────────────────────────

/// The seam between the agent driving loop and its presentation layer.
///
/// Default implementations auto-approve everything and do nothing —
/// suitable for headless tests.
pub trait TurnObserver: Send {
    /// Streaming token from the model's visible response.
    fn on_token(&mut self, _token: &str) {}
    /// Thinking/reasoning content (Qwen reasoning_content, etc.).
    fn on_thinking(&mut self, _text: &str) {}
    /// A tool is about to execute.
    fn on_tool_start(&mut self, _name: &str, _args: &str) {}
    /// A tool finished executing.
    fn on_tool_result(&mut self, _record: &ActionRecord) {}
    /// Should this tool call be approved? Default: auto-approve.
    fn approve_tool(&mut self, _tc: &ToolCall) -> bool { true }
    /// Model paused to narrate — we're pushing a continuation nudge.
    fn on_nudge(&mut self, _count: u32, _max: u32) {}
    /// Hit round limit; auto-continuing (or exhausted if `exhausted` is true).
    fn on_round_limit(&mut self, _continuation: u32, _max: u32, _exhausted: bool) {}
    /// Updated context token estimate.
    fn on_context_usage(&mut self, _tokens: u32) {}
    /// Check if the caller wants to abort (Ctrl+C, etc.).
    fn should_stop(&self) -> bool { false }
}

// ── Built-in observers ───────────────────────────────────────────────────────

/// Silent observer for integration tests and `Agent::run_turn()` backward compat.
pub struct SilentObserver;
impl TurnObserver for SilentObserver {}

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
    agent.on_new_user_input(prompt);

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

            // ── Send request (streaming) ──
            let stream = agent.send_streaming_request(tools_for_request.clone()).await?;
            total_llm_rounds += 1;

            // ── Consume the stream ──
            let (round_text, tool_calls, usage, finish_reason) =
                consume_stream(stream, observer).await;

            if let Some(u) = &usage {
                prompt_tokens += u.prompt_tokens.unwrap_or(0) as u64;
                completion_tokens += u.completion_tokens.unwrap_or(0) as u64;
            }

            // Record assistant text
            if !round_text.trim().is_empty() {
                agent.push_assistant_text(&round_text);
                last_assistant_text = round_text.clone();
            }

            // ── No tool calls: decide what to do ──
            if tool_calls.is_empty() {
                // finish_reason=length → recovery nudge
                let hit_length = finish_reason.as_deref() == Some("length");
                if hit_length && total_llm_rounds < max_rounds {
                    agent.push_message("user", LENGTH_RECOVERY_NUDGE);
                    continue;
                }

                // Text nudge: model narrated instead of acting.
                // Only nudge if the model didn't explicitly stop (finish_reason != "stop")
                // and tools were used earlier this turn.
                let model_stopped = finish_reason.as_deref() == Some("stop");
                if !model_stopped && tools_used_this_turn > 0 && text_nudges < MAX_TEXT_NUDGES {
                    text_nudges += 1;
                    observer.on_nudge(text_nudges, MAX_TEXT_NUDGES);
                    agent.push_continuation_nudge();
                    continue;
                }

                // Model genuinely done
                completed_naturally = true;
                break;
            }

            // ── Execute tool calls ──
            tools_used_this_turn += tool_calls.len();

            let mut to_execute: Vec<ToolCall> = vec![];
            for tc in &tool_calls {
                observer.on_tool_start(&tc.function.name, &tc.function.arguments);
                if observer.approve_tool(tc) {
                    to_execute.push(tc.clone());
                } else {
                    let deny = format!(
                        "DENIED: The user refused to approve this {} action. \
                         Do NOT retry the same action.",
                        tc.function.name
                    );
                    agent.record_tool_denial(tc, &deny);
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
                // Surface error as text so the caller sees it
                text = format!("LLM error: {}", e);
            }
        }
    }

    (text, tool_calls, usage, finish_reason)
}
