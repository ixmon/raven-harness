//! Core agent loop: multi-round tool use until the model stops calling tools.
//!
//! Strongly inspired by Raven Hotel's agent_runtime (MAX_TOOL_ROUNDS, strict
//! "never hallucinate success", prefer patch, Think/Act/Report discipline).

use anyhow::Result;

use crate::config::Config;
use crate::llm::{ChatRequest, LlmClient, Message, StreamChunk, ToolCall};
use crate::tools;
use serde_json::json;
use std::path::Path;
use tokio::sync::mpsc;


/// Safely truncate a string to at most `max_bytes` bytes without splitting
/// a multi-byte UTF-8 codepoint (which would panic).
fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn make_rel_path(p: &str, workspace: &Path) -> String {
    let abs = std::path::Path::new(p);
    if let Ok(stripped) = abs.strip_prefix(workspace) {
        stripped.to_string_lossy().to_string()
    } else if abs.is_absolute() {
        // fallback: last components
        abs.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string())
    } else {
        p.to_string()
    }
}

const MAX_TOOL_ROUNDS: u32 = 12;

#[derive(Debug, Clone)]
pub struct ActionRecord {
    pub tool: String,
    pub args: String,
    pub summary: String,
}

#[derive(Debug)]
pub struct TurnResult {
    pub final_text: String,
    pub actions: Vec<ActionRecord>,
    pub rounds_used: u32,
}

pub struct Agent {
    client: LlmClient,
    config: Config,
    /// Clean conversation turns only (user / assistant / tool). The rich session
    /// context (repo tree, goal, pitfalls, recent summary) is injected fresh on
    /// every prompt construction so it stays current.
    conversation: Vec<Message>,
    /// Persistent session (goal tracking, repo cache, full log, meta.json under ~/.raven-hotel/)
    session: Option<crate::session::Session>,
}

