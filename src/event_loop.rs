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
    if let Err(err) = res { eprintln!("TUI error: {:?}", err); }
    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    config: Config,
    chat_backend: ChatBackend,
    mut keystore: Keystore,
) -> Result<()> {
    let agent = Arc::new(tokio::sync::Mutex::new(Agent::new(config.clone(), chat_backend)));
    let mut app = App::new(&config);

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
    let (approval_req_tx, _approval_req_rx) = mpsc::channel::<(String, oneshot::Sender<bool>)>(4);
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
        exec_mode: agent.lock().await.current_exec_mode().clone(),
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
                    app.right_follow_output = true;
                    app.right_scroll = 10_000;
                    app.needs_redraw = true;
                }
                UiUpdate::Done { final_text } => {
                    let cleaned = raven_tui::llm::strip_xml_tool_call_blocks(&final_text);
                    let to_commit = if !cleaned.trim().is_empty() { cleaned } else { final_text };
                    if !to_commit.trim().is_empty() {
                        app.left_committed.push(to_commit);
                    }
                    if !app.current_thinking.is_empty() {
                        for line in app.current_thinking.lines() {
                            // Never prepend brain icon to tool call / debug lines.
                            // Only real thinking gets 🧠 ; tool output uses 🔧 on first line + indented ↳ .
                            let l = line.trim_start();
                            let settled = if l.starts_with("🔧") || l.starts_with("   ↳") || l.starts_with("⭐") {
                                line.to_string()
                            } else {
                                format!("🧠 {}", line)
                            };
                            app.trace_lines.push(settled);
                        }
                        app.current_thinking.clear();
                    }
                    app.current_response.clear();
                    app.is_processing = false;
                    app.needs_redraw = true;
                }
                UiUpdate::Error(e) => {
                    app.left_committed.push(format!("Error: {}", e));
                    app.is_processing = false;
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
                _ => {}
            }
        }

        // Balance updates
        while let Ok(label) = balance_rx.try_recv() {
            app.balance_label = label;
            app.needs_redraw = true;
        }

        // Defensive cleanup: never allow brain icon prepended to tool call/debug lines
        // (🔧 start or indented ↳ result). Ensures single tool icon on first line of tool output.
        for line in &mut app.trace_lines {
            if line.starts_with("🧠") {
                let rest = line.trim_start_matches("🧠").trim_start();
                if rest.starts_with("🔧") || rest.starts_with("   ↳") {
                    *line = rest.to_string();
                }
            }
        }

        if app.is_processing {
            app.spinner_tick = app.spinner_tick.wrapping_add(1);
        }

        if matches!(app.desktop.active, crate::desktop::ActiveDesktop::Picker) && !app.picker.loaded {
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

        let workspace_display = config.workspace.display().to_string();
        let show_gauge = app.is_processing;
        let gauge_h = if show_gauge { 1 } else { 0 };
        let show_status = matches!(app.desktop.active, crate::desktop::ActiveDesktop::Workspace);
        let show_input = show_status;
        let status_h = if show_status { 1 } else { 0 };
        let input_line_count = if show_input { app.input.lines().count().max(1) as u16 } else { 0 };
        let input_h = if show_input {
            (input_line_count + 2).clamp(3, 8)
        } else {
            0
        };

        // Draw
        if app.needs_redraw || app.is_processing || app.scroll_flash_timer > 0 || app.desktop.is_animating() {
            app.needs_redraw = false;

            let search_label = match app.desktop.active {
                crate::desktop::ActiveDesktop::Splash => "→ picker".to_string(),
                crate::desktop::ActiveDesktop::Picker => "← splash".to_string(),
                _ => app.search.status_label(),
            };

            terminal.draw(|f| {
                let size = f.area();
                let vertical = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(status_h), Constraint::Min(6), Constraint::Length(gauge_h), Constraint::Length(input_h)])
                    .split(size);

                let status_area = vertical[0];
                let content_area = vertical[1];
                let gauge_area = vertical[2];
                let input_area = vertical[3];

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
                    },
                    &tui_render::SplashData {
                        raven_art: &app.raven_art, base_url: &config.base_url,
                        model: &app.display_model, workspace: &workspace_display,
                    },
                    &tui_render::PickerDrawData {
                        workspaces: &app.picker.workspaces,
                        selected_workspace: app.picker.selected_workspace,
                        sessions: &app.picker.sessions,
                        selected_session: app.picker.selected_session,
                        focus: app.picker.focus,
                    },
                    &mut app.last_left_area, &mut app.last_right_area,
                    &mut app.last_left_line_count, &mut app.last_right_line_count,
                    &mut app.left_scroll, &mut app.right_scroll,
                    app.left_follow_output, app.right_follow_output,
                );

                if show_gauge {
                    tui_render::draw_context_gauge(f, gauge_area, &tui_render::ContextGaugeData {
                        turn_rounds: app.turn_rounds, max_rounds: config.max_rounds,
                        tool_calls_this_turn: app.tool_calls_this_turn,
                    });
                }

                if show_input {
                    tui_render::draw_input_bar(f, input_area, &tui_render::InputBarData {
                        input: &app.input, is_processing: app.is_processing,
                        spinner_tick: app.spinner_tick, search_mode: app.search_mode,
                        focused: app.focused_pane == Pane::Input,
                    });

                    tui_render::draw_overlays(
                        f, size, input_area, &app.settings, app.pending_approval.as_deref(),
                        &app.slash_commands, &app.input, app.slash_selected,
                        app.mode_menu_active, &app.approval_modes, app.selected_mode_idx,
                        app.agent_mode_menu_active, &app.agent_modes, app.selected_agent_mode_idx,
                    );

                    app.clamp_cursor();
                    let text_before = &app.input[..app.cursor_pos];
                    let cursor_line = text_before.matches('\n').count() as u16;
                    let last_nl = text_before.rfind('\n').map(|i| i + 1).unwrap_or(0);
                    let cursor_col = app.input[last_nl..app.cursor_pos].chars().count() as u16;
                    f.set_cursor_position((input_area.x + 1 + cursor_col, input_area.y + 1 + cursor_line));
                }

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
            }
            let _ = input_handler::handle_key_event(
                &mut app, ev, &config, &mut keystore, &agent, &balance_tx,
                &queued_interject, &instant_interject, &stop_signal, tx.clone(), approval_req_tx.clone(),
            ).await;
            app.needs_redraw = true;
        }

        if stop_signal.load(Ordering::SeqCst) { break; }
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