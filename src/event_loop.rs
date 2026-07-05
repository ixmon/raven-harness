//! Event loop and main run orchestration (extracted per refactor.md).
//!
//! This module owns the TUI event loop, terminal handling, UiUpdate processing,
//! drawing coordination, and TuiObserver integration with the agent driver.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{backend::CrosstermBackend, layout::{Constraint, Direction, Layout}, Terminal};
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::app_state::{App, Pane};
use crate::tui_render;
use raven_tui::agent::{ActionRecord, Agent};
use raven_tui::agent_driver::TurnObserver;
use raven_tui::chat_backend::ChatBackend;
use raven_tui::config::Config;
use crate::keystore::Keystore;
use raven_tui::llm::ToolCall;
use raven_tui::session::ExecApprovalMode;

use crate::input_handler;

// ── UiUpdate + helpers ───────────────────────────────────────────────────────

#[derive(Debug)]
pub enum UiUpdate {
    Token(String),
    Thinking(String),
    ToolStart { name: String, args: String },
    ToolResult { name: String, summary: String },
    RoundLimitHit {
        #[allow(dead_code)]
        continuation: u32,
        #[allow(dead_code)]
        max_continuations: u32,
        #[allow(dead_code)]
        exhausted: bool,
    },
    Done { final_text: String },
    Error(String),
    ApprovalRequested,
    ContextUsage { used_tokens: u32 },
    InterjectRestart,
    #[allow(dead_code)]
    SuperJudgeBegin,
    Usage {
        prompt_tokens: Option<u32>,
        completion_tokens: Option<u32>,
        total_tokens: Option<u32>,
    },
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) { end -= 1; }
        format!("{}...", &s[..end])
    }
}

/// The exact filtering we apply to a final_text (which may come from a tool
/// response / model content that contains raw XML tool call syntax) before
/// committing it to the conversation pane (`left_committed`).
///
/// This is the "circumstance" we must always hit: even if the strip function
/// itself is correct, we have to *use* it here (and not fall back to raw when
/// the result is empty).
fn clean_final_text_for_pane(final_text: &str) -> Option<String> {
    let cleaned = raven_tui::llm::strip_xml_tool_call_blocks(final_text);
    if cleaned.trim().is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

// ── TuiObserver ──────────────────────────────────────────────────────────────

pub struct TuiObserver {
    pub(crate) tx: mpsc::Sender<UiUpdate>,
    pub(crate) approval_req_tx: mpsc::Sender<(String, oneshot::Sender<bool>)>,
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) queued: Arc<Mutex<Option<String>>>,
    pub(crate) instant: Arc<Mutex<Option<String>>>,
    pub(crate) denials_this_turn: u32,
    pub(crate) halt_tools: bool,
    pub(crate) exec_mode: ExecApprovalMode,
}

#[async_trait]
impl TurnObserver for TuiObserver {
    fn on_token(&mut self, t: &str) { let _ = self.tx.try_send(UiUpdate::Token(t.to_string())); }
    fn on_thinking(&mut self, t: &str) { let _ = self.tx.try_send(UiUpdate::Thinking(t.to_string())); }
    fn on_tool_start(&mut self, name: &str, args: &str) {
        let _ = self.tx.try_send(UiUpdate::ToolStart { name: name.to_string(), args: args.to_string() });
    }
    fn on_tool_result(&mut self, record: &ActionRecord) {
        let _ = self.tx.try_send(UiUpdate::ToolResult { name: record.tool.clone(), summary: record.summary.clone() });
    }
    fn on_nudge(&mut self, count: u32, max: u32) {
        let _ = self.tx.try_send(UiUpdate::ToolResult {
            name: "system".into(),
            summary: format!("Nudging agent to continue (text-only pause {}/{})", count, max),
        });
    }
    fn on_round_limit(&mut self, continuation: u32, max: u32, exhausted: bool) {
        let _ = self.tx.try_send(UiUpdate::RoundLimitHit { continuation, max_continuations: max, exhausted });
    }
    fn on_stuck(&mut self, reason: &str, suggested: &str) {
        let _ = self.tx.try_send(UiUpdate::ToolResult {
            name: "system".into(),
            summary: format!("⚠ Agent looping: {}. Ask user: {}", reason, suggested),
        });
    }
    fn on_context_usage(&mut self, tokens: u32) {
        let _ = self.tx.try_send(UiUpdate::ContextUsage { used_tokens: tokens });
    }
    fn should_stop(&self) -> bool { self.stop.load(Ordering::SeqCst) }
    fn on_interject(&mut self, _msg: &str) { let _ = self.tx.try_send(UiUpdate::InterjectRestart); }
    fn take_interject(&mut self) -> Option<String> {
        if let Ok(mut guard) = self.instant.lock() {
            if let Some(msg) = guard.take() {
                if !msg.trim().is_empty() { self.stop.store(false, Ordering::SeqCst); return Some(msg); }
            }
        }
        if let Ok(mut guard) = self.queued.lock() {
            if let Some(msg) = guard.take() {
                if !msg.trim().is_empty() { self.stop.store(false, Ordering::SeqCst); return Some(msg); }
            }
        }
        None
    }
    async fn approve_tool(&mut self, tc: &ToolCall) -> bool {
        let name = &tc.function.name;
        let args = &tc.function.arguments;
        if !self.needs_approval(name, args) { return true; }
        let desc = Self::build_approval_description(name, args);
        let (resp_tx, resp_rx) = oneshot::channel::<bool>();
        let _ = self.approval_req_tx.send((desc.clone(), resp_tx)).await;
        let _ = self.tx.send(UiUpdate::ApprovalRequested).await;
        match resp_rx.await {
            Ok(true) => true,
            _ => {
                let deny = format!("DENIED: The user refused to approve this {} action. Do NOT retry the same action.", name);
                self.denials_this_turn += 1;
                let _ = self.tx.send(UiUpdate::ToolResult { name: name.clone(), summary: deny.clone() }).await;
                if self.denials_this_turn >= 3 {
                    let _ = self.tx.send(UiUpdate::ToolResult {
                        name: "system".into(),
                        summary: "3 actions denied this turn — stopping tool loop.".into(),
                    }).await;
                    self.halt_tools = true;
                }
                false
            }
        }
    }
    fn stop_tool_processing(&self) -> bool { self.halt_tools }
}

