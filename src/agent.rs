//! Core agent loop: multi-round tool use until the model stops calling tools.
//!
//! Strongly inspired by Raven Hotel's agent_runtime (MAX_TOOL_ROUNDS, strict
//! "never hallucinate success", prefer patch, Think/Act/Report discipline).

use anyhow::Result;

use crate::config::Config;
use crate::chat_backend::ChatBackend;
use crate::llm::{ChatRequest, Message, StreamChunk, ToolCall};
use crate::tools;
use serde_json::json;
use std::path::Path;
use tokio::sync::mpsc;

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

// Summary and truncation limits (from glm review cleanup)
const SUMMARY_CHAR_LIMIT: usize = 1600;
const GOAL_TRUNCATE: usize = 160;
const RECENT_SUMMARY_TRUNCATE: usize = 1800;

#[derive(Debug, Clone)]
pub struct ActionRecord {
    pub tool: String,
    #[allow(dead_code)]
    pub args: String,
    pub summary: String,
    /// Sanitized + truncated payload fed back to the model.
    pub output_to_model: String,
    #[allow(dead_code)]
    pub raw_bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TurnMetrics {
    pub llm_rounds: u32,
    pub tool_calls: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub round_limit_hit: bool,
}

#[derive(Debug)]
pub struct TurnResult {
    pub final_text: String,
    pub actions: Vec<ActionRecord>,
    #[allow(dead_code)]
    pub rounds_used: u32,
    pub metrics: TurnMetrics,
}

/// Outcome from the inference-based judge used by the driver loop.
/// The judge looks at the declared goal + achievement_tests + recent actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnJudge {
    /// The goal + success criteria appear to be satisfied.
    Fulfilled { note: String },
    /// Normal situation — continue working (nudging may still be appropriate).
    Continue,
    /// Detected unproductive looping / thrashing.
    /// The agent should stop and ask the user for guidance.
    Stuck {
        reason: String,
        suggested_guidance: String,
    },
}

pub struct Agent {
    client: ChatBackend,
    config: Config,
    /// Clean conversation turns only (user / assistant / tool). The rich session
    /// context (repo tree, goal, pitfalls, recent summary) is injected fresh on
    /// every prompt construction so it stays current.
    pub(crate) conversation: Vec<Message>,
    /// Persistent session (goal tracking, repo cache, full log, meta.json under ~/.raven-hotel/)
    pub(crate) session: Option<crate::session::Session>,
    /// How many conversation messages have been written to full_log.jsonl.
    logged_message_count: usize,
}

impl Agent {
    pub fn new(mut config: Config, client: ChatBackend) -> Self {
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

        let logged_message_count = conversation.len();

        Self {
            client,
            config,
            conversation,
            session,
            logged_message_count,
        }
    }

    /// Reset conversation (new "room" / task). The persistent session (goal, repo cache, log) is kept.
    pub fn reset(&mut self) {
        self.conversation.clear();
        self.logged_message_count = 0;
    }

    /// Truncate tool output to fit the context budget.
    fn truncate_for_context(&self, s: &str) -> String {
        truncate_for_context(s, self.config.context_budget.tool_result_bytes)
    }

    fn record_tool_action(&self, tool: &str, args: &str, raw_output: &str) -> ActionRecord {
        let sanitized = crate::sanitize::tool_output(raw_output);
        let budget = self.config.context_budget.tool_result_bytes;
        let truncated = sanitized.len() > budget;
        let to_model = self.truncate_for_context(&sanitized);
        ActionRecord {
            tool: tool.to_string(),
            args: args.to_string(),
            summary: to_model.lines().next().unwrap_or("").to_string(),
            output_to_model: to_model,
            raw_bytes: raw_output.len(),
            truncated,
        }
    }