impl Agent {
    pub fn new(mut config: Config) -> Self {
        let client = LlmClient::new(config.clone());
        // Use prebuilt session from main (if provided) so that trust prompt + repo
        // cache bootstrap performed before launching the TUI / --prompt is connected.
        // Falls back to a fresh init (loads from disk meta.json / context.db).
        let prebuilt = std::mem::take(&mut config.prebuilt_session);
        let session = prebuilt.or_else(|| {
            crate::session::Session::init(&config.workspace).ok()
        });
        let mut conversation = vec![];

        // Restore recent conversation from the persistent log so the model
        // remembers what it was doing after a restart (Ctrl+C / crash).
        if let Some(s) = &session {
            let recent = s.load_recent_conversation(20);
            for (role, content) in recent {
                conversation.push(Message {
                    role,
                    content: Some(content),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }

        Self {
            client,
            config,
            conversation,
            session,
        }
    }

    /// Reset conversation (new "room" / task). The persistent session (goal, repo cache, log) is kept.
    pub fn reset(&mut self) {
        self.conversation.clear();
    }

    /// Truncate tool output to fit the context budget.
    fn truncate_for_context(&self, s: &str) -> String {
        truncate_for_context(s, self.config.context_budget.tool_result_bytes)
    }

    /// High level entry: give the agent a user goal and let it run until it stops using tools.
    pub async fn run_turn(&mut self, user_input: &str) -> Result<TurnResult> {
        self.on_new_user_input(user_input);

        let mut actions = vec![];
        let mut last_assistant_text = String::new();
        let tools_schema = tools::all_tools();

        for round in 0..self.config.max_rounds.max(1).min(MAX_TOOL_ROUNDS) {
            let messages = self.build_messages_for_model();
            let req = ChatRequest {
                messages,
                tools: Some(tools_schema.clone()),
                temperature: self.config.temperature,
                max_tokens: self.config.max_tokens,
                stream: false,
            };

            let resp = self.client.chat(req).await?;

            if !resp.content.trim().is_empty() {
                last_assistant_text = resp.content.clone();
                self.conversation.push(Message {
                    role: "assistant".into(),
                    content: Some(resp.content.clone()),
                    tool_calls: if resp.tool_calls.is_empty() {
                        None
                    } else {
                        Some(resp.tool_calls.clone())
                    },
                    tool_call_id: None,
                });
            } else if !resp.tool_calls.is_empty() {
                self.conversation.push(Message {
                    role: "assistant".into(),
                    content: Some("".into()),
                    tool_calls: Some(resp.tool_calls.clone()),
                    tool_call_id: None,
                });
            }

            if resp.tool_calls.is_empty() {
                self.persist_turn().await;
                return Ok(TurnResult {
                    final_text: last_assistant_text,
                    actions,
                    rounds_used: round + 1,
                });
            }

            // Execute tools (special session tools are intercepted)
            for tc in &resp.tool_calls {
                let tool_name = &tc.function.name;
                let raw_args = &tc.function.arguments;

                // Invalidate file summaries for any write/patch (so read_summary sees fresh mtime next time)
                if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(raw_args) {
                    self.maybe_invalidate_summary(&tool_name, &args_val);
                }

                // Intercept session meta + summary cache tools
                if let Some(ack) = self.handle_session_tool(tool_name, raw_args).await {
                    actions.push(ActionRecord {
                        tool: tool_name.clone(),
                        args: raw_args.clone(),
                        summary: ack.lines().next().unwrap_or(&ack).to_string(),
                    });
                    self.conversation.push(Message {
                        role: "tool".into(),
                        content: Some(self.truncate_for_context(&ack)),
                        tool_calls: None,
                        tool_call_id: Some(tc.id.clone()),
                    });
                    continue;
                }

                let output = tools::execute(tool_name, raw_args, &self.config.workspace, self.config.context_budget.read_line_limit)
                    .await
                    .unwrap_or_else(|e| format!("X Tool execution error: {}", e));

                actions.push(ActionRecord {
                    tool: tool_name.clone(),
                    args: raw_args.clone(),
                    summary: output.lines().next().unwrap_or(&output).to_string(),
                });

                self.conversation.push(Message {
                    role: "tool".into(),
                    content: Some(self.truncate_for_context(&output)),
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                });
            }

            self.persist_turn().await;
        }

        // Exhausted rounds
        let exhaustion = "⏸️ Reached tool round limit for this turn. Send another message to continue.";
        self.conversation.push(Message {
            role: "assistant".into(),
            content: Some(exhaustion.into()),
            tool_calls: None,
            tool_call_id: None,
        });
        self.persist_turn().await;

        Ok(TurnResult {
            final_text: last_assistant_text + "\n\n" + exhaustion,
            actions,
            rounds_used: self.config.max_rounds,
        })
    }

    /// Get a streaming turn. The caller is responsible for consuming chunks and
    /// calling `feed_tool_result` when tool calls are completed.
    pub async fn run_turn_streaming(
        &mut self,
        user_input: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>> {
        self.on_new_user_input(user_input);
        self.prune_history().await;

        let messages = self.build_messages_for_model();
        let req = ChatRequest {
            messages,
            tools: Some(tools::all_tools()),
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            stream: true,
        };

        self.client.chat_stream(req).await
    }

    /// After a streaming turn produced tool calls, execute them and append the
    /// tool results to conversation so the next `run_turn_streaming` can continue.
    pub async fn execute_and_record_tool_calls(&mut self, tool_calls: &[ToolCall]) -> Vec<ActionRecord> {
        let mut records = vec![];
        for tc in tool_calls {
            let tool_name = tc.function.name.clone();
            let raw_args = tc.function.arguments.clone();

            // Invalidate summaries for mutations *before* execution
            let args_val: serde_json::Value = serde_json::from_str(&raw_args).unwrap_or(json!({}));
            self.maybe_invalidate_summary(&tool_name, &args_val);

            // Intercept session tools (goal, discovery, summaries)
            if let Some(ack) = self.handle_session_tool(&tool_name, &raw_args).await {
                records.push(ActionRecord {
                    tool: tool_name,
                    args: raw_args,
                    summary: ack.lines().next().unwrap_or("").to_string(),
                });
                self.conversation.push(Message {
                    role: "tool".into(),
                    content: Some(self.truncate_for_context(&ack)),
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                });
                continue;
            }

            let output = tools::execute(&tool_name, &raw_args, &self.config.workspace, self.config.context_budget.read_line_limit)
                .await
                .unwrap_or_else(|e| format!("❌ Tool error: {}", e));

            records.push(ActionRecord {
                tool: tool_name.clone(),
                args: raw_args.clone(),
                summary: output.lines().next().unwrap_or("").to_string(),
            });

            self.conversation.push(Message {
                role: "assistant".into(),
                content: None,
                tool_calls: Some(vec![tc.clone()]),
                tool_call_id: None,
            });

            self.conversation.push(Message {
                role: "tool".into(),
                content: Some(self.truncate_for_context(&output)),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
            });
        }
        self.persist_turn().await;
        records
    }

    pub fn push_assistant_text(&mut self, text: &str) {
        self.conversation.push(Message {
            role: "assistant".into(),
            content: Some(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
        // Log to persistent full_log so it survives restarts
        if let Some(s) = &self.session {
            let entry = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "role": "assistant",
                "content": text,
                "has_tool_calls": false,
            });
            let _ = s.append_log(&entry.to_string());
        }
    }

    /// Push a brief nudge into the conversation when the model pauses
    /// to narrate instead of continuing to call tools.
    pub fn push_continuation_nudge(&mut self) {
        self.conversation.push(Message {
            role: "user".into(),
            content: Some(
                "[Continue working. You paused to describe your plan instead of executing it. \
                 Call the next tool now — do not narrate.]".to_string()
            ),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    /// Continue with another streaming inference using the *current* conversation
    /// (no extra user message is pushed). This is used after tool results have
    /// already been appended.
    pub async fn continue_turn_streaming(&mut self) -> Result<mpsc::Receiver<StreamChunk>> {
        self.prune_history().await;
        let messages = self.build_messages_for_model();
        let tools_schema = tools::all_tools();
        let req = ChatRequest {
            messages,
            tools: Some(tools_schema),
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            stream: true,
        };
        self.client.chat_stream(req).await
    }

    /// Context management (evolved for sessions).
    /// We keep the *conversation* (user/assistant/tool turns) bounded.
    /// The rich repo + goal + recent summary lives in the Session and is injected
    /// fresh every time we build the prompt (see build_messages_for_model).
    async fn prune_history(&mut self) {
        const MAX_CONVERSATION: usize = 48;
        const MIN_TO_SUMMARIZE: usize = 6;

        if self.conversation.len() <= MAX_CONVERSATION {
            return;
        }

        let start = self.conversation.len() - MAX_CONVERSATION;
        let dropped = self.conversation[..start].to_vec();
        let recent = self.conversation[start..].to_vec();

        if dropped.len() >= MIN_TO_SUMMARIZE {
            let summary = self.summarize_messages(&dropped).await;
            // Push the compression into the session's rolling "recent turns" summary
            // (the injection block will surface a version of it).
            if let Some(s) = &mut self.session {
                let combined = if s.meta.recent_turns_summary.is_empty() {
                    summary.clone()
                } else {
                    format!("{}\n\n(earlier)\n{}", s.meta.recent_turns_summary, summary)
                };
                // Keep it small
                let trimmed = if combined.len() > 1800 { safe_truncate(&combined, 1800).to_string() + "..." } else { combined };
                let _ = s.set_recent_turns_summary(&trimmed);
            }
        }

        self.conversation = recent;
    }

    /// Ask the model (without tools) to produce a compact summary of a batch of
    /// older messages. This is our main "compression" mechanism.
    async fn summarize_messages(&self, msgs: &[Message]) -> String {
        if msgs.is_empty() {
            return String::new();
        }

        let history_dump = msgs
            .iter()
            .map(|m| {
                let role = &m.role;
                let content = m.content.as_deref().unwrap_or("");
                let tc = if let Some(tcs) = &m.tool_calls {
                    format!(" [tool_calls: {}]", tcs.iter().map(|t| t.function.name.as_str()).collect::<Vec<_>>().join(", "))
                } else { "".to_string() };
                format!("{role}: {content}{tc}")
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let summary_prompt = format!(
            "The following is older conversation history from an agent exploring/ working on a task in a codebase. Produce a concise, factual summary (bullet points or short paragraphs) of the key actions taken, files or information discovered, and current state of understanding. Omit low-value details. Focus on what would help the agent continue the original task effectively.\n\n{}",
            history_dump
        );

        let req = ChatRequest {
            messages: vec![Message {
                role: "user".into(),
                content: Some(summary_prompt),
                tool_calls: None,
                tool_call_id: None,
            }],
            tools: None,
            temperature: 0.3,
            max_tokens: 800,
            stream: false,
        };

        match self.client.chat(req).await {
            Ok(resp) => resp.content.trim().to_string(),
            Err(e) => format!("[summarization failed: {}]", e),
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Session-aware helpers (new context management)
    // ─────────────────────────────────────────────────────────────────

    fn on_new_user_input(&mut self, user_input: &str) {
        // Record the request for the injection block
        if let Some(s) = &mut self.session {
            let _ = s.set_last_user_request(user_input);
            // Seed an initial goal from the first real request if we don't have one yet
            if s.meta.current_goal.contains("not yet established") || s.meta.current_goal.trim().is_empty() {
                let g = safe_truncate(user_input, 160);
                let _ = s.update_goal(&format!("Initial goal from user: {}", g), None, None);
            }
        }

        if self.conversation.is_empty() {
            // We no longer push a static system into conversation. The dynamic
            // system (base + rich session block) is built on every prompt.
        }

        self.conversation.push(Message {
            role: "user".into(),
            content: Some(user_input.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });

        // Log user message to persistent full_log so it survives restarts
        if let Some(s) = &self.session {
            let entry = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "role": "user",
                "content": user_input,
                "has_tool_calls": false,
            });
            let _ = s.append_log(&entry.to_string());
        }
    }

    /// Build the exact messages array we will send to the model this turn.
    /// Always starts with a fresh system message containing the current
    /// repo cache + goal + pitfalls + recent summary from the Session.
    fn build_messages_for_model(&self) -> Vec<Message> {
        let base = system_message(&self.config.workspace);

        let mut msgs = vec![];
        if let Some(s) = &self.session {
            let injection = s.get_injection_block();
            let combined = if base.content.as_deref().unwrap_or("").is_empty() {
                injection
            } else {
                format!("{}\n\n{}", base.content.as_deref().unwrap_or(""), injection)
            };
            msgs.push(Message {
                role: "system".into(),
                content: Some(combined),
                tool_calls: None,
                tool_call_id: None,
            });
        } else {
            msgs.push(base);
        }

        // Append the actual conversation turns (already pruned)
        msgs.extend(self.conversation.iter().cloned());
        msgs
    }

    /// Handle session meta + file summary cache tools. Returns Some(ack) if handled.
    async fn handle_session_tool(&mut self, name: &str, args_json: &str) -> Option<String> {
        let args: serde_json::Value = serde_json::from_str(args_json).unwrap_or_else(|_| json!({}));

        if name == "update_goal" {
            if let Some(s) = &mut self.session {
                let goal = args.get("goal").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let tests = args.get("tests").and_then(|v| v.as_array()).map(|a| {
                    a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect::<Vec<_>>()
                });
                let pits = args.get("pitfalls").and_then(|v| v.as_array()).map(|a| {
                    a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect::<Vec<_>>()
                });
                let _ = s.update_goal(&goal, tests, pits);
                return Some(format!("Goal, tests and pitfalls updated in session meta. New goal: {}", goal));
            }
        }

        if name == "record_discovery" {
            if let Some(s) = &mut self.session {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let _ = s.record_discovery(&text);
                return Some(format!("Discovery recorded in session: {}", text));
            }
        }

        if name == "read_summary" {
            if let Some(s) = &self.session {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let rel = make_rel_path(path, &self.config.workspace);
                let abs = self.config.workspace.join(&rel);

                match s.get_file_summary(&rel) {
                    Ok(Some((mtime, summary))) => {
                        return Some(format!(
                            "FRESH SUMMARY (path={}, mtime={} matched current file mtime):\n{}",
                            rel, mtime, summary
                        ));
                    }
                    _ => {
                        // Stale or missing: give the model a capped raw view + the mtime to use when storing
                        let cur_mtime = crate::session::current_file_mtime_for_agent(&abs); // helper exposed
                        let raw = tools::read_file(path, None, &self.config.workspace, self.config.context_budget.read_line_limit);
                        return Some(format!(
                            "NO FRESH SUMMARY for {} (current on-disk mtime: {}).\n\nCapped raw view (analyze this, then call store_summary with the exact mtime above):\n{}\n\nAfter you understand the file, call store_summary with a concise summary.",
                            rel, cur_mtime, raw
                        ));
                    }
                }
            }
        }

        if name == "store_summary" {
            if let Some(s) = &mut self.session {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let mtime = args.get("mtime").and_then(|v| v.as_i64()).unwrap_or(0);
                let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                let rel = make_rel_path(path, &self.config.workspace);
                let _ = s.store_file_summary(&rel, mtime, summary);
                return Some(format!(
                    "✅ Summary cached for {} at mtime {}. Future read_summary calls with matching mtime will return this instead of raw source.",
                    rel, mtime
                ));
            }
        }

        None
    }

    fn maybe_invalidate_summary(&mut self, name: &str, args: &serde_json::Value) {
        if name != "write" && name != "patch" {
            return;
        }
        if let Some(s) = &mut self.session {
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                let rel = make_rel_path(p, &self.config.workspace);
                let _ = s.invalidate_file_summary(&rel);
            }
        }
    }

    async fn persist_turn(&mut self) {
        // Append the latest turn(s) to the on-disk full log (jsonl) for resume.
        if let Some(s) = &self.session {
            // Simple: append the last 1-2 messages as a small record.
            if let Some(last) = self.conversation.last() {
                let entry = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "role": last.role,
                    "content": last.content,
                    "has_tool_calls": last.tool_calls.is_some(),
                });
                let _ = s.append_log(&entry.to_string());
            }
            // Also refresh the rolling recent-turns summary occasionally
            // (lightweight; the prune path does heavier compression).
            if self.conversation.len() % 4 == 0 {
                let last_few: Vec<_> = self.conversation.iter().rev().take(4).cloned().collect();
                if !last_few.is_empty() {
                    let sm = self.summarize_messages(&last_few).await;
                    if let Some(s_mut) = &mut self.session {
                        // merge conservatively
                        let prev = &s_mut.meta.recent_turns_summary;
                        let merged = if prev.is_empty() { sm } else { format!("{}\n{}", prev, sm) };
                        let trimmed = if merged.len() > 1600 { safe_truncate(&merged, 1600).to_string() + "..." } else { merged };
                        let _ = s_mut.set_recent_turns_summary(&trimmed);
                    }
                }
            }
        }
    }

    /// Force-update the recent turns summary and persist session state.
    /// Called at the end of each turn (before sending Done to the UI) to
    /// ensure the session context is fresh for the next restart.
    pub async fn force_flush_session(&mut self) {
        if self.conversation.len() < 2 { return; }
        // Summarize the last several messages
        let recent: Vec<_> = self.conversation.iter().rev().take(6).cloned().collect();
        let sm = self.summarize_messages(&recent).await;
        if let Some(s) = &mut self.session {
            // Replace (not append) — keep it fresh and relevant
            let trimmed = if sm.len() > 1600 { safe_truncate(&sm, 1600).to_string() + "..." } else { sm };
            let _ = s.set_recent_turns_summary(&trimmed);
        }
    }
}

fn system_message(workspace: &std::path::Path) -> Message {
    let sys = format!(
r#"You are a sharp, practical coding agent running in a terminal-based agentic environment.

Workspace root: {}

## Core Loop (follow this every turn)
1. THINK — Understand the request. What do I already know? What files or information do I need?
2. ACT — Use the smallest number of tools possible. Prefer reading before writing.
3. REPORT — Clearly describe what actually happened based on tool output.

## Tool Discipline (critical)
- NEVER claim a file was read/written/edited unless a tool call just confirmed it.
- For any edit to existing code, prefer the `patch` tool over `write`. `patch` is safer and supports disambiguation via `near_line`.
- Before patching, call `read` (or `read` with a line range) so you have the exact text.
- Use `list` and `grep` heavily to explore the project.
- Use `exec` for building, testing, git, cargo, etc. Keep commands focused.
- `web_search` finds candidate pages. `browse` reads them. Use search → browse for research.
- If a tool fails, report the exact error and adapt. Do not pretend it succeeded.

## Available Tools
exec, read, write, patch, grep, list, web_search, browse, update_goal, record_discovery, read_summary, store_summary

read_summary and store_summary manage the mtime-matched file summary cache (see Context Management section below). update_goal and record_discovery update the persistent session meta (goal, tests, pitfalls, discoveries) that is injected into every prompt under "SESSION CONTEXT". Use the session tools when the user's request evolves or you learn something important.

## Output Style
- Be concise but complete.
- When you finish a meaningful chunk of work, give a short summary of changes + any commands the user should run next.
- Use markdown for code or file paths when helpful.

## Context Management (important for local models)
A rich, compact "SESSION CONTEXT" block (repo tree with sizes + ranked important files, current goal + achievement tests + pitfalls to avoid, key discoveries, and a summary of recent turns) is prepended to your system prompt on every turn. It comes from the persistent ~/.raven-hotel/ session for this workspace.

There is also a per-file summary cache (SQLite `context.db` in the session dir, keyed by relative path + mtime of the source file).

**Mandatory workflow to avoid getting stuck in read loops and to keep context small:**
1. Use the injected repo tree + "important_paths" to identify files worth looking at.
2. **Call read_summary(path) first** (never start with raw `read` for source files).
   - On "FRESH SUMMARY (mtime matched)": great, use the short version.
   - On "NO FRESH SUMMARY": you get the current mtime + a capped raw view. Analyze it, then **call store_summary(path, mtime=the_exact_number, summary="your concise factual summary")** right away so future turns get the cache hit.
3. Only fall back to raw `read` (ideally with a `lines="..."` range) when the summary is inadequate for the precise change you need to make.
4. `write` / `patch` automatically invalidate cached summaries for that path.

Also use `update_goal` (when intent shifts) and `record_discovery` for high-value facts.

You have access to the full workspace. You can run commands and modify files.

## Execution Style (critical — read carefully)
- **Keep calling tools until the task is actually done.** Do NOT stop to narrate plans or summarize progress mid-task. If there is more work to do, call the next tool immediately.
- Only stop calling tools when: (a) the goal is fully achieved and verified, or (b) you are genuinely blocked and need user input.
- When you ARE done, give a brief summary of what changed and any commands the user should run."#,
        workspace.display()
    );

    Message {
        role: "system".into(),
        content: Some(sys),
        tool_calls: None,
        tool_call_id: None,
    }
}

fn truncate_for_context(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        format!("{}...\n... ({} bytes total, truncated for context)", safe_truncate(s, limit), s.len())
    }
}