impl TuiObserver {
    fn needs_approval(&self, name: &str, args: &str) -> bool {
        // Wiki writes are always safe — private session scratchpad, never touches workspace
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
            if v.get("wiki").and_then(|w| w.as_bool()).unwrap_or(false) {
                return false;
            }
        }
        let is_mutating = matches!(name, "write" | "patch" | "exec");
        let is_outside = if name == "exec" {
            let cmd = serde_json::from_str::<serde_json::Value>(args).ok()
                .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(|s| s.to_owned()))
                .unwrap_or_default();
            cmd.contains("cd /") || cmd.contains("/etc") || cmd.contains("/root") || cmd.contains("curl ") || cmd.contains("wget ") || cmd.contains("nc ")
        } else { false };
        match self.exec_mode {
            ExecApprovalMode::Babysitter => is_mutating,
            ExecApprovalMode::SpringBreak => false,
            ExecApprovalMode::Vegas => name == "exec" && is_outside,
            ExecApprovalMode::Thunderdome => false,
        }
    }
    fn build_approval_description(name: &str, args: &str) -> String {
        match name {
            "write" => {
                let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
                let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
                let n = v.get("content").and_then(|c| c.as_str()).map(|s| s.len()).unwrap_or(0);
                format!("write {} ({} bytes)", path, n)
            }
            "patch" => {
                let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
                let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
                format!("patch {}", path)
            }
            "exec" => {
                let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
                let cmd = v.get("command").and_then(|c| c.as_str()).unwrap_or("");
                format!("exec: {}", truncate(cmd, 72))
            }
            "update_goal" => {
                let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
                let goal = v.get("goal").and_then(|g| g.as_str()).map(|s| s.to_string()).unwrap_or_else(|| args.to_string());
                format!("update_goal: {}", truncate(&goal, 80))
            }
            other => format!("{} (args omitted)", other),
        }
    }
}

// ── Main entry points ────────────────────────────────────────────────────────