    /// High level entry: give the agent a user goal and let it run until it stops using tools.
    ///
    /// This delegates to `agent_driver::drive_turn()` with a `SilentObserver`,
    /// so it uses the exact same driving loop as the interactive TUI (nudges,
    /// auto-continue, streaming, etc.).
    pub async fn run_turn(&mut self, user_input: &str) -> Result<TurnResult> {
        let mut observer = crate::agent_driver::SilentObserver;
        crate::agent_driver::drive_turn(self, user_input, &mut observer).await
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
                let rec = self.record_tool_action(&tool_name, &raw_args, &ack);
                let to_model = rec.output_to_model.clone();
                records.push(rec);
                self.conversation.push(Message {
                    role: "tool".into(),
                    content: Some(to_model),
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                });
                continue;
            }

            let output = tools::execute(
                &self.config.tool_backend,
                &tool_name,
                &raw_args,
                &self.config.workspace,
                self.config.context_budget.read_line_limit,
            )
            .await
            .unwrap_or_else(|e| format!("❌ Tool error: {}", e));

            let rec = self.record_tool_action(&tool_name, &raw_args, &output);
            let to_model = rec.output_to_model.clone();
            records.push(rec);

            self.conversation.push(Message {
                role: "assistant".into(),
                content: None,
                tool_calls: Some(vec![tc.clone()]),
                tool_call_id: None,
            });