pub async fn run(
    config: Config,
    chat_backend: ChatBackend,
    keystore: Keystore,
) -> Result<()> {
    // Install a panic hook that restores the terminal before the default
    // panic handler runs. This prevents raw panic messages (and any other
    // stderr from worker threads) from corrupting the TUI display.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Best-effort terminal restore (may be called from any thread).
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stderr(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show,
        );
        // Then let the original handler (which does the eprintln + backtrace) run.
        original_hook(panic_info);
    }));

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
    )?;
    let backend = CrosstermBackend::new(stdout);
    let backend = crate::palette::PaletteBackend::new(backend);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let res = run_app(&mut terminal, config, chat_backend, keystore).await;
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut().inner_mut(),
        crossterm::event::DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    if let Err(err) = res {
        // TUI loop exited with error. Avoid polluting if possible, but this is shutdown path.
        eprintln!("TUI error: {:?}", err);
    }
    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    config: Config,
    chat_backend: ChatBackend,
    mut keystore: Keystore,
) -> Result<()> {
    let agent = Arc::new(tokio::sync::Mutex::new(Agent::new(config.clone(), chat_backend)));
    // Set Brave Search API key from keystore (or BRAVE_API_KEY env var)
    {
        let brave_key = keystore.get_brave_key();
        agent.lock().await.brave_key = brave_key;
    }
    let mut app = App::new(&config);

    // On (re)start, prepopulate the conversation pane from the prebuilt/default
    // session's log so previous work is visible immediately in the UI panes.
    {
        if let Ok(ag) = agent.try_lock() {
            if let Some(s) = ag.session() {
                if !s.id.is_empty() {
                    let recent = s.load_recent_conversation(25);
                    if !recent.is_empty() {
                        app.left_committed.clear();
                        for (role, content) in recent {
                            let disp = if role == "user" {
                                format!("> {}", content)
                            } else {
                                raven_tui::llm::strip_xml_tool_call_blocks(&content)
                            };
                            if !disp.trim().is_empty() {
                                app.left_committed.push(disp);
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(pfile) = &config.harness.initial_prompt_file {
        if let Ok(text) = std::fs::read_to_string(pfile) {
            let text = text.trim().to_string();
            if !text.is_empty() {
                app.input = text;
                app.cursor_pos = app.input.len();
                app.needs_redraw = true;
            }
        }
    }

    let (tx, mut rx) = mpsc::channel::<UiUpdate>(64);
    let (input_tx, mut input_rx) = mpsc::channel::<Event>(64);
    let (approval_req_tx, mut approval_req_rx) = mpsc::channel::<(String, oneshot::Sender<bool>)>(4);
    let stop_signal = Arc::new(AtomicBool::new(false));
    let queued_interject: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let instant_interject: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let (balance_tx, mut balance_rx) = mpsc::channel::<String>(4);

    {
        let agent_bal = agent.clone();
        let btx = balance_tx.clone();
        tokio::spawn(async move {
            loop {
                refresh_balance_label(&agent_bal, &btx).await;
                tokio::time::sleep(Duration::from_secs(600)).await;
            }
        });
    }

    let _input_handle = std::thread::spawn(move || {
        loop {
            if event::poll(Duration::from_millis(10)).unwrap_or(false) {
                if let Ok(ev) = event::read() {
                    if input_tx.blocking_send(ev).is_err() { break; }
                }
            }
        }
    });

    let _observer = TuiObserver {
        tx: tx.clone(),
        approval_req_tx: approval_req_tx.clone(),
        stop: stop_signal.clone(),
        queued: queued_interject.clone(),
        instant: instant_interject.clone(),
        denials_this_turn: 0,
        halt_tools: false,
        exec_mode: agent.lock().await.current_exec_mode(),
    };

    app.needs_redraw = true;

    // Main event loop
    loop {
        // Process updates FIRST (so tool calls, thinking, etc. are in trace_lines before draw)
        while let Ok(update) = rx.try_recv() {
            match update {
                UiUpdate::Token(t) => {
                    let now = std::time::Instant::now();
                    if let Some(last) = app.last_token_time {
                        let delta = now.duration_since(last).as_secs_f64();
                        if delta < 1.5 {
                            app.generation_active_time += delta;
                        }
                    }
                    app.last_token_time = Some(now);
                    app.tokens_processed += 1;
                    if app.generation_active_time > 0.0 {
                        app.tps = app.tokens_processed as f64 / app.generation_active_time;
                        app.api_tps = app.tps;
                    }
                    app.current_response.push_str(&t);
                    // Re-apply XML tool call stripping to the live display buffer as a
                    // belt-and-suspenders so that tool XML never appears in the conversation pane.
                    app.current_response = raven_tui::llm::strip_xml_tool_call_blocks(&app.current_response);
                    app.needs_redraw = true;
                    app.left_follow_output = true;
                    app.left_scroll = 10_000;
                }
                UiUpdate::Thinking(t) => {
                    let now = std::time::Instant::now();
                    if let Some(last) = app.last_token_time {
                        let delta = now.duration_since(last).as_secs_f64();
                        if delta < 1.5 {
                            app.generation_active_time += delta;
                        }
                    }
                    app.last_token_time = Some(now);
                    app.tokens_processed += 1;
                    if app.generation_active_time > 0.0 {
                        app.tps = app.tokens_processed as f64 / app.generation_active_time;
                        app.api_tps = app.tps;
                    }
                    app.current_thinking.push_str(&t);
                    app.needs_redraw = true;
                    app.right_follow_output = true;
                    app.right_scroll = 10_000;
                }
                UiUpdate::ToolStart { name, args } => {
                    // Always ensure tool debug lines start with the tool call icon at the front.
                    let tool_debug = format!("🔧 {}({})", name, truncate(&args, 90));
                    app.trace_lines.push(tool_debug);
                    app.tool_calls_this_turn += 1;
                    app.right_follow_output = true;
                    app.right_scroll = 10_000;
                    app.needs_redraw = true;
                }
                UiUpdate::ToolResult { name, summary } => {
                    if name == "system" && summary.contains("JUDGE") {
                        app.trace_lines.push(format!("   ⭐⭐ JUDGE: {}", truncate(&summary, 140)));
                    } else if name == "system" {
                        // system debug notes (nudges, stuck, denials etc.) keep a 🔧 marker
                        app.trace_lines.push(format!("🔧 {}", truncate(&summary, 120)));
                    } else {
                        // Real tool result: use indented continuation (↳). Combined with the
                        // preceding ToolStart line, this puts a *single* 🔧 icon on the first
                        // line of the tool call block/output. No brain icons on tool lines.
                        app.trace_lines.push(format!("   ↳ {}", truncate(&summary, 120)));
                    }
                    // Advance plan progress on WORK_COMPLETE (or similar strong completion signals)
                    if app.plan.active && !app.plan.steps.is_empty() {
                        app.plan.advance_on_tool_result(&summary);
                        app.needs_redraw = true;
                    }
                    app.right_follow_output = true;
                    app.right_scroll = 10_000;
                    app.needs_redraw = true;
                }
                UiUpdate::Done { final_text } => {
                    // Use the component's filter (which applies strip and never falls back
                    // to raw XML). This is the key circumstance we test.
                    if let Some(to_commit) = clean_final_text_for_pane(&final_text) {
                        app.left_committed.push(to_commit);
                    }
                    if !app.current_thinking.is_empty() {
                        // Settle the thinking: 1 brain icon per thought block (not per line),
                        // 1 tool icon at start of tool blocks. Subsequent lines of a thought
                        // are indented without repeating the icon.
                        let lines: Vec<&str> = app.current_thinking.lines().collect();
                        if !lines.is_empty() {
                            let first = lines[0];
                            let l = first.trim_start();
                            let first_settled = if l.starts_with("🔧") || l.starts_with("   ↳") || l.starts_with("⭐") {
                                first.to_string()
                            } else {
                                format!("🧠 {}", first)
                            };
                            app.trace_lines.push(first_settled);

                            for line in &lines[1..] {
                                let l = line.trim_start();
                                let settled = if l.starts_with("🔧") || l.starts_with("   ↳") || l.starts_with("⭐") {
                                    line.to_string()
                                } else {
                                    format!("   {}", line)
                                };
                                app.trace_lines.push(settled);
                            }
                        }
                        app.current_thinking.clear();
                    }
                    app.current_response.clear();
                    app.is_processing = false;
                    // If stop was set (e.g. by Esc to abort a turn), reset it so the main
                    // loop doesn't exit the app. The stop flag is used both for aborting
                    // the current agent turn and for quitting the whole TUI.
                    if stop_signal.load(Ordering::SeqCst) {
                        stop_signal.store(false, Ordering::SeqCst);
                    }
                    // Advance plan progress on turn completion
                    if app.plan.active && !app.plan.steps.is_empty() {
                        app.plan.advance_on_turn_done(&final_text);
                        app.needs_redraw = true;
                    }
                    app.needs_redraw = true;
                }
                UiUpdate::Error(e) => {
                    app.left_committed.push(format!("Error: {}", e));
                    app.is_processing = false;
                    if stop_signal.load(Ordering::SeqCst) {
                        stop_signal.store(false, Ordering::SeqCst);
                    }
                    app.needs_redraw = true;
                }
                UiUpdate::ContextUsage { used_tokens } => {
                    app.ctx_used_tokens = used_tokens;
                    app.needs_redraw = true;
                }
                UiUpdate::ApprovalRequested => {
                    app.needs_redraw = true;
                }
                UiUpdate::Usage { prompt_tokens, completion_tokens, total_tokens } => {
                    app.api_prompt_tokens = prompt_tokens;
                    app.api_completion_tokens = completion_tokens;
                    app.api_total_tokens = total_tokens;
                    app.needs_redraw = true;
                }
                UiUpdate::SuperJudgeBegin => {
                    app.is_processing = true;
                    app.trace_lines.push("🔍 Super Judge review starting…".to_string());
                    app.right_follow_output = true;
                    app.right_scroll = 10_000;
                    app.needs_redraw = true;
                }
                _ => {}
            }
        }

        // Balance updates
        while let Ok(label) = balance_rx.try_recv() {
            app.balance_label = label;
            app.needs_redraw = true;
        }

        // Tool approval requests from TuiObserver::approve_tool().
        // The observer sends (description, oneshot::Sender<bool>) when a tool needs
        // user approval. We store them so the UI can show the popup and the key
        // handler can respond with Y/N.
        while let Ok((desc, responder)) = approval_req_rx.try_recv() {
            app.pending_approval = Some(desc);
            app.approval_responder = Some(responder);
            app.needs_redraw = true;
        }

        // Defensive cleanup: never allow brain icon prepended to tool call/debug lines
        // (🔧 start or indented ↳ result). Ensures single tool icon at start of each tool block.
        for line in &mut app.trace_lines {
            if line.starts_with("🧠") {
                let rest = line.trim_start_matches("🧠").trim_start();
                if rest.starts_with("🔧") || rest.starts_with("   ↳") {
                    *line = rest.to_string();
                }
            }
        }

        if app.is_processing && !stop_signal.load(Ordering::SeqCst) {
            app.spinner_tick = app.spinner_tick.wrapping_add(1);
        }
        if app.plan.active {
            app.plan.spinner_tick = app.plan.spinner_tick.wrapping_add(1);
            app.needs_redraw = true;
        }

        if (matches!(app.desktop.active, crate::desktop::ActiveDesktop::Picker)
            || matches!(app.desktop.active, crate::desktop::ActiveDesktop::Splash)
            || matches!(app.desktop.active, crate::desktop::ActiveDesktop::Overview))
            && !app.picker.loaded
        {
            app.refresh_picker();
        }

        // Labels
        let (approval_label, goal_text, agent_mode) = if let Ok(ag) = agent.try_lock() {
            let approval = ag.current_exec_mode().label().to_string();
            let goal = if config.flags.goal_tracking {
                ag.session().as_ref().and_then(|s| {
                    let g = s.meta.current_goal.as_str();
                    if g.is_empty() { None } else { Some(g.to_string()) }
                }).unwrap_or_else(|| "(no goal set)".into())
            } else { "none".into() };
            let amode = ag.current_agent_mode();
            (approval, goal, amode)
        } else {
            (app.cached_mode_label.clone(), app.cached_goal_text.clone(), app.cached_agent_mode.clone())
        };
        app.cached_mode_label = approval_label.clone();
        app.cached_goal_text = goal_text.clone();
        app.cached_agent_mode = agent_mode.clone();

        // Sync plan pane high-level fields from session (agent uses update_goal during clarification)
        if agent_mode == "plan" {
            if let Ok(ag) = agent.try_lock() {
                if let Some(s) = ag.session() {
                    let m = &s.meta;
                    if !m.current_goal.is_empty() {
                        app.plan.goal = m.current_goal.clone();
                    }
                    if !m.achievement_tests.is_empty() {
                        app.plan.success_criteria = m.achievement_tests.join(" | ");
                    }
                }
            }
        }

        // Auto-activate plan pane when run mode is "plan" (collection phase)
        // Do NOT pre-populate steps here -- steps appear only after user approves the plan
        if agent_mode == "plan" {
            if !app.plan.active {
                app.plan.active = true;
                if app.plan.goal.is_empty() {
                    // Do not set any boilerplate goal here. The goal must come from the user's
                    // actual planning request captured in the trigger (see input_handler).
                    // Only set safe defaults for the other plan fields if needed.
                    if app.plan.success_criteria.is_empty() {
                        app.plan.success_criteria = "Verification passes and goal achieved".to_string();
                    }
                    if app.plan.verification_steps.is_empty() {
                        let g = app.plan.goal.to_lowercase();
                        if g.contains("python") || g.contains(".py") {
                            app.plan.verification_steps = vec!["python3 <script>.py".into(), "check script output".into()];
                        } else if g.contains("c++") || g.contains("cpp") || g.contains("g++") || g.contains("clang") {
                            app.plan.verification_steps = vec![
                                "g++ -std=c++17 -Wall -o program program.cpp".into(),
                                "clang-tidy program.cpp -- -std=c++17".into(),
                                "./program".into(),
                            ];
                        } else {
                            app.plan.verification_steps = vec!["cargo check".into(), "cargo clippy -- -D warnings".into(), "cargo test".into()];
                        }
                    }
                    if app.plan.rollback.is_empty() {
                        app.plan.rollback = "git branch + checkpoints".to_string();
                    }
                    if app.plan.constraints.is_empty() {
                        app.plan.constraints = "Stay on feature branch; keep changes reviewable".to_string();
                    }
                    app.plan.steps.clear();
                    app.plan.current_step = 0;
                }
            }
            app.plan.spinner_tick = app.plan.spinner_tick.wrapping_add(1);
        } else if app.plan.active && agent_mode != "plan" && app.plan.steps.is_empty() {
            // only auto-hide the pane if we switch away *before* the plan was approved (no steps yet)
            app.plan.active = false;
        }

        let workspace_display = config.workspace.display().to_string();
        let show_gauge = app.is_processing && !stop_signal.load(Ordering::SeqCst);
        let gauge_h = if show_gauge { 1 } else { 0 };
        let overview_harness = matches!(app.desktop.active, crate::desktop::ActiveDesktop::Overview)
            && app.browser_nav_items.get(app.browser_selected_nav)
                .map(|it| it.kind == crate::app_state::NavItemKind::Harness).unwrap_or(false);
        let show_status = matches!(app.desktop.active, crate::desktop::ActiveDesktop::Workspace);
        let show_input = show_status
            || (matches!(app.desktop.active, crate::desktop::ActiveDesktop::Picker) && app.picker.adding_workspace);
        // Overview harness shows status + input + conv to right of nav (Coding Harness selected)
        let status_h = if show_status { 1 } else { 0 };
        let input_line_count = if show_input { app.input.lines().count().max(1) as u16 } else { 0 };
        let input_h = if show_input {
            (input_line_count + 2).clamp(3, 8)
        } else {
            0
        };

        if matches!(app.desktop.active, crate::desktop::ActiveDesktop::Picker)
            || matches!(app.desktop.active, crate::desktop::ActiveDesktop::Splash)
            || matches!(app.desktop.active, crate::desktop::ActiveDesktop::Overview)
        {
            // rough estimate; updated inside draw if possible
            app.picker.last_summary_height = 25;
        }

        // Draw
        if app.needs_redraw || app.is_processing || app.scroll_flash_timer > 0 || app.desktop.is_animating() {
            app.needs_redraw = false;

            let search_label = match app.desktop.active {
                crate::desktop::ActiveDesktop::Splash => "→ picker".to_string(),
                crate::desktop::ActiveDesktop::Picker => "← splash".to_string(),
                crate::desktop::ActiveDesktop::Overview => "← splash / nav".to_string(),
                _ => app.search.status_label(),
            };

            terminal.draw(|f| {
                let size = f.area();
                // Ensure a consistent black background on all screens (picker, workspace,
                // wiki viewer, splash, etc.). Individual panes reinforce with their blocks.
                f.render_widget(
                    ratatui::widgets::Block::default()
                        .style(ratatui::style::Style::default().bg(ratatui::style::Color::Black)),
                    size,
                );
                let vertical = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(status_h), Constraint::Min(6), Constraint::Length(gauge_h), Constraint::Length(input_h)])
                    .split(size);

                let status_area = vertical[0];
                let content_area = vertical[1];
                let gauge_area = vertical[2];
                let input_area = vertical[3];

                if matches!(app.desktop.active, crate::desktop::ActiveDesktop::Picker) {
                    app.picker.last_summary_height = content_area.height.saturating_sub(4).max(10);
                } else if matches!(app.desktop.active, crate::desktop::ActiveDesktop::Splash) {
                    // lower half summary area
                    let lower_h = (content_area.height as f32 * 0.5) as u16;
                    app.picker.last_summary_height = lower_h.saturating_sub(4).max(5);
                } else if matches!(app.desktop.active, crate::desktop::ActiveDesktop::Overview) {
                    app.picker.last_summary_height = content_area.height.saturating_sub(4).max(8);
                }

                let left_focused = app.focused_pane == Pane::Left;
                let right_focused = app.focused_pane == Pane::Right;

                let left_highlight = if app.search.active && app.search.pane == tui_render::Pane::Left && !app.search.match_lines.is_empty() {
                    Some(app.search.match_lines[app.search.match_idx])
                } else { None };
                let right_highlight = if app.search.active && app.search.pane == tui_render::Pane::Right && !app.search.match_lines.is_empty() {
                    Some(app.search.match_lines[app.search.match_idx])
                } else { None };

                if show_status {
                    tui_render::draw_status_bar(f, status_area, &tui_render::StatusBarData {
                        display_model: &app.display_model,
                        balance_label: &app.balance_label,
                        ctx_used_tokens: app.ctx_used_tokens,
                        budget: &app.display_budget,
                        mode_label: &approval_label,
                        agent_mode: &agent_mode,
                        goal_text: &goal_text,
                        search_label: &search_label,
                        tps: app.api_tps,
                    });
                }

                if app.desktop.showing_wiki_viewer() {
                    if app.wiki_viewer.selected_is_harness() {
                        // Nav (left) + conversation (right of nav); status top + input bottom enabled outer
                        let cols = ratatui::layout::Layout::default()
                            .direction(ratatui::layout::Direction::Horizontal)
                            .constraints([ratatui::layout::Constraint::Percentage(30), ratatui::layout::Constraint::Percentage(70)])
                            .split(content_area);
                        tui_render::draw_wiki_nav_pane(f, cols[0], &app.wiki_viewer, true);
                        let mut dummy_rect = ratatui::layout::Rect::default();
                        let mut dummy_cnt = 0u16;
                        tui_render::draw_left_pane(
                            f,
                            &app.left_committed,
                            &app.current_response,
                            cols[1],
                            &mut dummy_rect,
                            &mut dummy_cnt,
                            app.left_follow_output,
                            &mut app.left_scroll,
                            tui_render::Pane::Left,
                            app.scroll_flash_timer,
                            left_highlight,
                            true,
                        );
                    } else {
                        tui_render::draw_wiki_viewer(f, content_area, &app.wiki_viewer);
                    }
                } else {
                    tui_render::draw_content_desktop(
                    f, content_area, &app.desktop,
                    &tui_render::WorkspaceDrawData {
                        left_committed: &app.left_committed,
                        current_response: &app.current_response,
                        trace_lines: &app.trace_lines,
                        current_thinking: &app.current_thinking,
                        left_scroll: app.left_scroll,
                        right_scroll: app.right_scroll,
                        left_focused, right_focused,
                        scroll_flash_timer: app.scroll_flash_timer,
                        left_highlight, right_highlight,
                        plan: if app.plan.active || agent_mode == "plan" { Some(&app.plan) } else { None },
                    },
                    &tui_render::SplashData {
                        raven_art: &app.raven_art, base_url: &config.base_url,
                        model: &app.display_model, workspace: &workspace_display,
                        splash_focus: app.splash_focus,
                    },
                    &tui_render::PickerDrawData {
                        picker_items: &app.picker.picker_items,
                        selected_item: app.picker.selected_item,
                        focus: app.picker.focus,
                        summary: &app.picker.summary,
                        summary_scroll: app.picker.summary_scroll,
                        wiki_links: &app.picker.wiki_links,
                        active_link_idx: app.picker.active_link_idx,
                        summary_action: app.picker.summary_action,
                        view_focus: app.view_focus,
                        browser_nav_items: &app.browser_nav_items,
                        browser_selected_nav: app.browser_selected_nav,
                        browser_wiki_content: &app.browser_wiki_content,
                        browser_wiki_scroll: app.browser_wiki_scroll,
                    },
                    &app.wiki_viewer,
                    &mut app.last_left_area, &mut app.last_right_area,
                    &mut app.last_left_line_count, &mut app.last_right_line_count,
                    &mut app.left_scroll, &mut app.right_scroll,
                    app.left_follow_output, app.right_follow_output,
                );
                }

                if matches!(app.desktop.active, crate::desktop::ActiveDesktop::Overview) && overview_harness {
                    // Draw real upper status bar and input inside the right "content" column of Screen 2
                    // so it appears as the "upper status bar" above conversation when Coding Harness selected.
                    let hcols = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(30), Constraint::Percentage(30), Constraint::Percentage(40)])
                        .split(content_area);
                    let right_col = hcols[2];
                    let srect = ratatui::layout::Rect { x: right_col.x, y: content_area.y, width: right_col.width, height: 1 };
                    tui_render::draw_status_bar(f, srect, &tui_render::StatusBarData {
                        display_model: &app.display_model,
                        balance_label: &app.balance_label,
                        ctx_used_tokens: app.ctx_used_tokens,
                        budget: &app.display_budget,
                        mode_label: &approval_label,
                        agent_mode: &agent_mode,
                        goal_text: &goal_text,
                        search_label: &search_label,
                        tps: app.api_tps,
                    });
                    let i_h = 3u16;
                    let irect = ratatui::layout::Rect {
                        x: right_col.x,
                        y: content_area.y + content_area.height.saturating_sub(i_h),
                        width: right_col.width,
                        height: i_h,
                    };
                    let input_focused = app.focused_pane == Pane::Input;
                    tui_render::draw_input_bar(f, irect, &tui_render::InputBarData {
                        input: &app.input, is_processing: app.is_processing,
                        spinner_tick: app.spinner_tick, search_mode: app.search_mode,
                        focused: input_focused,
                    });
                    if input_focused {
                        app.clamp_cursor();
                        let text_before = &app.input[..app.cursor_pos];
                        let cursor_line = text_before.matches('\n').count() as u16;
                        let last_nl = text_before.rfind('\n').map(|i| i + 1).unwrap_or(0);
                        let cursor_col = app.input[last_nl..app.cursor_pos].chars().count() as u16;
                        f.set_cursor_position((irect.x + 1 + cursor_col, irect.y + 1 + cursor_line));
                    }
                }

                if show_gauge && !app.desktop.showing_wiki_viewer() && !matches!(app.desktop.active, crate::desktop::ActiveDesktop::Overview) {
                    tui_render::draw_context_gauge(f, gauge_area, &tui_render::ContextGaugeData {
                        turn_rounds: app.turn_rounds, max_rounds: config.max_rounds,
                        tool_calls_this_turn: app.tool_calls_this_turn,
                    });
                }

                if show_input && !app.desktop.showing_wiki_viewer() && !matches!(app.desktop.active, crate::desktop::ActiveDesktop::Overview) {
                    let input_focused = app.focused_pane == Pane::Input;
                    tui_render::draw_input_bar(f, input_area, &tui_render::InputBarData {
                        input: &app.input, is_processing: app.is_processing,
                        spinner_tick: app.spinner_tick, search_mode: app.search_mode,
                        focused: input_focused,
                    });

                    if !app.desktop.showing_wiki_viewer() && !matches!(app.desktop.active, crate::desktop::ActiveDesktop::Overview) {
                        tui_render::draw_overlays(
                            f, size, input_area, &app.settings, app.pending_approval.as_deref(),
                            &app.slash_commands, &app.input, app.slash_selected,
                            app.mode_menu_active, &app.approval_modes, app.selected_mode_idx,
                            app.agent_mode_menu_active, &app.agent_modes, app.selected_agent_mode_idx,
                        );
                    }

                    if input_focused && !app.desktop.showing_wiki_viewer() && !matches!(app.desktop.active, crate::desktop::ActiveDesktop::Overview) {
                        app.clamp_cursor();
                        let text_before = &app.input[..app.cursor_pos];
                        let cursor_line = text_before.matches('\n').count() as u16;
                        let last_nl = text_before.rfind('\n').map(|i| i + 1).unwrap_or(0);
                        let cursor_col = app.input[last_nl..app.cursor_pos].chars().count() as u16;
                        f.set_cursor_position((input_area.x + 1 + cursor_col, input_area.y + 1 + cursor_line));
                    }
                }

                // For Overview (Screen 2), the status/conv/input for harness case are drawn inside the content column (see tui_render).
                // No outer full-width bars for the diorama.

                if app.scroll_flash_timer > 0 { app.scroll_flash_timer -= 1; }
                if app.desktop.tick() { app.needs_redraw = true; }
            }).ok();
        }

        // Input
        let input_ev = tokio::time::timeout(Duration::from_millis(30), input_rx.recv()).await;
        if let Ok(Some(ev)) = input_ev {
            if let Event::Key(k) = &ev {
                if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                    stop_signal.store(true, Ordering::SeqCst);
                    break;
                }
                // Handle tool approval popup (Y/N) before general input
                if app.handle_approval_key(*k) {
                    app.needs_redraw = true;
                    continue;
                }
            }
            let _ = input_handler::handle_key_event(
                &mut app, ev, &config, &mut keystore, &agent, &balance_tx,
                &queued_interject, &instant_interject, &stop_signal, tx.clone(), approval_req_tx.clone(),
            ).await;
            app.needs_redraw = true;
        }

        if stop_signal.load(Ordering::SeqCst) && !app.is_processing {
            break;
        }
    }

    Ok(())
}

// Supporting helpers (duplicates removed from tui_app)
async fn refresh_balance_label(agent: &Arc<tokio::sync::Mutex<Agent>>, tx: &mpsc::Sender<String>) {
    let (base_url, api_key) = {
        let ag = agent.lock().await;
        let cfg = ag.current_config();
        (cfg.base_url.clone(), cfg.api_key.clone())
    };
    let label = raven_tui::llm::balance_label_for(&base_url, api_key.as_deref()).await;
    let _ = tx.send(label).await;
}

#[allow(dead_code)]
fn schedule_balance_refresh(agent: &Arc<tokio::sync::Mutex<Agent>>, tx: &mpsc::Sender<String>) {
    let agent2 = agent.clone();
    let btx = tx.clone();
    tokio::spawn(async move { refresh_balance_label(&agent2, &btx).await; });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_final_text_for_pane_drops_pure_tool_xml_from_tool_response() {
        // Simulate a "final_text" that came from a tool-using turn where the
        // model (or XML fallback path) put the raw tool call into the content.
        // No live inference — we just feed the string that would reach the
        // Done handler.
        let xml_from_tool_response = r#"<tool_call>
<function=browse>
<parameter=url>https://arxiv.org/abs/2607.01224</parameter>
</function>
</tool_call>"#;

        // This exercises the exact filter used by the conversation-pane commit
        // component. If we ever stop calling the strip (or reintroduce the old
        // "else use raw" fallback), this test will fail.
        assert!(
            clean_final_text_for_pane(xml_from_tool_response).is_none(),
            "pure XML from a tool response must be filtered out before touching left_committed"
        );
    }

    #[test]
    fn clean_final_text_for_pane_keeps_narrative_but_drops_xml() {
        let mixed = "I should search arXiv for related papers.\n<tool_call>\n<function=browse>\n<parameter=url>https://arxiv.org/abs/2607.01224</parameter>\n</function>\n</tool_call>";

        let result = clean_final_text_for_pane(mixed)
            .expect("should keep the narrative text");

        assert!(result.contains("I should search arXiv"));
        assert!(!result.contains("<tool_call"));
        assert!(!result.contains("function=browse"));
    }
}