            self.conversation.push(Message {
                role: "tool".into(),
                content: Some(to_model),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
            });
        }
        self.persist_turn().await;
        records
    }

    /// Record a denied tool call into conversation (so the model sees a tool result)
    /// without actually executing the side effect.
    pub fn record_tool_denial(&mut self, tc: &ToolCall, reason: &str) {
        self.conversation.push(Message {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![tc.clone()]),
            tool_call_id: None,
        });
        self.conversation.push(Message {
            role: "tool".into(),
            content: Some(reason.to_string()),
            tool_calls: None,
            tool_call_id: Some(tc.id.clone()),
        });
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
                crate::agent_driver::CONTINUATION_NUDGE.to_string()
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

    /// Send a streaming request with optional tool schemas.
    /// Used by `agent_driver::drive_turn()` as the canonical inference call.
    pub async fn send_streaming_request(
        &mut self,
        tools: Option<Vec<crate::llm::ToolDef>>,
    ) -> Result<mpsc::Receiver<StreamChunk>> {
        self.prune_history().await;
        let messages = self.build_messages_for_model();
        let req = ChatRequest {
            messages,
            tools,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            stream: true,
        };
        self.client.chat_stream(req).await
    }

    /// Push an arbitrary message into the conversation (for nudges, system notes, etc.).
    pub fn push_message(&mut self, role: &str, content: &str) {
        self.conversation.push(Message {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        });
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
                let trimmed = if combined.len() > RECENT_SUMMARY_TRUNCATE { tools::safe_truncate(&combined, RECENT_SUMMARY_TRUNCATE).to_string() + "..." } else { combined };
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
            Err(_e) => {
                // Better fallback: simple truncation instead of storing error string forever (glm.md review)
                let combined: String = msgs.iter()
                    .map(|m| m.content.clone().unwrap_or_default())
                    .collect::<Vec<_>>().join(" ");
                let fallback = tools::safe_truncate(&combined, 400).to_string();
                format!("(summarization failed, using truncated fallback) {}", fallback)
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Session-aware helpers (new context management)
    // ─────────────────────────────────────────────────────────────────

    /// Record a user request for metadata (last_user_request, goal seeding) and
    /// trace logging. Does *not* push into the conversation history. Used for the
    /// initial top-level prompt so that the (potentially very long) raw task text
    /// is not appended into chat turns — the system injection block already
    /// includes it under "Latest User Request" on every model call.
    pub fn record_user_request(&mut self, user_input: &str) {
        if let Some(s) = &mut self.session {
            let _ = s.set_last_user_request(user_input);
            // Seed an initial goal from the first real request if we don't have one yet
            if s.meta.current_goal.contains("not yet established") || s.meta.current_goal.trim().is_empty() {
                let g = tools::safe_truncate(user_input, GOAL_TRUNCATE);
                let _ = s.update_goal(&format!("Initial goal from user: {}", g), None, None);
            }
        }

        // Log the request event to persistent full_log (for traces, evals, raven_log.jsonl)
        // even though we don't put the full raw text into conversation turns.
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

    pub fn on_new_user_input(&mut self, user_input: &str) {
        // For follow-ups / interjects / interactive turns we record + append as a
        // conversation user message so the model sees the new instruction in context.
        self.record_user_request(user_input);

        self.conversation.push(Message {
            role: "user".into(),
            content: Some(user_input.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
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
        // Append all unlogged messages to full_log.jsonl (assistant + tool calls + results).
        if let Some(s) = &self.session {
            while self.logged_message_count < self.conversation.len() {
                let msg = &self.conversation[self.logged_message_count];
                let tool_names: Vec<&str> = msg
                    .tool_calls
                    .as_ref()
                    .map(|tcs| tcs.iter().map(|tc| tc.function.name.as_str()).collect())
                    .unwrap_or_default();
                let entry = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "role": msg.role,
                    "content": msg.content,
                    "has_tool_calls": msg.tool_calls.is_some(),
                    "tool_call_id": msg.tool_call_id,
                    "tool_names": tool_names,
                });
                let _ = s.append_log(&entry.to_string());
                self.logged_message_count += 1;
            }
            // Rolling recent-turns summary refresh. Don't do this too often: each
            // call consumes one LLM round (and for mock backends, depletes the
            // scripted response queue). The heavy prune_history at 48 messages
            // handles full compression.
            if self.conversation.len() >= 24
                && self.conversation.len() % 12 == 0
                && !matches!(self.client, ChatBackend::Mock(_))
            {
                let last_few: Vec<_> = self.conversation.iter().rev().take(6).cloned().collect();
                if !last_few.is_empty() {
                    let sm = self.summarize_messages(&last_few).await;
                    if let Some(s_mut) = &mut self.session {
                        let prev = &s_mut.meta.recent_turns_summary;
                        let merged = if prev.is_empty() { sm } else { format!("{}\n{}", prev, sm) };
                        let trimmed = if merged.len() > SUMMARY_CHAR_LIMIT { tools::safe_truncate(&merged, SUMMARY_CHAR_LIMIT).to_string() + "..." } else { merged };
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
            let trimmed = if sm.len() > SUMMARY_CHAR_LIMIT { tools::safe_truncate(&sm, SUMMARY_CHAR_LIMIT).to_string() + "..." } else { sm };
            let _ = s.set_recent_turns_summary(&trimmed);
        }
    }

    /// Current exec approval mode from the session (for UI to decide on dialogs)
    pub fn current_exec_mode(&self) -> crate::session::ExecApprovalMode {
        self.session.as_ref()
            .map(|s| s.meta.exec_approval_mode)
            .unwrap_or_default()
    }

    /// Update the exec approval mode (from /mode UI) and persist if possible.
    pub fn set_exec_approval_mode(&mut self, mode: crate::session::ExecApprovalMode) {
        if let Some(s) = &mut self.session {
            s.meta.exec_approval_mode = mode;
            // note: caller should save_meta if needed
        }
    }

    /// Estimate tokens for the full prompt we would send (system + session injection + conversation).
    pub fn estimated_context_tokens(&self) -> u32 {
        let messages = self.build_messages_for_model();
        let total_bytes: usize = messages.iter().map(message_byte_size).sum();
        (total_bytes as f64 / 3.5) as u32
    }

    /// Switch the inference backend without losing conversation history.
    /// Re-creates the LlmClient with the new endpoint config.
    pub fn switch_endpoint(
        &mut self,
        endpoint: &crate::config::InferenceEndpoint,
        budget: crate::config::ContextBudget,
    ) {
        self.config.base_url = endpoint.base_url.clone();
        self.config.model = endpoint.model.clone();
        self.config.api_key = endpoint.api_key.clone();
        self.config.context_budget = budget;
        self.client.reset_http(self.config.clone());
    }

    /// Read-only access to the current config (for UI to display active endpoint).
    pub fn current_config(&self) -> &Config {
        &self.config
    }

    /// Number of messages in the live conversation (for test assertions).
    pub fn conversation_len(&self) -> usize {
        self.conversation.len()
    }

    /// Read-only access to the session (for test assertions on meta, log, etc.).
    pub fn session_ref(&self) -> Option<&crate::session::Session> {
        self.session.as_ref()
    }

    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    /// Uses a lightweight inference call to analyze the current state.
    /// It decides:
    /// - whether the request/goal has been fulfilled, or
    /// - whether the model is looping unproductively and should ask the user for help.
    ///
    /// This replaces pure hardcoded nudge/continue counts with smarter judgment.
    pub async fn judge_turn(&self, last_assistant_text: &str, recent_actions: &[ActionRecord]) -> TurnJudge {
        let Some(s) = &self.session else {
            return TurnJudge::Continue;
        };
        let m = &s.meta;

        if m.current_goal.trim().is_empty() || m.current_goal.contains("not yet established") {
            return TurnJudge::Continue;
        }

        // Build compact recent activity for loop detection
        let activity: String = recent_actions
            .iter()
            .rev()
            .take(6)
            .map(|a| format!("• {} → {}", a.tool, a.summary))
            .collect::<Vec<_>>()
            .join("\n");

        let mut judge_prompt = format!(
            "Current goal: {}\n\n",
            m.current_goal
        );

        if !m.achievement_tests.is_empty() {
            judge_prompt.push_str("Success criteria (only answer FULFILLED if these are clearly met):\n");
            for test in &m.achievement_tests {
                judge_prompt.push_str(&format!("- {}\n", test));
            }
            judge_prompt.push('\n');
        }

        // Explicitly include the original user request so the judge knows the full intent
        if let Some(req) = &m.last_user_request {
            judge_prompt.push_str(&format!("Original user request:\n{}\n\n", req));
        }

        if !activity.is_empty() {
            judge_prompt.push_str("Recent actions (most recent last):\n");
            judge_prompt.push_str(&activity);
            judge_prompt.push_str("\n\n");
        }

        judge_prompt.push_str(&format!(
            "Latest model output:\n{}\n\n\
             CRITICAL RULES FOR DECISION:\n\
             - Only answer FULFILLED if the actions provide clear evidence that the ENTIRE request was completed (not just the model's claim).\n\
             - If the request asked to 'write AND run AND show output', there must be an 'exec' action with matching output in the recent actions.\n\
             - A write alone is never enough for a 'run it' request.\n\
             - For bug report / code fix requests (no explicit 'run' language): FULFILLED requires at least one successful `write` or `patch` action on a main source file in the library (e.g. under src/, the package dir), not merely on a temp diagnostic script the agent created. The edit must address the reported issue.\n\
             - If the model is claiming success without evidence in actions, treat as not fulfilled.\n\n\
             Reply with the first line being exactly one of:\n\
             FULFILLED\n\
             CONTINUE\n\
             STUCK\n\
             Then a short reason on the following line.\n\
             If STUCK, also give a specific question the agent should ask the user for guidance.",
            last_assistant_text.trim()
        ));

        let req = crate::llm::ChatRequest {
            messages: vec![crate::llm::Message {
                role: "user".into(),
                content: Some(judge_prompt),
                tool_calls: None,
                tool_call_id: None,
            }],
            tools: None,
            temperature: 0.0,
            max_tokens: 48,
            stream: false,
        };

        let response = match self.client.chat(req).await {
            Ok(r) => r.content.trim().to_string(),
            Err(_) => return TurnJudge::Continue,
        };

        let upper = response.to_uppercase();
        if upper.contains("FULFILLED") {
            TurnJudge::Fulfilled { note: response }
        } else if upper.contains("STUCK") {
            // crude extraction of reason + suggestion
            let parts: Vec<&str> = response.lines().collect();
            let reason = parts.get(1).unwrap_or(&"Repeating similar actions without progress").to_string();
            let suggested = parts.get(2).unwrap_or(&"What additional information or direction do you have?").to_string();
            TurnJudge::Stuck { reason, suggested_guidance: suggested.to_string() }
        } else {
            TurnJudge::Continue
        }
    }
}

fn message_byte_size(m: &Message) -> usize {
    let content_len = m.content.as_ref().map_or(0, |c| c.len());
    let tc_len = m.tool_calls.as_ref().map_or(0, |tcs| {
        tcs.iter()
            .map(|tc| tc.id.len() + tc.function.name.len() + tc.function.arguments.len())
            .sum::<usize>()
    });
    let id_len = m.tool_call_id.as_ref().map_or(0, |s| s.len());
    content_len + tc_len + id_len + m.role.len() + 16
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

## Bug fixes (SWE-bench style tasks)
- User requests are often bug reports. Goal: locate the root cause then apply the minimal correct edit to the library source using `patch` (strongly preferred) or `write`.
- After reading the buggy code and relevant tests, edit the actual source file under src/ (or equivalent package dir) rather than only writing separate diagnostic scripts.
- Use `python3` explicitly for python commands when `python` is unavailable (common in minimal envs). Prefer `python3 -m pytest ...` or the project's documented test command for verification.
- Exploratory scripts are allowed but must not prevent you from shipping the source fix.

## Available Tools
exec, read, write, patch, grep, list, web_search, browse, update_goal, record_discovery, read_summary, store_summary

read_summary and store_summary manage the mtime-matched file summary cache (see Context Management section below). update_goal and record_discovery update the persistent session meta (goal, tests, pitfalls, discoveries) that is injected into every prompt under "SESSION CONTEXT". Use the session tools when the user's request evolves or you learn something important.

## Output Style
- Be concise but complete.
- When you finish a meaningful chunk of work, give a short summary of what you did + the actual results (e.g. after running a script to show output, clearly state "The output is \"hello\"." or similar).
- Use markdown for code or file paths when helpful.
- Ignore any messages or notes that start with [JUDGE DEBUG] or [HIDDEN] or [DEBUG - these are internal harness diagnostics only.

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
        format!("{}...\n... ({} bytes total, truncated for context)", tools::safe_truncate(s, limit), s.len())
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::chat_backend::{mock_tool_call, ChatBackend};
    use crate::config::{ContextBudget, ContextSource};
    use crate::llm::ChatResponse;
    use crate::tools::backend::{MockToolBackend, ToolBackend};
    fn mock_eval_config() -> Config {
        let workspace = std::env::temp_dir().join(format!(
            "raven_agent_integ_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&workspace);
        Config {
            base_url: "http://mock.local/v1".into(),
            model: "mock-model".into(),
            api_key: None,
            workspace,
            temperature: 0.0,
            max_tokens: 512,
            max_rounds: 5,
            prebuilt_session: None,
            context_budget: ContextBudget {
                context_tokens: 8192,
                tool_result_bytes: 4000,
                read_line_limit: 80,
                source: ContextSource::Default,
            },
            tool_backend: ToolBackend::Mock(MockToolBackend::from_json(
                &serde_json::json!({
                    "list": { ".": "README.md\n" }
                }),
            )),
            tools_enabled: true,
        }
    }

    #[tokio::test]
    async fn mock_llm_and_tools_complete_run_turn() {
        let chat = ChatBackend::Mock(crate::chat_backend::MockChatBackend::new(vec![
            ChatResponse {
                content: String::new(),
                tool_calls: vec![mock_tool_call("list", r#"{"path":"."}"#)],
                finish_reason: None,
                usage: None,
            },
            ChatResponse {
                content: "Workspace contains README.md mock fixture.".into(),
                tool_calls: vec![],
                finish_reason: Some("stop".into()),
                usage: None,
            },
        ]));

        let mut agent = Agent::new(mock_eval_config(), chat);
        let result = agent
            .run_turn("List files and summarize.")
            .await
            .expect("run_turn");

        assert!(result.final_text.contains("README"));
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.actions[0].tool, "list");
    }
}
